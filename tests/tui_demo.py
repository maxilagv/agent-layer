#!/usr/bin/env python3
"""Open the real terminal UI against a local fixture model, so its look can be judged
without configuring any provider. Usage: python3 tests/tui_demo.py [--release]"""
import http.server
import os
import pathlib
import subprocess
import sys
import tempfile
import threading

sys.path.insert(0, str(pathlib.Path(__file__).parent))
from system_smoke import Model  # noqa: E402


def main():
    flavor = 'release' if '--release' in sys.argv else 'debug'
    binary = str(pathlib.Path(f'target/{flavor}/layeragent').resolve())
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='layeragent-demo-') as temporary:
        base = pathlib.Path(temporary)
        home, workspace = base / 'state', base / 'workspace'
        workspace.mkdir()
        command = [binary, '--home', str(home)]
        subprocess.run(command + ['init', '--protocol', 'openai_chat', '--model', 'demo-fixture',
                                  '--base-url', f'http://127.0.0.1:{server.server_port}/v1'],
                       check=True, capture_output=True)
        print('Opening the terminal UI. Type a message and press Enter; Ctrl+C quits.')
        subprocess.run(command + ['tui', '--workspace', str(workspace)], env=dict(os.environ, TERM=os.environ.get('TERM', 'xterm-256color')))
        subprocess.run(command + ['stop'], capture_output=True)


if __name__ == '__main__':
    main()
