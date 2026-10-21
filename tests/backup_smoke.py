#!/usr/bin/env python3
"""Exercise the actual backup CLI, SQLite WAL snapshot and failed restore cleanup."""
import argparse
import json
import os
import pathlib
import sqlite3
import subprocess
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--binary', default='target/debug/layeragent')
    binary = str(pathlib.Path(parser.parse_args().binary).resolve())
    with tempfile.TemporaryDirectory(prefix='layeragent-backup-') as directory:
        base = pathlib.Path(directory).resolve()
        home = base / 'home'
        home.mkdir()
        (home / 'config.toml').write_text('instructions = "backup smoke"\n')
        (home / 'service.env').write_text('API_KEY=private-fixture')
        dbpath = home / 'data/default/tenants/default/state.db'
        dbpath.parent.mkdir(parents=True)
        with sqlite3.connect(dbpath) as db:
            db.execute('PRAGMA journal_mode=WAL')
            db.execute('CREATE TABLE fixture(value TEXT)')
            db.execute('INSERT INTO fixture VALUES(?)', ('survives restore',))
            db.commit()

            def cli(*args, success=True):
                result = subprocess.run([binary, '--home', str(home), 'backup', *map(str, args)],
                                        capture_output=True, text=True, timeout=30)
                assert (result.returncode == 0) == success, (args, result.stderr)
                return result

            key = base / 'key.ageidentity'
            recipient = cli('keygen', '--identity', key).stdout.strip()
            archive = base / 'snapshot.age'
            cli('create', '--output', archive, '--recipient', recipient)
        manifest = json.loads(cli('verify', '--archive', archive, '--identity', key).stdout)
        assert {f['path'] for f in manifest['files']} == {'config.toml', 'data/default/tenants/default/state.db'}
        restored = base / 'restored'
        cli('restore', '--archive', archive, '--identity', key, '--destination', restored)
        with sqlite3.connect(restored / 'data/default/tenants/default/state.db') as db:
            assert db.execute('SELECT value FROM fixture').fetchone()[0] == 'survives restore'
            assert db.execute('PRAGMA integrity_check').fetchone()[0] == 'ok'
        assert not (restored / 'service.env').exists()
        cli('restore', '--archive', archive, '--identity', key, '--destination', restored, success=False)
        cli('create', '--output', archive, '--recipient', recipient, success=False)
        tampered = bytearray(archive.read_bytes())
        tampered[-1] ^= 1
        archive.write_bytes(tampered)
        failed = base / 'failed'
        cli('restore', '--archive', archive, '--identity', key, '--destination', failed, success=False)
        assert not failed.exists()
        if os.name == 'posix':
            assert restored.stat().st_mode & 0o777 == 0o700
        print(json.dumps({'encrypted_backup_restore': 'passed', 'tamper_fails_without_publishing_home': True}))


if __name__ == '__main__':
    main()
