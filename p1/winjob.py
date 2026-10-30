"""Windows process trees: assign while suspended, never hand off on parent exit alone."""
import ctypes
from ctypes import wintypes as w
import os
import queue
import subprocess
import threading
import time

if os.name == 'nt':
    import _winapi
    import msvcrt
    kernel = ctypes.WinDLL('kernel32', use_last_error=True)


class IO_COUNTERS(ctypes.Structure):
    _fields_ = [(name, ctypes.c_ulonglong) for name in ('ReadOperationCount', 'WriteOperationCount', 'OtherOperationCount', 'ReadTransferCount', 'WriteTransferCount', 'OtherTransferCount')]


class BASIC_LIMIT(ctypes.Structure):
    _fields_ = [('PerProcessUserTimeLimit', ctypes.c_longlong), ('PerJobUserTimeLimit', ctypes.c_longlong),
        ('LimitFlags', w.DWORD), ('MinimumWorkingSetSize', ctypes.c_size_t), ('MaximumWorkingSetSize', ctypes.c_size_t),
        ('ActiveProcessLimit', w.DWORD), ('Affinity', ctypes.c_size_t), ('PriorityClass', w.DWORD), ('SchedulingClass', w.DWORD)]


class EXTENDED_LIMIT(ctypes.Structure):
    _fields_ = [('BasicLimitInformation', BASIC_LIMIT), ('IoInfo', IO_COUNTERS),
        ('ProcessMemoryLimit', ctypes.c_size_t), ('JobMemoryLimit', ctypes.c_size_t),
        ('PeakProcessMemoryUsed', ctypes.c_size_t), ('PeakJobMemoryUsed', ctypes.c_size_t)]


class ACCOUNTING(ctypes.Structure):
    _fields_ = [('TotalUserTime', ctypes.c_longlong), ('TotalKernelTime', ctypes.c_longlong),
        ('ThisPeriodTotalUserTime', ctypes.c_longlong), ('ThisPeriodTotalKernelTime', ctypes.c_longlong),
        ('TotalPageFaultCount', w.DWORD), ('TotalProcesses', w.DWORD), ('ActiveProcesses', w.DWORD), ('TotalTerminatedProcesses', w.DWORD)]


class COMPLETION_PORT(ctypes.Structure):
    _fields_ = [('CompletionKey', ctypes.c_void_p), ('CompletionPort', w.HANDLE)]


if os.name == 'nt':
    kernel.CreateIoCompletionPort.argtypes = [w.HANDLE, w.HANDLE, ctypes.c_size_t, w.DWORD]
    kernel.CreateIoCompletionPort.restype = w.HANDLE
    kernel.GetQueuedCompletionStatus.argtypes = [w.HANDLE, ctypes.POINTER(w.DWORD), ctypes.POINTER(ctypes.c_size_t), ctypes.POINTER(ctypes.c_void_p), w.DWORD]
    kernel.GetQueuedCompletionStatus.restype = w.BOOL
    kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
    kernel.OpenProcess.restype = w.HANDLE
    kernel.IsProcessInJob.argtypes = [w.HANDLE, w.HANDLE, ctypes.POINTER(w.BOOL)]
    kernel.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
    kernel.WaitForSingleObject.restype = w.DWORD
    kernel.CreateJobObjectW.argtypes = [ctypes.c_void_p, w.LPCWSTR]
    kernel.CreateJobObjectW.restype = w.HANDLE
    kernel.SetInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD]
    kernel.AssignProcessToJobObject.argtypes = [w.HANDLE, w.HANDLE]
    kernel.QueryInformationJobObject.argtypes = [w.HANDLE, ctypes.c_int, ctypes.c_void_p, w.DWORD, ctypes.c_void_p]
    kernel.TerminateJobObject.argtypes = [w.HANDLE, w.UINT]
    kernel.ResumeThread.argtypes = [w.HANDLE]
    kernel.ResumeThread.restype = w.DWORD
    kernel.CloseHandle.argtypes = [w.HANDLE]


def checked(value):
    if not value:
        raise ctypes.WinError(ctypes.get_last_error())
    return value


