#!/usr/bin/env python3
"""Real daemon/CLI with a scripted loopback provider; never makes paid requests.

The default run exercises collaboration and records an inconclusive finding.
--native requires actual confinement and runs the toy gateway negative control.
No test sandbox or unconfined execution fallback is installed.
"""
import argparse
import contextlib
import http.server
import json
import os
import pathlib
import re
import shutil
import signal
import sqlite3
import subprocess
import tempfile
import threading
import time
import uuid

from system_smoke import UnixHTTPConnection


class Scenario:
    def __init__(self, native=False, held=False):
        self.native = native
        self.entered = threading.Event()
        self.release = threading.Event()
        if not held:
            self.release.set()
        self.done = threading.Event()
        self.calls = 0
        self.errors = []

    def decide(self, messages):
        prompts = [m.get('content', '') for m in messages
                   if m.get('role') == 'user' and str(m.get('content', '')).startswith('Cooperative template')]
        assert prompts, 'missing cooperative activation prompt'
        role = re.search(r'You are (\w+)', prompts[-1]).group(1)
        self.calls += 1
        if role == 'coordinator' and not self.entered.is_set():
            self.entered.set()
        # Hold every initial request, not just the coordinator, so controls are
        # tested against a stable in-flight boundary rather than racing fixture
        # mutations that the operator has not yet seen in the first snapshot.
        assert self.release.wait(45), 'fixture gate timed out'

        calls = {c['id']: c['function'] for m in messages for c in m.get('tool_calls', [])}
        results = []
        for m in messages:
            if m.get('role') == 'tool' and m.get('tool_call_id') in calls:
                call = calls[m['tool_call_id']]
                try:
                    result = json.loads(m['content'])
                except json.JSONDecodeError as error:
                    raise AssertionError(f"invalid JSON in {call['name']} fixture result: {m['content']!r}") from error
                results.append((call['name'], json.loads(call['arguments']), result))
                if result.get('error'):
                    self.errors.append((role, call['name'], result['error']))

        def result(name):
            return next((r for n, _, r in reversed(results) if n == name and 'error' not in r), None)

        def sent(purpose):
            return any(n == 'team_message' and a.get('purpose') == purpose and 'error' not in r
                       for n, a, r in results)

        mail = []
        for m in messages:
            body = str(m.get('content', ''))
            if body.startswith('Peer reference message'):
                try:
                    mail.append(json.loads(body.split(': ', 1)[1]))
                except json.JSONDecodeError as error:
                    raise AssertionError(f'invalid JSON in fixture peer message: {body!r}') from error

        def received(purpose):
            return next((m for m in reversed(mail) if m['purpose'] == purpose), None)

        def tool(name, **arguments):
            return name, arguments

        if result('team_initial_assessment') is None:
            return tool('team_initial_assessment', text=f'{role}: fixture assessment; inspect gateway before inferring a bypass.')
        members = result('team_members')
        if members is None:
            return tool('team_members')
        ids = {m['name']: m['id'] for m in members['members']}

        if role == 'coordinator':
            noticed = any(str(m.get('content', '')).startswith('Runtime supervisor notice') for m in messages)
            if noticed and self.done.is_set() and result('team_close') is None:
                return tool('team_close', reason='Fixture investigation completed; limitations retained.')
            return 'Fixture coordinator: waiting for evidence.'

        if role == 'analyst':
            finding = result('team_finding')
            if finding is None:
                return tool('team_finding', claim='Unauthenticated requests may reach storage.',
                            state='hypothesis', reason='Storage has no local authorization check; gateway not yet checked.')
            answer = received('response')
            if answer and result('team_finding_transition') is None:
                evidence = json.loads(answer['body'])
                args = dict(finding_id=finding['id'], expected_revision=1,
                            state='discarded' if self.native else 'inconclusive',
                            reason=('Gateway check rejects anonymous requests and accepts the positive control.'
                                    if self.native else 'Reviewer identified the missing gateway premise; confined check not executed in fixture-only mode.'))
                if self.native:
                    assert evidence.get('expectation_met'), evidence
                    args.update(evidence_ids=[evidence['evidence_id']], snapshot_sha256=evidence['snapshot'])
                return tool('team_finding_transition', **args)
            if result('team_finding_transition') is not None:
                self.done.set()
                return 'Conclusion changed after the objection; evidence and limitations retained.'
            if not sent('question'):
                return tool('team_message', recipient=ids['reviewer'], purpose='question',
                            finding_id=finding['id'], body='Does the gateway authorize before storage?')
            if result('team_wait') is None:
                return tool('team_wait', reason='Waiting for the reviewer and verifier.')
            return 'Analyst waiting for the peer response.'

        if role == 'reviewer':
            question = received('question')
            response = received('response')
            if question and result('team_objection') is None:
                return tool('team_objection', finding_id=question['finding_id'],
                            claim='A missing storage check does not establish a reachable bypass.',
                            check_required='Run gateway(False) and gateway(True) as a negative control.')
            if response and not sent('response'):
                return tool('team_message', recipient=ids['analyst'], purpose='response',
                            reply_to=question['id'], finding_id=question['finding_id'], body=response['body'])
            if question and not sent('verification_request'):
                return tool('team_message', recipient=ids['verifier'], purpose='verification_request',
                            finding_id=question['finding_id'], body='Check anonymous rejection and authenticated positive control.')
            return 'Reviewer waiting for a check, not treating consensus as evidence.'

        if role == 'verifier':
            question = received('verification_request')
            if question and self.native and result('team_verify') is None:
                return tool('team_verify', finding_id=question['finding_id'], command='python3 -B check.py',
                            expected='anonymous=401; authenticated=200', expected_exit_code=0,
                            negative_control='Authenticated request must succeed.',
                            limitations=['Toy local gateway; scripted provider; no real-model quality claim.'])
            if question and not sent('response'):
                evidence = result('team_verify') if self.native else {'qualification': 'fixture-only', 'executed': False}
                return tool('team_message', recipient=ids['reviewer'], purpose='response',
                            reply_to=question['id'], finding_id=question['finding_id'], body=json.dumps(evidence))
            return 'Verifier: no execution receipt is invented.'
        raise AssertionError(role)


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        scenario = self.server.scenario
        try:
            request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
            assert request['model'] == 'cooperative-fixture'
            assert request['stream']
            decision = scenario.decide(request['messages'])
            if isinstance(decision, tuple):
                name, args = decision
                delta = {'tool_calls': [{'index': 0, 'id': 'call-' + uuid.uuid4().hex,
                                        'type': 'function', 'function': {'name': name, 'arguments': json.dumps(args)}}]}
                finish = 'tool_calls'
            else:
                delta, finish = {'content': decision}, 'stop'
            events = [{'choices': [{'delta': delta, 'finish_reason': None}]},
                      {'choices': [{'delta': {}, 'finish_reason': finish}],
                       'usage': {'prompt_tokens': 80, 'completion_tokens': 30}}]
            self.send_response(200)
            self.send_header('Content-Type', 'text/event-stream')
            self.end_headers()
            for event in events:
                self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
            self.wfile.write(b'data: [DONE]\n\n')
        except (BrokenPipeError, ConnectionResetError):
            pass  # Cancellation/restart intentionally disconnects in-flight calls.
        except Exception as error:
            scenario.errors.append(str(error))
            self.send_error(500, 'scripted fixture failed')


