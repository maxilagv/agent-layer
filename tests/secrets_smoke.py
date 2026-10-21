#!/usr/bin/env python3
"""Real provider/CLI/database regression for credential registration and split output."""
import http.server
import json
import os
import pathlib
import sqlite3
import subprocess
import tempfile
import threading

SECRET = 'opaque-credential-for-local-regression-9842'
received = []


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        raw = self.rfile.read(int(self.headers['Content-Length']))
        received.append(raw)
        assert self.headers['Authorization'] == 'Bearer ' + SECRET
        assert SECRET.encode() not in raw
        body = json.loads(raw)
        assert body['model'] == 'secrets-fixture'
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        for text in ['Safe prefix ', SECRET[:13], SECRET[13:], '\nDone.']:
            item = {'choices': [{'delta': {'content': text}, 'finish_reason': None}]}
            self.wfile.write(('data: ' + json.dumps(item) + '\n\n').encode())
            self.wfile.flush()
        self.wfile.write(b'data: [DONE]\n\n')


def main():
    binary = str(pathlib.Path('target/debug/layeragent').resolve())
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    with tempfile.TemporaryDirectory(prefix='layeragent-secrets-') as directory:
        home = pathlib.Path(directory) / 'state'
        workspace = pathlib.Path(directory) / 'workspace'
        workspace.mkdir()
        environment = dict(os.environ, LAYERAGENT_SMOKE_CREDENTIAL=SECRET)

        def cli(*args):
            return subprocess.run([binary, '--home', str(home), *args], env=environment,
                                  check=True, capture_output=True, text=True, timeout=30)

        try:
            cli('init', '--protocol', 'openai_chat', '--model', 'secrets-fixture',
                '--base-url', f'http://127.0.0.1:{server.server_port}/v1',
                '--key-env', 'LAYERAGENT_SMOKE_CREDENTIAL')
            first = cli('run', 'Inspect this value: ' + SECRET, '--workspace', str(workspace), '--json')
            assert SECRET not in first.stdout + first.stderr
            events = [json.loads(line) for line in first.stdout.splitlines()]
            final = next(item for item in events if item.get('text', '').endswith('Done.'))
            assert 'REDACTED' in final['text']
            cli('resume', final['session_id'], 'Continue safely.', '--json')
            cli('stop')
            database = home / 'data/default/tenants/default/state.db'
            with sqlite3.connect(database) as connection:
                dump = '\n'.join(connection.iterdump())
                assert SECRET not in dump
                assert 'REDACTED' in dump
            for artifact in home.glob('data/*/tenants/*/artifacts/*'):
                assert SECRET.encode() not in artifact.read_bytes()
            assert len(received) == 2
            print(json.dumps({'credential_registration': 'passed', 'split_model_output': 'passed',
                              'outbound_history': 'passed', 'durable_state': 'passed'}))
        finally:
            subprocess.run([binary, '--home', str(home), 'stop'], env=environment,
                           capture_output=True, timeout=10)
            server.shutdown()


if __name__ == '__main__':
    main()
