#!/usr/bin/env python3
"""Exercise the real CLI, daemon, streaming HTTP adapter and SQLite without API keys."""
import argparse
import http.client
import http.server
import json
import os
import pathlib
import signal
import socket
import sqlite3
import subprocess
import tempfile
import threading
import time


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        request = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        assert request['model'] == 'smoke-fixture'
        # One prompt is deliberately slow so a restart can interrupt real work.
        if any('slow task across restart' in str(message.get('content', '')) for message in request['messages']):
            time.sleep(3)
        assert request['stream'] is True
        assert request['tools']
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        events = [
            {'choices': [{'delta': {'content': 'LayerAgent verified 🦀'}, 'finish_reason': None}]},
            {'choices': [{'delta': {}, 'finish_reason': 'stop'}], 'usage': {'prompt_tokens': 20, 'completion_tokens': 6}},
        ]
        payload = ''.join('data: ' + json.dumps(event) + '\n\n' for event in events) + 'data: [DONE]\n\n'
        raw = payload.encode()
        for i in range(0, len(raw), 7):
            self.wfile.write(raw[i:i + 7])
            self.wfile.flush()


class UnixHTTPConnection(http.client.HTTPConnection):
    """Minimal HTTP client over the daemon's UNIX socket."""

    def __init__(self, path):
        super().__init__('localhost', timeout=30)
        self.socket_path = path

    def connect(self):
        self.sock = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        self.sock.settimeout(30)
        self.sock.connect(self.socket_path)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', default='target/debug/layeragent')
    binary = str(pathlib.Path(parser.parse_args().binary).resolve())
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='layeragent-smoke-') as temporary:
        base = pathlib.Path(temporary)
        home = base / 'state'
        workspace = base / 'workspace'
        workspace.mkdir()
        (workspace / 'user.txt').write_text('existing uncommitted work')

        def cli(*args):
            result = subprocess.run([binary, '--home', str(home), *args], capture_output=True, text=True, timeout=30)
            if result.returncode:
                raise AssertionError(f'{args}: {result.stderr}\n{result.stdout}')
            return result

        try:
            cli('init', '--protocol', 'openai_chat', '--model', 'smoke-fixture', '--base-url', f'http://127.0.0.1:{server.server_port}/v1')
            first = cli('run', 'Verify the actual transport.', '--workspace', str(workspace), '--json')
            events = [json.loads(line) for line in first.stdout.splitlines()]
            result = next(e for e in events if e.get('text') == 'LayerAgent verified 🦀')
            assert any(e.get('kind') == 'text_delta' for e in events)
            assert any(e.get('kind') == 'attempt_started' for e in events)
            session = result['session_id']
            cli('resume', session, 'Remember this session.', '--json')
            sessions = json.loads(cli('sessions').stdout)
            assert len(sessions) == 1 and sessions[0]['id'] == session
            path = home / 'data/default/tenants/default/state.db'
            with sqlite3.connect(path) as database:
                assert database.execute('PRAGMA journal_mode').fetchone()[0] == 'wal'
                assert database.execute('SELECT count(*) FROM messages').fetchone()[0] == 4
                assert database.execute("SELECT count(*) FROM turns WHERE state='completed'").fetchone()[0] == 2
                assert database.execute('SELECT count(*) FROM memories').fetchone()[0] == 2
                assert database.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
            assert (workspace / 'user.txt').read_text() == 'existing uncommitted work'

            def api(method, path, body=None, key=None):
                connection = UnixHTTPConnection(str(home / 'runtime/default.sock'))
                headers = {'content-type': 'application/json'}
                if key:
                    headers['idempotency-key'] = key
                payload = None if body is None else json.dumps(body)
                connection.request(method, path, payload, headers)
                response = connection.getresponse()
                raw = response.read()
                connection.close()
                return response.status, (json.loads(raw) if raw else None)

            def wait_terminal(task_id, expected=None):
                for _ in range(240):
                    _, task = api('GET', f'/v1/tasks/{task_id}')
                    if task['state'] in ('completed', 'failed', 'cancelled', 'blocked'):
                        if expected is None or task['state'] == expected:
                            return task
                        raise AssertionError(task)
                    time.sleep(.05)
                raise AssertionError('task did not finish')

            request = {'session_id': session, 'objective': 'Follow-up through the task API.'}
            status, task = api('POST', '/v1/tasks', request, 'smoke-task-1')
            assert status == 201, (status, task)
            task_id = task['id']
            assert task['state'] in ('prepared', 'running', 'completed')
            completed = wait_terminal(task_id, 'completed')
            assert completed['usage']['output_tokens'] == 6, completed['usage']
            # Cursor pagination reads the durable log without a live connection.
            status, page = api('GET', f'/v1/tasks/{task_id}/events?after=0')
            assert status == 200 and page['events']
            assert page['events'][0]['kind'] == 'task_prepared'
            assert any(event['kind'] == 'turn_completed' for event in page['events'])
            if page['next'] is not None:
                _, rest = api('GET', f'/v1/tasks/{task_id}/events?after={page["next"]}')
                assert all(event['seq'] > page['next'] for event in rest['events'])
            # An identical replay returns the same task; reuse with new content conflicts.
            status, replay = api('POST', '/v1/tasks', request, 'smoke-task-1')
            assert status == 200 and replay['id'] == task_id, (status, replay)
            status, conflict = api('POST', '/v1/tasks', {
                'session_id': session,
                'objective': 'A different request.',
            }, 'smoke-task-1')
            assert status == 409, (status, conflict)
            status, cancelled = api('POST', f'/v1/tasks/{task_id}/cancel', {})
            assert status == 200 and cancelled['cancelled'] is False
            with sqlite3.connect(path) as database:
                assert database.execute('SELECT count(*) FROM turns WHERE id=?', (task_id,)).fetchone()[0] == 1
                row = database.execute('SELECT state,last_event_seq FROM tasks WHERE id=?', (task_id,)).fetchone()
                assert row[0] == 'completed' and row[1] > 0, row
                # Sequences are global and monotonic; a task's own events keep
                # their gaps, so the cursor advances to the highest sequence.
                last = database.execute('SELECT max(seq) FROM task_events WHERE task_id=?', (task_id,)).fetchone()[0]
                assert last == row[1], (last, row)

            # A hard restart must leave the interrupted task blocked, never resumed.
            status, slow = api('POST', '/v1/tasks', {
                'session_id': session,
                'objective': 'slow task across restart',
            }, 'smoke-task-restart')
            assert status == 201, (status, slow)
            slow_id = slow['id']
            for _ in range(200):
                _, running = api('GET', f'/v1/tasks/{slow_id}')
                if running['state'] == 'running':
                    break
                time.sleep(.05)
            else:
                raise AssertionError('slow task never started')
            daemon_pid = None
            for entry in pathlib.Path('/proc').iterdir():
                if not entry.name.isdigit():
                    continue
                try:
                    argv = (entry / 'cmdline').read_bytes().split(b'\0')
                except OSError:
                    continue
                if str(home).encode() in argv and b'serve' in argv and b'--ipc-only' in argv:
                    daemon_pid = int(entry.name)
                    break
            assert daemon_pid, 'daemon process not found'
            os.kill(daemon_pid, signal.SIGKILL)
            time.sleep(.5)
            cli('sessions')  # restarts the daemon against the same state
            recovered = wait_terminal(slow_id, 'blocked')
            assert 'reconcile' in (recovered['error'] or '').lower(), recovered
            with sqlite3.connect(path) as database:
                assert database.execute('SELECT count(*) FROM turns WHERE id=?', (slow_id,)).fetchone()[0] == 1
                kinds = [row[0] for row in database.execute('SELECT kind FROM task_events WHERE task_id=? ORDER BY seq', (slow_id,))]
                assert 'task_interrupted' in kinds and 'turn_completed' not in kinds, kinds

            # Revoking a connection stops pending work and refuses new work.
            status, conns = api('GET', '/v1/connections')
            assert status == 200 and isinstance(conns, list), (status, conns)
            provider = next(c for c in conns if c['kind'] == 'provider')
            status, pending = api('POST', '/v1/tasks', {
                'session_id': session,
                'objective': 'slow task across restart',
            }, 'smoke-task-revoke')
            assert status == 201, (status, pending)
            pending_id = pending['id']
            for _ in range(200):
                _, running = api('GET', f'/v1/tasks/{pending_id}')
                if running['state'] == 'running':
                    break
                time.sleep(.05)
            else:
                raise AssertionError('task for revocation never started')
            status, revoked = api('POST', f"/v1/connections/{provider['id']}/revoke", {})
            assert status == 200 and revoked['enabled'] is False, (status, revoked)
            stopped = wait_terminal(pending_id)
            assert stopped['state'] in ('cancelled', 'blocked', 'failed'), stopped
            status, refused = api('POST', '/v1/tasks', {
                'session_id': session,
                'objective': 'must not start after revocation',
            }, 'smoke-task-after-revoke')
            assert status == 409, (status, refused)
            assert 'revoked' in (refused.get('error') or '').lower(), refused
            print(json.dumps({
                'cli_daemon_streaming_sqlite': 'passed',
                'turns': 2,
                'session_reused': True,
                'task_api': 'passed',
                'task_replay_idempotent': True,
                'task_restart_blocked': True,
                'connection_revoke_stops_pending': True,
            }))
        finally:
            subprocess.run([binary, '--home', str(home), 'stop'], capture_output=True, timeout=10)
            time.sleep(.2)
    server.shutdown()


if __name__ == '__main__':
    main()
