#!/usr/bin/env python3
"""Sign exact source-archive bytes with an externally managed Ed25519 key.

Requires OpenSSL 3. Never generates a release key, publishes, or establishes key
trust. Verification requires an explicitly selected trusted public key. The JSON
sidecar is unsigned descriptive metadata; only the detached signature authenticates
archive bytes (including any manifest inside the archive). Outputs and private
keys must be outside this project. Private keys must be unencrypted PEM files
with private permissions; encrypted/hardware keys need a separate signing process.
"""
import argparse
import datetime
import hashlib
import json
import os
import pathlib
import shutil
import stat
import subprocess
import sys
import tempfile

ROOT = pathlib.Path(__file__).resolve().parents[1]
MAX_ARCHIVE = 512 * 1024 * 1024  # OpenSSL Ed25519 uses a bounded one-shot operation.
ED25519_SPKI_PREFIX = bytes.fromhex('302a300506032b6570032100')


def safe_path(path):
    path = pathlib.Path(os.path.abspath(path))
    for parent in (path, *path.parents):
        try:
            metadata = parent.lstat()
        except FileNotFoundError:
            continue
        if stat.S_ISLNK(metadata.st_mode):
            raise ValueError('symlink paths are refused')
    return path


def external(path):
    path = safe_path(path)
    if path == ROOT or ROOT in path.parents:
        raise ValueError('private keys and signing outputs must be outside the project')
    return path


def read_regular(path, maximum, private=False):
    path = safe_path(path)
    descriptor = os.open(path, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0))
    with os.fdopen(descriptor, 'rb') as source:
        before = os.fstat(source.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= maximum:
            raise ValueError('input must be a nonempty regular file within size limits')
        if private and os.name == 'posix' and before.st_mode & 0o077:
            raise ValueError('private key permissions must be 600 or stricter')
        raw = source.read(maximum + 1)
        after = os.fstat(source.fileno())
        if len(raw) > maximum or (before.st_ino, before.st_dev, before.st_size, before.st_mtime_ns) != (after.st_ino, after.st_dev, after.st_size, after.st_mtime_ns):
            raise ValueError('input changed while reading')
        return raw


def create_new(path, data):
    path = safe_path(path)
    descriptor = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL | getattr(os, 'O_NOFOLLOW', 0), 0o600)
    try:
        with os.fdopen(descriptor, 'wb') as target:
            target.write(data)
            target.flush()
            os.fsync(target.fileno())
    except BaseException:
        path.unlink(missing_ok=True)
        raise


def openssl(binary, *args, input_data=None):
    result = subprocess.run([binary, *map(str, args)], input=input_data, capture_output=True, timeout=120)
    if result.returncode:
        # OpenSSL diagnostics can contain paths or input material. Keep them out
        # of machine-readable release output and failure reports.
        raise ValueError(f'OpenSSL operation failed ({args[0]}); check key type, signature and input')
    return result.stdout


def require_openssl(binary):
    version = openssl(binary, 'version').decode('ascii', errors='replace')
    if not version.startswith('OpenSSL 3.'):
        raise ValueError('OpenSSL 3 is required')


def public_der(binary, public_pem):
    der = openssl(binary, 'pkey', '-pubin', '-outform', 'DER', input_data=public_pem)
    if len(der) != 44 or not der.startswith(ED25519_SPKI_PREFIX):
        raise ValueError('only Ed25519 keys are accepted')
    return der


def snapshot(directory, archive):
    # The public archive may be large, so stream it into a private snapshot.
    archive = safe_path(archive)
    descriptor = os.open(archive, os.O_RDONLY | getattr(os, 'O_NOFOLLOW', 0) | getattr(os, 'O_NONBLOCK', 0))
    target = directory / 'archive'
    digest = hashlib.sha256()
    with os.fdopen(descriptor, 'rb') as source:
        before = os.fstat(source.fileno())
        if not stat.S_ISREG(before.st_mode) or not 0 < before.st_size <= MAX_ARCHIVE:
            raise ValueError('archive must be a nonempty regular file of at most 512 MiB')
        with target.open('xb') as output:
            os.chmod(target, 0o600)
            total = 0
            while block := source.read(1024 * 1024):
                total += len(block)
                if total > MAX_ARCHIVE:
                    raise ValueError('archive exceeds 512 MiB')
                output.write(block)
                digest.update(block)
        after = os.fstat(source.fileno())
        if total != before.st_size or (before.st_ino, before.st_dev, before.st_size, before.st_mtime_ns) != (after.st_ino, after.st_dev, after.st_size, after.st_mtime_ns):
            raise ValueError('archive changed while snapshotting')
    return target, digest.hexdigest(), total


