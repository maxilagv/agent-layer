#!/usr/bin/env python3
"""Exercise terminal input, streaming, completion and terminal restoration in a PTY."""
import fcntl
import http.server
import json
import os
import pathlib
import pty
import select
import sqlite3
import struct
import subprocess
import tempfile
import termios
import threading
import time
from system_smoke import Model


def main():
    binary = str(pathlib.Path('target/debug/layeragent').resolve())
    server = http.server.ThreadingHTTPServer(('127.0.0.1', 0), Model)
    threading.Thread(target=server.serve_forever, daemon=True).start()
    with tempfile.TemporaryDirectory(prefix='layeragent-tui-') as temporary:
        base = pathlib.Path(temporary)
        home, workspace = base / 'state', base / 'workspace'
        workspace.mkdir()
        command = [binary, '--home', str(home)]
        subprocess.run(command + ['init', '--protocol', 'openai_chat', '--model', 'smoke-fixture', '--base-url', f'http://127.0.0.1:{server.server_port}/v1'], check=True, capture_output=True)
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 100, 0, 0))
        process = subprocess.Popen(command + ['tui', '--workspace', str(workspace)], stdin=slave, stdout=slave, stderr=slave, env=dict(os.environ, TERM='xterm-256color'), start_new_session=True)
        os.close(slave)
        captured = bytearray()

        def until(predicate, seconds=20):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    try:
                        captured.extend(os.read(master, 65536))
                    except OSError:
                        pass
                if predicate():
                    return
                if process.poll() is not None:
                    raise AssertionError(f'TUI exited early: {captured[-2000:]!r}')
            raise AssertionError(f'TUI timeout: {captured[-2000:]!r}')

        try:
            until(lambda: b'LayerAgent' in captured and 'Escribe tu mensaje'.encode() in captured)
            os.write(master, b'Verify the terminal flow\r')
            database = home / 'data/default/tenants/default/state.db'

            def completed():
                with sqlite3.connect(database) as connection:
                    committed = connection.execute("SELECT count(*) FROM turns WHERE state='completed'").fetchone()[0] == 1
                # The database commits before the client receives the final event.
                # Ctrl+C while the UI is still busy means cancel, not quit. Wait
                # for the actual idle status rendered after the assistant text.
                answer = captured.find(b'verified')
                idle = answer >= 0 and b'Listo' in captured[answer:]
                return committed and idle

            until(completed)
            os.write(master, b'\x03')
            # Keep consuming the farewell animation. A PTY has a small output
            # buffer; waiting without reading can block the terminal writer and
            # falsely report a shutdown timeout.
            until(lambda: process.poll() is not None, seconds=5)
            process.wait(timeout=1)
            assert process.returncode == 0
            if select.select([master], [], [], .2)[0]:
                try:
                    captured.extend(os.read(master, 65536))
                except OSError:
                    pass
            assert b'\x1b[?1049l' in captured, 'alternate screen was not restored'
            print(json.dumps({'tui_pty_input_streaming_completion_restore': 'passed'}))
        finally:
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
            os.close(master)
            subprocess.run(command + ['stop'], capture_output=True, timeout=10)
    server.shutdown()


if __name__ == '__main__':
    main()
