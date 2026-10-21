#!/usr/bin/env python3
"""Stable fingerprint of maintained inputs, excluding outputs and generated reports."""
import hashlib
import json
import os
import pathlib

ROOT_FILES = {'Cargo.toml', 'Cargo.lock', 'deny.toml', 'rust-toolchain.toml', '.gitignore', 'README.md',
              'LICENSE', 'AGENTS.md', 'PRODUCT.md', 'SECURITY.md', 'CONTRIBUTING.md', 'CHANGELOG.md'}
SOURCE_DIRS = {'crates', 'tests', 'scripts', 'examples', 'deploy', '.github', 'assets'}


def source_paths(root):
    ignored = {'target', '.git', '__pycache__', '.pytest_cache', 'node_modules'}
    paths = []
    for name in ROOT_FILES:
        path = root / name
        if path.is_symlink():
            raise ValueError(f'symlink in maintained input: {name}')
        if path.is_file():
            paths.append(path)
    for tree in sorted(SOURCE_DIRS | {'docs'}):
        base = root / tree
        if base.is_symlink():
            raise ValueError(f'symlink in maintained input: {tree}')
        if not base.is_dir():
            continue
        for directory, children, filenames in os.walk(base, followlinks=False):
            children[:] = [name for name in children if name not in ignored]
            for name in children:
                child = pathlib.Path(directory) / name
                if child.is_symlink():
                    raise ValueError(f'symlink in maintained input: {child.relative_to(root)}')
            for name in filenames:
                if name in ignored:
                    continue
                path = pathlib.Path(directory) / name
                if tree == 'docs' and path.suffix != '.md':
                    continue
                if path.is_symlink():
                    raise ValueError(f'symlink in maintained input: {path.relative_to(root)}')
                if path.is_file():
                    paths.append(path)
    return sorted(paths, key=lambda p: p.relative_to(root).as_posix())


def fingerprint(root):
    digest = hashlib.sha256()
    paths = source_paths(root)
    for path in paths:
        digest.update(path.relative_to(root).as_posix().encode())
        digest.update(b'\0')
        digest.update(path.read_bytes())
        digest.update(b'\0')
    return {'algorithm': 'sha256-path-nul-content-nul-v1', 'source_sha256': digest.hexdigest(),
            'input_count': len(paths)}


if __name__ == '__main__':
    print(json.dumps(fingerprint(pathlib.Path(__file__).resolve().parents[1]), indent=2))
