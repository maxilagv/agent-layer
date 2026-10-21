#!/usr/bin/env python3
"""Repeat local checks and record evidence without representing blocked checks as passes."""
import argparse
import datetime
import json
import pathlib
import re
import shutil
import subprocess
import sys
import time


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--check-current', action='store_true', help='fail if stored verification evidence does not match maintained inputs')
    parser.add_argument('--native', action='store_true', help='also run full Linux confinement in an administrator-authorized delegated service')
    parser.add_argument('--native-user', action='store_true', help='run unchanged native assertions through an already delegated systemd user manager')
    parser.add_argument('--remote', action='store_true', help='also run disposable PostgreSQL/mTLS bridge qualification (requires Docker and OpenSSL)')
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    cargo = shutil.which('cargo') or str(pathlib.Path.home() / '.cargo/bin/cargo')
    logs = root / 'target/verification'
    logs.mkdir(parents=True, exist_ok=True)
    sys.path.insert(0, str(root / 'scripts'))
    from source_fingerprint import fingerprint
    source = fingerprint(root)
    if args.check_current:
        report = json.loads((root / 'docs/verification.json').read_text())
        current = report.get('source_sha256') == source['source_sha256']
        print(json.dumps({'current': current, **source}))
        return 0 if current else 1
    checks = [
        ('dependency_waivers', [sys.executable, 'tests/dependency_policy.py']),
        ('dependencies', [sys.executable, 'scripts/check_dependencies.py', '--cargo', cargo]),
        ('format', [cargo, 'fmt', '--all', '--check']),
        ('clippy', [cargo, 'clippy', '--workspace', '--all-targets', '--locked', '--', '-D', 'warnings']),
        ('tests', [cargo, 'test', '--workspace', '--locked']),
        ('embeddings_build', [cargo, 'check', '-p', 'layeragent-cli', '--features', 'embeddings', '--locked']),
        ('binary_build', [cargo, 'build', '-p', 'layeragent-cli', '--locked']),
        ('system_smoke', [sys.executable, 'tests/system_smoke.py']),
        ('auth_smoke', [sys.executable, 'tests/auth_smoke.py']),
        ('tui_smoke', [sys.executable, 'tests/tui_smoke.py']),
        ('secrets_smoke', [sys.executable, 'tests/secrets_smoke.py']),
        ('recovery_smoke', [sys.executable, 'tests/recovery_smoke.py']),
        ('backup_smoke', [sys.executable, 'tests/backup_smoke.py']),
        ('release_hygiene', [sys.executable, 'tests/release_hygiene.py']),
        ('source_signing', [sys.executable, 'tests/source_signing.py']),
        ('live_provider_parser', [sys.executable, 'tests/live_provider.py', '--self-test']),
    ]
    if args.remote:
        remote = [sys.executable, 'tests/remote_smoke.py']
        if args.native or args.native_user:
            remote.append('--native')
        if args.native_user:
            remote.append('--user-service')
        checks.append(('remote_bridge', remote))
    if args.native or args.native_user:
        native = [sys.executable, 'scripts/run_native_tests.py']
        if args.native_user:
            native.append('--user-service')
        checks.append(('native_confinement', native))
    report = {'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), **source, 'checks': []}
    for name, command in checks:
        start = time.monotonic()
        result = subprocess.run(command, cwd=root, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True)
        (logs / f'{name}.log').write_text(result.stdout)
        entry = {'name': name, 'exit_code': result.returncode, 'seconds': round(time.monotonic() - start, 3), 'passed': result.returncode == 0, 'log': f'target/verification/{name}.log'}
        if name == 'tests':
            totals = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored', result.stdout)
            entry.update({'passed_tests': sum(int(row[0]) for row in totals), 'ignored_tests': sum(int(row[2]) for row in totals)})
        report['checks'].append(entry)
        print(json.dumps(entry), flush=True)
        if result.returncode and name != 'native_confinement':
            print(result.stdout[-4000:], flush=True)
            break
    report['source_changed_during_verification'] = fingerprint(root)['source_sha256'] != source['source_sha256']
    required_count = len([name for name, _ in checks if name != 'native_confinement'])
    required = [entry for entry in report['checks'] if entry['name'] != 'native_confinement']
    report['development_checks_passed'] = len(required) == required_count and all(entry['passed'] for entry in required) and not report['source_changed_during_verification']
    report['remote_bridge_verified'] = any(entry['name'] == 'remote_bridge' and entry['passed'] for entry in report['checks'])
    report['native_confinement_verified'] = any(entry['name'] == 'native_confinement' and entry['passed'] for entry in report['checks'])
    (root / 'docs/verification.json').write_text(json.dumps(report, indent=2) + '\n')
    return 0 if report['development_checks_passed'] and all(entry['passed'] for entry in report['checks']) else 1


if __name__ == '__main__':
    raise SystemExit(main())
