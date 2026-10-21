#!/usr/bin/env python3
"""Pinned dependency checks with exact-version, exact-parent, expiring waivers.

cargo-deny ignores advisory IDs globally. This wrapper first proves each waiver
has only the reviewed dependency edges and is within its review deadline. CI
and the local verifier invoke this wrapper rather than bypassing those checks.
"""
import argparse
import datetime
import json
import pathlib
import shutil
import subprocess
import sys
import tomllib


def waiver_scope(metadata, waivers, today):
    packages = {p['id']: p for p in metadata['packages']}
    parents = {pkg: set() for pkg in packages}
    for node in metadata['resolve']['nodes']:
        parent = packages[node['id']]
        for dep in node['deps']:
            parents[dep['pkg']].add(f"{parent['name']}@{parent['version']}")
    for waiver in waivers:
        if today > datetime.date.fromisoformat(waiver['review_by']):
            raise ValueError(f"expired dependency waiver: {waiver['id']}")
        matches = [p for p in packages.values() if p['name'] == waiver['crate']]
        if not matches:
            raise ValueError(f"unused dependency waiver: {waiver['id']}; remove it")
        for package in matches:
            if package['version'] != waiver['version'] or parents[package['id']] != set(waiver['parents']):
                raise ValueError(f"dependency waiver scope changed: {waiver['id']} {package['name']}@{package['version']} parents={sorted(parents[package['id']])}")


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--cargo', default=shutil.which('cargo') or str(pathlib.Path.home() / '.cargo/bin/cargo'))
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parents[1]
    try:
        version = subprocess.run([args.cargo, 'deny', '--version'], check=True, capture_output=True, text=True).stdout.strip()
        if version != 'cargo-deny 0.20.2':
            raise ValueError('install pinned checker: cargo install cargo-deny --version 0.20.2 --locked')
        waivers = json.loads((root / 'scripts/dependency_waivers.json').read_text())['waivers']
        policy = tomllib.loads((root / 'deny.toml').read_text())
        ignored = {item['id'] for item in policy['advisories']['ignore']}
        if ignored != {w['id'] for w in waivers}:
            raise ValueError('deny.toml ignores must exactly match reviewed dependency waivers')
        result = subprocess.run([args.cargo, 'metadata', '--locked', '--all-features', '--format-version', '1'], cwd=root, check=True, capture_output=True, text=True)
        waiver_scope(json.loads(result.stdout), waivers, datetime.datetime.now(datetime.timezone.utc).date())
        command = [args.cargo, 'deny', '--all-features', 'check', 'advisories', 'bans', 'licenses', 'sources']
        result = subprocess.run(command, cwd=root, capture_output=True, text=True)
        logs = root / 'target/verification'
        logs.mkdir(parents=True, exist_ok=True)
        (logs / 'dependency_policy_diagnostics.log').write_text(result.stdout + result.stderr)
        sys.path.insert(0, str(root / 'scripts'))
        from source_fingerprint import fingerprint
        report = {'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(), 'checker': version,
                  **fingerprint(root), 'passed': result.returncode == 0, 'waivers': waivers,
                  'diagnostics': 'target/verification/dependency_policy_diagnostics.log'}
        (logs / 'dependency_policy.json').write_text(json.dumps(report, indent=2) + '\n')
        print(json.dumps({'dependency_policy_passed': report['passed'], 'maintenance_waivers': len(waivers), 'report': 'target/verification/dependency_policy.json'}))
        if result.returncode:
            print((result.stdout + result.stderr)[-8000:])
        return result.returncode
    except (ValueError, subprocess.CalledProcessError, FileNotFoundError) as error:
        print(f'Dependency policy failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
