#!/usr/bin/env python3
"""Measure process startup and Linux RSS for an optimized, empty local profile."""
import argparse
import json
import pathlib
import socket
import statistics
import subprocess
import tempfile
import time


def request(path, method='GET', route='/health'):
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(2)
        client.connect(str(path))
        client.sendall(f'{method} {route} HTTP/1.1\r\nHost: localhost\r\nContent-Length: 0\r\nConnection: close\r\n\r\n'.encode())
        return client.recv(4096)


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', default='target/release/layeragent')
    parser.add_argument('--samples', type=int, default=5)
    parser.add_argument('--output')
    args = parser.parse_args()
    binary = str(pathlib.Path(args.binary).resolve())
    starts, rss = [], []
    for _ in range(args.samples):
        with tempfile.TemporaryDirectory(prefix='layeragent-bench-') as temporary:
            home = pathlib.Path(temporary) / 'state'
            start = time.perf_counter()
            process = subprocess.Popen([binary, '--home', str(home), 'serve', '--ipc-only'], stdout=subprocess.DEVNULL, stderr=subprocess.PIPE)
            path = home / 'runtime/default.sock'
            try:
                deadline = time.monotonic() + 10
                while time.monotonic() < deadline:
                    try:
                        if b'200 OK' in request(path):
                            starts.append((time.perf_counter() - start) * 1000)
                            break
                    except OSError:
                        pass
                    if process.poll() is not None:
                        raise AssertionError(process.stderr.read().decode())
                    time.sleep(.002)
                else:
                    raise AssertionError('runtime startup timeout')
                time.sleep(.2)
                status = pathlib.Path(f'/proc/{process.pid}/status').read_text()
                rss.append(int(next(line.split()[1] for line in status.splitlines() if line.startswith('VmRSS:'))) / 1024)
                request(path, 'POST', '/v1/shutdown')
                process.wait(timeout=5)
            finally:
                if process.poll() is None:
                    process.terminate()
                    process.wait(timeout=5)
    result = {
        'profile': 'release; empty configuration; SQLite initialized; no models or channels loaded',
        'samples': args.samples,
        'process_start_ms': starts,
        'process_start_p50_ms': statistics.median(starts),
        'process_start_max_ms': max(starts),
        'idle_rss_mib': rss,
        'idle_rss_max_mib': max(rss),
    }
    if args.output:
        pathlib.Path(args.output).write_text(json.dumps(result, indent=2) + '\n')
    print(json.dumps(result))


if __name__ == '__main__':
    main()
