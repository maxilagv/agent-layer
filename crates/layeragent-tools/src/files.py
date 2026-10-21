import sys, json, pathlib, hashlib, os, tempfile, stat

def secret(name):
    return name.startswith('.env') or name in ('.layeragent', '.ssh', '.aws', '.gnupg', '.kube', '.netrc', '.npmrc', '.pypirc', '.git-credentials')

def bounded_read(file):
    with file.open('rb') as source:
        raw = source.read(16 * 1024 * 1024 + 1)
    if len(raw) > 16 * 1024 * 1024:
        raise ValueError('file exceeds read limit')
    return raw

args = json.load(sys.stdin)
root = pathlib.Path.cwd().resolve()
path = (root / args.get('path', '.')).resolve()
if not path.is_relative_to(root):
    raise ValueError('path leaves workspace')
operation = sys.argv[1]
parts = path.relative_to(root).parts
if any(secret(p) for p in parts):
    raise ValueError('sensitive path')
if operation in ('write_file', 'edit_file') and any(p in ('.git', '.agents') for p in parts):
    raise ValueError('protected metadata')

if operation == 'read_file':
    if path.stat().st_size > 16 * 1024 * 1024:
        raise ValueError('file exceeds read limit')
    raw = bounded_read(path)
    text = raw.decode('utf-8', errors='replace')
    offset = args.get('offset', 0)
    limit = min(args.get('limit', 16384), 65536)
    result = {'text': text[offset:offset + limit], 'offset': offset, 'characters': len(text), 'sha256': hashlib.sha256(raw).hexdigest()}
elif operation in ('write_file', 'edit_file'):
    existed = path.exists()
    raw = bounded_read(path) if existed else b''
    if operation == 'write_file' and existed and 'expected_sha256' not in args:
        raise ValueError('read the existing file and supply expected_sha256 before replacing it')
    if 'expected_sha256' in args and hashlib.sha256(raw).hexdigest() != args['expected_sha256']:
        raise ValueError('file changed since it was read')
    text = args['content'] if operation == 'write_file' else raw.decode('utf-8')
    if operation == 'edit_file':
        if not args['old'] or text.count(args['old']) != 1:
            raise ValueError('old text must match exactly once')
        text = text.replace(args['old'], args['new'], 1)
    path.parent.mkdir(parents=True, exist_ok=True)
    temporary = None
    try:
        with tempfile.NamedTemporaryFile(mode='w', encoding='utf-8', dir=path.parent, prefix='.layeragent-write-', delete=False) as output:
            temporary = output.name
            output.write(text)
            output.flush()
            os.fsync(output.fileno())
        if existed:
            os.chmod(temporary, stat.S_IMODE(path.stat().st_mode) & 0o777)
        if path.exists() != existed or (existed and hashlib.sha256(bounded_read(path)).digest() != hashlib.sha256(raw).digest()):
            raise ValueError('file changed during editing')
        os.replace(temporary, path)
        temporary = None
    finally:
        if temporary is not None:
            os.unlink(temporary)
    result = {'written': str(path.relative_to(root)), 'sha256': hashlib.sha256(text.encode()).hexdigest()}
else:
    results = []
    for directory, folders, files in os.walk(path, followlinks=False):
        folders[:] = [f for f in folders if not secret(f) and f not in ('.git', '.agents', 'node_modules', 'target') and not (pathlib.Path(directory) / f).is_symlink()]
        for name in files:
            file = pathlib.Path(directory) / name
            if file.is_symlink() or secret(name):
                continue
            if operation == 'list_files':
                results.append(str(file.relative_to(root)))
            elif file.stat().st_size <= 1024 * 1024:
                try:
                    for number, line in enumerate(file.read_text().splitlines(), 1):
                        if args['query'] in line:
                            results.append({'path': str(file.relative_to(root)), 'line': number, 'text': line[:1024]})
                            if len(results) >= 200:
                                break
                except (UnicodeError, OSError):
                    pass
            if len(results) >= 200:
                break
        if len(results) >= 200:
            break
    result = {'results': results, 'limit': 200}
print(json.dumps(result, ensure_ascii=False))
