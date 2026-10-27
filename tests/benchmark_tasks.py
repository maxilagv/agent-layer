#!/usr/bin/env python3
"""Measure task latency, memory, tokens and cost against the local daemon.

The harness records the conditions of every run (hardware, versions,
configuration, load, model, source and binary hashes) so that two runs are
only compared when they happened under the same conditions.

Provider prices must be configured before a live run. The projected cost of
the next request is computed before it is sent, and the run stops before the
budget would be exceeded; a live run defaults to USD 2. A sample that cannot
reach a statistically useful size publishes p95 as "not qualified" instead of
a number. `--billed-usd` records the provider invoice figure next to the
estimate.

Local fixture models measure the engine. Their numbers are labelled as
fixture measurements and never presented as provider latency.

Usage:
  python3 tests/benchmark_tasks.py --home ~/.layeragent --samples 5
  python3 tests/benchmark_tasks.py --home ~/.layeragent --samples 20 \
      --budget-usd 2 --model minimax --output docs/task-benchmark.json
  python3 tests/benchmark_tasks.py --compare docs/task-benchmark.json \
      docs/task-benchmark-new.json
"""
import argparse
import hashlib
import json
import os
import pathlib
import platform
import shutil
import socket
import statistics
import subprocess
import sys
import tempfile
import time
import uuid

TERMINAL = {'completed', 'failed', 'cancelled', 'blocked'}
CASES = ('small_project', 'large_repository', 'four_sessions', 'long_work', 'search_cache')
# A p95 needs more than a handful of samples to mean anything.
MIN_QUALIFIED_SAMPLES = 20


def decode_body(header, payload):
    """Decode a response body, including the chunked transfer encoding."""
    if b'transfer-encoding: chunked' not in header.lower():
        return payload
    body = b''
    rest = payload
    while True:
        line, _, rest = rest.partition(b'\r\n')
        try:
            size = int(line.split(b';')[0].strip(), 16)
        except ValueError:
            return body + rest
        if size == 0:
            return body
        body += rest[:size]
        rest = rest[size + 2 :]


def daemon_request(path, method='GET', route='/health', body=None, headers=None, socket_path=None):
    """One HTTP/1.1 request over the profile socket."""
    payload = b'' if body is None else json.dumps(body).encode()
    lines = [
        f'{method} {route} HTTP/1.1',
        'Host: localhost',
        f'Content-Length: {len(payload)}',
        'Connection: close',
    ]
    for name, value in (headers or {}).items():
        lines.append(f'{name}: {value}')
    if body is not None:
        lines.append('Content-Type: application/json')
    request = ('\r\n'.join(lines) + '\r\n\r\n').encode() + payload
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(120)
        client.connect(str(socket_path))
        client.sendall(request)
        chunks = []
        while True:
            piece = client.recv(65536)
            if not piece:
                break
            chunks.append(piece)
    raw = b''.join(chunks)
    header, _, payload_bytes = raw.partition(b'\r\n\r\n')
    status = 0
    for line in header.split(b'\r\n'):
        if line.startswith(b'HTTP/'):
            try:
                status = int(line.split(b' ')[1])
            except (IndexError, ValueError):
                status = 0
    payload_bytes = decode_body(header, payload_bytes)
    try:
        decoded = json.loads(payload_bytes.decode() or 'null')
    except json.JSONDecodeError:
        decoded = payload_bytes.decode(errors='replace')
    return status, decoded


def percentile(values, fraction):
    """Nearest-rank percentile; a single sample never claims a p95."""
    if not values:
        return None
    ordered = sorted(values)
    if len(ordered) == 1:
        return float(ordered[0])
    rank = max(1, min(len(ordered), int(round(fraction * len(ordered) + 0.5))))
    return float(ordered[rank - 1])


def sha256_file(path):
    digest = hashlib.sha256()
    with open(path, 'rb') as handle:
        for block in iter(lambda: handle.read(65536), b''):
            digest.update(block)
    return digest.hexdigest()


