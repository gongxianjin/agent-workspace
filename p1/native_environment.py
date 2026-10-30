"""Explicit pinned native assets. Inspection never executes candidates or falls back to PATH."""
from dataclasses import dataclass
import hashlib
import json
import os
from pathlib import Path
import re
import stat

from environment import RuntimePaths
from platform_info import native_executable, private_chain, target


@dataclass(frozen=True)
class NativeEnvironment:
    paths: RuntimePaths
    claude: Path
    reason_sha256: str


def trusted_file(path):
    if not isinstance(path, (str, Path)):
        raise ValueError('Invalid native asset path')
    path = Path(path)
    if not path.is_absolute() or '..' in path.parts:
        raise ValueError('Absolute native asset path required')
    resolved = private_chain(path)
    if not resolved.is_file():
        raise ValueError('Native asset file required')
    return resolved


def checked_entry(entry):
    if (not isinstance(entry, dict) or set(entry) != {'path', 'sha256'}
            or not isinstance(entry['path'], str) or len(entry['path']) > 4096
            or not isinstance(entry['sha256'], str) or not re.fullmatch('[a-f0-9]{64}', entry['sha256'])):
        raise ValueError('Invalid pinned native asset')
    path = trusted_file(entry['path'])
    digest = hashlib.sha256()
    fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
    with os.fdopen(fd, 'rb') as stream:
        info = os.fstat(stream.fileno())
        if (not stat.S_ISREG(info.st_mode) or info.st_uid not in (0, os.getuid())
                or info.st_mode & 0o022):
            raise ValueError('Untrusted native asset')
        for chunk in iter(lambda: stream.read(1024 * 1024), b''):
            digest.update(chunk)
    if digest.hexdigest() != entry['sha256']:
        raise ValueError('Native asset integrity check failed')
    return path


def resolve_native_environment(root):
    try:
        platform = target()
        if platform not in ('linux-x64', 'linux-arm64'):
            raise ValueError('Native Linux assets required')
        root = Path(root).absolute()
        manifest = root / 'native-runtime.json'
        if private_chain(root) != root or manifest.is_symlink():
            raise ValueError('Linked native manifest root')
        manifest = trusted_file(manifest)
        if not 0 < manifest.stat().st_size <= 8 * 1024 * 1024:
            raise ValueError('Missing or oversized native manifest')
        data = json.loads(manifest.read_text(encoding='utf-8'))
        if (not isinstance(data, dict) or set(data) != {'format', 'platform', 'files', 'assetRoots', 'inventory'}
                or type(data['format']) is not int or data['format'] != 1 or data['platform'] != platform
                or not isinstance(data['files'], dict)
                or set(data['files']) != set(RuntimePaths.__dataclass_fields__) | {'claude'}
                or not isinstance(data['assetRoots'], list) or not 1 <= len(data['assetRoots']) <= 16
                or not isinstance(data['inventory'], list) or not 1 <= len(data['inventory']) <= 20000):
            raise ValueError('Invalid native runtime manifest')
        roots = []
        for name in data['assetRoots']:
            if not isinstance(name, str) or not Path(name).is_absolute() or '..' in Path(name).parts:
                raise ValueError('Invalid native asset root')
            path = Path(name)
            if private_chain(path) != path or not path.is_dir():
                raise ValueError('Linked or missing native asset root')
            if any(path.is_relative_to(other) or other.is_relative_to(path) for other in roots):
                raise ValueError('Overlapping native asset roots')
            roots.append(path)
        inventory = set()
        for entry in data['inventory']:
            path = checked_entry(entry)
            if path != Path(entry['path']) or path in inventory or not any(path.is_relative_to(p) for p in roots):
                raise ValueError('Linked, duplicated or outside native inventory')
            inventory.add(path)
        actual = set()
        for asset_root in roots:
            for parent, directories, files in os.walk(asset_root, followlinks=False):
                for name in (*directories, *files):
                    path = Path(parent) / name
                    if path.is_symlink() or private_chain(path) != path:
                        raise ValueError('Linked native asset inventory')
                    if name in files:
                        if not path.is_file():
                            raise ValueError('Non-file native asset inventory')
                        actual.add(path)
                        if len(actual) > 20000:
                            raise ValueError('Oversized native asset inventory')
        if actual != inventory:
            raise ValueError('Incomplete or changed native asset inventory')
        paths = {name: checked_entry(entry) for name, entry in data['files'].items()}
        if paths['playwright'].name != 'package.json' or paths['playwright'] not in inventory:
            raise ValueError('Playwright must be inside the pinned native inventory')
        for name, path in paths.items():
            if name != 'playwright' and native_executable(path, platform) != path:
                raise ValueError('Trusted native executable required')
        return NativeEnvironment(RuntimePaths(**{key: paths[key] for key in RuntimePaths.__dataclass_fields__}),
            paths['claude'], data['files']['reason']['sha256'])
    except (OSError, KeyError, TypeError, UnicodeError, RecursionError, RuntimeError) as error:
        raise ValueError('Invalid or unavailable native runtime assets') from error


def discover_native_claude(environ=None):
    env = os.environ if environ is None else environ
    candidates = []
    if env.get('HOME') and Path(env['HOME']).is_absolute():
        candidates.append(Path(env['HOME']) / '.local/bin/claude')
    for directory in env.get('PATH', '').split(os.pathsep):
        if directory and Path(directory).is_absolute():
            candidates.append(Path(directory) / 'claude')
    platform = target()
    return next((found for path in candidates if (found := native_executable(path, platform))), None)
