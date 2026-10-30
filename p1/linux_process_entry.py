"""Private owned-unit supervisor. Never receives target arguments or secrets via argv/journal."""
import argparse
import base64
import ctypes
import json
import os
from pathlib import Path
import selectors
import socket
import struct
import subprocess
import sys
import threading
import time


def send_frame(stream, kind, value):
    stream.sendall(kind + struct.pack('!I', len(value)) + value)


def exact(stream, size):
    data = bytearray()
    while len(data) < size:
        chunk = stream.recv(size - len(data))
        if not chunk:
            raise EOFError('Owned control channel closed')
        data.extend(chunk)
    return bytes(data)


def receive_frame(stream, limit=16 * 1024 * 1024):
    header = exact(stream, 5)
    size = struct.unpack('!I', header[1:])[0]
    if size > limit:
        raise ValueError('Owned control frame exceeds limit')
    return header[:1], exact(stream, size)


def properties(unit):
    result = subprocess.run(['/usr/bin/systemctl', '--system' if os.getuid() == 0 else '--user', 'show', unit,
        '--property=MainPID,InvocationID,ControlGroup,Type,KillMode,ActiveState,SubState,LoadState,BindsTo'],
        capture_output=True, text=True, timeout=5)
    if result.returncode and 'LoadState=not-found' not in result.stdout:
        raise ValueError('Cannot verify owned manager state')
    return dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)


def start_identity(pid):
    # /proc stat comm may contain spaces or parentheses; fields begin after its final ')'.
    text = Path('/proc', str(pid), 'stat').read_text()
    return text[text.rfind(')') + 2:].split()[19]


def current_group():
    groups = [line[3:] for line in Path('/proc/self/cgroup').read_text().splitlines() if line.startswith('0::')]
    if len(groups) != 1 or not groups[0].startswith('/') or '..' in groups[0].split('/'):
        raise ValueError('Unified owned cgroup required')
    return groups[0]


def cgroup_empty(group):
    root = Path('/sys/fs/cgroup') / group.lstrip('/')
    try:
        return 'populated 0' in (root / 'cgroup.events').read_text().splitlines()
    except FileNotFoundError:
        return not root.exists()


def members(group):
    root = Path('/sys/fs/cgroup') / group.lstrip('/')
    result = set()
    for path in [root / 'cgroup.procs', *root.glob('**/cgroup.procs')]:
        try:
            result.update(int(value) for value in path.read_text().split())
        except FileNotFoundError:
            if path.parent == root:
                raise
    return result


def supervise(address, host_pid, host_start, unit):
    group = current_group()
    own = properties(unit)
    if (group.rsplit('/', 1)[-1] != unit or own.get('MainPID') != str(os.getpid())
            or own.get('ControlGroup') != group or own.get('Type') != 'exec'
            or own.get('KillMode') != 'control-group' or not own.get('InvocationID')
            or own['InvocationID'] != os.environ.get('INVOCATION_ID')):
        raise ValueError('Supervisor ownership mismatch')
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0):
        raise OSError(ctypes.get_errno(), 'Cannot adopt descendants')
    with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as channel:
        channel.settimeout(10)
        channel.connect('\0' + address)
        peer_pid, peer_uid, _ = struct.unpack('3i', channel.getsockopt(socket.SOL_SOCKET, socket.SO_PEERCRED, 12))
        if peer_pid != host_pid or peer_uid != os.getuid() or start_identity(peer_pid) != host_start:
            raise ValueError('Host identity mismatch')
        send_frame(channel, b'H', json.dumps({'unit': unit, 'invocationId': own['InvocationID'],
            'supervisorPid': os.getpid(), 'group': group}).encode())
        kind, raw = receive_frame(channel)
        if kind != b'G' or start_identity(host_pid) != host_start:
            raise ValueError('No owned launch grant')
        launch = json.loads(raw)
        if (not isinstance(launch.get('argv'), list) or not launch['argv']
                or not Path(launch['argv'][0]).is_absolute() or not Path(launch['cwd']).is_absolute()
                or not isinstance(launch.get('env'), dict)):
            raise ValueError('Invalid launch grant')
        channel.settimeout(None)
        child = subprocess.Popen(launch['argv'], cwd=launch['cwd'], env=launch['env'],
            stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE, close_fds=True)
        send_frame(channel, b'S', json.dumps({'pid': child.pid, 'startedAt': time.time() * 1000}).encode())
        data = base64.b64decode(launch['input'], validate=True)
        def write_input():
            try:
                child.stdin.write(data)
            except (BrokenPipeError, OSError):
                pass
            finally:
                child.stdin.close()
        writer = threading.Thread(target=write_input, daemon=True)
        writer.start()
        with selectors.DefaultSelector() as selector:
            for stream, tag in ((child.stdout, b'O'), (child.stderr, b'E')):
                os.set_blocking(stream.fileno(), False)
                selector.register(stream, selectors.EVENT_READ, tag)
            while True:
                for key, _ in selector.select(.02):
                    chunk = os.read(key.fd, 8192)
                    if chunk:
                        send_frame(channel, key.data, chunk)
                    else:
                        selector.unregister(key.fileobj)
                        key.fileobj.close()
                code = child.poll()
                if code is None:
                    continue
                while True:
                    try:
                        adopted, _ = os.waitpid(-1, os.WNOHANG)
                        if not adopted:
                            break
                    except ChildProcessError:
                        break
                if not selector.get_map() and members(group) <= {os.getpid()}:
                    writer.join(timeout=1)
                    send_frame(channel, b'T', json.dumps({'unit': unit, 'invocationId': own['InvocationID'],
                        'group': group, 'pid': child.pid, 'exitCode': code, 'descendantsStopped': True}).encode())
                    return 0


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--address', required=True)
    parser.add_argument('--host-pid', type=int, required=True)
    parser.add_argument('--host-start', required=True)
    parser.add_argument('--unit', required=True)
    args = parser.parse_args()
    os.umask(0o077)
    try:
        return supervise(args.address, args.host_pid, args.host_start, args.unit)
    except (OSError, ValueError, EOFError, subprocess.TimeoutExpired):
        # Never log launch payload, environment, target output or exception text.
        return 125


if __name__ == '__main__':
    sys.exit(main())
