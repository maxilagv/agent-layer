#!/usr/bin/env python3
"""Ephemeral-key regression checks; never creates a trusted release signature."""
import hashlib
import json
import os
import pathlib
import shutil
import subprocess
import sys
import tempfile

SCRIPT = pathlib.Path(__file__).resolve().parents[1] / 'scripts/sign_source.py'


def main():
    openssl = shutil.which('openssl')
    if not openssl:
        raise RuntimeError('OpenSSL 3 is required; missing prerequisite is not a pass')
    with tempfile.TemporaryDirectory(prefix='layeragent-signing-test-') as temporary:
        base = pathlib.Path(temporary).resolve()
        archive = base / 'fixture.tar.gz'
        original = b'ephemeral source archive test; not a release\n' * 100
        archive.write_bytes(original)
        key = base / 'ephemeral-test.pem'
        wrong_key = base / 'wrong-test.pem'
        wrong_public = base / 'wrong-public.pem'
        for private in (key, wrong_key):
            subprocess.run([openssl, 'genpkey', '-algorithm', 'ED25519', '-out', str(private)],
                           check=True, capture_output=True, timeout=30)
            os.chmod(private, 0o600)
        subprocess.run([openssl, 'pkey', '-in', str(wrong_key), '-pubout', '-out', str(wrong_public)],
                       check=True, capture_output=True, timeout=30)
        signature = base / 'fixture.sig'
        public = base / 'public.pem'
        metadata = base / 'metadata.json'

        def cli(action, success=True, **arguments):
            command = [sys.executable, str(SCRIPT), '--openssl', openssl, action]
            for option, path in arguments.items():
                command.extend(['--' + option.replace('_', '-'), str(path)])
            result = subprocess.run(command, capture_output=True, text=True, timeout=30)
            assert (result.returncode == 0) == success, (command, result.stdout, result.stderr)
            assert key.read_text().strip() not in result.stdout + result.stderr
            return result

        signing = dict(archive=archive, private_key=key, signature=signature,
                       public_key_output=public, metadata=metadata)
        cli('sign', **signing)
        assert len(signature.read_bytes()) == 64
        sidecar = json.loads(metadata.read_text())
        assert sidecar['archive_sha256'] == hashlib.sha256(original).hexdigest()
        verified = json.loads(cli('verify', archive=archive, signature=signature, public_key=public).stdout)
        assert verified['verified'] and verified['archive_sha256'] == sidecar['archive_sha256']
        cli('verify', archive=archive, signature=signature, public_key=wrong_public, success=False)
        metadata.write_text(json.dumps({'public_key': str(wrong_public), 'verified': True}))
        cli('verify', archive=archive, signature=signature, public_key=wrong_public, success=False)
        cli('verify', archive=archive, signature=signature, public_key=public)
        archive.write_bytes(original + b'tampered')
        cli('verify', archive=archive, signature=signature, public_key=public, success=False)
        archive.write_bytes(original)
        sigbytes = signature.read_bytes()
        signature.write_bytes(bytes([sigbytes[0] ^ 1]) + sigbytes[1:])
        cli('verify', archive=archive, signature=signature, public_key=public, success=False)
        signature.write_bytes(sigbytes)
        before = [path.read_bytes() for path in (signature, public, metadata)]
        cli('sign', **signing, success=False)
        assert before == [path.read_bytes() for path in (signature, public, metadata)]
        if os.name == 'posix':
            link = base / 'archive-link'
            link.symlink_to(archive)
            cli('verify', archive=link, signature=signature, public_key=public, success=False)
            key_link = base / 'key-link'
            key_link.symlink_to(key)
            new_outputs = dict(archive=archive, private_key=key_link, signature=base / 'new.sig',
                               public_key_output=base / 'new-public.pem', metadata=base / 'new.json')
            cli('sign', **new_outputs, success=False)
            assert not (base / 'new.sig').exists()
            os.chmod(key, 0o644)
            new_outputs['private_key'] = key
            cli('sign', **new_outputs, success=False)
            os.chmod(key, 0o600)
            fifo = base / 'not-an-archive'
            os.mkfifo(fifo)
            cli('verify', archive=fifo, signature=signature, public_key=public, success=False)
        unsafe = dict(signing)
        unsafe.update(signature=SCRIPT.parent / 'must-not-create.sig',
                      public_key_output=base / 'new-public.pem', metadata=base / 'new.json')
        cli('sign', **unsafe, success=False)
        assert not (SCRIPT.parent / 'must-not-create.sig').exists()
        print(json.dumps({'source_signing': 'passed', 'algorithm': 'Ed25519',
                          'wrong_key_and_tamper_rejected': True, 'ephemeral_test_identity_only': True}))


if __name__ == '__main__':
    main()
