#!/usr/bin/env python3
"""Exercise the real CLI, daemon, streaming HTTP adapter and SQLite without API keys."""
import argparse
import http.server
import json
import pathlib
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
            print(json.dumps({'cli_daemon_streaming_sqlite': 'passed', 'turns': 2, 'session_reused': True}))
        finally:
            subprocess.run([binary, '--home', str(home), 'stop'], capture_output=True, timeout=10)
            time.sleep(.2)
    server.shutdown()


if __name__ == '__main__':
    main()
