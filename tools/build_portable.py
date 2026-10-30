"""Offline allowlist assembler. Never installs packages or runs downloaded code."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import shutil
import stat
import tarfile
import zipfile

ROOT_FILES = ('README.md', 'START-HERE.txt', 'USER-GUIDE.html', 'index.html', 'styles.css', 'model.js', 'app.js', 'live.js', 'live-output.js', 'setup.js', 'start-workbench.cmd')


def sha256(path):
    digest = hashlib.sha256()
    with Path(path).open('rb') as source:
        for chunk in iter(lambda: source.read(1024 * 1024), b''):
            digest.update(chunk)
    return digest.hexdigest()


def verify_archive(path, expected):
    if not isinstance(expected, str) or len(expected) != 64 or sha256(path) != expected:
        raise ValueError('Dependency SHA256 mismatch')


def member_path(root, name):
    if not isinstance(name, str) or '\\' in name or ':' in name or name.startswith('/'):
        raise ValueError('Unsafe archive member')
    parts = PurePosixPath(name).parts
    if not parts or '..' in parts:
        raise ValueError('Unsafe archive member')
    target = (root / Path(*parts)).resolve()
    if not target.is_relative_to(root):
        raise ValueError('Archive member outside output')
    return target


def extract_archive(archive, output, selected=None):
    output = Path(output).resolve()
    output.mkdir(parents=True, exist_ok=False)
    total = 0
    if zipfile.is_zipfile(archive):
        with zipfile.ZipFile(archive) as source:
            entries = source.infolist()
            if len(entries) > 50000:
                raise ValueError('Too many archive members')
            for entry in entries:
                target = member_path(output, entry.filename)
                mode = entry.external_attr >> 16
                if stat.S_ISLNK(mode):
                    raise ValueError('Archive links are not permitted')
                total += entry.file_size
                if total > 2 * 1024 ** 3:
                    raise ValueError('Archive exceeds extraction limit')
                if selected is not None and entry.filename not in selected:
                    continue
                if entry.is_dir():
                    target.mkdir(parents=True, exist_ok=True)
                else:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with source.open(entry) as incoming, target.open('xb') as outgoing:
                        shutil.copyfileobj(incoming, outgoing)
    else:
        with tarfile.open(archive, 'r:*') as source:
            for index, entry in enumerate(source):
                target = member_path(output, entry.name)
                total += entry.size
                if index > 50000 or total > 2 * 1024 ** 3 or not (entry.isfile() or entry.isdir()):
                    raise ValueError('Unsafe or oversized archive')
                if selected is not None and entry.name not in selected:
                    continue
                if entry.isdir():
                    target.mkdir(parents=True, exist_ok=True)
                else:
                    target.parent.mkdir(parents=True, exist_ok=True)
                    with source.extractfile(entry) as incoming, target.open('xb') as outgoing:
                        shutil.copyfileobj(incoming, outgoing)


def copy_sources(source, output):
    source, output = Path(source).resolve(), Path(output).resolve()
    for name in ROOT_FILES + ('demo/index.html', 'demo/demo.css', 'demo/demo.js'):
        path = source / name
        if path.is_file() and not path.is_symlink():
            target = output / name
            target.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(path, target)
    target = output / 'p1'
    target.mkdir(exist_ok=True)
    for path in (source / 'p1').iterdir():
        if path.is_file() and not path.is_symlink() and path.suffix in ('.py', '.cjs'):
            shutil.copyfile(path, target / path.name)


def build(source, dependencies, output):
    source, dependencies, output = Path(source).resolve(), Path(dependencies).resolve(), Path(output).resolve()
    lock = json.loads((dependencies / 'dependencies.json').read_text(encoding='utf-8'))
    if lock.get('format') not in (1, 2) or lock.get('platform') != 'win32-x64':
        raise ValueError('Unsupported dependency lock')
    if lock['format'] == 2 and lock.get('browser') != {'mode': 'first-use', 'version': '145.0.7632.6', 'revision': '1208'}:
        raise ValueError('Unsupported first-use browser')
    terms_raw = None
    if 'distributionTerms' in lock:
        terms = lock['distributionTerms']
        if (not isinstance(terms, dict) or set(terms) != {'path', 'sha256'}
                or terms['path'] != 'DISTRIBUTION-TERMS.txt'
                or not isinstance(terms['sha256'], str) or not re.fullmatch('[a-f0-9]{64}', terms['sha256'])):
            raise ValueError('Invalid distribution terms metadata')
        document = source / 'DISTRIBUTION-TERMS.txt'
        if document.is_symlink() or document.is_junction() or not document.is_file() or not 0 < document.stat().st_size <= 65536:
            raise ValueError('Missing or oversized distribution terms')
        terms_raw = document.read_bytes()
        if hashlib.sha256(terms_raw).hexdigest() != terms['sha256']:
            raise ValueError('Distribution terms SHA256 mismatch')
        terms_raw.decode('utf-8')
    output.mkdir(parents=True, exist_ok=False)
    copy_sources(source, output)
    if terms_raw is not None:
        (output / 'DISTRIBUTION-TERMS.txt').write_bytes(terms_raw)
    for entry in lock['archives']:
        archive = member_path(dependencies, entry['archive'])
        verify_archive(archive, entry['sha256'])
        if not entry.get('license') or not entry.get('source', '').startswith('https://'):
            raise ValueError('Missing dependency provenance/license')
        staging = output / ('stage-' + entry['name'])
        selected = None
        if 'include' in entry:
            prefix = entry.get('subdir', '')
            selected = {(PurePosixPath(prefix) / relative).as_posix() for relative in entry['include']}
        extract_archive(archive, staging, selected)
        origin = member_path(staging, entry['subdir']) if entry.get('subdir') else staging
        destination = member_path(output, entry['destination'])
        destination.parent.mkdir(parents=True, exist_ok=True)
        if 'include' in entry:
            destination.mkdir(exist_ok=False)
            for relative in entry['include']:
                selected = member_path(origin, relative)
                if not selected.is_file():
                    raise ValueError('Selected dependency file is missing')
                target = member_path(destination, relative)
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(selected, target)
        else:
            shutil.move(str(origin), str(destination))
        if staging.exists():
            shutil.rmtree(staging)
    for entry in lock.get('documents', []):
        document = member_path(dependencies, entry['file'])
        verify_archive(document, entry['sha256'])
        target = member_path(output, entry['destination'])
        target.parent.mkdir(parents=True, exist_ok=True)
        with document.open('rb') as incoming, target.open('xb') as outgoing:
            shutil.copyfileobj(incoming, outgoing)
    (output / 'DEPENDENCIES.json').write_text(json.dumps(lock, ensure_ascii=False, indent=2), encoding='utf-8')
    inventory = [{'path': p.relative_to(output).as_posix(), 'sha256': sha256(p)} for p in sorted(output.rglob('*')) if p.is_file()]
    keys = lock['paths']
    paths = {name: {'path': relative, 'sha256': sha256(member_path(output, relative))} for name, relative in keys.items()}
    manifest = {'format': lock['format'], 'platform': 'win32-x64', 'files': paths, 'inventory': inventory}
    if lock['format'] == 2:
        manifest['browser'] = lock['browser']
    if terms_raw is not None:
        manifest['distributionTerms'] = lock['distributionTerms']
    (output / 'portable.json').write_text(json.dumps(manifest, indent=2), encoding='utf-8')
    return output


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--source', required=True, type=Path)
    parser.add_argument('--dependencies', required=True, type=Path)
    parser.add_argument('--output', required=True, type=Path)
    args = parser.parse_args()
    build(args.source, args.dependencies, args.output)