def wait_for(check, seconds=45):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(.2)
    raise AssertionError('cooperative scenario timed out')


@contextlib.contextmanager
def environment(binary, native=False, held=True):
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    server.daemon_threads = True
    server.scenario = Scenario(native, held)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='layeragent-cooperative-') as temporary:
        base = pathlib.Path(temporary)
        home, workspace = base / 'state', base / 'workspace'
        workspace.mkdir()
        shutil.copyfile(pathlib.Path(__file__).resolve().parents[1] / 'examples/cooperative-demo/check.py', workspace / 'check.py')
        command = [str(binary), '--home', str(home)]

        def cli(*args, check=True):
            result = subprocess.run(command + list(args), capture_output=True, text=True, timeout=60)
            if check and result.returncode:
                raise AssertionError(f'{args}: {result.stderr}\n{result.stdout}')
            return result

        def api(method, path, body=None, key=None):
            connection = UnixHTTPConnection(str(home / 'runtime/default.sock'))
            headers = {'content-type': 'application/json'}
            if key:
                headers['idempotency-key'] = key
            connection.request(method, path, None if body is None else json.dumps(body), headers)
            response = connection.getresponse()
            raw = response.read()
            connection.close()
            if raw and response.getheader('Content-Type', '').startswith('application/json'):
                return response.status, json.loads(raw)
            return response.status, raw.decode()

        daemon = None
        try:
            cli('init', '--protocol', 'openai_chat', '--model', 'cooperative-fixture', '--base-url',
                f'http://127.0.0.1:{server.server_port}/v1')
            # Own the daemon process so even a failed/rate-limited shutdown cannot
            # leave a detached test daemon after the temporary home is deleted.
            daemon = subprocess.Popen(command + ['serve', '--ipc-only'],
                                      stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL,
                                      start_new_session=True)

            def ready():
                assert daemon.poll() is None, 'fixture daemon exited during startup'
                try:
                    return api('GET', '/health')[0] == 200
                except OSError:
                    return False

            wait_for(ready, seconds=15)
            yield server, home, workspace, command, cli, api
        finally:
            server.scenario.release.set()
            try:
                cli('stop', check=False)
            finally:
                if daemon is not None:
                    if daemon.poll() is None:
                        daemon.terminate()
                    try:
                        daemon.wait(timeout=5)
                    except subprocess.TimeoutExpired:
                        daemon.kill()
                        daemon.wait(timeout=5)
            server.shutdown()
            server.server_close()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='target/debug/layeragent')
    parser.add_argument('--native', action='store_true', help='require actual confined gateway check')
    parser.add_argument('--user-service', action='store_true', help='run --native in an already delegated systemd user service')
    parser.add_argument('--output', type=pathlib.Path, help='write the sanitized demo export to a new file')
    args = parser.parse_args()
    binary = pathlib.Path(args.binary).resolve()
    if args.user_service:
        assert args.native, '--user-service requires --native'
        command = ['systemd-run', '--user', '--wait', '--pipe', '--collect',
                   '--unit=layeragent-cooperative-' + uuid.uuid4().hex,
                   '--working-directory=' + str(pathlib.Path(__file__).resolve().parents[1]),
                   '--property=Delegate=memory pids cpu', '--property=DelegateSubgroup=supervisor',
                   '--property=RuntimeMaxSec=180', 'python3', str(pathlib.Path(__file__).resolve()),
                   '--binary', str(binary), '--native']
        if args.output:
            command.extend(['--output', str(args.output.resolve())])
        raise SystemExit(subprocess.run(command).returncode)
    with environment(binary, args.native) as (server, home, workspace, command, cli, api):
        status, task = api('POST', '/v1/tasks', {
            'workspace': str(workspace), 'objective': 'Review the toy gateway authorization. Scripted fixture.',
            'execution': 'cooperative', 'cooperative': {'limits': {'max_concurrency': 1, 'max_active_seconds': 180}},
        }, 'cooperative-smoke')
        assert status == 201, (status, task)
        task_id = task['id']
        assert server.scenario.entered.wait(20)

        def snapshot():
            status, value = api('GET', f'/v1/tasks/{task_id}/team')
            assert status == 200, value
            return value['team']

        cli('team', 'pause', task_id, '--reason', 'Check safe-boundary pause.')
        assert snapshot()['state'] == 'paused'
        server.scenario.release.set()
        wait_for(lambda: snapshot().get('pause_pending') is False)
        calls = server.scenario.calls
        time.sleep(.25)
        assert server.scenario.calls == calls, 'a provider call started while paused'
        cli('team', 'message', task_id, '--to', 'analyst', 'Keep environmental limits explicit.')
        cli('team', 'resume', task_id)
        wait_for(lambda: snapshot()['state'] in ('terminated', 'failed', 'blocked'))
        assert snapshot()['state'] == 'terminated', (snapshot(), server.scenario.errors)
        assert server.scenario.done.is_set(), server.scenario.errors
        watched = cli('team', 'watch', task_id, '--json')
        assert watched.returncode == 0
        export = json.loads(cli('team', 'export', task_id, '--format', 'json').stdout)
        assert export['findings'][0]['state'] == ('discarded' if args.native else 'inconclusive'), export
        assert len(export['finding_revisions']) == 2
        assert export['objections'] and export['messages'] and export['deliveries']
        assert any(m['sender_kind'] == 'user' for m in export['messages'])
        assert any(d.get('context_seq') is not None for d in export['deliveries'])
        assert export['evidence'] if args.native else not export['evidence']
        markdown = cli('team', 'export', task_id, '--format', 'markdown').stdout
        assert 'gateway' in markdown.lower()
        status, events = api('GET', f'/v1/tasks/{task_id}/events?limit=1000')
        assert status == 200 and any(e['kind'] == 'team_member_event' for e in events['events'])
        assert api('GET', f'/v1/tasks/{task_id}')[1]['state'] == 'completed'
        with sqlite3.connect(home / 'data/default/tenants/default/state.db') as database:
            assert database.execute('SELECT count(*) FROM tasks').fetchone()[0] == 1, 'watch created another task'
            assert database.execute("SELECT count(*) FROM team_deliveries WHERE state='incorporated' AND context_seq IS NULL").fetchone()[0] == 0
            assert database.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
        if args.output:
            with args.output.open('x') as output:
                json.dump(export, output, indent=2)
                output.write('\n')
        print(json.dumps({'cooperative_daemon_cli': 'passed', 'safe_boundary_pause': 'passed',
                          'objection_changes_conclusion': 'passed', 'export': 'passed',
                          'provider': 'scripted fixture', 'native_evidence': args.native}))

    # Exercise CLI creation and its final result without pausing the invocation.
    with environment(binary, args.native, held=False) as (server, _, workspace, _, cli, api):
        result = cli('run', 'Review the toy gateway using the scripted fixture.', '--workspace', str(workspace),
                     '--execution', 'cooperative', '--template', 'audit', '--max-seconds', '120', '--json')
        events = [json.loads(line) for line in result.stdout.splitlines()]
        assert any(e.get('kind') == 'team_completed' or e.get('text') for e in events), events[-3:]
        tasks = api('GET', '/v1/tasks')[1]['tasks']
        assert len(tasks) == 1 and tasks[0]['state'] == 'completed', tasks
        assert server.scenario.done.is_set()
        print(json.dumps({'cooperative_cli_creation_completion': 'passed'}))

    # Restart while paused: no automatic activation and no stale generation reuse.
    with environment(binary, held=True) as (server, home, workspace, _, cli, api):
        status, task = api('POST', '/v1/tasks', {'workspace': str(workspace), 'objective': 'Restart fixture',
                          'execution': 'cooperative'}, 'restart-team')
        assert status == 201, task
        task_id = task['id']
        assert server.scenario.entered.wait(20)
        cli('team', 'pause', task_id)
        # Terminate the daemon with the provider call still in flight.
        daemon_pid = None
        for entry in pathlib.Path('/proc').iterdir():
            if entry.name.isdigit():
                try:
                    argv = (entry / 'cmdline').read_bytes().split(b'\0')
                except OSError:
                    continue
                if str(home).encode() in argv and b'serve' in argv and b'--ipc-only' in argv:
                    daemon_pid = int(entry.name)
                    break
        assert daemon_pid
        os.kill(daemon_pid, signal.SIGKILL)
        server.scenario.release.set()
        cli('sessions')
        snapshot = api('GET', f'/v1/tasks/{task_id}/team')[1]['team']
        assert snapshot['state'] == 'paused', snapshot
        calls = server.scenario.calls
        time.sleep(.2)
        assert calls == server.scenario.calls, 'restart reactivated a paused team'
        cli('team', 'cancel', task_id)
        assert api('GET', f'/v1/tasks/{task_id}')[1]['state'] == 'cancelled'
        print(json.dumps({'cooperative_paused_restart_cancel': 'passed'}))


if __name__ == '__main__':
    main()
