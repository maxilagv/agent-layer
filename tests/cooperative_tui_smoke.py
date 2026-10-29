#!/usr/bin/env python3
"""Run the cooperative terminal commands and panel in an actual PTY."""
import fcntl
import json
import os
import pathlib
import pty
import select
import struct
import subprocess
import termios
import time

from cooperative_smoke import environment
from terminal_screen import TextScreen


def main():
    binary = pathlib.Path('target/debug/layeragent').resolve()
    with environment(binary) as (server, _, workspace, command, _, api):
        master, slave = pty.openpty()
        fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 36, 150, 0, 0))
        process = subprocess.Popen(command + ['tui', '--workspace', str(workspace)],
                                   stdin=slave, stdout=slave, stderr=slave,
                                   env=dict(os.environ, TERM='xterm-256color'), start_new_session=True)
        os.close(slave)
        captured = bytearray()
        screen = TextScreen(150, 36)

        def until(check, seconds=45):
            deadline = time.monotonic() + seconds
            while time.monotonic() < deadline:
                if select.select([master], [], [], .05)[0]:
                    try:
                        chunk = os.read(master, 65536)
                        captured.extend(chunk)
                        screen.feed(chunk)
                    except OSError:
                        pass
                if check():
                    return
                if process.poll() is not None:
                    raise AssertionError(f'TUI exited early:\n{screen.text()}')
                time.sleep(.15)
            raise AssertionError(f'TUI timeout:\n{screen.text()}')

        def send(text):
            os.write(master, text.encode() + b'\r')

        try:
            until(lambda: 'LayerAgent' in screen.text() and 'Escribe tu mensaje' in screen.text())
            send('/team start audit -- Review the toy gateway with the scripted provider.')
            until(lambda: server.scenario.entered.is_set())
            # Runtime startup precedes delivery of the creation response and UI
            # snapshot. Wait until an operator can actually see the selected team.
            until(lambda: 'Equipo cooperativo' in screen.text() and 'coordinator' in screen.text())
            tasks = api('GET', '/v1/tasks')[1]['tasks']
            assert len(tasks) == 1, tasks
            task = tasks[0]['id']

            def snapshot():
                return api('GET', f'/v1/tasks/{task}/team')[1]['team']

            send('/team pause')
            until(lambda: snapshot()['state'] == 'paused')
            server.scenario.release.set()
            until(lambda: not snapshot()['pause_pending'])
            send('/team message analyst Keep uncertainty visible in the report.')
            until(lambda: any(m['sender_kind'] == 'user' for m in api('GET', f'/v1/tasks/{task}/team/messages')[1]['items']))
            send('/team resume')
            until(lambda: snapshot()['state'] == 'terminated')
            until(lambda: 'terminated' in screen.text())
            send('/team findings')
            until(lambda: 'inconclusive' in screen.text())
            # The same retained panel remains usable at a compact terminal size.
            fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 24, 80, 0, 0))
            screen.resize(80, 24)
            send('/team')
            send('/team attach ' + task)
            send('/team members')
            until(lambda: 'analyst' in screen.text() and 'reviewer' in screen.text())
            assert len(api('GET', '/v1/tasks')[1]['tasks']) == 1, 'attach started another task'
            send('/quit')
            until(lambda: process.poll() is not None, seconds=8)
            assert process.wait(timeout=1) == 0
            assert b'\x1b[?1049l' in captured, 'alternate screen was not restored'
            print(json.dumps({'cooperative_tui_create_panel_pause_message_resume_attach': 'passed',
                              'terminal_restore': 'passed', 'provider': 'scripted fixture'}))
        finally:
            server.scenario.release.set()
            if process.poll() is None:
                process.terminate()
                process.wait(timeout=5)
            os.close(master)


if __name__ == '__main__':
    main()
