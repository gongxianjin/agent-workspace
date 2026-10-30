"""Bounded Linux same-UID control channel; no socket files or capability logs."""
import errno
import hashlib
import json
import os
from pathlib import Path
import socket
import struct
import threading
import time

OPERATIONS = frozenset(('probe', 'access', 'shutdown'))


def address(root):
    scope = str(os.getuid()) + '|' + str(Path(root).resolve())
    return b'\0relay-workbench-' + hashlib.sha256(scope.encode()).hexdigest().encode()


def peer(connection):
    return struct.unpack('3i', connection.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))


def read_message(connection, limit):
    data = bytearray()
    while not data.endswith(b'\n'):
        block = connection.recv(min(1024, limit + 1 - len(data)))
        if not block:
            raise ValueError('Incomplete local control message')
        data.extend(block)
        if len(data) > limit:
            raise ValueError('Local control message exceeds limit')
    if b'\n' in data[:-1]:
        raise ValueError('Invalid local control message')
    return bytes(data[:-1])


class InstanceChannel:
    def __init__(self, root, handler):
        self.handler = handler
        self.stopping = threading.Event()
        self.socket = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            self.socket.bind(address(root))
            self.socket.listen(8)
            self.socket.settimeout(.2)
        except BaseException:
            self.socket.close()
            raise
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        while not self.stopping.is_set():
            try:
                connection, _ = self.socket.accept()
            except socket.timeout:
                continue
            except OSError as error:
                if self.stopping.is_set():
                    break
                if error.errno in (errno.EMFILE, errno.ENFILE, errno.ECONNABORTED, errno.EINTR, errno.ENOBUFS, errno.ENOMEM):
                    self.stopping.wait(.05)
                    continue
                break
            with connection:
                try:
                    connection.settimeout(2)
                    _, uid, _ = peer(connection)
                    if uid != os.getuid():
                        continue
                    operation = read_message(connection, 32).decode('ascii')
                    if operation not in OPERATIONS:
                        raise ValueError('Unsupported local control operation')
                    result = self.handler(operation)
                    raw = json.dumps(result, ensure_ascii=True).encode() + b'\n'
                    if len(raw) > 8192:
                        raise ValueError('Invalid local control response')
                    connection.sendall(raw)
                except Exception:
                    # Deny just this operation; never expose handler details or retry it.
                    try:
                        connection.sendall(b'{"ok":false,"error":"Local control operation denied"}\n')
                    except OSError:
                        pass

    def close(self):
        self.stopping.set()
        self.socket.close()
        self.thread.join(3)
        if self.thread.is_alive():
            raise RuntimeError('Local instance channel did not stop')


def request(root, operation, timeout=2):
    if operation not in OPERATIONS:
        raise ValueError('Unsupported local control operation')
    deadline = time.monotonic() + timeout
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as connection:
        connection.settimeout(timeout)
        connection.connect(address(root))
        _, uid, _ = peer(connection)
        if uid != os.getuid():
            raise ValueError('Wrong local instance owner')
        connection.sendall(operation.encode('ascii') + b'\n')
        connection.settimeout(max(.01, deadline - time.monotonic()))
        result = json.loads(read_message(connection, 8192))
        if not isinstance(result, dict) or result.get('ok') is not True:
            raise ValueError('Local control operation denied')
        return result