def source_fingerprint(root):
    sys.path.insert(0, str(root / 'scripts'))
    try:
        from source_fingerprint import fingerprint
        return fingerprint(root)
    except Exception as error:  # noqa: BLE001 - reported, never fatal
        return {'source_sha256': None, 'error': str(error)}


def command_version(command, arguments=('--version',)):
    executable = shutil.which(command)
    if not executable:
        # The Rust toolchain is often installed outside the default PATH.
        candidate = pathlib.Path.home() / '.cargo' / 'bin' / command
        if candidate.exists():
            executable = str(candidate)
    if not executable:
        return None
    try:
        output = subprocess.run(
            [executable, *arguments], capture_output=True, text=True, timeout=20
        )
        return (output.stdout or output.stderr).strip().splitlines()[0]
    except Exception:  # noqa: BLE001 - a missing version is recorded as absent
        return None


def hardware():
    info = {
        'platform': platform.platform(),
        'machine': platform.machine(),
        'python': platform.python_version(),
        'logical_cpus': os.cpu_count(),
    }
    try:
        text = pathlib.Path('/proc/cpuinfo').read_text()
        for line in text.splitlines():
            if line.startswith('model name'):
                info['cpu'] = line.split(':', 1)[1].strip()
                break
    except OSError:
        info['cpu'] = None
    try:
        for line in pathlib.Path('/proc/meminfo').read_text().splitlines():
            if line.startswith('MemTotal:'):
                info['memory_total_kb'] = int(line.split()[1])
                break
    except OSError:
        info['memory_total_kb'] = None
    try:
        info['load_average'] = [round(value, 2) for value in os.getloadavg()]
    except OSError:
        info['load_average'] = None
    return info


def prepare_workspace(case, root):
    """Build the fixture workspace of one case."""
    workspace = root / case
    workspace.mkdir(parents=True, exist_ok=True)
    if case == 'large_repository':
        for index in range(400):
            directory = workspace / f'package{index // 20}'
            directory.mkdir(exist_ok=True)
            (directory / f'module{index}.txt').write_text(
                f'module {index}\n' + ('content line\n' * 200)
            )
    elif case == 'small_project':
        (workspace / 'README.md').write_text('# small project\n')
        (workspace / 'main.txt').write_text('hello\n')
    elif case == 'long_work':
        for index in range(40):
            (workspace / f'note{index}.txt').write_text(f'note {index}\n' + ('x' * 4096))
    else:
        (workspace / 'note.txt').write_text('note\n')
    return workspace


def objective_for(case):
    return {
        'small_project': 'List the files of this project and summarize them.',
        'large_repository': 'Describe the structure of this repository.',
        'four_sessions': 'Describe the project in one sentence.',
        'long_work': 'Read the notes of this project and summarize them.',
        'search_cache': 'Search the project memory for prior decisions.',
    }[case]


def run_task(socket_path, workspace, objective, model, timeout=600):
    """Submit one task and wait for a terminal state."""
    key = f'benchmark-{uuid.uuid4()}'
    status, body = daemon_request(
        '/v1/tasks',
        method='POST',
        route='/v1/tasks',
        body={
            'workspace': str(workspace),
            'objective': objective,
            'model': model,
            'executor': 'local',
            'idempotency_key': key,
        },
        headers={'Idempotency-Key': key},
        socket_path=socket_path,
    )
    if status not in (200, 201) or not isinstance(body, dict) or 'id' not in body:
        return {'error': 'task submission failed', 'status': status, 'body': body}
    task_id = body['id']
    deadline = time.monotonic() + timeout
    task = body
    while time.monotonic() < deadline:
        status, task = daemon_request(
            '/v1/tasks/' + task_id, route=f'/v1/tasks/{task_id}', socket_path=socket_path
        )
        if isinstance(task, dict) and task.get('state') in TERMINAL:
            break
        time.sleep(0.25)
    metrics_status, metrics = daemon_request(
        '/v1/tasks/' + task_id + '/metrics',
        route=f'/v1/tasks/{task_id}/metrics',
        socket_path=socket_path,
    )
    return {
        'task_id': task_id,
        'task': task if isinstance(task, dict) else {},
        'metrics': metrics.get('metrics') if isinstance(metrics, dict) else None,
        'timing': metrics.get('timing') if isinstance(metrics, dict) else None,
        'metrics_status': metrics_status,
    }


