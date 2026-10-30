"""Exclusive kernel ownership acquired before opening or recovering project data."""
import errno
import os
from pathlib import Path
import stat


class OwnedRoot(RuntimeError):
    pass


class RootLock:
    def __init__(self, root):
        root = Path(root).absolute()
        if os.name == 'nt':
            import msvcrt
            root = root.resolve()
            root.mkdir(parents=True, exist_ok=True)
            self.file = (root / 'service.lock').open('a+b')
            self.file.seek(0)
            try:
                msvcrt.locking(self.file.fileno(), msvcrt.LK_NBLCK, 1)
            except OSError as error:
                self.file.close()
                raise OwnedRoot('This runtime root is already owned by another service') from error
        else:
            import fcntl
            if any(part.is_symlink() for part in (root, *root.parents)):
                raise ValueError('Linked runtime directories are not supported')
            root.mkdir(parents=True, mode=0o700, exist_ok=True)
            metadata = root.stat()
            if metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
                raise ValueError('Runtime root must belong to the current user and have private permissions (0700)')
            fd = os.open(root / 'service.lock', os.O_RDWR | os.O_CREAT | os.O_NOFOLLOW | os.O_CLOEXEC, 0o600)
            try:
                metadata = os.fstat(fd)
                if not stat.S_ISREG(metadata.st_mode) or metadata.st_uid != os.getuid() or metadata.st_nlink != 1:
                    raise ValueError('Invalid runtime ownership file')
                os.fchmod(fd, 0o600)
                try:
                    fcntl.flock(fd, fcntl.LOCK_EX | fcntl.LOCK_NB)
                except OSError as error:
                    if error.errno not in (errno.EAGAIN, errno.EACCES):
                        raise
                    raise OwnedRoot('This runtime root is already owned by another service') from error
                self.file = os.fdopen(fd, 'r+b', buffering=0)
                fd = None
            finally:
                if fd is not None:
                    os.close(fd)

    def close(self):
        if not self.file.closed:
            if os.name == 'nt':
                import msvcrt
                self.file.seek(0)
                msvcrt.locking(self.file.fileno(), msvcrt.LK_UNLCK, 1)
            else:
                import fcntl
                fcntl.flock(self.file.fileno(), fcntl.LOCK_UN)
            self.file.close()
