#!/usr/bin/env python3
"""Check publication boundaries and maintained-source evidence deterministically."""
import json
import pathlib
import sys
import tarfile
import tempfile

sys.path.insert(0, str(pathlib.Path(__file__).resolve().parents[1] / 'scripts'))
from prepare_release import payload, prepare
from source_fingerprint import fingerprint


def main():
    with tempfile.TemporaryDirectory(prefix='layeragent-release-') as directory:
        base = pathlib.Path(directory)
        root = base / 'layeragent'
        root.mkdir()
        (root / 'Cargo.lock').write_text('version = 4\n[[package]]\nname="fixture"\nversion="1.0.0"\n')
        (root / 'README.md').write_text('Public documentation')
        (root / 'crates').mkdir()
        (root / 'crates/src.rs').write_text('fn main() {}')
        (root / 'target').mkdir()
        (root / 'target/private.env').write_text('not included')
        (base / 'private-dossier.txt').write_text('must stay outside')
        before = fingerprint(root)
        archive = base / 'source.tar.gz'
        prepare(root, archive)
        with tarfile.open(archive) as bundle:
            names = bundle.getnames()
            assert 'layeragent/README.md' in names
            assert 'layeragent/DEPENDENCIES.json' in names
            assert not any('target' in name or 'private' in name for name in names)
        (root / 'docs').mkdir()
        (root / 'docs/verification.json').write_text('{"generated":true}')
        assert fingerprint(root) == before
        (root / 'crates/src.rs').write_text('fn main() { panic!(); }')
        assert fingerprint(root) != before
        (root / 'README.md').write_text('file' + ':///private/dossier')
        try:
            payload(root)
            raise AssertionError('private reference accepted')
        except ValueError:
            pass
        (root / 'README.md').write_text('public')
        (root / 'crates/.env').write_text('SECRET=must-be-refused')
        try:
            payload(root)
            raise AssertionError('secret filename accepted')
        except ValueError:
            pass
    # Audit the actual source tree as well, without producing an export.
    count = len(payload(pathlib.Path(__file__).resolve().parents[1]))
    print(json.dumps({'release_hygiene': 'passed', 'allowlisted_files': count}))


if __name__ == '__main__':
    main()
