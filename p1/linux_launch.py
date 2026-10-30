"""Start a transient Linux console unit, or access its existing same-UID host."""
import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from linux_entry import private_access
from linux_instance import request
from root_lock import RootLock, OwnedRoot


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('operation', choices=('start', 'access', 'status', 'stop'))
    parser.add_argument('--root', type=Path, default=Path(__file__).resolve().parents[1] / 'userdata')
    parser.add_argument('--port', type=int, default=8787)
    parser.add_argument('--preview-port', type=int, default=8788)
    args = parser.parse_args()
    os.umask(0o077)
    if sys.platform != 'linux' or sys.version_info[:2] != (3, 12):
        parser.error('Native Linux Python 3.12 is required')
    if any(not 0 <= port <= 65535 for port in (args.port, args.preview_port)) or args.port and args.port == args.preview_port:
        parser.error('Two distinct loopback ports are required')
    stage = 'root'
    try:
        root = args.root.absolute()
        if args.operation == 'access':
            result = private_access(root)
        elif args.operation in ('status', 'stop'):
            result = request(root, 'probe' if args.operation == 'status' else 'shutdown')
        else:
            if any(part.is_symlink() for part in (root, *root.parents)):
                raise ValueError('Linked runtime directory')
            try:
                result = request(root, 'probe', timeout=.3)
            except (OSError, ValueError):
                stage = 'reopen'
                ownership_deadline = time.monotonic() + 5
                while True:
                    try:
                        lock = RootLock(root)
                        lock.close()
                        break
                    except OwnedRoot:
                        try:
                            result = request(root, 'probe', timeout=.2)
                            print(json.dumps({**result, 'reused': True}), flush=True)
                            return 0
                        except (OSError, ValueError):
                            if time.monotonic() >= ownership_deadline:
                                raise TimeoutError('Existing owner is unavailable; ownership was not removed')
                            time.sleep(.05)
                scope = str(os.getuid()) + '|' + str(root.resolve())
                unit = 'relay-console-' + hashlib.sha256(scope.encode()).hexdigest()[:24]
                manager = '--system' if os.getuid() == 0 else '--user'
                stage = 'previous_unit'
                release_deadline = time.monotonic() + 10
                while True:
                    try:
                        result = request(root, 'probe', timeout=.2)
                        print(json.dumps({**result, 'reused': True}), flush=True)
                        return 0
                    except (OSError, ValueError):
                        pass
                    previous = subprocess.run(['/usr/bin/systemctl', manager, 'show', unit,
                        '--property=LoadState'], capture_output=True, text=True, timeout=2)
                    properties = dict(line.split('=', 1) for line in previous.stdout.splitlines() if '=' in line)
                    if properties.get('LoadState') == 'not-found':
                        break
                    if previous.returncode or time.monotonic() >= release_deadline:
                        raise RuntimeError('Previous unit is unavailable or still closing; nothing was stopped or removed')
                    time.sleep(.05)
                command = ['/usr/bin/systemd-run', manager, '--quiet', '--collect', '--unit', unit,
                    '--property=Type=exec', '--property=KillMode=control-group',
                    '--property=TimeoutStopSec=15s', '--property=UMask=0077', '--property=Restart=no',
                    '--property=WorkingDirectory=' + str(Path(__file__).resolve().parents[1]),
                    sys.executable, '-I', '-B', '-X', 'utf8', str(Path(__file__).with_name('linux_entry.py')),
                    'serve', '--root', str(root), '--port', str(args.port), '--preview-port', str(args.preview_port)]
                stage = 'start'
                started = subprocess.run(command, capture_output=True, text=True, timeout=10)
                stage = 'ready'
                deadline = time.monotonic() + 10
                while True:
                    try:
                        result = {**request(root, 'probe', timeout=.5), 'unit': unit}
                        break
                    except (OSError, ValueError):
                        if time.monotonic() >= deadline:
                            raise TimeoutError('Console did not become ready; inspect its unit, no ownership was removed')
                        time.sleep(.05)
                result['startCommandAccepted'] = started.returncode == 0
            result['sessionPersistence'] = 'Transient system unit' if os.getuid() == 0 else 'User manager required; linger not changed'
            result['rebootAutostart'] = False
        print(json.dumps(result, ensure_ascii=True), flush=True)
        return 0
    except (OSError, ValueError, RuntimeError, subprocess.TimeoutExpired) as error:
        category = ('timeout' if isinstance(error, (TimeoutError, subprocess.TimeoutExpired)) else
            'os_error' if isinstance(error, OSError) else
            'invalid_input' if isinstance(error, ValueError) else 'internal_error')
        number = getattr(error, 'errno', None) if isinstance(error, OSError) else None
        print(json.dumps({'ok': False, 'code': 'linux_launcher_failed', 'stage': stage, 'category': category,
            'errno': number if type(number) is int and -(2 ** 31) <= number < 2 ** 31 else None}), file=sys.stderr)
        return 1


if __name__ == '__main__':
    sys.exit(main())
