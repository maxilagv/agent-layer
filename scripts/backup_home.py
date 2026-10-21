#!/usr/bin/env python3
"""Offline encrypted backup for an optional user-systemd maintenance window.

The recipient file contains only the public age recipient. This script never
needs the private identity. With --service it stops and restarts that user
service; every other LayerAgent runtime must also be stopped.
"""
import argparse
import json
import pathlib
import subprocess
import time


def run(command):
    return subprocess.run(command, check=True, capture_output=True, text=True, timeout=600)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--binary', default='layeragent')
    parser.add_argument('--home', type=pathlib.Path, required=True)
    parser.add_argument('--directory', type=pathlib.Path, required=True)
    parser.add_argument('--recipient-file', type=pathlib.Path, required=True)
    parser.add_argument('--keep', type=int, default=14)
    parser.add_argument('--service', help='optional user-systemd service stopped only for this backup')
    args = parser.parse_args()
    if args.keep < 1:
        parser.error('--keep must be positive')
    args.directory.mkdir(mode=0o700, parents=True, exist_ok=True)
    if args.directory.is_symlink():
        parser.error('backup directory cannot be a symlink')
    recipient = args.recipient_file.read_text().strip()
    if not recipient.startswith('age1'):
        parser.error('recipient file must contain a native age public recipient')
    restarted = False
    if args.service:
        state = subprocess.run(['systemctl', '--user', 'is-active', '--quiet', args.service], timeout=30)
        if state.returncode not in (0, 3):
            parser.error('cannot determine user service state')
        restarted = state.returncode == 0
        if restarted:
            run(['systemctl', '--user', 'stop', args.service])
    try:
        archive = args.directory / f'layeragent-backup-{time.time_ns():020d}.age'
        run([args.binary, '--home', str(args.home), 'backup', 'create', '--output', str(archive), '--recipient', recipient])
        run([args.binary, 'backup', 'retain', '--directory', str(args.directory), '--keep', str(args.keep)])
        print(json.dumps({'archive': str(archive), 'encrypted': True, 'restore_drill_required': True}))
    finally:
        if restarted:
            run(['systemctl', '--user', 'start', args.service])


if __name__ == '__main__':
    main()
