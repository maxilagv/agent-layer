#!/usr/bin/env python3
"""Opt-in MiniMax through Vercel AI Gateway CLI/stream/persistence check.

Example (costs real provider usage):
  python3 tests/live_provider.py --run --env-file /external/private/.env --tool-check

No shell sourcing of dotenv, credential output, native reasoning output, or
automatic retries. Coding commands run only inside the confined fixture. max_cost_usd is a runtime PER-TURN estimate, not a provider account cap.
Two text turns are mandatory; --tool-check and --coding-check each add at most
one turn if Linux doctor checks permit it. Secret contents and temporary runtime data are never printed.
"""
import argparse
import ast
import hashlib
import json
import os
import pathlib
import re
import sqlite3
import stat
import subprocess
import sys
import tempfile
import uuid

MODEL = 'minimax/minimax-m3.1-flash'
BASE_URL = 'https://ai-gateway.vercel.sh/v1'
CHILD_KEY_ENV = 'LAYERAGENT_LIVE_PROVIDER_KEY'
MAX_ENV_BYTES = 65536


class LiveError(Exception):
    """Only constant, credential-free diagnostics may be raised with this type."""


def parse_env(text, selected):
    """Literal KEY=value syntax only: no evaluation, interpolation or subprocesses."""
    if not re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', selected):
        raise LiveError('invalid credential variable name')
    found = None
    for raw in text.splitlines():
        line = raw.strip()
        if not line or line.startswith('#'):
            continue
        if line.startswith('export '):
            line = line[7:].lstrip()
        name, separator, value = line.partition('=')
        if not separator or name.strip() != selected:
            continue
        if found is not None:
            raise LiveError('duplicate credential assignment')
        value = value.strip()
        if value.startswith(('"', "'")):
            quote = value[0]
            closing = value.find(quote, 1)
            if closing < 0 or (value[closing + 1:].strip() and not value[closing + 1:].lstrip().startswith('#')):
                raise LiveError('invalid quoted credential assignment')
            value = value[1:closing]
        else:
            value = re.split(r'\s+#', value, maxsplit=1)[0].strip()
        if not value or any(character.isspace() or ord(character) < 32 for character in value):
            raise LiveError('empty or invalid credential value')
        found = value
    if found is None:
        raise LiveError('credential variable is missing')
    return found


def load_external_key(path, selected):
    root = pathlib.Path(__file__).resolve().parents[1]
    source = pathlib.Path(path).expanduser().absolute()
    try:
        if source.resolve().is_relative_to(root):
            raise LiveError('credential input must be outside the project')
        descriptor = os.open(source, os.O_RDONLY | os.O_NOFOLLOW | os.O_NONBLOCK)
        with os.fdopen(descriptor, 'rb') as handle:
            info = os.fstat(handle.fileno())
            if not stat.S_ISREG(info.st_mode) or info.st_uid != os.getuid() or info.st_mode & 0o077:
                raise LiveError('credential file must be user-owned, regular and private (0600 or 0400)')
            raw = handle.read(MAX_ENV_BYTES + 1)
            if len(raw) > MAX_ENV_BYTES:
                raise LiveError('credential file exceeds the input limit')
        return parse_env(raw.decode('utf-8'), selected)
    except (OSError, UnicodeError):
        raise LiveError('cannot securely read external credential file') from None


