"""systemd249 supervisor prototype: remain main PID while owned cgroup descendants live.

Not an execution permission gate: callers still need the host ledger/start handshake
before this can be wired into model execution. Current production console never calls it.
"""
import argparse
import ctypes
import json
import os
from pathlib import Path
import re
import subprocess
import sys
import time
import uuid


def context():
    lines = Path('/proc/self/cgroup').read_text().splitlines()
    unified = [line[3:] for line in lines if line.startswith('0::')]
    if len(unified) != 1:
        raise ValueError('Unified cgroup v2 is required')
    group = unified[0]
    unit = group.rsplit('/', 1)[-1]
    if not re.fullmatch('relay-(?:lifecycle-test|child)-[a-f0-9]{32}\.service', unit):
        raise ValueError('An explicitly owned execution unit is required')
    command = ['/usr/bin/systemctl', '--system' if os.getuid() == 0 else '--user', 'show', unit,
        '--property=MainPID', '--property=InvocationID', '--property=ControlGroup',
        '--property=Type', '--property=KillMode']
    result = subprocess.run(command, capture_output=True, text=True, timeout=10)
    if result.returncode:
        raise ValueError('Unit manager cannot verify ownership')
    properties = dict(line.split('=', 1) for line in result.stdout.splitlines() if '=' in line)
    if (properties.get('MainPID') != str(os.getpid())
            or properties.get('ControlGroup') != group
            or properties.get('InvocationID') != os.environ.get('INVOCATION_ID')
            or properties.get('Type') != 'exec'
            or properties.get('KillMode') != 'control-group'):
        raise ValueError('Unit ownership does not match this supervisor')
    directory = Path('/sys/fs/cgroup') / group.lstrip('/')
    if not directory.is_dir() or not (directory / 'cgroup.events').is_file():
        raise ValueError('Owned cgroup cannot be verified')
    return unit, properties['InvocationID'], directory


def members(directory):
    paths = [directory / 'cgroup.procs', *directory.glob('**/cgroup.procs')]
    pids = set()
    for path in paths:
        try:
            pids.update(int(value) for value in path.read_text().split())
        except FileNotFoundError:
            if path.parent == directory:
                raise
    return pids


def supervise(argv, report):
    unit, invocation, directory = context()
    libc = ctypes.CDLL(None, use_errno=True)
    if libc.prctl(36, 1, 0, 0, 0) != 0:  # PR_SET_CHILD_SUBREAPER
        raise OSError(ctypes.get_errno(), 'Cannot adopt orphan descendants')
    started = time.time()
    child = subprocess.Popen(argv, close_fds=True)
    while True:
        code = child.poll()
        if code is not None:
            while True:
                try:
                    pid, _ = os.waitpid(-1, os.WNOHANG)
                    if not pid:
                        break
                except ChildProcessError:
                    break
            if members(directory) <= {os.getpid()}:
                break
        time.sleep(.02)
    data = {'unit': unit, 'invocationId': invocation, 'pid': child.pid,
        'exitCode': code, 'startedAt': started, 'finishedAt': time.time(),
        'descendantsStopped': True}
    report = Path(report)
    if any(part.is_symlink() for part in (report.parent, *report.parent.parents)):
        raise ValueError('Linked report directory')
    temporary = report.with_name(report.name + '.' + uuid.uuid4().hex + '.tmp')
    fd = os.open(temporary, os.O_WRONLY | os.O_CREAT | os.O_EXCL | os.O_NOFOLLOW, 0o600)
    with os.fdopen(fd, 'w', encoding='utf-8') as stream:
        json.dump(data, stream)
        stream.flush()
        os.fsync(stream.fileno())
    # Never overwrite an existing terminal record.
    os.link(temporary, report, follow_symlinks=False)
    temporary.unlink()
    return code if code >= 0 else 128 - code


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--result', type=Path, required=True)
    parser.add_argument('argv', nargs=argparse.REMAINDER)
    args = parser.parse_args()
    argv = args.argv[1:] if args.argv[:1] == ['--'] else args.argv
    if not argv or not Path(argv[0]).is_absolute():
        parser.error('An absolute trusted executable is required')
    os.umask(0o077)
    try:
        return supervise(argv, args.result)
    except (OSError, ValueError, subprocess.TimeoutExpired):
        print('{"ok":false,"code":"unverified_cgroup_supervisor"}', file=sys.stderr)
        return 125


if __name__ == '__main__':
    sys.exit(main())
