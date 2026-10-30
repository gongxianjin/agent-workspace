"""Headless Linux console host; execution stays blocked until native verification."""
import argparse
import json
import os
from pathlib import Path
import re
import signal
import stat
import sys
import uuid
import threading
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from linux_instance import InstanceChannel, request
from root_lock import OwnedRoot
from server import Application


def private_access(root):
    root = Path(root).absolute()
    if any(part.is_symlink() for part in (root, *root.parents)):
        raise ValueError('Linked runtime directory')
    flags = os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW | os.O_CLOEXEC
    root_fd = os.open(root, flags)
    base_fd = child_fd = None
    try:
        metadata = os.fstat(root_fd)
        if metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
            raise ValueError('Runtime root is not private')
        try:
            os.mkdir('private-access', mode=0o700, dir_fd=root_fd)
        except FileExistsError:
            pass
        base_fd = os.open('private-access', flags, dir_fd=root_fd)
        metadata = os.fstat(base_fd)
        if metadata.st_uid != os.getuid() or metadata.st_mode & 0o077:
            raise ValueError('Access directory is not private')
        for entry in os.listdir(base_fd):
            if not re.fullmatch('[a-f0-9]{32}', entry):
                continue
            metadata = os.stat(entry, dir_fd=base_fd, follow_symlinks=False)
            if not stat.S_ISDIR(metadata.st_mode) or metadata.st_uid != os.getuid() or time.time() - metadata.st_mtime < 60:
                continue
            expired_fd = os.open(entry, flags, dir_fd=base_fd)
            try:
                if os.listdir(expired_fd) == ['launch.txt']:
                    metadata = os.stat('launch.txt', dir_fd=expired_fd, follow_symlinks=False)
                    if stat.S_ISREG(metadata.st_mode) and metadata.st_uid == os.getuid():
                        os.unlink('launch.txt', dir_fd=expired_fd)
                if not os.listdir(expired_fd):
                    os.rmdir(entry, dir_fd=base_fd)
            finally:
                os.close(expired_fd)
        name = uuid.uuid4().hex
        os.mkdir(name, mode=0o700, dir_fd=base_fd)
        child_fd = os.open(name, flags, dir_fd=base_fd)
        result = request(root, 'access')
        fd = os.open('launch.txt', os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW | os.O_CLOEXEC,
            0o600, dir_fd=child_fd)
        with os.fdopen(fd, 'w', encoding='utf-8') as stream:
            stream.write(result['url'] + '\n')
            stream.flush()
            os.fsync(stream.fileno())
        return {'ok': True, 'accessFile': str(root / 'private-access' / name / 'launch.txt'), 'expiresIn': 60}
    finally:
        for fd in (child_fd, base_fd, root_fd):
            if fd is not None:
                os.close(fd)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('serve', 'probe', 'access', 'shutdown'))
    parser.add_argument('--root', type=Path, required=True)
    parser.add_argument('--port', type=int, default=8787)
    parser.add_argument('--preview-port', type=int, default=8788)
    args = parser.parse_args()
    if sys.platform != 'linux':
        parser.error('Linux entry requires native Linux')
    if any(not 0 <= port <= 65535 for port in (args.port, args.preview_port)):
        parser.error('Ports must be between 0 and 65535')
    if args.port and args.port == args.preview_port:
        parser.error('Control and preview need separate ports')
    os.umask(0o077)
    if args.operation != 'serve':
        try:
            result = private_access(args.root) if args.operation == 'access' else request(args.root, args.operation)
            print(json.dumps(result, ensure_ascii=True), flush=True)
            return 0
        except (OSError, ValueError):
            print('{"ok":false,"error":"No responsive owned instance or operation denied"}', file=sys.stderr)
            return 1
    app = channel = thread = None
    stage = 'initialize'
    try:
        try:
            app = Application(args.root, args.port, preview_port=args.preview_port)
        except OwnedRoot:
            stage = 'reopen'
            deadline = time.monotonic() + 5
            while True:
                try:
                    result = request(args.root, 'probe')
                    print(json.dumps({**result, 'reused': True}), flush=True)
                    return 0
                except (OSError, ValueError):
                    if time.monotonic() >= deadline:
                        raise TimeoutError('Owned instance is unavailable; ownership not removed')
                    time.sleep(.05)

        def handle(operation):
            if operation == 'probe':
                with app.action_lock:
                    if not app.accepting:
                        raise ValueError('Workbench is shutting down')
                    return {'ok': True, 'pid': os.getpid(), 'origin': app.origin,
                        'previewOrigin': 'http://127.0.0.1:' + str(app.preview_port),
                        'executionMode': app.execution_mode(), 'workers': app.worker_enabled}
            if operation == 'access':
                with app.action_lock:
                    if not app.accepting:
                        raise ValueError('Workbench is shutting down')
                    return {'ok': True, 'url': app.origin + '/#bootstrap=' + app.issue_bootstrap()}
            app.request_shutdown()
            return {'ok': True}

        def stop(signum, frame):
            try:
                app.request_shutdown()
            except ValueError:
                print('{"ok":false,"error":"shutdown_denied_active_or_unknown_lease"}', file=sys.stderr, flush=True)

        signal.signal(signal.SIGTERM, stop)
        signal.signal(signal.SIGINT, stop)
        thread = threading.Thread(target=app.serve, daemon=True)
        thread.start()
        stage = 'channel'
        channel = InstanceChannel(args.root, handle)
        print(json.dumps(handle('probe')), flush=True)
        stage = 'running'
        while not app.shutdown_requested.wait(.2):
            if not thread.is_alive() or not channel.thread.is_alive():
                raise RuntimeError('A required console service stopped unexpectedly')
        return 0
    except (OSError, ValueError, RuntimeError) as error:
        category = ('timeout' if isinstance(error, TimeoutError) else
            'os_error' if isinstance(error, OSError) else
            'invalid_input' if isinstance(error, ValueError) else 'internal_error')
        number = getattr(error, 'errno', None) if isinstance(error, OSError) else None
        diagnostic = {'ok': False, 'code': 'startup_failed', 'stage': stage, 'category': category,
            'errno': number if type(number) is int and -(2 ** 31) <= number < 2 ** 31 else None}
        print(json.dumps(diagnostic), file=sys.stderr, flush=True)
        return 1
    finally:
        if channel:
            channel.close()
        if app:
            app.close()
        if thread:
            thread.join(5)
            if thread.is_alive():
                raise RuntimeError('Control host did not stop')


if __name__ == '__main__':
    sys.exit(main())
