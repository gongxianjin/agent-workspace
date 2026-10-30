"""Owned Linux process trees: explicit host context, pre-launch handshake, durable ledger."""
import base64
import json
import os
from pathlib import Path
import queue
import re
import socket
import stat
import struct
import subprocess
import sys
import threading
import time
import uuid

from linux_process_entry import cgroup_empty, current_group, properties, receive_frame, send_frame, start_identity

_active = None
_context_lock = threading.RLock()


def durable(path, data):
    temporary = path.with_name(path.name + '.' + uuid.uuid4().hex + '.tmp')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    try:
        with os.fdopen(fd, 'w', encoding='utf-8') as stream:
            json.dump(data, stream)
            stream.flush()
            os.fsync(stream.fileno())
        os.replace(temporary, path)
        directory = os.open(path.parent, os.O_RDONLY | os.O_DIRECTORY | os.O_NOFOLLOW)
        try:
            os.fsync(directory)
        finally:
            os.close(directory)
    finally:
        if temporary.exists():
            temporary.unlink()


class HostContext:
    def __init__(self, root):
        self.root = Path(root).absolute()
        self.pid = os.getpid()
        self.start = start_identity(self.pid)
        self.group = current_group()
        self.unit = self.group.rsplit('/', 1)[-1]
        self.children = set()
        self.lock = threading.RLock()
        self.closed = False
        self.unknown = False
        if not re.fullmatch(r'relay-(?:console-[a-f0-9]{24}|process-test-[a-f0-9]{32})\.service', self.unit):
            raise ValueError('An owned Relay systemd host is required')
        own = properties(self.unit)
        if (own.get('MainPID') != str(self.pid) or own.get('ControlGroup') != self.group
                or own.get('Type') != 'exec' or own.get('KillMode') != 'control-group'
                or not own.get('InvocationID') or own['InvocationID'] != os.environ.get('INVOCATION_ID')):
            raise ValueError('Host ownership cannot be verified')
        self.invocation = own['InvocationID']
        if any(path.is_symlink() for path in (self.root, *self.root.parents)):
            raise ValueError('Linked process ownership directory')
        self.root.mkdir(mode=0o700, parents=False, exist_ok=True)
        info = self.root.stat()
        if info.st_uid != os.getuid() or stat.S_IMODE(info.st_mode) != 0o700:
            raise ValueError('Process ownership directory must be private')
        for record in self.root.glob('*.json'):
            if record.is_symlink():
                raise ValueError('Linked ownership ledger')
            data = json.loads(record.read_text())
            if data.get('status') != 'stopped':
                raise ValueError('Unsettled owned process ledger; no execution granted')
        with _context_lock:
            global _active
            if _active is not None:
                raise ValueError('Another owned host context is active')
            _active = self

    def __enter__(self):
        return self

    def __exit__(self, *args):
        self.close()

    def verify_local(self):
        """Read-only liveness for reporting; never grants process launch authority."""
        if self.closed or self.unknown or os.getpid() != self.pid or start_identity(self.pid) != self.start:
            raise ValueError('Owned host is closed or changed')

    def verify(self):
        self.verify_local()
        own = properties(self.unit)
        if own.get('InvocationID') != self.invocation or own.get('MainPID') != str(self.pid):
            raise ValueError('Owned host identity changed')

    def close(self):
        with self.lock:
            self.closed = True
            children = list(self.children)
        for child in children:
            child.close()
        if self.unknown:
            return False
        with _context_lock:
            global _active
            if _active is self:
                _active = None
        return True


def active_context():
    with _context_lock:
        if _active is None:
            raise ValueError('No verified owned Linux host context')
        _active.verify()
        return _active


