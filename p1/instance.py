"""Current-user, local-only Windows pipe for reopening an owned workbench."""
import ctypes
from ctypes import wintypes as w
import hashlib
import os
from pathlib import Path
import threading
import time

kernel = ctypes.WinDLL('kernel32', use_last_error=True)
advapi = ctypes.WinDLL('advapi32', use_last_error=True)
INVALID = ctypes.c_void_p(-1).value


class SecurityAttributes(ctypes.Structure):
    _fields_ = [('length', w.DWORD), ('descriptor', w.LPVOID), ('inherit', w.BOOL)]


for name, args, result in (
    ('OpenProcess', [w.DWORD, w.BOOL, w.DWORD], w.HANDLE),
    ('CreateNamedPipeW', [w.LPCWSTR, w.DWORD, w.DWORD, w.DWORD, w.DWORD, w.DWORD, w.DWORD, ctypes.POINTER(SecurityAttributes)], w.HANDLE),
    ('ConnectNamedPipe', [w.HANDLE, w.LPVOID], w.BOOL),
    ('CreateFileW', [w.LPCWSTR, w.DWORD, w.DWORD, w.LPVOID, w.DWORD, w.DWORD, w.HANDLE], w.HANDLE),
    ('GetNamedPipeServerProcessId', [w.HANDLE, ctypes.POINTER(w.ULONG)], w.BOOL),
    ('GetNamedPipeClientProcessId', [w.HANDLE, ctypes.POINTER(w.ULONG)], w.BOOL),
    ('ReadFile', [w.HANDLE, w.LPVOID, w.DWORD, ctypes.POINTER(w.DWORD), w.LPVOID], w.BOOL),
    ('WriteFile', [w.HANDLE, w.LPCVOID, w.DWORD, ctypes.POINTER(w.DWORD), w.LPVOID], w.BOOL),
    ('PeekNamedPipe', [w.HANDLE, w.LPVOID, w.DWORD, ctypes.POINTER(w.DWORD), ctypes.POINTER(w.DWORD), ctypes.POINTER(w.DWORD)], w.BOOL),
    ('DisconnectNamedPipe', [w.HANDLE], w.BOOL), ('CloseHandle', [w.HANDLE], w.BOOL),
    ('LocalFree', [w.HANDLE], w.HANDLE),
):
    function = getattr(kernel, name)
    function.argtypes, function.restype = args, result
advapi.OpenProcessToken.argtypes = [w.HANDLE, w.DWORD, ctypes.POINTER(w.HANDLE)]
advapi.OpenProcessToken.restype = w.BOOL
advapi.GetTokenInformation.argtypes = [w.HANDLE, ctypes.c_int, w.LPVOID, w.DWORD, ctypes.POINTER(w.DWORD)]
advapi.GetTokenInformation.restype = w.BOOL
advapi.ConvertSidToStringSidW.argtypes = [w.LPVOID, ctypes.POINTER(w.LPWSTR)]
advapi.ConvertSidToStringSidW.restype = w.BOOL
advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW.argtypes = [w.LPCWSTR, w.DWORD, ctypes.POINTER(w.LPVOID), ctypes.POINTER(w.ULONG)]
advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW.restype = w.BOOL


def checked(value):
    if not value or value == INVALID:
        raise ctypes.WinError(ctypes.get_last_error())
    return value


def process_sid(pid):
    process = checked(kernel.OpenProcess(0x1000, False, pid))
    token = w.HANDLE()
    try:
        checked(advapi.OpenProcessToken(process, 8, ctypes.byref(token)))
        size = w.DWORD()
        advapi.GetTokenInformation(token, 1, None, 0, ctypes.byref(size))
        data = ctypes.create_string_buffer(size.value)
        checked(advapi.GetTokenInformation(token, 1, data, size, ctypes.byref(size)))
        sid = ctypes.cast(data, ctypes.POINTER(w.LPVOID))[0]
        string = w.LPWSTR()
        checked(advapi.ConvertSidToStringSidW(sid, ctypes.byref(string)))
        try:
            return string.value
        finally:
            kernel.LocalFree(ctypes.cast(string, w.HANDLE))
    finally:
        if token:
            kernel.CloseHandle(token)
        kernel.CloseHandle(process)


def pipe_name(root, sid):
    scope = sid + '|' + os.path.normcase(str(Path(root).resolve()))
    return '\\\\.\\pipe\\relay-workbench-' + hashlib.sha256(scope.encode('utf-8')).hexdigest()[:40]


