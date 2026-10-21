#!/usr/bin/env python3
"""Real PostgreSQL + mTLS + pairing + encrypted runner effects; no model keys.

Creates only disposable Docker/test state. --native additionally qualifies
command/model execution in an administrator-authorized delegated Linux service.
"""
import argparse
import getpass
import hashlib
import http.server
import json
import os
import pathlib
import signal
import socket
import ssl
import subprocess
import tempfile
import threading
import time
import urllib.error
import urllib.request
import uuid


def private(path, value):
    path.write_text(json.dumps(value))
    path.chmod(0o600)
    return path


def command(args, **kwargs):
    result = subprocess.run([str(a) for a in args], capture_output=True, text=True,
                            timeout=kwargs.pop('timeout', 60), **kwargs)
    if result.returncode:
        raise AssertionError(f'command failed: {args[:3]}\n{result.stderr[-4000:]}\n{result.stdout[-2000:]}')
    return result.stdout


def port():
    with socket.socket() as s:
        s.bind(('127.0.0.1', 0))
        return s.getsockname()[1]


def wait(predicate, timeout=30):
    deadline = time.monotonic() + timeout
    last_error = None
    while time.monotonic() < deadline:
        try:
            value = predicate()
            if value:
                return value
        except (OSError, AssertionError, urllib.error.URLError) as error:
            last_error = error
        time.sleep(.1)
    raise AssertionError(f'condition did not become true before deadline: {last_error}')