class LinuxProcess:
    def __init__(self, argv, cwd, env=None, input_data=b'', output_limit=8 * 1024 * 1024,
            stdout_observer=None, wait_observer=None):
        self.host = active_context()
        self.argv, self.cwd = list(argv), str(Path(cwd).absolute())
        if (not self.argv or not Path(self.argv[0]).is_absolute() or not all(isinstance(a, str) for a in self.argv)
                or not isinstance(input_data, bytes) or len(input_data) > 8 * 1024 * 1024
                or not isinstance(output_limit, int) or not 0 < output_limit <= 64 * 1024 * 1024):
            raise ValueError('Invalid bounded native process launch')
        self.unit = 'relay-child-' + uuid.uuid4().hex + '.service'
        self.record_path = self.host.root / (self.unit + '.json')
        self.record = {'unit': self.unit, 'hostUnit': self.host.unit, 'hostInvocationId': self.host.invocation,
            'hostPid': self.host.pid, 'status': 'starting'}
        self.pid = self.started_at = self.exit_code = None
        self.invocation = self.group = None
        self.closed = self.cancelled = self.stopped = self.output_limited = False
        self.output_limit = output_limit
        self.buffers = {'stdout': bytearray(), 'stderr': bytearray()}
        self.buffer_lock = threading.Lock()
        self.lifecycle_lock = threading.RLock()
        self.observer = stdout_observer
        self.wait_observer = wait_observer
        self.wait_started_at = self.timeout_at = None
        self.observer_queue = queue.Queue(maxsize=64)
        self.observer_stop = threading.Event()
        self.observation_gap = threading.Event()
        self.observation_failed = False
        self.observer_thread = self.reader = self.channel = None
        self.finished = threading.Event()
        self.start_event = threading.Event()
        self.transport_failed = False
        address = 'relay-process-' + uuid.uuid4().hex
        manager = '--system' if os.getuid() == 0 else '--user'
        self.manager = manager
        self.command_issued = False
        with self.host.lock:
            self.host.verify()
            self.host.children.add(self)
            durable(self.record_path, self.record)
        listener = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
        try:
            listener.bind('\0' + address)
            listener.listen(1)
            listener.settimeout(10)
            command = ['/usr/bin/systemd-run', manager, '--quiet', '--collect', '--unit', self.unit,
                '--property=Type=exec', '--property=KillMode=control-group', '--property=TimeoutStopSec=2s',
                '--property=UMask=0077', '--property=Restart=no', '--property=BindsTo=' + self.host.unit,
                '--property=After=' + self.host.unit, sys.executable, '-I', '-B',
                str(Path(__file__).with_name('linux_process_entry.py')), '--address', address,
                '--host-pid', str(self.host.pid), '--host-start', self.host.start, '--unit', self.unit]
            self.command_issued = True
            started = subprocess.run(command, capture_output=True, timeout=10)
            if started.returncode:
                raise ValueError('Owned unit creation failed')
            channel, _ = listener.accept()
            self.channel = channel
            channel.settimeout(10)
            peer_pid, peer_uid, _ = struct.unpack('3i', channel.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
            kind, raw = receive_frame(channel, 4096)
            hello = json.loads(raw)
            own = properties(self.unit)
            self.invocation, self.group = own.get('InvocationID'), own.get('ControlGroup')
            if (kind != b'H' or peer_uid != os.getuid() or own.get('MainPID') != str(peer_pid)
                    or hello.get('supervisorPid') != peer_pid or hello.get('unit') != self.unit
                    or not self.invocation or hello.get('invocationId') != self.invocation
                    or not self.group or hello.get('group') != self.group
                    or self.group.rsplit('/', 1)[-1] != self.unit or own.get('Type') != 'exec'
                    or own.get('KillMode') != 'control-group' or self.host.unit not in own.get('BindsTo', '').split()):
                raise ValueError('Owned child handshake mismatch')
            with self.host.lock:
                self.host.verify()
                self.record.update(status='running', invocationId=self.invocation, group=self.group,
                    supervisorPid=peer_pid)
                durable(self.record_path, self.record)
                launch = {'argv': self.argv, 'cwd': self.cwd, 'env': dict(os.environ if env is None else env),
                    'input': base64.b64encode(input_data).decode('ascii')}
                send_frame(channel, b'G', json.dumps(launch).encode())
            channel.settimeout(None)
            if self.observer:
                self.observer_thread = threading.Thread(target=self._observe, daemon=True)
                self.observer_thread.start()
            self.reader = threading.Thread(target=self._read, daemon=True)
            self.reader.start()
            if not self.start_event.wait(10) or not self.pid:
                raise ValueError('Owned target start not acknowledged')
        except BaseException:
            self.close()
            raise
        finally:
            listener.close()

    def _observe(self):
        try:
            gap_sent = False
            while True:
                if self.observation_gap.is_set():
                    if not gap_sent:
                        self.observer(None)
                        gap_sent = True
                    if self.observer_stop.wait(.02):
                        return
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

    def _read(self):
        try:
            while True:
                kind, raw = receive_frame(self.channel)
                if kind == b'S':
                    data = json.loads(raw)
                    if self.pid or type(data.get('pid')) is not int or data['pid'] <= 0:
                        raise ValueError('Invalid owned start receipt')
                    self.pid, self.started_at = data['pid'], data['startedAt']
                    self.start_event.set()
                elif kind in (b'O', b'E'):
                    name = 'stdout' if kind == b'O' else 'stderr'
                    with self.buffer_lock:
                        room = self.output_limit - sum(len(v) for v in self.buffers.values())
                        self.buffers[name].extend(raw[:max(0, room)])
                        self.output_limited |= len(raw) > room
                    if kind == b'O' and self.observer and not self.observation_gap.is_set():
                        try:
                            self.observer_queue.put_nowait(raw)
                        except queue.Full:
                            self.observation_gap.set()
                    if self.output_limited:
                        threading.Thread(target=self.stop, daemon=True).start()
                        return
                elif kind == b'T':
                    data = json.loads(raw)
                    if (data.get('unit') != self.unit or data.get('invocationId') != self.invocation
                            or data.get('group') != self.group or data.get('pid') != self.pid
                            or type(data.get('exitCode')) is not int or data.get('descendantsStopped') is not True):
                        raise ValueError('Invalid owned terminal receipt')
                    self.exit_code = data['exitCode']
                    return
                else:
                    raise ValueError('Unknown owned process frame')
        except (OSError, ValueError, EOFError):
            self.transport_failed = True
        finally:
            self.start_event.set()
            self.finished.set()

    def poll(self):
        return self.exit_code

    def tree_stopped(self):
        with self.lifecycle_lock:
            if self.stopped:
                return True
            if not self.invocation or not self.group:
                return False
            try:
                own = properties(self.unit)
                if own.get('LoadState') == 'not-found':
                    settled = cgroup_empty(self.group)
                else:
                    settled = (own.get('InvocationID') == self.invocation
                        and own.get('ActiveState') in ('inactive', 'failed')
                        and own.get('MainPID') == '0' and cgroup_empty(self.group))
                if settled:
                    self.record.update(status='stopped', treeStopped=True, exitCode=self.exit_code)
                    durable(self.record_path, self.record)
                    self.stopped = True
                return self.stopped
            except (OSError, ValueError, subprocess.TimeoutExpired):
                return False

    def stop(self):
        self.cancelled = True
        with self.lifecycle_lock:
            if self.stopped:
                return True
            if not self.command_issued:
                return False
            try:
                own = properties(self.unit)
                if self.invocation and own.get('LoadState') != 'not-found' and own.get('InvocationID') != self.invocation:
                    return False
                if not self.invocation:
                    # Startup metadata may lag; never adopt an empty group or a different invocation.
                    invocation = own.get('InvocationID')
                    metadata_deadline = time.monotonic() + 2
                    while True:
                        if (own.get('LoadState') == 'not-found' or not invocation
                                or own.get('InvocationID') != invocation
                                or self.host.unit not in own.get('BindsTo', '').split()
                                or own.get('Type') != 'exec' or own.get('KillMode') != 'control-group'):
                            return False
                        group = own.get('ControlGroup')
                        if group and group.rsplit('/', 1)[-1] == self.unit:
                            self.invocation, self.group = invocation, group
                            break
                        if time.monotonic() >= metadata_deadline:
                            return False
                        time.sleep(.02)
                        own = properties(self.unit)
                if own.get('LoadState') != 'not-found':
                    subprocess.run(['/usr/bin/systemctl', self.manager, 'stop', self.unit],
                        capture_output=True, timeout=6)
            except (OSError, ValueError, subprocess.TimeoutExpired):
                return False
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            if self.tree_stopped():
                if self.exit_code is None:
                    self.exit_code = -15
                return True
            time.sleep(.02)
        return False

    def wait(self, timeout):
        self.wait_started_at = time.time() * 1000
        self.timeout_at = self.wait_started_at + timeout * 1000
        if self.wait_observer:
            try:
                self.wait_observer({'kind': 'process', 'waitStartedAt': self.wait_started_at, 'timeoutAt': self.timeout_at})
            except Exception:
                self.observation_failed = True
        started = time.monotonic()
        deadline = started + timeout
        timed_out = False
        while not self.tree_stopped():
            now = time.monotonic()
            if now >= deadline:
                timed_out = True
                self.stop()
                break
            interval = .02 if now - started < 2 else .25
            time.sleep(min(interval, deadline - now))
        if self.reader:
            self.reader.join(timeout=2)
        self.observer_stop.set()
        if self.observer_thread:
            self.observer_thread.join(timeout=1)
            if self.observer_thread.is_alive():
                self.observation_gap.set()
        return {'pid': self.pid, 'exitCode': self.exit_code, 'treeStopped': self.tree_stopped(),
            'startedAt': self.started_at, 'waitStartedAt': self.wait_started_at, 'timeoutAt': self.timeout_at,
            'observationGap': self.observation_gap.is_set(), 'observationFailed': self.observation_failed,
            'observerQueueCapacity': 64, 'timedOut': timed_out, 'outputLimited': self.output_limited,
            **{name: bytes(value).decode('utf-8', errors='replace') for name, value in self.buffers.items()}}

    def close(self):
        if self.closed:
            return
        if not self.command_issued:
            # No manager request was submitted; there can be no target or owned unit.
            self.record.update(status='stopped', treeStopped=True, launched=False)
            durable(self.record_path, self.record)
            self.stopped = True
        elif not self.tree_stopped():
            self.stop()
        if self.channel:
            try:
                self.channel.shutdown(socket.SHUT_RDWR)
            except OSError:
                pass
            self.channel.close()
        if self.reader and self.reader is not threading.current_thread():
            self.reader.join(timeout=2)
        self.observer_stop.set()
        with self.host.lock:
            if not self.stopped:
                self.host.unknown = True
            else:
                self.host.children.discard(self)
        self.closed = True
