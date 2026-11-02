#!/usr/bin/env python3
"""Verify HTTP authorization and workspace boundaries on the actual server."""
import json
import os
import pathlib
import socket
import subprocess
import sys
import tempfile
import time
import urllib.request
import urllib.error

# LayerAgent refuses private directories below symlinks; macOS reaches its
# temporary directory through one (/var -> /private/var).
# macOS temporary directories are long enough to push runtime sockets past the
# 104-byte UNIX socket path limit; use the short real /tmp there instead.
tempfile.tempdir = os.path.realpath('/tmp' if sys.platform == 'darwin' else tempfile.gettempdir())


def main():
    binary = str(pathlib.Path('target/debug/layeragent').resolve())
    with tempfile.TemporaryDirectory(prefix='layeragent-auth-') as temporary:
        base = pathlib.Path(temporary)
        state = base / 'state'
        allowed = base / 'allowed'
        other = base / 'other'
        allowed.mkdir()
        other.mkdir()
        subprocess.run([binary, '--home', str(state), 'init', '--model', 'auth-fixture',
                        '--protocol', 'openai_chat', '--base-url', 'http://127.0.0.1:9/v1'],
                       check=True, capture_output=True)
        with socket.socket() as listener:
            listener.bind(('127.0.0.1', 0))
            port = listener.getsockname()[1]
        config = state / 'config.toml'
        text = config.read_text().replace('127.0.0.1:9473', f'127.0.0.1:{port}')
        # Empty access serializes as access = []; replace it with an array-of-table entry.
        text = text.replace('access = []\n', '')
        text += f'\n[[access]]\ntoken_env="LAYERAGENT_TEST_BOB"\ntenant="bob"\nprincipal="bob"\nworkspaces=[{json.dumps(str(allowed))}]\n'
        text += f'\n[channels.whatsapp]\nkind="whatsapp"\ntoken_env="LAYERAGENT_TEST_UNUSED"\nsecret_env="LAYERAGENT_TEST_SIGNATURE"\nverify_token_env="LAYERAGENT_TEST_SUBSCRIPTION"\nworkspace={json.dumps(str(allowed))}\n'
        config.write_text(text)
        environment = dict(os.environ, LAYERAGENT_TOKEN='fixture-owner-token', LAYERAGENT_TEST_BOB='fixture-bob-token', LAYERAGENT_TEST_SUBSCRIPTION='fixture-subscription')
        process = subprocess.Popen([binary, '--home', str(state), 'serve'], env=environment, stdout=subprocess.DEVNULL, stderr=subprocess.PIPE, text=True)

        def request(path, token=None, payload=None, extra_headers=None):
            headers = {'Content-Type': 'application/json'}
            headers.update(extra_headers or {})
            if token:
                headers['Authorization'] = 'Bearer ' + token
            data = None if payload is None else json.dumps(payload).encode()
            req = urllib.request.Request(f'http://127.0.0.1:{port}' + path, data=data, headers=headers)
            try:
                response = urllib.request.urlopen(req, timeout=5)
                raw = response.read()
                return response.status, json.loads(raw) if response.headers.get('content-type', '').startswith('application/json') else raw.decode()
            except urllib.error.HTTPError as error:
                raw = error.read()
                return error.code, json.loads(raw) if raw and error.headers.get('content-type', '').startswith('application/json') else raw.decode()

        def oversized_status(path, token, size):
            """Status for a body larger than the gateway limit.

            The gateway refuses by declared length and closes the connection without
            reading the body, so a client already streaming it can see a broken pipe
            instead of the 413. Expect: 100-continue makes the client wait for the
            answer first; if the server asks for the body, it is sent and the final
            status is read, so the assertion stays the same either way.
            """
            head = (f'POST {path} HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\n'
                    f'Authorization: Bearer {token}\r\nContent-Type: application/json\r\n'
                    f'Content-Length: {size}\r\nExpect: 100-continue\r\nConnection: close\r\n\r\n')
            with socket.create_connection(('127.0.0.1', port), timeout=5) as connection:
                connection.sendall(head.encode())
                reader = connection.makefile('rb')
                status = int(reader.readline().split()[1])
                if status == 100:
                    while reader.readline() not in (b'\r\n', b''):
                        pass
                    try:
                        connection.sendall(b'x' * size)
                    except (BrokenPipeError, ConnectionResetError):
                        pass
                    status = int(reader.readline().split()[1])
                return status

        try:
            for _ in range(100):
                try:
                    if request('/health')[0] == 401:
                        break
                except OSError:
                    pass
                if process.poll() is not None:
                    raise AssertionError(process.stderr.read())
                time.sleep(.05)
            else:
                raise AssertionError('HTTP server did not become ready')
            owner = 'fixture-owner-token'
            assert request('/health', owner, extra_headers={'Origin': 'https://attacker.example'})[0] == 403
            assert request('/health', owner, extra_headers={'Origin': f'http://127.0.0.1:{port}'})[0] == 200
            bob = 'fixture-bob-token'
            assert request('/v1/config', bob)[0] == 403
            assert request('/v1/config/reload', bob, {})[0] == 403
            assert request('/v1/config', owner)[0] == 200
            assert oversized_status('/v1/sessions/unused/turns', owner, 1_048_576 + 64) == 413
            challenge = '/webhooks/whatsapp?hub.mode=subscribe&hub.challenge=verified-challenge&hub.verify_token='
            assert request(challenge + 'wrong')[0] == 403
            assert request(challenge + 'fixture-subscription') == (200, 'verified-challenge')
            status, session = request('/v1/sessions', owner, {'workspace': str(other)})
            assert status == 200
            assert request('/v1/sessions', bob)[1] == []
            assert request('/v1/sessions', bob, {'workspace': str(other)})[0] == 400
            # Cooperative routes use the same actual bearer authentication and
            # task ownership boundary, even if the unavailable fixture model fails.
            status, team_task = request('/v1/tasks', owner, {
                'session_id': session['id'], 'objective': 'Authorization fixture only',
                'execution': 'cooperative', 'idempotency_key': 'auth-team',
            })
            assert status == 201, team_task
            team_path = f'/v1/tasks/{team_task["id"]}/team'
            for suffix in ('', '/messages', '/deliveries', '/finding_revisions', '/export'):
                assert request(team_path + suffix)[0] == 401
                assert request(team_path + suffix, bob)[0] == 404
                assert request(team_path + suffix, owner)[0] == 200
            assert request(team_path + '/messages', bob, {
                'idempotency_key': 'foreign-message', 'expected_revision': 1,
                'recipient': None, 'purpose': 'user_instruction', 'body': 'Foreign instruction',
            })[0] == 404
            status, bob_session = request('/v1/sessions', bob, {'workspace': str(allowed)})
            assert status == 200
            assert request(f'/v1/sessions/{session["id"]}/events', bob)[0] == 400
            for suffix, payload in [('history', None), ('context', None), ('preferences', None), ('preferences', {'effort':'high'}), ('compact', {})]:
                assert request(f'/v1/sessions/{session["id"]}/{suffix}', bob, payload)[0] == 400
            assert request(f'/v1/sessions/{bob_session["id"]}/preferences', bob, {'effort':'high'})[0] == 200
            assert request(f'/v1/sessions/{bob_session["id"]}/preferences', bob)[1]['effort'] == 'high'
            assert request(f'/v1/sessions/{bob_session["id"]}/preferences', bob, {'bridge':True})[0] == 400
            assert request('/mcp', bob, {'jsonrpc': '2.0', 'id': 1, 'method': 'tools/list'})[0] == 403
            assert request('/v1/shutdown', bob, {})[0] == 400
            config.write_text(config.read_text().replace(json.dumps(str(allowed)), json.dumps(str(other))))
            assert request('/v1/config/reload', owner, {})[0] == 200
            assert request('/v1/sessions', bob)[1] == []
            assert request(f'/v1/sessions/{bob_session["id"]}/events', bob)[0] == 400
            status, rejected = request(f'/v1/sessions/{bob_session["id"]}/turns', bob, {'prompt': 'Use a revoked workspace'})
            assert status == 400 and 'workspace is not authorized' in rejected['error']
            assert request('/v1/memory/search', bob, {'workspace': str(allowed), 'query': 'old project'})[0] == 400
            assert request('/v1/shutdown', owner, {})[0] == 200
            process.wait(timeout=5)
            print(json.dumps({'http_authentication': 'passed', 'cross_tenant_isolation': 'passed', 'workspace_allowlist': 'passed', 'mcp_owner_boundary': 'passed', 'cooperative_route_authentication': 'passed'}))
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)


if __name__ == '__main__':
    main()