class Model(http.server.BaseHTTPRequestHandler):
    def log_message(self, *_):
        pass

    def do_POST(self):
        body = json.loads(self.rfile.read(int(self.headers['Content-Length'])))
        tools = [t['function']['name'] for t in body['tools']]
        assert set(tools) <= {'read_file', 'list_files', 'search_files', 'write_file', 'edit_file', 'shell'}
        assert body['model'] == 'remote-fixture'
        self.send_response(200)
        self.send_header('Content-Type', 'text/event-stream')
        self.end_headers()
        context = json.dumps(body['messages'])
        probe = next((name for name in ['MODEL_EDIT_PROBE', 'MODEL_PROPOSE_PROBE', 'MODEL_DELETE_PROBE']
                      if name in context), None)
        has_result = any(message.get('role') == 'tool' for message in body['messages'])
        if probe and not has_result:
            if probe == 'MODEL_DELETE_PROBE':
                tool = 'shell'
                arguments = {'command': 'rm docs/delete-me.txt; echo proposal > docs/unapproved.txt'}
            else:
                tool = 'write_file'
                arguments = {'path': 'docs/model-edit.txt' if probe == 'MODEL_EDIT_PROBE' else 'docs/model-proposal.txt',
                             'content': 'x' * 20000}
            events = [{'choices': [{'delta': {'tool_calls': [{'index': 0, 'id': 'fixture-effect', 'type': 'function',
                       'function': {'name': tool, 'arguments': json.dumps(arguments)}}]}, 'finish_reason': None}]},
                      {'choices': [{'delta': {}, 'finish_reason': 'tool_calls'}],
                       'usage': {'prompt_tokens': 100, 'completion_tokens': 3000}}]
        else:
            events = [{'choices': [{'delta': {'content': 'Remote model remains on the runner'}, 'finish_reason': None}]},
                      {'choices': [{'delta': {}, 'finish_reason': 'stop'}]}]
        for event in events:
            self.wfile.write(('data: ' + json.dumps(event) + '\n\n').encode())
        self.wfile.write(b'data: [DONE]\n\n')


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', default='target/debug/layeragent')
    parser.add_argument('--native', action='store_true')
    parser.add_argument('--queue-only', action='store_true')
    parser.add_argument('--user-service', action='store_true', help='use an already delegated systemd user manager for native tests')
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    binary = (root / args.binary).resolve()
    name = 'layeragent-remote-test-' + uuid.uuid4().hex
    processes = []
    unit = None
    model_server = None
    with tempfile.TemporaryDirectory(prefix='layeragent-remote-') as temporary:
        base = pathlib.Path(temporary)
        try:
            command(['docker', 'run', '--rm', '--detach', '--name', name,
                     '--publish', '127.0.0.1::5432', '--env', 'POSTGRES_HOST_AUTH_METHOD=trust',
                     '--env', 'POSTGRES_USER=layeragent', '--env', 'POSTGRES_DB=layeragent',
                     'postgres:18.6-alpine'])
            wait(lambda: subprocess.run(['docker', 'exec', name, 'pg_isready', '-h', '127.0.0.1', '-U', 'layeragent'],
                                        capture_output=True).returncode == 0)
            published = command(['docker', 'port', name, '5432/tcp']).strip()
            db_port = int(published.rsplit(':', 1)[1])
            command(['docker', 'exec', name, 'psql', '-U', 'layeragent', '-c', 'CREATE DATABASE queue_tests'])
            env = os.environ.copy()
            env['LAYERAGENT_REMOTE_TEST_DSN'] = f'host=127.0.0.1 port={db_port} user=layeragent dbname=queue_tests'
            cargo = pathlib.Path.home() / '.cargo/bin/cargo'
            command([cargo, 'test', '-p', 'layeragent-remote', '--lib', 'durable_queue_fences',
                     '--', '--ignored', '--nocapture'], cwd=root, env=env, timeout=180)
            if args.queue_only:
                print(json.dumps({'postgres_assignment_and_effect_recovery': 'passed'}))
                return

            def openssl(*a):
                return command(['openssl', *a])

            openssl('req', '-x509', '-newkey', 'ed25519', '-nodes', '-keyout', base/'ca.key',
                    '-out', base/'ca.pem', '-days', '2', '-subj', '/CN=LayerAgent disposable test CA')
            for identity in ['server', 'owner', 'runner', 'stranger']:
                openssl('req', '-new', '-newkey', 'ed25519', '-nodes', '-keyout', base/f'{identity}.key',
                        '-out', base/f'{identity}.csr', '-subj', f'/CN={identity}')
                extensions = base/f'{identity}.ext'
                extensions.write_text('basicConstraints=CA:FALSE\nkeyUsage=digitalSignature\n' +
                                      ('extendedKeyUsage=serverAuth\nsubjectAltName=DNS:localhost,IP:127.0.0.1\n'
                                       if identity == 'server' else 'extendedKeyUsage=clientAuth\n'))
                openssl('x509', '-req', '-in', base/f'{identity}.csr', '-CA', base/'ca.pem',
                        '-CAkey', base/'ca.key', '-CAcreateserial', '-out', base/f'{identity}.pem',
                        '-days', '2', '-extfile', extensions)

            def cli(*a, expect_success=True):
                result = subprocess.run([str(binary), 'remote', *[str(v) for v in a]],
                                        capture_output=True, text=True, timeout=35)
                if expect_success and result.returncode:
                    raise AssertionError(f'{a[:2]}: {result.stderr}\n{result.stdout}')
                if not expect_success:
                    assert result.returncode != 0, result.stdout
                return result

            for role in ['owner', 'runner']:
                cli('keygen', '--keys', base/f'{role}.keys.json', '--certificate', base/f'{role}.pem',
                    '--public', base/f'{role}.public.json')
            owner = json.loads((base/'owner.public.json').read_text())
            epoch = str(uuid.uuid4())
            server_port = port()
            endpoint = f'https://localhost:{server_port}/'
            server_config = private(base/'coordinator.json', {
                'bind': f'127.0.0.1:{server_port}', 'ca': str(base/'ca.pem'),
                'certificate': str(base/'server.pem'), 'tls_key': str(base/'server.key'),
                'database_env': 'LAYERAGENT_TEST_DATABASE', 'account': 'personal', 'epoch': epoch,
                'owner_signing_key': owner['signing_key'], 'owner_certificate_sha256': owner['certificate_sha256'],
                'authority_guard': str(base/'authority.guard')})
            client_config = private(base/'client.json', {
                'endpoint': endpoint, 'ca': str(base/'ca.pem'), 'certificate': str(base/'owner.pem'),
                'tls_key': str(base/'owner.key'), 'keys': str(base/'owner.keys.json'),
                'account': 'personal', 'epoch': epoch})
            env['LAYERAGENT_TEST_DATABASE'] = f'host=127.0.0.1 port={db_port} user=layeragent dbname=layeragent'

            def start_server(*flags):
                log = open(base/'server.log', 'a')
                process = subprocess.Popen([str(binary), 'remote', 'coordinator', '--config', str(server_config), *flags],
                                           env=env, stdout=log, stderr=log)
                processes.append(process)
                wait(lambda: process.poll() is None and cli('jobs', '--config', client_config,
                                                            expect_success=True).returncode == 0)
                return process

            coordinator = start_server('--initialize')
            request = base/'request.json'
            response = cli('pair-request', '--keys', base/'runner.keys.json', '--certificate', base/'runner.pem',
                           '--account', 'personal', '--project', 'alpha', '--runner', 'runner-a',
                           '--device', 'linux-a', '--out', request)
            fingerprint = json.loads(response.stdout)['fingerprint']
            cli('authorize', '--config', client_config, '--request', request, '--confirm-fingerprint',
                'incorrect', '--operations', 'read', '--out', base/'wrong.json', expect_success=False)
            caps = 'read,write,delete,execute,send' if args.native else 'read,write,delete'
            cli('authorize', '--config', client_config, '--request', request, '--confirm-fingerprint',
                fingerprint, '--operations', caps, '--out', base/'enrollment-transfer.json')
            cli('pair-accept', '--keys', base/'runner.keys.json', '--certificate', base/'runner.pem',
                '--request', request, '--enrollment', base/'enrollment-transfer.json', '--owner-key',
                owner['signing_key'], '--out', base/'enrollment.json')
            cli('pair-accept', '--keys', base/'runner.keys.json', '--certificate', base/'runner.pem',
                '--request', request, '--enrollment', base/'enrollment-transfer.json', '--owner-key',
                owner['signing_key'], '--out', base/'enrollment.json', expect_success=False)
            project = base/'project'
            (project/'docs').mkdir(parents=True)
            (project/'docs/a.txt').write_text('original project content')
            (project/'.env').write_text('TEST_ONLY_SECRET_MUST_NEVER_LEAVE_PROJECT')
            cli('grant', '--root', project, '--project', 'alpha', '--operations', 'read,write,delete',
                '--destinations', '.' if args.native else 'docs', '--out', base/'grant.json')
            state = base/'runner-state'
            state.mkdir(mode=0o700)
            runner_config = {'endpoint': endpoint, 'ca': str(base/'ca.pem'), 'certificate': str(base/'runner.pem'),
                             'tls_key': str(base/'runner.key'), 'keys': str(base/'runner.keys.json'),
                             'owner_signing_key': owner['signing_key'], 'owner_encryption_key': owner['encryption_key'],
                             'enrollment': str(base/'enrollment.json'), 'state': str(state), 'grant': str(base/'grant.json')}
            if args.native:
                model_server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
                threading.Thread(target=model_server.serve_forever, daemon=True).start()
                agent_home = base/'agent-home'
                command([binary, '--home', agent_home, 'init', '--protocol', 'openai_chat', '--model',
                         'remote-fixture', '--base-url', f'http://127.0.0.1:{model_server.server_port}/v1'])
                runner_config.update(agent_home=str(agent_home), agent_profile_revision=json.loads(cli('profile-revision', '--home', agent_home).stdout)['revision'], allowed_models=['primary'], templates={
                    'check': {'argv': ['/bin/sh', '-c', 'cat docs/a.txt; test ! -e .env; echo scratch > local-only.txt'], 'timeout_secs': 30},
                    'wait': {'argv': ['/bin/sh', '-c', 'sleep 90'], 'timeout_secs': 120}})
            runner_path = private(base/'runner.json', runner_config)

            def start_runner():
                nonlocal unit
                log = open(base/'runner.log', 'a')
                argv = [str(binary), 'remote', 'runner', '--config', str(runner_path)]
                if args.native:
                    unit = 'layeragent-remote-' + uuid.uuid4().hex
                    manager = ['systemd-run', '--user'] if args.user_service else ['sudo', '-n', 'systemd-run', '--uid='+getpass.getuser()]
                    argv = [*manager, '--wait', '--pipe', '--collect', '--unit='+unit,
                            '--working-directory='+str(root),
                            '--property=Delegate=memory pids cpu', '--property=DelegateSubgroup=supervisor',
                            '--property=RuntimeMaxSec=240', '--setenv=HOME='+str(pathlib.Path.home()),
                            '--setenv=PATH='+os.defpath, *argv]
                process = subprocess.Popen(argv, stdout=log, stderr=log)
                processes.append(process)
                return process

            runner_process = start_runner()

            submitted_at = {}
            dispatch_seconds = {}

            def submit(job_id, task):
                submitted_at[job_id] = time.monotonic()
                task_file = private(base/f'{job_id}.task.json', task)
                cli('submit', '--config', client_config, '--enrollment', base/'enrollment.json',
                    '--task', task_file, '--id', job_id)

            def jobs():
                return json.loads(cli('jobs', '--config', client_config, '--decrypt').stdout)

            def finished(job_id, expected='completed'):
                def poll():
                    assert runner_process.poll() is None, (base/'runner.log').read_text()[-4000:]
                    return next((j for j in jobs() if j['id'] == job_id and j['status'] == expected), None)
                result = wait(poll)
                dispatch_seconds[job_id] = round(time.monotonic() - submitted_at[job_id], 4)
                return result

            submit('read', {'kind': 'file', 'tool': 'read_file', 'arguments': {'path': 'docs/a.txt'}})
            assert finished('read')['decrypted']['text'] == 'original project content'
            submit('write', {'kind': 'file', 'tool': 'write_file', 'arguments': {'path': 'docs/new.txt', 'content': 'encrypted remote effect'}})
            finished('write')
            assert (project/'docs/new.txt').read_text() == 'encrypted remote effect'
            inode = (project/'docs/new.txt').stat().st_ino
            cli('resend', '--config', client_config, '--submission', base/'submission-write.json')
            assert (project/'docs/new.txt').stat().st_ino == inode
            for job_id, path in [('escape', '../outside'), ('secret', '.env'), ('destination', 'outside.txt')]:
                if args.native and job_id == 'destination':
                    continue
                submit(job_id, {'kind': 'file', 'tool': 'write_file', 'arguments': {'path': path, 'content': 'forbidden'}})
                finished(job_id, 'failed')
            assert not (project/'outside.txt').exists()
            assert (project/'.env').read_text() == 'TEST_ONLY_SECRET_MUST_NEVER_LEAVE_PROJECT'
            context = ssl.create_default_context(cafile=str(base/'ca.pem'))
            context.load_cert_chain(base/'runner.pem', base/'runner.key')
            try:
                urllib.request.urlopen(endpoint+'remote/v1/jobs', context=context)
                raise AssertionError('runner accessed owner job list')
            except urllib.error.HTTPError as error:
                assert error.code == 403
            dump = command(['docker', 'exec', name, 'pg_dump', '-U', 'layeragent', 'layeragent'])
            assert 'encrypted remote effect' not in dump
            assert 'TEST_ONLY_SECRET_MUST_NEVER_LEAVE_PROJECT' not in dump

            if args.native:
                submit('execute', {'kind': 'execute', 'template': 'check'})
                assert finished('execute')['decrypted']['execution']['code'] == 0
                assert not (project/'local-only.txt').exists()
                submit('agent', {'kind': 'agent', 'prompt': 'Summarize the allowed project.', 'model': 'primary'})
                assert finished('agent')['decrypted']['agent']['text'] == 'Remote model remains on the runner'
                submit('agent-edit', {'kind': 'agent', 'prompt': 'MODEL_EDIT_PROBE', 'model': 'primary', 'apply_edits': True})
                assert finished('agent-edit')['decrypted']['applied']
                assert (project/'docs/model-edit.txt').read_text() == 'x' * 20000
                submit('agent-propose', {'kind': 'agent', 'prompt': 'MODEL_PROPOSE_PROBE', 'model': 'primary'})
                assert finished('agent-propose')['decrypted']['changes']
                assert not (project/'docs/model-proposal.txt').exists()
                (project/'docs/delete-me.txt').write_text('approved deletion target')
                submit('agent-delete', {'kind': 'agent', 'prompt': 'MODEL_DELETE_PROBE', 'model': 'primary',
                                        'apply_deletes': True, 'allow_shell': True})
                assert finished('agent-delete')['decrypted']['applied']
                assert not (project/'docs/delete-me.txt').exists()
                assert not (project/'docs/unapproved.txt').exists()
                # Cancel a real running process, reconcile its effects, then keep using the runner.
                submit('active-cancel', {'kind': 'execute', 'template': 'wait'})
                wait(lambda: any(j['id'] == 'active-cancel' and j['status'] == 'leased' for j in jobs()))
                cli('cancel', '--config', client_config, 'active-cancel')
                wait(lambda: any(r['job'] == 'active-cancel' and r['state'] == 'unknown'
                                 for r in json.loads(cli('local-receipts', '--state', state).stdout)), timeout=25)
                proof = base/'inspection.txt'
                proof.write_text('Inspected the confined wait template and project: no original-project effects. Process was cancelled.')
                proof.chmod(0o600)
                cli('reconcile', '--config', client_config, 'active-cancel', '--outcome', 'failed', '--evidence', proof)
                assert next(j for j in jobs() if j['id'] == 'active-cancel')['status'] == 'failed'
                # A network outage must stop active controlled work, not resume its old result.
                submit('partition', {'kind': 'execute', 'template': 'wait'})
                wait(lambda: any(j['id'] == 'partition' and j['status'] == 'leased' for j in jobs()))
                coordinator.send_signal(signal.SIGINT)
                coordinator.wait(timeout=10)
                wait(lambda: any(r['job'] == 'partition' and r['state'] == 'unknown'
                                 for r in json.loads(cli('local-receipts', '--state', state).stdout)), timeout=25)
                coordinator = start_server()
                submit('after-partition', {'kind': 'file', 'tool': 'read_file', 'arguments': {'path': 'docs/a.txt'}})
                finished('after-partition')
                assert next(j for j in jobs() if j['id'] == 'partition')['status'] == 'unknown'
                dump = command(['docker', 'exec', name, 'pg_dump', '-U', 'layeragent', 'layeragent'])
                assert 'Remote model remains on the runner' not in dump

            # Local grant changes are immutable for the entire runner lifetime.
            grant_path = base/'grant.json'
            original_grant = grant_path.read_text()
            narrowed = json.loads(original_grant)
            narrowed['tools'].pop('write_file')
            private(grant_path, narrowed)
            wait(lambda: runner_process.poll() is not None)
            submit('after-grant-change', {'kind': 'file', 'tool': 'write_file',
                                         'arguments': {'path': 'docs/no-stale-grant.txt', 'content': 'no'}})
            time.sleep(.3)
            assert not (project/'docs/no-stale-grant.txt').exists()
            cli('cancel', '--config', client_config, 'after-grant-change')
            # Explicitly restore the original test grant and restart the same fixture identity.
            grant_path.write_text(original_grant)
            grant_path.chmod(0o600)
            runner_process = start_runner()
            submit('after-policy-review', {'kind': 'file', 'tool': 'read_file', 'arguments': {'path': 'docs/a.txt'}})
            finished('after-policy-review')

            cli('disconnect', '--state', state)
            wait(lambda: runner_process.poll() is not None)
            submit('cancelled', {'kind': 'file', 'tool': 'write_file', 'arguments': {'path': 'docs/cancelled.txt', 'content': 'no'}})
            cli('cancel', '--config', client_config, 'cancelled')
            assert next(j for j in jobs() if j['id'] == 'cancelled')['status'] == 'cancelled'
            assert not (project/'docs/cancelled.txt').exists()
            cli('revoke', '--config', client_config, 'runner-a')
            task_file = private(base/'after.task.json', {'kind': 'file', 'tool': 'read_file', 'arguments': {'path': 'docs/a.txt'}})
            cli('submit', '--config', client_config, '--enrollment', base/'enrollment.json',
                '--task', task_file, '--id', 'after-revoke', expect_success=False)
            # Simulate restoring database control state while retaining the independent checkpoint.
            command(['docker', 'exec', name, 'psql', '-U', 'layeragent', '-c',
                     'UPDATE la_remote_authority SET control_sequence=0'])
            cli('jobs', '--config', client_config, expect_success=False)
            coordinator.send_signal(signal.SIGINT)
            coordinator.wait(timeout=10)
            server_data = json.loads(server_config.read_text())
            server_data['epoch'] = str(uuid.uuid4())
            private(server_config, server_data)
            coordinator = start_server('--recover')
            runners = json.loads(cli('runners', '--config', client_config).stdout)
            assert all(r['revoked'] for r in runners)
            print(json.dumps({'postgres_queue': 'passed', 'mtls': 'passed', 'pairing': 'passed',
                              'encrypted_file_effects': 'passed', 'permission_rejections': 'passed',
                              'local_disconnect_and_revocation': 'passed', 'restore_guard': 'passed',
                              'native_execution_and_agent': 'passed' if args.native else 'not requested',
                              'observed_submit_to_result_seconds': dispatch_seconds}))
        except Exception:
            for label in ['runner', 'server']:
                log_path = base / (label + '.log')
                if log_path.exists():
                    print(label + ' log: ' + log_path.read_text()[-6000:], flush=True)
            raise
        finally:
            if unit:
                manager = ['systemctl', '--user'] if args.user_service else ['sudo', '-n', 'systemctl']
                subprocess.run([*manager, 'stop', unit+'.service'], capture_output=True)
            for process in reversed(processes):
                if process.poll() is None:
                    process.send_signal(signal.SIGINT)
                    try:
                        process.wait(timeout=10)
                    except subprocess.TimeoutExpired:
                        process.kill()
                        process.wait()
            if model_server:
                model_server.shutdown()
            subprocess.run(['docker', 'rm', '-f', name], capture_output=True)


if __name__ == '__main__':
    main()