def model_prices(config, alias):
    models = config.get('models') or {}
    model = models.get(alias) or {}
    input_price = model.get('input_per_million')
    output_price = model.get('output_per_million')
    provider_name = model.get('provider')
    providers = config.get('providers') or {}
    base_url = (providers.get(provider_name) or {}).get('base_url')
    return input_price, output_price, base_url, bool(model)


def is_local_fixture(base_url):
    if not base_url:
        return False
    return '127.0.0.1' in base_url or 'localhost' in base_url or '::1' in base_url


class Budget:
    """Projected cost guard for a live run."""

    def __init__(self, limit_usd, input_price, output_price, cap_output_tokens):
        self.limit_usd = limit_usd
        self.input_price = input_price
        self.output_price = output_price
        self.cap_output_tokens = cap_output_tokens
        self.expected_input_tokens = 4096
        self.spent_usd = 0.0
        self.requests = 0
        self.exhausted = False
        self.reason = None

    def projected_next(self, input_tokens):
        if self.input_price is None or self.output_price is None:
            return None
        return (
            input_tokens * self.input_price + self.cap_output_tokens * self.output_price
        ) / 1_000_000.0

    def allows(self, input_tokens=4096):
        if self.limit_usd is None:
            return True
        # Reserve against the larger of the previous prompt and a small floor,
        # so a prompt that grew still uses a bounded estimate.
        expected = max(input_tokens, self.expected_input_tokens)
        projected = self.projected_next(expected)
        if projected is None:
            return True
        if self.spent_usd + projected > self.limit_usd:
            self.exhausted = True
            self.reason = (
                'the projected cost of the next request would exceed the budget'
            )
            return False
        return True

    def record(self, usage):
        self.requests += 1
        if not isinstance(usage, dict):
            return
        input_tokens = usage.get('input_tokens') or 0
        output_tokens = usage.get('output_tokens') or 0
        if input_tokens:
            self.expected_input_tokens = max(4096, int(input_tokens * 1.25))
        cost = usage.get('cost_usd')
        if self.input_price is not None and self.output_price is not None:
            computed = (
                input_tokens * self.input_price + output_tokens * self.output_price
            ) / 1_000_000.0
            cost = computed if cost is None else max(float(cost), computed)
        self.spent_usd += float(cost or 0.0)


