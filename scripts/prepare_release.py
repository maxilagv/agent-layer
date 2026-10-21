#!/usr/bin/env python3
"""Prepare a local, reviewable source bundle. Never invokes git or publishes anything."""
import argparse
import gzip
import hashlib
import io
import json
import pathlib
import re
import tarfile
import tomllib

from source_fingerprint import ROOT_FILES, SOURCE_DIRS, fingerprint, source_paths


def allowed(relative):
    # Entire source trees are intentional. Generated state, build outputs and all
    # sibling directories are outside this allowlist.
    return (len(relative.parts) == 1 and relative.name in ROOT_FILES
            or relative.parts[0] in SOURCE_DIRS
            or relative.parts[0] == 'docs' and relative.suffix == '.md'
            and bool(re.match(r'^\d\d-', relative.name)))


def payload(root):
    paths = []
    for path in source_paths(root):
        rel = path.relative_to(root)
        if not allowed(rel):
            continue
        if path.is_symlink():
            raise ValueError(f'release symlink refused: {rel}')
        if not path.is_file():
            continue
        if any(p == '.env' or p.startswith('.env.') or p.endswith(('.pem', '.key'))
               or p in {'service.env', 'credentials.json', 'secrets.json'} for p in rel.parts):
            raise ValueError(f'credential file refused: {rel}')
        raw = path.read_bytes()
        # Reject actual user paths and internal dossier links, including escaped
        # spaces in file URLs. These are publication boundaries, not a secret scanner.
        text = raw.decode('utf-8', errors='replace')
        markers = ('file' + ':///home/', 'file' + ':///Users/', 'file' + ':///private/', 'file' + ':///private-research/',
                   '../' + 'codex', '../' + 'hermes', '../' + 'evidence', '../' + 'dossiers')
        private_home = any(match.group(1) not in {'user', 'operator', 'developer', 'runner', '*', '<user>'}
                           for match in re.finditer(r"/(?:home|Users)/([^/\s\"']+)", text))
        if private_home or any(marker in text for marker in markers):
            raise ValueError(f'private workspace reference refused: {rel}')
        paths.append((rel.as_posix(), raw))
    return sorted(paths)


def prepare(root, destination):
    if destination.exists():
        raise ValueError('release destination already exists')
    if root.resolve() in destination.resolve().parents:
        raise ValueError('release output must be outside the project')
    files = payload(root)
    manifest = {'format': 1, 'purpose': 'local source review; not published',
                'source_evidence': fingerprint(root),
                'files': [{'path': name, 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()}
                          for name, raw in files]}
    lock = tomllib.loads((root / 'Cargo.lock').read_text())
    inventory = {'format': 1, 'source': 'Cargo.lock',
                 'packages': [{key: p[key] for key in ('name', 'version', 'source', 'checksum') if key in p}
                              for p in lock['package']],
                 'note': 'Dependency inventory, not a vulnerability scan or license certification. Run cargo audit and cargo deny separately.'}
    files += [('RELEASE-MANIFEST.json', (json.dumps(manifest, indent=2) + '\n').encode()),
              ('DEPENDENCIES.json', (json.dumps(inventory, indent=2) + '\n').encode())]
    # Deterministic ordering and metadata, with no absolute paths or host ownership.
    try:
        with destination.open('xb') as target:
            with gzip.GzipFile(filename='', fileobj=target, mode='wb', mtime=0) as zipped:
                with tarfile.open(fileobj=zipped, mode='w') as archive:
                    for name, raw in sorted(files):
                        info = tarfile.TarInfo('layeragent/' + name)
                        info.size = len(raw)
                        info.mode = 0o644
                        archive.addfile(info, io.BytesIO(raw))
    except BaseException:
        destination.unlink(missing_ok=True)
        raise
    return {'archive': str(destination), 'files': len(files),
            'sha256': hashlib.sha256(destination.read_bytes()).hexdigest()}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=pathlib.Path, required=True)
    args = parser.parse_args()
    print(json.dumps(prepare(pathlib.Path(__file__).resolve().parents[1], args.output), indent=2))
