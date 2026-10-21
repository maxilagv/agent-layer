#!/usr/bin/env python3
"""Run full Linux qualification in an explicitly delegated transient service.

Requires systemd >=254, passwordless administrator access and a supporting
Bubblewrap/AppArmor/Landlock host. Does not alter host-wide kernel protections.
This is CI preparation, not a fallback for ordinary tool execution.
"""
import getpass
import json
import os
import pathlib
import shutil
import subprocess
import sys
import uuid


def main():
    root = pathlib.Path(__file__).resolve().parents[1]
    cargo = shutil.which('cargo') or str(pathlib.Path.home() / '.cargo/bin/cargo')
    if sys.platform != 'linux' or not pathlib.Path('/run/systemd/system').is_dir():
        raise SystemExit('native qualification requires a Linux systemd host; no test was certified')
    result = subprocess.run([cargo, 'test', '-p', 'layeragent-cli', '--test', 'native',
                             '--locked', '--no-run', '--message-format=json'],
                            cwd=root, check=True, stdout=subprocess.PIPE, text=True)
    binaries = []
    for line in result.stdout.splitlines():
        item = json.loads(line)
        if item.get('reason') == 'compiler-artifact' and item.get('target', {}).get('name') == 'native':
            if item.get('executable'):
                binaries.append(item['executable'])
    if len(binaries) != 1:
        raise SystemExit('cannot identify the exact native test executable')
    unit = 'layeragent-native-' + uuid.uuid4().hex
    command = ['sudo', '-n', 'systemd-run', '--wait', '--pipe', '--collect',
               '--unit=' + unit, '--uid=' + getpass.getuser(),
               '--working-directory=' + str(root),
               '--property=Delegate=memory pids cpu',
               '--property=DelegateSubgroup=supervisor',
               '--property=RuntimeMaxSec=600',
               '--setenv=HOME=' + str(pathlib.Path.home()),
               '--setenv=PATH=' + os.defpath,
               binaries[0], '--ignored', '--nocapture']
    return subprocess.run(command, cwd=root).returncode


if __name__ == '__main__':
    sys.exit(main())