def measure_case(case, socket_path, workspace_root, model, samples, budget, cap_output_tokens):
    """Run one case and summarize its measurements."""
    observations = []
    workspace = prepare_workspace(case, workspace_root)
    for _ in range(samples):
        if budget is not None and not budget.allows(4096):
            break
        started = time.perf_counter()
        result = run_task(socket_path, workspace, objective_for(case), model)
        elapsed = (time.perf_counter() - started) * 1000
        metrics = result.get('metrics') or {}
        usage = (result.get('task') or {}).get('usage') or {}
        if budget is not None:
            budget.record({**usage, 'cost_usd': usage.get('cost_usd')})
        observations.append(
            {
                'wall_ms': round(elapsed, 2),
                'total_ms': metrics.get('total_ms'),
                'queue_ms': metrics.get('queue_ms'),
                'engine_ms': metrics.get('engine_ms'),
                'tool_ms': metrics.get('tool_ms'),
                'provider_ms': metrics.get('provider_ms'),
                'network_ms': metrics.get('network_ms'),
                'first_content_ms': metrics.get('first_content_ms'),
                'max_rss_bytes': metrics.get('max_rss_bytes'),
                'cpu_ms': metrics.get('cpu_ms'),
                'input_tokens': usage.get('input_tokens'),
                'output_tokens': usage.get('output_tokens'),
                'cost_usd': usage.get('cost_usd'),
                'state': (result.get('task') or {}).get('state'),
                'error': result.get('error'),
            }
        )
    totals = [item['total_ms'] for item in observations if item.get('total_ms')]
    walls = [item['wall_ms'] for item in observations]
    states = [item.get('state') for item in observations]
    qualified = len(observations) >= MIN_QUALIFIED_SAMPLES
    reasons = []
    if not qualified:
        reasons.append(
            f'fewer than {MIN_QUALIFIED_SAMPLES} samples were measured'
        )
    if budget is not None and budget.exhausted:
        reasons.append(budget.reason)
    return {
        'case': case,
        'samples': observations,
        'sample_count': len(observations),
        'p50_ms': percentile(totals or walls, 0.50),
        'p95_ms': percentile(totals or walls, 0.95) if qualified else 'not qualified',
        'p95_reason': reasons or None,
        'max_rss_bytes': max(
            [item['max_rss_bytes'] for item in observations if item.get('max_rss_bytes')]
            or [0]
        ),
        'cpu_ms_total': sum(
            item['cpu_ms'] for item in observations if item.get('cpu_ms')
        ),
        'terminal_states': sorted({state for state in states if state}),
        'quality': {
            'all_terminal': all(state in TERMINAL for state in states) if states else False,
            'completed': states.count('completed'),
            'failed': states.count('failed'),
        },
    }