def config(tools=False, coding=False):
    return f'''schema_version = 1
primary = "minimax_live"
fallbacks = []
max_rounds = {8 if coding else 4}
max_input_tokens = 16384
max_output_tokens = 4096
max_cost_usd = 0.10
auto_skills = false
instructions = "Run only the requested minimal validation. Never disclose credentials or native reasoning."

[providers.minimax_live]
wire = "openai_chat"
base_url = "{BASE_URL}"
key_env = "{CHILD_KEY_ENV}"

[models.minimax_live]
provider = "minimax_live"
id = "{MODEL}"
context_window = 32768
tools = {str(tools or coding).lower()}
emulate_tools = false
input_per_million = 0.15
output_per_million = 0.60

[models.minimax_live.parameters]
reasoning_effort = "low"
max_tokens = 4096
stream_options = {{ include_usage = true }}

[policy]
shell = {str(coding).lower()}
write = {str(coding).lower()}
network = false
allowed_tools = {json.dumps(["read_file", "write_file", "shell"] if coding else ["read_file"])}
approval_tools = []
protected_paths = {json.dumps(["test_calculator.py"] if coding else [])}
timeout_secs = 30
max_output_bytes = 65536

[browser]
enabled = false
auto_install = false

[agents]
enabled = false

[memory]
embeddings = false

[privacy]
local_models_only = false
allowed_providers = ["minimax_live"]
'''


CODING_COMMAND = 'python3 -I -S -B test_calculator.py'
FIXED_SOURCE = 'def add_one(value):\n    return value + 1\n'


def sha256_file(path):
    digest = hashlib.sha256()
    with path.open('rb') as handle:
        for block in iter(lambda: handle.read(1_048_576), b''):
            digest.update(block)
    return digest.hexdigest()


def source_provenance():
    root = pathlib.Path(__file__).resolve().parents[1]
    sys.path.insert(0, str(root / 'scripts'))
    from source_fingerprint import fingerprint
    return fingerprint(root)


def coding_fixture(workspace, proof):
    source = workspace / 'calculator.py'
    tests = workspace / 'test_calculator.py'
    source.write_text('def add_one(value):\n    return value - 1\n')
    tests.write_text(
        "import pathlib\nimport runpy\n"
        "add_one = runpy.run_path('calculator.py')['add_one']\n"
        "for value in (-17, -1, 0, 1, 9, 12345):\n"
        "    assert add_one(value) == value + 1\n"
        + "pathlib.Path('coding_result.txt').write_text(" + repr(proof) + ")\n"
        + "print(" + repr('CODING_TESTS_OK:' + proof) + ")\n")
    return sha256_file(tests)


def verify_coding_fixture(workspace, tests_hash, proof, events):
    source = workspace / 'calculator.py'
    tests = workspace / 'test_calculator.py'
    marker = workspace / 'coding_result.txt'
    if any(path.is_symlink() or not path.is_file() for path in (source, tests, marker)):
        raise LiveError('coding fixture contains missing or symlinked evidence')
    if sha256_file(tests) != tests_hash:
        raise LiveError('coding test fixture was modified')
    if source.stat().st_size > 8192 or marker.stat().st_size > 128:
        raise LiveError('coding evidence exceeds fixture size limits')
    try:
        # Parse the original bytes exactly as Python/runpy do, including encoding
        # declarations. Parsing predecoded text could hide executable statements.
        actual = ast.dump(ast.parse(source.read_bytes()), include_attributes=False)
        expected = ast.dump(ast.parse(FIXED_SOURCE), include_attributes=False)
    except (SyntaxError, UnicodeError):
        raise LiveError('generated source does not parse (details suppressed)') from None
    if actual != expected or marker.read_text() != proof:
        raise LiveError('generated source or confined test marker failed the static contract')
    test_calls = {
        event.get('data', {}).get('id') for event in events
        if event.get('kind') == 'tool_started'
        and event.get('data', {}).get('name') == 'shell'
        and event.get('data', {}).get('arguments', {}).get('command', '').strip() == CODING_COMMAND
    }
    passed = any(
        event.get('kind') == 'tool_completed'
        and event.get('data', {}).get('call_id') in test_calls
        and event.get('data', {}).get('result', {}).get('code') == 0
        and 'CODING_TESTS_OK:' + proof in event.get('data', {}).get('result', {}).get('stdout', '')
        and not event.get('data', {}).get('result', {}).get('truncated', False)
        and not event.get('data', {}).get('result', {}).get('interrupted', False)
        for event in events)
    reads = any(event.get('kind') == 'tool_started' and event.get('data', {}).get('name') == 'read_file'
                and event.get('data', {}).get('arguments', {}).get('path') == 'calculator.py' for event in events)
    if not passed or not reads:
        raise LiveError('coding turn lacks a source read and successful exact confined test invocation')
    return {'status': 'passed', 'shell_exit_code': 0, 'tests_unchanged': True,
            'source_static_contract': 'exact add_one(value) returns value+1',
            'generated_code_executed_on_host': False,
            'test_source_sha256': tests_hash, 'fixed_source_sha256': sha256_file(source)}