def read_message(handle, timeout, stopping=None):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline and not (stopping and stopping.is_set()):
        available = w.DWORD()
        checked(kernel.PeekNamedPipe(handle, None, 0, None, ctypes.byref(available), None))
        if available.value:
            if available.value > 128:
                raise ValueError('Invalid local control message')
            buffer = ctypes.create_string_buffer(128)
            count = w.DWORD()
            checked(kernel.ReadFile(handle, buffer, 128, ctypes.byref(count), None))
            return buffer.raw[:count.value]
        time.sleep(.01)
    raise TimeoutError('Local instance did not respond')


def write_message(handle, value):
    count = w.DWORD()
    checked(kernel.WriteFile(handle, value, len(value), ctypes.byref(count), None))
    if count.value != len(value):
        raise OSError('Incomplete local control message')


class InstanceChannel:
    def __init__(self, root, open_ui):
        self.sid = process_sid(os.getpid())
        self.name = pipe_name(root, self.sid)
        self.open_ui = open_ui
        self.stopping = threading.Event()
        descriptor = w.LPVOID()
        checked(advapi.ConvertStringSecurityDescriptorToSecurityDescriptorW(
            'D:P(A;;GA;;;' + self.sid + ')', 1, ctypes.byref(descriptor), None))
        try:
            attributes = SecurityAttributes(ctypes.sizeof(SecurityAttributes), descriptor, False)
            self.handle = checked(kernel.CreateNamedPipeW(self.name, 3 | 0x80000, 4 | 2 | 8,
                1, 256, 256, 0, ctypes.byref(attributes)))
        finally:
            kernel.LocalFree(descriptor)
        self.thread = threading.Thread(target=self._serve, daemon=True)
        self.thread.start()

    def _serve(self):
        while not self.stopping.is_set():
            connected = kernel.ConnectNamedPipe(self.handle, None)
            if not connected and ctypes.get_last_error() != 535:
                break
            try:
                if self.stopping.is_set():
                    break
                pid = w.ULONG()
                checked(kernel.GetNamedPipeClientProcessId(self.handle, ctypes.byref(pid)))
                if process_sid(pid.value) != self.sid:
                    raise ValueError('Wrong control owner')
                message = read_message(self.handle, 2, self.stopping)
                if message not in (b'probe', b'open-ui'):
                    write_message(self.handle, b'denied')
                    continue
                if message == b'open-ui':
                    self.open_ui()
                write_message(self.handle, b'ok')
                # Keep response available until the client has read and closed it.
                deadline = time.monotonic() + 1
                while not self.stopping.is_set() and time.monotonic() < deadline:
                    available = w.DWORD()
                    if not kernel.PeekNamedPipe(self.handle, None, 0, None, ctypes.byref(available), None):
                        break
                    time.sleep(.005)
            except (OSError, ValueError, TimeoutError):
                pass
            finally:
                kernel.DisconnectNamedPipe(self.handle)

    def close(self):
        if self.stopping.is_set():
            return
        self.stopping.set()
        wake = kernel.CreateFileW(self.name, 0xC0000000, 0, None, 3, 0, None)
        if wake != INVALID:
            kernel.CloseHandle(wake)
        self.thread.join(3)
        if self.thread.is_alive():
            raise RuntimeError('Local instance channel did not stop')
        kernel.CloseHandle(self.handle)


def request(root, operation, timeout=2):
    if operation not in ('probe', 'open-ui'):
        raise ValueError('Unsupported local control operation')
    sid = process_sid(os.getpid())
    name = pipe_name(root, sid)
    deadline = time.monotonic() + timeout
    while True:
        handle = kernel.CreateFileW(name, 0xC0000000, 0, None, 3, 0, None)
        if handle != INVALID:
            break
        if time.monotonic() >= deadline:
            raise TimeoutError('No matching owned workbench instance')
        time.sleep(.01)
    try:
        pid = w.ULONG()
        checked(kernel.GetNamedPipeServerProcessId(handle, ctypes.byref(pid)))
        if process_sid(pid.value) != sid:
            raise ValueError('Wrong control owner')
        write_message(handle, operation.encode('ascii'))
        if read_message(handle, max(.05, deadline - time.monotonic())) != b'ok':
            raise ValueError('Local control operation denied')
        return {'ok': True}
    finally:
        kernel.CloseHandle(handle)