class JobProcess:
    def __init__(self, argv, cwd, env=None, input_data=b'', output_limit=8 * 1024 * 1024, stdout_observer=None, wait_observer=None):
        if os.name != 'nt':
            raise RuntimeError('This runner requires Windows Job Objects')
        self.argv, self.cwd = list(argv), str(cwd)
        self.job = checked(kernel.CreateJobObjectW(None, None))
        self.process = None
        self.pid = None
        self.output_limit = output_limit
        self.output_limited = False
        self.buffers = {'stdout': bytearray(), 'stderr': bytearray()}
        self.buffer_lock = threading.Lock()
        self.threads = []
        self.closed = False
        self.cancelled = False
        self.stopped = False
        self.exit_code = None
        self.lifecycle_lock = threading.RLock()
        self.completion_port = None
        self.observed_processes = 0
        self.process_handles = []
        self.tracking_failed = False
        self.started_at = None
        self.wait_started_at = self.timeout_at = None
        self.wait_observer = wait_observer
        self.observer = stdout_observer
        self.observer_queue = queue.Queue(maxsize=64)
        self.observer_stop = threading.Event()
        self.observation_gap = threading.Event()
        self.observation_failed = False
        self.observer_thread = None
        pipes = []
        try:
            self.completion_port = checked(kernel.CreateIoCompletionPort(w.HANDLE(-1), None, 0, 1))
            association = COMPLETION_PORT(1, self.completion_port)
            checked(kernel.SetInformationJobObject(self.job, 7, ctypes.byref(association), ctypes.sizeof(association)))
            limits = EXTENDED_LIMIT()
            limits.BasicLimitInformation.LimitFlags = 0x2000  # KILL_ON_JOB_CLOSE; no breakaway flags.
            checked(kernel.SetInformationJobObject(self.job, 9, ctypes.byref(limits), ctypes.sizeof(limits)))
            in_read, in_write = os.pipe()
            out_read, out_write = os.pipe()
            err_read, err_write = os.pipe()
            pipes = [in_read, in_write, out_read, out_write, err_read, err_write]
            child_fds = (in_read, out_write, err_write)
            handles = [msvcrt.get_osfhandle(fd) for fd in child_fds]
            for handle in handles:
                os.set_handle_inheritable(handle, True)
            startup = subprocess.STARTUPINFO()
            startup.dwFlags = subprocess.STARTF_USESTDHANDLES
            startup.hStdInput, startup.hStdOutput, startup.hStdError = handles
            startup.lpAttributeList = {'handle_list': handles}
            flags = 0x4 | subprocess.CREATE_NO_WINDOW | 0x400  # suspended, no window, Unicode environment.
            process, thread, pid, _ = _winapi.CreateProcess(self.argv[0], subprocess.list2cmdline(self.argv),
                None, None, True, flags, env, self.cwd, startup)
            self.process, self.pid = process, pid
            try:
                checked(kernel.AssignProcessToJobObject(self.job, process))
                self.started_at = time.time() * 1000
                if kernel.ResumeThread(thread) == 0xffffffff:
                    raise ctypes.WinError(ctypes.get_last_error())
            except BaseException:
                _winapi.TerminateProcess(process, 1)
                raise
            finally:
                _winapi.CloseHandle(thread)
            for fd in child_fds:
                os.close(fd)
                pipes.remove(fd)
            self.input = os.fdopen(in_write, 'wb', buffering=0)
            self.stdout = os.fdopen(out_read, 'rb', buffering=0)
            self.stderr = os.fdopen(err_read, 'rb', buffering=0)
            pipes.clear()
            if self.observer:
                self.observer_thread = threading.Thread(target=self._observe, daemon=True)
                self.observer_thread.start()
            for name, stream in [('stdout', self.stdout), ('stderr', self.stderr)]:
                worker = threading.Thread(target=self._read, args=(name, stream), daemon=True)
                self.threads.append(worker)
                worker.start()
            self.writer = threading.Thread(target=self._write, args=(input_data,), daemon=True)
            self.writer.start()
        except BaseException:
            for fd in pipes:
                os.close(fd)
            self.close()
            raise

    def _write(self, data):
        try:
            self.input.write(data)
        except (BrokenPipeError, OSError):
            pass
        finally:
            self.input.close()

    def _observe(self):
        gap_sent = False
        try:
            while True:
                if self.observation_gap.is_set():
                    if not gap_sent:
                        self.observer(None)
                        gap_sent = True
                    while True:
                        try:
                            self.observer_queue.get_nowait()
                        except queue.Empty:
                            break
                    if self.observer_stop.is_set():
                        return
                    self.observer_stop.wait(.02)
                    continue
                try:
                    chunk = self.observer_queue.get(timeout=.02)
                except queue.Empty:
                    if self.observer_stop.is_set():
                        self.observer(b'')
                        return
                    continue
                self.observer(chunk)
        except Exception:
            self.observation_failed = True
            self.observation_gap.set()

    def _flush_observer(self):
        self.observer_stop.set()
        if self.observer_thread:
            self.observer_thread.join(timeout=1)
            if self.observer_thread.is_alive():
                self.observation_gap.set()

    def _read(self, name, stream):
        try:
            while True:
                chunk = stream.read(8192)
                if not chunk:
                    break
                with self.buffer_lock:
                    room = self.output_limit - sum(len(v) for v in self.buffers.values())
                    self.buffers[name].extend(chunk[:max(0, room)])
                    if len(chunk) > room:
                        self.output_limited = True
                if name == 'stdout' and self.observer and not self.observation_gap.is_set():
                    try:
                        self.observer_queue.put_nowait(chunk)
                    except queue.Full:
                        self.observation_gap.set()
                if self.output_limited:
                    self.stop()
                    break
        finally:
            stream.close()

    def _accounting(self):
        info = ACCOUNTING()
        checked(kernel.QueryInformationJobObject(self.job, 1, ctypes.byref(info), ctypes.sizeof(info), None))
        return info

    def _track_processes(self):
        # Associate before assignment; missed NEW_PROCESS messages fail closed against TotalProcesses.
        while True:
            message, key, pid = w.DWORD(), ctypes.c_size_t(), ctypes.c_void_p()
            if not kernel.GetQueuedCompletionStatus(self.completion_port, ctypes.byref(message), ctypes.byref(key), ctypes.byref(pid), 0):
                if ctypes.get_last_error() == 258:
                    break
                raise ctypes.WinError(ctypes.get_last_error())
            if message.value != 6:  # JOB_OBJECT_MSG_NEW_PROCESS
                continue
            self.observed_processes += 1
            if self.observed_processes > 4096:
                self.tracking_failed = True
                continue
            handle = kernel.OpenProcess(0x100000 | 0x1000, False, pid.value)
            if not handle:
                # A deleted process object has finished cleanup. Other failures are not proof.
                if ctypes.get_last_error() != 87:
                    self.tracking_failed = True
                continue
            member = w.BOOL()
            if not kernel.IsProcessInJob(handle, self.job, ctypes.byref(member)) or not member.value:
                self.tracking_failed = True
                kernel.CloseHandle(handle)
                continue
            self.process_handles.append(handle)
        pending = []
        for handle in self.process_handles:
            status = kernel.WaitForSingleObject(handle, 0)
            if status == 0:
                kernel.CloseHandle(handle)
            else:
                pending.append(handle)
                if status != 258:
                    self.tracking_failed = True
        self.process_handles = pending

    def active_processes(self):
        with self.lifecycle_lock:
            if self.closed:
                return 0 if self.stopped else None
            return self._accounting().ActiveProcesses

    def poll(self):
        with self.lifecycle_lock:
            if self.process is None:
                return self.exit_code
            code = _winapi.GetExitCodeProcess(self.process)
            return None if code == 259 else code

    def tree_stopped(self):
        with self.lifecycle_lock:
            if self.closed or self.stopped:
                return self.stopped
            self._track_processes()
            info = self._accounting()
            root_done = self.process is not None and kernel.WaitForSingleObject(self.process, 0) == 0
            self.stopped = bool(root_done and info.ActiveProcesses == 0 and not self.process_handles
                and not self.tracking_failed and self.observed_processes == info.TotalProcesses)
            return self.stopped

    def stop(self):
        self.cancelled = True
        with self.lifecycle_lock:
            if self.closed:
                return self.stopped
            self._track_processes()
            checked(kernel.TerminateJobObject(self.job, 1))
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if self.tree_stopped():
                return True
            time.sleep(.02)
        return False

    def wait(self, timeout):
        started = time.monotonic()
        deadline = started + timeout
        self.wait_started_at = time.time() * 1000
        self.timeout_at = self.wait_started_at + timeout * 1000
        if self.wait_observer:
            try:
                self.wait_observer({'kind': 'process', 'waitStartedAt': self.wait_started_at, 'timeoutAt': self.timeout_at})
            except Exception:
                self.observation_failed = True
        timed_out = False
        while not self.tree_stopped():
            if time.monotonic() >= deadline:
                timed_out = True
                self.stop()
                break
            time.sleep(.02)
        for worker in self.threads:
            worker.join(timeout=2)
        self.writer.join(timeout=2)
        self._flush_observer()
        return {'pid': self.pid, 'exitCode': self.poll(), 'treeStopped': self.tree_stopped(),
            'startedAt': self.started_at, 'waitStartedAt': self.wait_started_at, 'timeoutAt': self.timeout_at,
            'observationGap': self.observation_gap.is_set(), 'observationFailed': self.observation_failed, 'observerQueueCapacity': 64,
            'timedOut': timed_out, 'outputLimited': self.output_limited,
            **{k: bytes(v).decode('utf-8', errors='replace') for k, v in self.buffers.items()}}

    def close(self):
        with self.lifecycle_lock:
            if self.closed:
                return
            if self.process is not None:
                if not self.tree_stopped():
                    self.stop()
                self.exit_code = self.poll()
                _winapi.CloseHandle(self.process)
                self.process = None
            for handle in self.process_handles:
                kernel.CloseHandle(handle)
            self.process_handles.clear()
            if self.job:
                kernel.CloseHandle(self.job)
                self.job = None
            if self.completion_port:
                kernel.CloseHandle(self.completion_port)
                self.completion_port = None
            self.closed = True
            self.observer_stop.set()