def write_report(path, report, env_file, binary):
    destination = pathlib.Path(path).expanduser().absolute()
    root = pathlib.Path(__file__).resolve().parents[1]
    resolved = destination.resolve()
    if resolved in (pathlib.Path(env_file).expanduser().resolve(), binary.resolve()):
        raise LiveError('report cannot overwrite credential input or executable')
    if resolved.is_relative_to(root) and not resolved.is_relative_to(root / 'target'):
        raise LiveError('report inside the project must be under target/')
    destination.parent.mkdir(parents=True, exist_ok=True)
    descriptor = os.open(destination, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(descriptor, 'w') as handle:
        json.dump(report, handle, indent=2)
        handle.write('\n')


def self_test():
    import unittest

    class ParserTests(unittest.TestCase):
        def test_literals(self):
            for text in ['KEY=fixture', 'export KEY="fixture" # comment', "KEY='fixture'", '# ignored\nOTHER=value\nKEY=fixture # note']:
                self.assertEqual(parse_env(text, 'KEY'), 'fixture')
            self.assertEqual(parse_env('KEY=$(never-executed)', 'KEY'), '$(never-executed)')
            self.assertEqual(parse_env('KEY=`never-executed`', 'KEY'), '`never-executed`')

        def test_invalid(self):
            for text in ['KEY=', 'OTHER=fixture', 'KEY=one\nKEY=two', 'KEY="unterminated', 'KEY="fixture" trailing', 'KEY=with space']:
                with self.assertRaises(LiveError):
                    parse_env(text, 'KEY')
            with self.assertRaises(LiveError):
                parse_env('KEY=fixture', 'KEY;unsafe')

        def test_private_external_fixture(self):
            with tempfile.TemporaryDirectory(prefix='layeragent-live-parser-') as directory:
                path = pathlib.Path(directory) / 'fixture.env'
                path.write_text('KEY=not-a-real-credential\n')
                path.chmod(0o600)
                self.assertEqual(load_external_key(path, 'KEY'), 'not-a-real-credential')
                path.chmod(0o644)
                with self.assertRaises(LiveError):
                    load_external_key(path, 'KEY')
                link = pathlib.Path(directory) / 'link.env'
                link.symlink_to(path)
                with self.assertRaises(LiveError):
                    load_external_key(link, 'KEY')

        def test_fifo_is_rejected_without_waiting_for_a_writer(self):
            with tempfile.TemporaryDirectory(prefix='layeragent-live-parser-') as directory:
                path = pathlib.Path(directory) / 'fixture.pipe'
                os.mkfifo(path, 0o600)
                with self.assertRaises(LiveError):
                    load_external_key(path, 'KEY')

        def test_configuration_limits(self):
            import tomllib
            value = tomllib.loads(config())
            self.assertEqual(value['models']['minimax_live']['id'], MODEL)
            self.assertEqual(value['max_output_tokens'], 4096)
            self.assertEqual(value['max_rounds'], 4)
            self.assertEqual(value['max_cost_usd'], .10)
            self.assertFalse(value['models']['minimax_live']['tools'])
            coding = tomllib.loads(config(coding=True))
            self.assertEqual(coding['max_rounds'], 8)
            self.assertEqual(coding['policy']['allowed_tools'], ['read_file', 'write_file', 'shell'])
            self.assertEqual(coding['policy']['approval_tools'], [])
            self.assertFalse(coding['policy']['network'])
            self.assertTrue(coding['policy']['write'])
            self.assertIn('test_calculator.py', coding['policy']['protected_paths'])

        def test_coding_static_contract_and_evidence(self):
            with tempfile.TemporaryDirectory(prefix='layeragent-coding-parser-') as directory:
                workspace = pathlib.Path(directory)
                proof = 'dummy-proof'
                tests_hash = coding_fixture(workspace, proof)
                (workspace / 'calculator.py').write_text(FIXED_SOURCE)
                (workspace / 'coding_result.txt').write_text(proof)
                events = [
                    {'kind':'tool_started','data':{'id':'r','name':'read_file','arguments':{'path':'calculator.py'}}},
                    {'kind':'tool_started','data':{'id':'s','name':'shell','arguments':{'command':CODING_COMMAND}}},
                    {'kind':'tool_completed','data':{'call_id':'s','result':{'code':0,'stdout':'CODING_TESTS_OK:'+proof}}}]
                self.assertEqual(verify_coding_fixture(workspace, tests_hash, proof, events)['status'], 'passed')
                (workspace / 'calculator.py').write_text(FIXED_SOURCE + "print('unexpected code')\n")
                with self.assertRaises(LiveError):
                    verify_coding_fixture(workspace, tests_hash, proof, events)
                (workspace / 'calculator.py').write_text(
                    '# coding: unicode_escape\n# \\u000aprint("hidden extra statement")\n' + FIXED_SOURCE)
                with self.assertRaises(LiveError):
                    verify_coding_fixture(workspace, tests_hash, proof, events)
                (workspace / 'calculator.py').write_text(FIXED_SOURCE)
                events[-1]['data']['result']['code'] = 1
                with self.assertRaises(LiveError):
                    verify_coding_fixture(workspace, tests_hash, proof, events)
                events[-1]['data']['result']['code'] = 0
                (workspace / 'test_calculator.py').write_text('modified')
                with self.assertRaises(LiveError):
                    verify_coding_fixture(workspace, tests_hash, proof, events)

        def test_private_report_no_clobber(self):
            with tempfile.TemporaryDirectory(prefix='layeragent-report-parser-') as directory:
                base = pathlib.Path(directory)
                report = base / 'report.json'
                write_report(report, {'status':'dummy'}, base / 'key.env', base / 'binary')
                self.assertEqual(json.loads(report.read_text()), {'status':'dummy'})
                self.assertEqual(stat.S_IMODE(report.stat().st_mode), 0o600)
                with self.assertRaises(FileExistsError):
                    write_report(report, {}, base / 'key.env', base / 'binary')


    result = unittest.TextTestRunner(verbosity=1).run(unittest.defaultTestLoader.loadTestsFromTestCase(ParserTests))
    return 0 if result.wasSuccessful() else 1


def live(args):
    if sys.platform != 'linux':
        raise LiveError('this live validation currently supports Linux only')
    binary = pathlib.Path(args.binary).expanduser().resolve()
    if not binary.is_file():
        raise LiveError('build the real CLI binary first')
    source_before = source_provenance()
    binary_before = sha256_file(binary)
    # This is the sole credential read, reached only with explicit --run --env-file.
    key = load_external_key(args.env_file, args.key_env)
    environment = {name: os.environ[name] for name in ('PATH', 'HOME', 'LANG', 'LC_ALL', 'TERM', 'TMPDIR') if name in os.environ}
    environment[CHILD_KEY_ENV] = key
    with tempfile.TemporaryDirectory(prefix='layeragent-live-minimax-') as directory:
        base = pathlib.Path(directory)
        home, workspace = base / 'state', base / 'workspace'
        home.mkdir(mode=0o700)
        workspace.mkdir(mode=0o700)
        configuration = home / 'config.toml'
        configuration.write_text(config())
        configuration.chmod(0o600)
        command = [str(binary), '--home', str(home)]

        def cli(*arguments, check=True):
            try:
                result = subprocess.run(command + list(arguments), env=environment, capture_output=True, text=True, timeout=150)
            except (OSError, subprocess.TimeoutExpired):
                raise LiveError('CLI execution failed or timed out; provider outcome may be uncertain; no automatic retry') from None
            if key in result.stdout or key in result.stderr:
                raise LiveError('credential leak detected in CLI output (output suppressed)')
            if check and result.returncode:
                status = re.search(r'provider HTTP (\d{3})', result.stderr + result.stdout)
                if status:
                    raise LiveError('provider returned HTTP ' + status[1] + '; no automatic retry')
                raise LiveError('CLI command failed (provider response and native reasoning suppressed); no automatic retry')
            return result

        def turn(*arguments):
            result = cli(*arguments, '--json')
            try:
                events = [json.loads(line) for line in result.stdout.splitlines() if line.strip()]
                final = next(event for event in reversed(events) if isinstance(event.get('text'), str) and event.get('session_id'))
            except (ValueError, StopIteration, AttributeError):
                raise LiveError('CLI did not produce a completed JSON result') from None
            if not any(event.get('kind') == 'text_delta' for event in events):
                raise LiveError('streaming text delta was not observed')
            return events, final

        def assert_no_credential_in_state():
            for path in home.rglob('*'):
                if path.is_file() and not path.is_symlink() and key.encode() in path.read_bytes():
                    raise LiveError('credential leak detected in temporary state (contents suppressed)')

        tool_status = 'not_requested'
        coding_status = {'status': 'not_requested'}
        turns = 0
        usage = []
        try:
            marker = 'LIVE_STREAM_' + uuid.uuid4().hex[:12]
            _, first = turn('run', 'Do not call tools. Reply with exactly this marker, no explanation: ' + marker, '--workspace', str(workspace))
            if marker not in first['text']:
                raise LiveError('first text response did not preserve the requested marker')
            turns += 1
            usage.append(first.get('usage', {}))
            session = first['session_id']
            # Restart the daemon before resuming, so persistence is not only in memory.
            cli('stop')
            _, second = turn('resume', session, 'Do not call tools. Repeat only the exact LIVE_STREAM marker from your previous response.')
            if second['session_id'] != session or marker not in second['text']:
                raise LiveError('resumed response did not recover previous session context')
            turns += 1
            usage.append(second.get('usage', {}))
            ready = False
            if args.tool_check or args.coding_check:
                doctor = cli('doctor', '--json', check=False)
                try:
                    checks = json.loads(doctor.stdout)
                    required = {'user_namespace', 'pid_namespace', 'net_namespace', 'all_namespaces', 'landlock', 'python3'}
                    ready = required.issubset({check['name'] for check in checks if check['available']})
                except (ValueError, KeyError, TypeError):
                    ready = False
            if args.tool_check:
                if not ready:
                    tool_status = 'blocked_host_confinement_not_ready'
                else:
                    cli('stop')
                    configuration.write_text(config(tools=True))
                    proof = 'LIVE_FILE_' + uuid.uuid4().hex[:12]
                    (workspace / 'live_fixture.txt').write_text(proof + '\n')
                    events, third = turn('resume', session, 'Use read_file to read live_fixture.txt. Then reply with only its content. Do not guess; the proof is not in this prompt.')
                    if not any(event.get('kind') == 'tool_started' and event.get('data', {}).get('name') == 'read_file' for event in events):
                        raise LiveError('provider did not call the requested read_file tool')
                    if proof not in third['text']:
                        raise LiveError('tool read did not return the actual fixture proof')
                    turns += 1
                    usage.append(third.get('usage', {}))
                    tool_status = 'passed'
            if args.coding_check:
                if not ready:
                    coding_status = {'status': 'blocked_host_confinement_not_ready'}
                else:
                    cli('stop')
                    configuration.write_text(config(coding=True))
                    proof = 'LIVE_CODING_' + uuid.uuid4().hex[:12]
                    tests_hash = coding_fixture(workspace, proof)
                    events, coding_result = turn('resume', session,
                        'Coding fixture task: read calculator.py and test_calculator.py with read_file. '
                        'Fix only calculator.py: its complete source must define only add_one(value), '
                        'whose sole statement is return value + 1. Use write_file with expected_sha256 from the read. '
                        'Do not edit the tests or create extra implementation files. '
                        'Then run exactly this shell command: ' + CODING_COMMAND + '. '
                        'The tests create their own result marker. Report briefly after they pass.')
                    coding_status = verify_coding_fixture(workspace, tests_hash, proof, events)
                    turns += 1
                    usage.append(coding_result.get('usage', {}))
            cli('stop')
            database = home / 'data/default/tenants/default/state.db'
            with sqlite3.connect(database) as connection:
                completed = connection.execute("SELECT count(*) FROM turns WHERE state='completed'").fetchone()[0]
                sessions = connection.execute('SELECT count(*) FROM sessions').fetchone()[0]
                integrity = connection.execute('PRAGMA integrity_check').fetchone()[0]
                messages = connection.execute('SELECT count(*) FROM messages').fetchone()[0]
                if completed != turns or sessions != 1 or messages < turns * 2 or integrity != 'ok':
                    raise LiveError('durable session verification failed')
            assert_no_credential_in_state()
            source_after = source_provenance()
            binary_after = sha256_file(binary)
            report = {'provider': 'vercel-ai-gateway', 'model': MODEL, 'text_streaming': 'passed',
                      'daemon_restart_persistence': 'passed', 'turns': turns, 'tool_read': tool_status,
                      'coding': coding_status, 'usage': usage, 'max_cost_usd_per_turn': .10,
                      'budget_scope': 'runtime per-turn estimate; not a provider-account or global spend cap',
                      'no_credential_in_output_or_state': True,
                      'provenance': {'binary_sha256_before': binary_before, 'binary_sha256_after': binary_after,
                                     'source_before': source_before, 'source_after': source_after,
                                     'source_unchanged': source_before == source_after,
                                     'binary_unchanged': binary_before == binary_after,
                                     'binary_source_linkage': 'not proven by fingerprints alone'}}
            if args.report:
                write_report(args.report, report, args.env_file, binary)
            print(json.dumps(report))
            return 0
        finally:
            try:
                subprocess.run(command + ['stop'], env=environment, capture_output=True, timeout=10)
            except (OSError, subprocess.TimeoutExpired):
                pass


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument('--run', action='store_true', help='explicit opt-in to paid external provider requests')
    parser.add_argument('--env-file', help='explicit external user-owned private dotenv file (never sourced)')
    parser.add_argument('--key-env', default='AI_GATEWAY_API_KEY', help='literal variable to select from the dotenv file')
    parser.add_argument('--binary', default=str(pathlib.Path(__file__).resolve().parents[1] / 'target/debug/layeragent'))
    parser.add_argument('--tool-check', action='store_true', help='one additional tool turn only if Linux host confinement preflight is ready')
    parser.add_argument('--coding-check', action='store_true', help='one additional confined Python coding/test turn in the temporary fixture; no host execution')
    parser.add_argument('--report', help='optional new private JSON report path; project reports must be under target/')
    parser.add_argument('--self-test', action='store_true', help='offline parser/config tests with generated dummy fixtures; no real credential read')
    args = parser.parse_args()
    if args.self_test:
        if args.run or args.env_file or args.report or args.tool_check or args.coding_check:
            parser.error('--self-test cannot be combined with credential/live arguments')
        return self_test()
    if not args.run or not args.env_file:
        parser.error('paid calls require both --run and an explicit --env-file; use --self-test offline')
    try:
        return live(args)
    except LiveError as error:
        print(json.dumps({'status': 'failed', 'error': str(error), 'no_automatic_retry': True}), file=sys.stderr)
        return 1
    except Exception as error:
        # Third-party errors can contain raw output/credentials: never format them.
        print(json.dumps({'status': 'failed', 'error_type': type(error).__name__, 'details_suppressed': True, 'no_automatic_retry': True}), file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
