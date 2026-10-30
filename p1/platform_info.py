"""Native platform and executable inspection. No candidate code is executed."""
from collections import deque
import os
from pathlib import Path
import platform
import stat
import struct


def target(system=None, machine=None):
    system = platform.system() if system is None else system
    machine = (platform.machine() if machine is None else machine).lower()
    if system == 'Windows' and machine in ('amd64', 'x86_64'):
        return 'win32-x64'
    if system == 'Linux':
        if machine in ('amd64', 'x86_64'):
            return 'linux-x64'
        if machine in ('arm64', 'aarch64'):
            return 'linux-arm64'
    raise ValueError('Unsupported native platform')


def executable_header(header, target_name):
    if target_name == 'win32-x64':
        return header[:2] == b'MZ'
    machine = {'linux-x64': 62, 'linux-arm64': 183}.get(target_name)
    if machine is None or len(header) < 64 or header[:7] != b'\x7fELF\x02\x01\x01':
        return False
    kind, architecture, version = struct.unpack_from('<HHI', header, 16)
    return (kind in (2, 3) and architecture == machine and version == 1
        and struct.unpack_from('<H', header, 52)[0] == 64)


def private_chain(path):
    pending = deque(path.parts[1:])
    current = Path(path.anchor)
    links = 0
    while pending:
        part = pending.popleft()
        if part == '..':
            current = current.parent
            continue
        candidate = current / part
        metadata = candidate.lstat()
        if metadata.st_uid not in (0, os.getuid()):
            raise ValueError('Untrusted executable path owner')
        if stat.S_ISLNK(metadata.st_mode):
            links += 1
            if links > 40:
                raise ValueError('Executable link cycle')
            destination = Path(os.readlink(candidate))
            if destination.is_absolute():
                current = Path(destination.anchor)
                parts = destination.parts[1:]
            else:
                parts = destination.parts
            pending.extendleft(reversed(parts))
            continue
        # A root-owned sticky ancestor such as /tmp protects the owned child entry.
        sticky_root = (stat.S_ISDIR(metadata.st_mode) and metadata.st_uid == 0
            and metadata.st_mode & stat.S_ISVTX)
        if metadata.st_mode & 0o022 and not sticky_root:
            raise ValueError('Writable executable path')
        if pending and not stat.S_ISDIR(metadata.st_mode):
            raise ValueError('Invalid executable parent')
        current = candidate
    return current


def native_executable(path, target_name):
    try:
        path = Path(path)
        if not path.is_absolute():
            return None
        if target_name == 'win32-x64':
            if path.suffix.lower() != '.exe' or not path.is_file():
                return None
            with path.open('rb') as stream:
                return path.resolve() if executable_header(stream.read(64), target_name) else None
        if target_name not in ('linux-x64', 'linux-arm64') or os.name == 'nt':
            return None
        path = private_chain(path)
        fd = os.open(path, os.O_RDONLY | os.O_NOFOLLOW | os.O_CLOEXEC | os.O_NONBLOCK)
        try:
            metadata = os.fstat(fd)
            if (not stat.S_ISREG(metadata.st_mode) or not metadata.st_mode & 0o111
                    or metadata.st_mode & 0o022 or not os.access(path, os.X_OK)):
                return None
            return path if executable_header(os.read(fd, 64), target_name) else None
        finally:
            os.close(fd)
    except (OSError, ValueError, RuntimeError):
        return None