def temp_root():
    path = pathlib.Path(tempfile.gettempdir()).resolve()
    if path == ROOT or ROOT in path.parents:
        raise ValueError('temporary directory must be outside the project')
    return path


def sign(args):
    outputs = [external(path) for path in (args.signature, args.public_key_output, args.metadata)]
    if len(set(outputs)) != 3 or any(path.exists() for path in outputs):
        raise ValueError('three distinct, nonexistent signing output paths are required')
    if any(not path.parent.is_dir() for path in outputs):
        raise ValueError('signing output parent directories must already exist')
    private = read_regular(external(args.private_key), 65_536, private=True)
    with tempfile.TemporaryDirectory(prefix='layeragent-sign-', dir=temp_root()) as temporary:
        directory = pathlib.Path(temporary)
        key = directory / 'key.pem'
        create_new(key, private)
        del private
        archive, digest, size = snapshot(directory, args.archive)
        public = openssl(args.openssl, 'pkey', '-in', key, '-passin', 'pass:', '-pubout')
        der = public_der(args.openssl, public)
        signature = openssl(args.openssl, 'pkeyutl', '-sign', '-rawin', '-inkey', key, '-passin', 'pass:', '-in', archive)
        if len(signature) != 64:
            raise ValueError('unexpected Ed25519 signature length')
        # Validate the signature before publishing any output.
        verify_key = directory / 'public.pem'
        verify_signature = directory / 'signature'
        create_new(verify_key, public)
        create_new(verify_signature, signature)
        openssl(args.openssl, 'pkeyutl', '-verify', '-pubin', '-inkey', verify_key, '-rawin', '-in', archive, '-sigfile', verify_signature)
        metadata = {
            'format': 1, 'algorithm': 'Ed25519', 'utc': datetime.datetime.now(datetime.timezone.utc).isoformat(),
            'archive_sha256': digest, 'archive_bytes': size,
            'public_key_spki_sha256': hashlib.sha256(der).hexdigest(),
            'signature_sha256': hashlib.sha256(signature).hexdigest(),
            'coverage': 'Detached signature authenticates exact archive bytes. This JSON sidecar is unsigned.',
            'trust': 'Select and authenticate the trusted public key independently; never auto-trust an exported key.',
        }
        created = []
        try:
            for path, data in zip(outputs, (signature, public, (json.dumps(metadata, indent=2) + '\n').encode())):
                create_new(path, data)
                created.append(path)
        except BaseException:
            for path in created:
                path.unlink(missing_ok=True)
            raise
    return metadata


def verify(args):
    # Deliberately no metadata or archive-provided key lookup.
    public = read_regular(args.public_key, 65_536)
    der = public_der(args.openssl, public)
    signature = read_regular(args.signature, 64)
    if len(signature) != 64:
        raise ValueError('Ed25519 signatures must have exactly 64 bytes')
    with tempfile.TemporaryDirectory(prefix='layeragent-verify-', dir=temp_root()) as temporary:
        directory = pathlib.Path(temporary)
        archive, digest, size = snapshot(directory, args.archive)
        key = directory / 'trusted-public.pem'
        sigfile = directory / 'signature'
        create_new(key, public)
        create_new(sigfile, signature)
        openssl(args.openssl, 'pkeyutl', '-verify', '-pubin', '-inkey', key, '-rawin', '-in', archive, '-sigfile', sigfile)
    return {'verified': True, 'algorithm': 'Ed25519', 'archive_sha256': digest, 'archive_bytes': size,
            'trusted_public_key_spki_sha256': hashlib.sha256(der).hexdigest()}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--openssl', default=shutil.which('openssl') or 'openssl')
    commands = parser.add_subparsers(dest='action', required=True)
    signing = commands.add_parser('sign', help='sign with an externally managed private key')
    for option in ('archive', 'private-key', 'signature', 'public-key-output', 'metadata'):
        signing.add_argument('--' + option, type=pathlib.Path, required=True)
    verification = commands.add_parser('verify', help='verify using an explicitly trusted public key')
    for option in ('archive', 'signature', 'public-key'):
        verification.add_argument('--' + option, type=pathlib.Path, required=True)
    args = parser.parse_args()
    try:
        require_openssl(args.openssl)
        print(json.dumps(sign(args) if args.action == 'sign' else verify(args), indent=2))
        return 0
    except (OSError, ValueError, subprocess.SubprocessError) as error:
        print(f'Source signing failed: {error}', file=sys.stderr)
        return 1


if __name__ == '__main__':
    raise SystemExit(main())
