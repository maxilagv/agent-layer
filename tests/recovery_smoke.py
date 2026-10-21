#!/usr/bin/env python3
"""Kill/restart the real daemon with durable in-flight intents; never repeat effects."""
import json
import os
import pathlib
import socket
import sqlite3
import subprocess
import tempfile
import time
import urllib.error
import urllib.request
import uuid


def main():
    binary = str(pathlib.Path('target/debug/layeragent').resolve())
    with tempfile.TemporaryDirectory(prefix='layeragent-recovery-') as temporary:
        home = pathlib.Path(temporary) / 'state'
        workspace = pathlib.Path(temporary) / 'workspace'
        workspace.mkdir()
        subprocess.run([binary, '--home', str(home), 'init'], check=True, capture_output=True)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        config = home / 'config.toml'
        config.write_text(config.read_text().replace('127.0.0.1:9473', f'127.0.0.1:{port}'))
        environment = dict(os.environ, LAYERAGENT_TOKEN='recovery-owner-token')
        process = None

        def request(path, payload=None):
            data = None if payload is None else json.dumps(payload).encode()
            req = urllib.request.Request(f'http://127.0.0.1:{port}{path}', data=data,
                                         headers={'Authorization': 'Bearer recovery-owner-token',
                                                  'Content-Type': 'application/json'})
            with urllib.request.urlopen(req, timeout=5) as response:
                return json.loads(response.read())

        def start():
            result = subprocess.Popen([binary, '--home', str(home), 'serve'], env=environment,
                                      stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)
            for _ in range(100):
                try:
                    request('/health')
                    return result
                except (OSError, urllib.error.HTTPError):
                    if result.poll() is not None:
                        raise AssertionError(result.stderr.read())
                    time.sleep(.05)
            result.kill()
            result.wait(timeout=5)
            raise AssertionError('runtime did not become ready')

        try:
            process = start()
            session = request('/v1/sessions', {'workspace': str(workspace)})
            database = home / 'data/default/tenants/default/state.db'
            turn, call = str(uuid.uuid4()), str(uuid.uuid4())
            # Represents durable intent immediately before a crash. No external
            # system can establish whether these operations completed.
            with sqlite3.connect(database) as connection:
                connection.execute("INSERT INTO turns VALUES(?,?, 'tools','fixture',1)", (turn, session['id']))
                connection.execute("INSERT INTO tool_runs VALUES(?,?, 'shell',?,'running',NULL)",
                                   (turn, call, json.dumps({'command': 'touch must-not-run'})))
                connection.execute("INSERT INTO inbox(channel,external_id,principal,destination,body,status) VALUES('fixture','1','owner','fixture','uncertain input','processing')")
                connection.execute("INSERT INTO outbox(id,principal,channel,destination,body,status) VALUES('1','owner','fixture','fixture','uncertain send','sending')")
            process.kill()
            process.wait(timeout=5)
            process = start()
            for _ in range(2):
                with sqlite3.connect(database) as connection:
                    assert connection.execute('SELECT state FROM turns WHERE id=?', (turn,)).fetchone()[0] == 'interrupted'
                    assert connection.execute('SELECT status FROM tool_runs WHERE call_id=?', (call,)).fetchone()[0] == 'unknown'
                    assert connection.execute('SELECT status FROM inbox').fetchone()[0] == 'unknown'
                    assert connection.execute('SELECT status FROM outbox').fetchone()[0] == 'unknown'
                    assert connection.execute('SELECT attempts FROM outbox').fetchone()[0] == 0
                    assert connection.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
                assert not (workspace / 'must-not-run').exists()
                request('/v1/shutdown', {})
                process.wait(timeout=5)
                process = start()
            request('/v1/shutdown', {})
            process.wait(timeout=5)
            print(json.dumps({'actual_daemon_restart': 'passed', 'uncertain_intent_preserved': 'passed',
                              'no_automatic_effect_replay': 'passed', 'recovery_idempotent': 'passed'}))
        finally:
            if process is not None and process.poll() is None:
                process.kill()
                process.wait(timeout=5)


if __name__ == '__main__':
    main()