def compare(first, second):
    """Compare two reports and refuse an unsound comparison."""
    same_environment = (
        first.get('environment', {}).get('hardware')
        == second.get('environment', {}).get('hardware')
        and first.get('environment', {}).get('model')
        == second.get('environment', {}).get('model')
    )
    result = {'comparable': bool(same_environment), 'cases': {}}
    if not same_environment:
        result['reason'] = (
            'the runs did not happen under the same hardware and model; '
            'the comparison is not qualified'
        )
    for case in CASES:
        left = first.get('cases', {}).get(case, {})
        right = second.get('cases', {}).get(case, {})
        left_p95 = left.get('p95_ms')
        right_p95 = right.get('p95_ms')
        entry = {
            'first_p50_ms': left.get('p50_ms'),
            'second_p50_ms': right.get('p50_ms'),
            'first_p95_ms': left_p95,
            'second_p95_ms': right_p95,
        }
        if isinstance(left_p95, (int, float)) and isinstance(right_p95, (int, float)):
            entry['p95_delta_ms'] = round(right_p95 - left_p95, 2)
        else:
            entry['p95_delta_ms'] = 'not qualified'
        result['cases'][case] = entry
    return result


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--home', default=os.environ.get('LAYERAGENT_HOME'))
    parser.add_argument('--profile', default='default')
    parser.add_argument('--model', default=None)
    parser.add_argument('--samples', type=int, default=3)
    parser.add_argument('--cases', default=','.join(CASES))
    parser.add_argument('--budget-usd', type=float, default=2.0)
    parser.add_argument('--cap-output-tokens', type=int, default=4096)
    parser.add_argument(
        '--billed-usd',
        type=float,
        default=None,
        help='amount actually billed by the provider, to compare with the estimate',
    )
    parser.add_argument(
        '--fixture',
        action='store_true',
        help='label the run as a local fixture measurement (engine only)',
    )
    parser.add_argument('--binary', default='target/release/layeragent')
    parser.add_argument('--output')
    parser.add_argument('--compare', nargs=2, metavar=('BEFORE', 'AFTER'))
    args = parser.parse_args()

    if args.compare:
        before = json.loads(pathlib.Path(args.compare[0]).read_text())
        after = json.loads(pathlib.Path(args.compare[1]).read_text())
        print(json.dumps(compare(before, after), indent=2))
        return 0

    if not args.home:
        print('a runtime home is required', file=sys.stderr)
        return 2
    root = pathlib.Path(__file__).resolve().parents[1]
    socket_path = (
        pathlib.Path(args.home) / 'runtime' / f'{args.profile}.sock'
    )
    if not socket_path.exists():
        print(f'the daemon socket {socket_path} does not exist', file=sys.stderr)
        return 2

    status, config_body = daemon_request(
        '/v1/config', route='/v1/config', socket_path=socket_path
    )
    if status != 200 or not isinstance(config_body, dict):
        print('the daemon did not answer the configuration request', file=sys.stderr)
        return 2
    config = config_body.get('config') or {}
    alias = args.model or config.get('primary')
    input_price, output_price, base_url, model_exists = model_prices(config, alias)

    budget = None
    # A local or unconfigured model measures the engine only. Its numbers are
    # never presented as provider latency.
    fixture = args.fixture or not model_exists or is_local_fixture(base_url)
    if not fixture:
        # A live run needs configured prices and a bounded token budget.
        if input_price is None or output_price is None:
            print(
                'the selected model has no configured prices; a live measurement '
                'requires input_per_million and output_per_million',
                file=sys.stderr,
            )
            return 3
        if args.budget_usd is None or args.budget_usd <= 0:
            print('a live measurement needs a positive --budget-usd', file=sys.stderr)
            return 3
        configured_cap = config.get('max_output_tokens')
        if configured_cap and configured_cap > args.cap_output_tokens:
            print(
                'the configured output token limit exceeds the measurement cap; '
                'lower max_output_tokens before measuring',
                file=sys.stderr,
            )
            return 3
        budget = Budget(
            args.budget_usd, input_price, output_price, args.cap_output_tokens
        )

    cases = [case.strip() for case in args.cases.split(',') if case.strip()]
    results = {}
    with tempfile.TemporaryDirectory(prefix='layeragent-benchmark-') as temporary:
        workspace_root = pathlib.Path(temporary)
        for case in cases:
            results[case] = measure_case(
                case,
                socket_path,
                workspace_root,
                alias,
                args.samples,
                budget,
                args.cap_output_tokens,
            )

    binary = pathlib.Path(args.binary)
    report = {
        'generated_at': time.strftime('%Y-%m-%dT%H:%M:%S%z'),
        'measurement': 'live provider' if not fixture else 'local fixture',
        'environment': {
            'hardware': hardware(),
            'versions': {
                'rustc': command_version('rustc'),
                'cargo': command_version('cargo'),
                'node': command_version('node'),
            },
            'config_revision': config_body.get('revision'),
            'model': alias,
            'provider_base_url': base_url,
            'source': source_fingerprint(root),
            'binary_sha256': sha256_file(binary) if binary.exists() else None,
        },
        'cases': results,
        'budget': None
        if budget is None
        else {
            'limit_usd': budget.limit_usd,
            'spent_usd': round(budget.spent_usd, 6),
            'requests': budget.requests,
            'exhausted': budget.exhausted,
            'reason': budget.reason,
            'cap_input_tokens': config.get('max_input_tokens'),
            'cap_output_tokens': budget.cap_output_tokens,
        },
        'cost': {
            'estimated_usd': round(budget.spent_usd, 6) if budget else None,
            # A billed amount is recorded when the operator supplies the
            # invoice/provider figure; otherwise it stays explicitly absent.
            'billed_usd': args.billed_usd,
            'difference_usd': (
                round(args.billed_usd - budget.spent_usd, 6)
                if budget is not None and args.billed_usd is not None
                else None
            ),
        },
    }
    text = json.dumps(report, indent=2)
    if args.output:
        pathlib.Path(args.output).write_text(text + '\n')
    print(text)
    return 0


if __name__ == '__main__':
    sys.exit(main())
