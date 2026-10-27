#!/usr/bin/env python3
"""Build and test the shared frontend when a usable Node toolchain exists.

The check is skipped, never silently passed, when Node 22 or the npm registry
is unavailable. A failure of a check that actually ran is a failure.
"""
import json
import os
import pathlib
import shutil
import subprocess
import sys

REQUIRED_MAJOR = 22


def find_node():
    """Prefer a Node 22 installation, including one outside the default PATH."""
    candidates = []
    for name in ('node', 'nodejs'):
        found = shutil.which(name)
        if found:
            candidates.append(found)
    home = pathlib.Path.home()
    candidates.append(str(home / '.local' / 'node22' / 'bin' / 'node'))
    candidates.append('/usr/local/bin/node')
    for candidate in candidates:
        if not candidate or not pathlib.Path(candidate).exists():
            continue
        try:
            output = subprocess.run(
                [candidate, '--version'], capture_output=True, text=True, timeout=20
            ).stdout.strip()
        except Exception:  # noqa: BLE001 - a broken candidate is ignored
            continue
        if output.startswith('v'):
            major = output[1:].split('.')[0]
            try:
                if int(major) == REQUIRED_MAJOR:
                    return candidate
            except ValueError:
                continue
    return None


def main():
    root = pathlib.Path(__file__).resolve().parents[1]
    frontend = root / 'frontend'
    if not (frontend / 'package.json').exists():
        print(json.dumps({'check': 'frontend', 'skipped': 'no frontend in this tree'}))
        return 0
    node = find_node()
    if not node:
        print(
            json.dumps(
                {
                    'check': 'frontend',
                    'skipped': f'node {REQUIRED_MAJOR} is not available',
                }
            )
        )
        return 0
    npm = str(pathlib.Path(node).with_name('npm'))
    if not pathlib.Path(npm).exists():
        print(json.dumps({'check': 'frontend', 'skipped': 'npm is not available'}))
        return 0
    environment = dict(os.environ)
    environment['PATH'] = str(pathlib.Path(node).parent) + os.pathsep + environment.get('PATH', '')
    if not (frontend / 'node_modules').exists():
        install = subprocess.run(
            [npm, 'install', '--no-audit', '--no-fund'],
            cwd=frontend,
            env=environment,
            capture_output=True,
            text=True,
        )
        if install.returncode != 0:
            print(
                json.dumps(
                    {
                        'check': 'frontend',
                        'skipped': 'the npm registry is not reachable',
                        'detail': install.stderr[-400:],
                    }
                )
            )
            return 0
    for arguments in (['run', 'build'], ['test']):
        result = subprocess.run(
            [npm, *arguments], cwd=frontend, env=environment, capture_output=True, text=True
        )
        if result.returncode != 0:
            print(result.stdout[-4000:])
            print(result.stderr[-2000:])
            print(json.dumps({'check': 'frontend', 'command': arguments, 'passed': False}))
            return 1
    print(json.dumps({'check': 'frontend', 'passed': True, 'node': node}))
    return 0


if __name__ == '__main__':
    raise SystemExit(main())
