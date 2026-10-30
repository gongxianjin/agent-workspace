"""Native Linux entrypoint and same-UID capability handoff, without model calls."""
from contextlib import ExitStack
import http.client
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


@unittest.skipUnless(sys.platform == 'linux', 'Linux abstract Unix control channel')
class LinuxEntryTests(unittest.TestCase):
    def channel(self):
        try:
            import linux_instance
            return linux_instance
        except ImportError as error:
            self.fail('Linux same-UID instance channel is missing: ' + str(error))

    def launch(self, root, control=0, preview=0):
        return subprocess.Popen([sys.executable, '-I', '-B', '-X', 'utf8',
            str(ROOT / 'p1/linux_entry.py'), 'serve', '--root', str(root),
            '--port', str(control), '--preview-port', str(preview)],
            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)

    def ready(self, child, root):
        channel = self.channel()
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            if child.poll() is not None:
                stdout, stderr = child.communicate()
                self.fail('Linux entrypoint did not start: ' + stdout + stderr)
            try:
                return channel.request(root, 'probe', timeout=.2)
            except (OSError, TimeoutError):
                time.sleep(.03)
        self.fail('Linux owned console did not become ready')

    def cleanup_child(self, child, root):
        if child.poll() is None:
            try:
                self.channel().request(root, 'shutdown')
                child.wait(10)
            except (OSError, ValueError, TimeoutError, subprocess.TimeoutExpired):
                child.kill()
                child.wait(5)
        child.communicate()

    def shutdown_and_wait(self, root):
        try:
            info = self.channel().request(root, 'probe')
        except (OSError, ValueError):
            return
        from linux_process_entry import start_identity
        pid = info['pid']
        identity = start_identity(pid)
        self.channel().request(root, 'shutdown')
        deadline = time.monotonic() + 10
        while time.monotonic() < deadline:
            try:
                if start_identity(pid) != identity:
                    return
            except FileNotFoundError:
                return
            time.sleep(.02)
        self.fail('Fixture shutdown acknowledged but its owned host is still alive')

    def http(self, origin, path, body=None, token=None):
        connection = http.client.HTTPConnection(origin.removeprefix('http://'), timeout=5)
        headers = {'Origin': origin}
        if token:
            headers['Authorization'] = 'Bearer ' + token
        if body is not None:
            headers['Content-Type'] = 'application/json'
        try:
            connection.request('GET' if body is None else 'POST', path,
                None if body is None else json.dumps(body), headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def test_entry_reopens_without_restart_and_persists_after_normal_exit(self):
        with tempfile.TemporaryDirectory(prefix='relay-entry-') as directory, ExitStack() as cleanup:
            root = Path(directory) / ('长目录 space ' * 8) / 'runtime'
            child = self.launch(root)
            cleanup.callback(self.cleanup_child, child, root)
            info = self.ready(child, root)
            self.assertTrue(info['ok'])
            self.assertEqual(info['pid'], child.pid)
            self.assertNotEqual(info['origin'], info['previewOrigin'])
            self.assertNotIn('bootstrap', json.dumps(info))
            second = self.launch(root)
            stdout, stderr = second.communicate(timeout=10)
            self.assertEqual(second.returncode, 0, stdout + stderr)
            self.assertNotIn('bootstrap=', stdout + stderr)
            self.assertEqual(self.channel().request(root, 'probe')['pid'], child.pid)
            access = self.channel().request(root, 'access')
            bootstrap = access['url'].split('#bootstrap=')[1]
            status, session = self.http(info['origin'], '/api/session', {'bootstrap': bootstrap})
            self.assertEqual(status, 200)
            self.assertEqual(self.http(info['origin'], '/api/session', {'bootstrap': bootstrap})[0], 403)
            status, project = self.http(info['origin'], '/api/projects', {'name': '210持久化项目'}, session['token'])
            self.assertEqual(status, 200)
            self.assertEqual(self.channel().request(root, 'shutdown'), {'ok': True})
            stdout, stderr = child.communicate(timeout=10)
            self.assertEqual(child.returncode, 0, stdout + stderr)
            self.assertNotIn('bootstrap=', stdout + stderr)
            self.assertEqual((root.stat().st_mode & 0o777), 0o700)
            restarted = self.launch(root)
            cleanup.callback(self.cleanup_child, restarted, root)
            info = self.ready(restarted, root)
            access = self.channel().request(root, 'access')
            _, session = self.http(info['origin'], '/api/session', {'bootstrap': access['url'].split('#bootstrap=')[1]})
            status, state = self.http(info['origin'], '/api/state', token=session['token'])
            self.assertEqual(status, 200)
            self.assertEqual(state['projects'][0]['id'], project['id'])
            self.assertEqual(state['projects'][0]['name'], '210持久化项目')
            self.assertFalse(state['workers'])

    def test_entry_rejects_occupied_port_without_capability_leak(self):
        with tempfile.TemporaryDirectory(prefix='relay-conflict-') as directory, socket.socket() as occupied:
            occupied.bind(('127.0.0.1', 0))
            occupied.listen()
            child = self.launch(Path(directory) / 'runtime', control=occupied.getsockname()[1])
            stdout, stderr = child.communicate(timeout=10)
            self.assertNotEqual(child.returncode, 0)
            self.assertIn('startup_failed', stdout + stderr)
            self.assertNotIn('bootstrap=', stdout + stderr)
            self.assertNotIn('Traceback', stdout + stderr)

    def test_cli_access_is_private_and_never_prints_capability(self):
        with tempfile.TemporaryDirectory(prefix='relay-access-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            child = self.launch(root)
            cleanup.callback(self.cleanup_child, child, root)
            self.ready(child, root)
            result = subprocess.run([sys.executable, '-I', '-B', str(ROOT / 'p1/linux_entry.py'),
                'access', '--root', str(root)], capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertFalse('bootstrap' in result.stdout + result.stderr, 'CLI must not print access capabilities')
            response = json.loads(result.stdout)
            access_file = Path(response['accessFile'])
            self.assertTrue(access_file.is_relative_to(root / 'private-access'))
            self.assertEqual(access_file.stat().st_mode & 0o777, 0o600)
            self.assertEqual(access_file.parent.stat().st_mode & 0o777, 0o700)
            self.assertIn('#bootstrap=', access_file.read_text())
            access_file.unlink()
            access_file.parent.rmdir()

    def test_expired_private_access_files_are_cleaned_without_following_links(self):
        with tempfile.TemporaryDirectory(prefix='relay-access-expire-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            child = self.launch(root)
            cleanup.callback(self.cleanup_child, child, root)
            self.ready(child, root)
            from linux_entry import private_access
            first = private_access(root)
            expired = Path(first['accessFile'])
            old = time.time() - 65
            os.utime(expired.parent, (old, old))
            outside = Path(directory) / 'outside'
            outside.mkdir()
            target = outside / 'launch.txt'
            target.write_text('untouched')
            linked = root / 'private-access' / ('a' * 32)
            linked.symlink_to(outside, target_is_directory=True)
            second = private_access(root)
            self.assertFalse(expired.exists(), 'Expired handoff must not accumulate')
            self.assertEqual(target.read_text(), 'untouched')
            self.assertTrue(Path(second['accessFile']).is_file())

    def test_cli_rejects_linked_access_directory(self):
        with tempfile.TemporaryDirectory(prefix='relay-access-link-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            child = self.launch(root)
            cleanup.callback(self.cleanup_child, child, root)
            self.ready(child, root)
            outside = Path(directory) / 'outside'
            outside.mkdir()
            (root / 'private-access').symlink_to(outside, target_is_directory=True)
            result = subprocess.run([sys.executable, '-I', '-B', str(ROOT / 'p1/linux_entry.py'),
                'access', '--root', str(root)], capture_output=True, text=True, timeout=5)
            self.assertNotEqual(result.returncode, 0)
            self.assertFalse('bootstrap' in result.stdout + result.stderr, 'CLI must not print access capabilities')
            self.assertEqual(list(outside.iterdir()), [])

    def test_shell_entry_starts_owned_service_without_autostart(self):
        with tempfile.TemporaryDirectory(prefix='relay-shell-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            entry = ROOT / 'start-workbench.sh'
            command = ['/bin/sh', str(entry), 'start', '--root', str(root), '--port', '0', '--preview-port', '0']
            result = subprocess.run(command, cwd='/', capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            response = json.loads(result.stdout)
            self.assertTrue(response['ok'])
            self.assertNotIn('bootstrap', result.stdout + result.stderr)
            cleanup.callback(self.shutdown_and_wait, root)
            info = self.channel().request(root, 'probe')
            self.assertTrue(info.get('workers'), 'Supported owned host must expose its live scheduler')
            access = self.channel().request(root, 'access')
            _, session = self.http(info['origin'], '/api/session', {'bootstrap': access['url'].split('#bootstrap=')[1]})
            status, state = self.http(info['origin'], '/api/state', token=session['token'])
            self.assertEqual(status, 200)
            self.assertTrue(state['workers'])
            self.assertEqual(info['executionMode'], state['executionMode'])
            self.assertEqual(state['executionMode'], 'blocked', 'Scheduler availability is not model authority')
            again = subprocess.run(command, cwd='/', capture_output=True, text=True, timeout=20)
            self.assertEqual(again.returncode, 0, again.stdout + again.stderr)
            self.assertEqual(self.channel().request(root, 'probe')['pid'], info['pid'])
            result = subprocess.run(['/bin/sh', str(entry), 'status', '--root', str(root)],
                cwd='/', capture_output=True, text=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr)
            self.assertEqual(json.loads(result.stdout)['pid'], info['pid'])
            self.assertTrue(response['unit'].startswith('relay-console-'))
            manager = '--system' if os.getuid() == 0 else '--user'
            unit_state = subprocess.run(['/usr/bin/systemctl', manager, 'show', response['unit'],
                '--property=MainPID', '--property=KillMode', '--property=Type'],
                capture_output=True, text=True, timeout=5)
            self.assertEqual(unit_state.returncode, 0, unit_state.stderr)
            self.assertIn('MainPID=' + str(info['pid']), unit_state.stdout)
            self.assertIn('KillMode=control-group', unit_state.stdout)

    def test_start_waits_for_previous_unit_after_root_lock_is_released(self):
        import hashlib
        with tempfile.TemporaryDirectory(prefix='relay-unit-closing-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            root.mkdir(mode=0o700)
            unit = 'relay-console-' + hashlib.sha256((str(os.getuid()) + '|' + str(root.resolve())).encode()).hexdigest()[:24]
            manager = '--system' if os.getuid() == 0 else '--user'
            marker = Path(directory) / 'closing'
            code = 'import time;from pathlib import Path;Path(' + repr(str(marker)) + ').write_text("closing");time.sleep(2)'
            created = subprocess.run(['/usr/bin/systemd-run', manager, '--quiet', '--collect', '--unit', unit,
                '--property=Type=exec', sys.executable, '-I', '-c', code], capture_output=True, text=True, timeout=10)
            self.assertEqual(created.returncode, 0, created.stderr)
            cleanup.callback(lambda: subprocess.run(['/usr/bin/systemctl', manager, 'stop', unit], capture_output=True, timeout=10))
            deadline = time.monotonic() + 3
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.02)
            self.assertTrue(marker.exists())
            result = subprocess.run(['/bin/sh', str(ROOT / 'start-workbench.sh'), 'start',
                '--root', str(root), '--port', '0', '--preview-port', '0'], capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            cleanup.callback(self.shutdown_and_wait, root)
            self.assertTrue(self.channel().request(root, 'probe')['ok'])

    def test_supported_launcher_restarts_same_root_after_normal_shutdown(self):
        with tempfile.TemporaryDirectory(prefix='relay-restart-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            command = ['/bin/sh', str(ROOT / 'start-workbench.sh'), 'start',
                '--root', str(root), '--port', '0', '--preview-port', '0']
            cleanup.callback(self.shutdown_and_wait, root)
            first = subprocess.run(command, capture_output=True, text=True, timeout=20)
            self.assertEqual(first.returncode, 0, first.stdout + first.stderr)
            for _ in range(3):
                self.assertEqual(self.channel().request(root, 'shutdown'), {'ok': True})
                restarted = subprocess.run(command, capture_output=True, text=True, timeout=20)
                self.assertEqual(restarted.returncode, 0, restarted.stdout + restarted.stderr)
                self.assertTrue(self.channel().request(root, 'probe')['ok'])

    def test_start_does_not_reuse_owner_that_has_already_accepted_shutdown(self):
        with tempfile.TemporaryDirectory(prefix='relay-closing-probe-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            marker = Path(directory) / 'closing'
            code = ('import sys,time;from pathlib import Path;sys.path.insert(0,sys.argv[1]);'
                'import linux_entry;original=linux_entry.InstanceChannel.close;marker=Path(sys.argv[3]);'
                '\ndef delayed_close(channel):\n'
                ' marker.write_text("closing");time.sleep(1.5);original(channel)\n'
                'linux_entry.InstanceChannel.close=delayed_close\n'
                'sys.argv=["linux_entry.py","serve","--root",sys.argv[2],"--port","0","--preview-port","0"]\n'
                'raise SystemExit(linux_entry.main())')
            child = subprocess.Popen([sys.executable, '-I', '-B', '-c', code, str(ROOT / 'p1'), str(root), str(marker)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            cleanup.callback(self.cleanup_child, child, root)
            first = self.ready(child, root)
            self.assertEqual(self.channel().request(root, 'shutdown'), {'ok': True})
            deadline = time.monotonic() + 3
            while not marker.exists() and time.monotonic() < deadline:
                time.sleep(.01)
            self.assertTrue(marker.exists(), 'The old host must be closing before start is invoked')
            result = subprocess.run(['/bin/sh', str(ROOT / 'start-workbench.sh'), 'start',
                '--root', str(root), '--port', '0', '--preview-port', '0'], capture_output=True, text=True, timeout=25)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            response = json.loads(result.stdout)
            if response['pid'] != first['pid']:
                cleanup.callback(self.shutdown_and_wait, root)
            self.assertNotEqual(response['pid'], first['pid'], 'An acknowledged closing owner is not a reusable ready host')
            child.communicate(timeout=10)
            self.assertTrue(self.channel().request(root, 'probe')['ok'])

    def test_start_waits_for_old_owner_to_finish_closing(self):
        with tempfile.TemporaryDirectory(prefix='relay-closing-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            from root_lock import RootLock
            lock = RootLock(root)
            cleanup.callback(lock.close)
            release = threading.Timer(1.5, lock.close)
            release.start()
            cleanup.callback(release.join)
            result = subprocess.run(['/bin/sh', str(ROOT / 'start-workbench.sh'), 'start',
                '--root', str(root), '--port', '0', '--preview-port', '0'],
                capture_output=True, text=True, timeout=20)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            cleanup.callback(self.shutdown_and_wait, root)
            self.assertTrue(self.channel().request(root, 'probe')['ok'])

    def assert_safe_failure(self, child, root, code, stage, category):
        stdout, stderr = child.communicate(timeout=18)
        self.assertNotEqual(child.returncode, 0)
        self.assertEqual(stdout, '')
        self.assertLess(len(stderr.encode('utf-8')), 2048)
        for forbidden in (str(root), 'bootstrap', 'Bearer', 'Traceback'):
            self.assertNotIn(forbidden, stderr)
        diagnostic = json.loads(stderr)
        self.assertEqual(set(diagnostic), {'ok', 'code', 'stage', 'category', 'errno'})
        self.assertEqual(diagnostic['ok'], False)
        self.assertEqual(diagnostic['code'], code)
        self.assertEqual(diagnostic['stage'], stage)
        self.assertEqual(diagnostic['category'], category)
        return diagnostic

    def test_startup_failure_has_bounded_safe_structured_diagnostic(self):
        from root_lock import RootLock
        with tempfile.TemporaryDirectory(prefix='relay-safe-diagnostic-') as directory, socket.socket() as occupied:
            root = Path(directory) / 'runtime'
            occupied.bind(('127.0.0.1', 0))
            occupied.listen()
            child = self.launch(root, control=occupied.getsockname()[1])
            try:
                diagnostic = self.assert_safe_failure(child, root, 'startup_failed', 'initialize', 'os_error')
                self.assertIsInstance(diagnostic['errno'], int)
            finally:
                self.cleanup_child(child, root)
            lock = RootLock(root)
            lock.close()

    def test_supported_launcher_returns_nonzero_on_occupied_port(self):
        from root_lock import RootLock
        with tempfile.TemporaryDirectory(prefix='relay-shell-conflict-') as directory, socket.socket() as occupied:
            root = Path(directory) / 'runtime'
            occupied.bind(('127.0.0.1', 0))
            occupied.listen()
            child = subprocess.Popen(['/bin/sh', str(ROOT / 'start-workbench.sh'), 'start',
                '--root', str(root), '--port', str(occupied.getsockname()[1]), '--preview-port', '0'],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                diagnostic = self.assert_safe_failure(child, root, 'linux_launcher_failed', 'ready', 'timeout')
                self.assertIsNone(diagnostic['errno'])
                lock = RootLock(root)
                lock.close()
                with self.assertRaises((OSError, TimeoutError)):
                    self.channel().request(root, 'probe', timeout=.15)
            finally:
                self.cleanup_child(child, root)

    def test_reopen_timeout_preserves_lock_and_safe_stage(self):
        from root_lock import OwnedRoot, RootLock
        for entry in ('serve', 'start'):
            with self.subTest(entry=entry), tempfile.TemporaryDirectory(prefix='relay-reopen-timeout-') as directory:
                root = Path(directory) / 'runtime'
                lock = RootLock(root)
                path = root / 'service.lock'
                inode = path.stat().st_ino
                try:
                    if entry == 'serve':
                        child = self.launch(root)
                    else:
                        child = subprocess.Popen(['/bin/sh', str(ROOT / 'start-workbench.sh'), 'start',
                            '--root', str(root), '--port', '0', '--preview-port', '0'],
                            stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
                    try:
                        diagnostic = self.assert_safe_failure(child, root,
                            'startup_failed' if entry == 'serve' else 'linux_launcher_failed', 'reopen', 'timeout')
                        self.assertIsNone(diagnostic['errno'])
                        with self.assertRaises(OwnedRoot):
                            contender = RootLock(root)
                            contender.close()
                        self.assertEqual(path.stat().st_ino, inode)
                        self.assertEqual(path.read_bytes(), b'')
                    finally:
                        self.cleanup_child(child, root)
                finally:
                    lock.close()
                replacement = RootLock(root)
                replacement.close()

    def test_failed_channel_start_preserves_handoff_and_releases_listeners_and_lock(self):
        from root_lock import RootLock
        with tempfile.TemporaryDirectory(prefix='relay-partial-start-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            root.mkdir(mode=0o700)
            handoff = root / 'private-access' / ('a' * 32) / 'launch.txt'
            handoff.parent.mkdir(parents=True, mode=0o700)
            handoff.write_bytes(b'synthetic existing private handoff, not a capability\n')
            before = handoff.stat()
            held = self.channel().InstanceChannel(root, lambda operation: {'ok': True, 'fixture': 'original'})
            cleanup.callback(held.close)
            reservations = [socket.socket(), socket.socket()]
            try:
                for reserved in reservations:
                    reserved.bind(('127.0.0.1', 0))
                ports = [reserved.getsockname()[1] for reserved in reservations]
            finally:
                for reserved in reservations:
                    reserved.close()
            child = self.launch(root, *ports)
            cleanup.callback(self.cleanup_child, child, root)
            self.assert_safe_failure(child, root, 'startup_failed', 'channel', 'os_error')
            self.assertEqual(handoff.read_bytes(), b'synthetic existing private handoff, not a capability\n')
            self.assertEqual((handoff.stat().st_ino, handoff.stat().st_mtime_ns),
                (before.st_ino, before.st_mtime_ns))
            self.assertEqual(list(handoff.parent.iterdir()), [handoff])
            lock = RootLock(root)
            lock.close()
            for port in ports:
                with socket.socket() as rebound:
                    rebound.bind(('127.0.0.1', port))
            self.assertEqual(self.channel().request(root, 'probe'), {'ok': True, 'fixture': 'original'})
            held.close()
            with self.assertRaises((OSError, TimeoutError)):
                self.channel().request(root, 'probe', timeout=.15)

    def test_simultaneous_supported_starts_share_one_owned_host(self):
        import concurrent.futures
        import hashlib
        with tempfile.TemporaryDirectory(prefix='relay-racing-start-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            command = ['/bin/sh', str(ROOT / 'start-workbench.sh'), 'start',
                '--root', str(root), '--port', '0', '--preview-port', '0']
            cleanup.callback(self.shutdown_and_wait, root)
            barrier = threading.Barrier(2)
            def start(_):
                barrier.wait(timeout=5)
                return subprocess.run(command, cwd='/', capture_output=True, text=True, timeout=25)
            with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                results = list(pool.map(start, range(2)))
            for result in results:
                self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
                self.assertNotIn('bootstrap', result.stdout + result.stderr)
            replies = [json.loads(result.stdout) for result in results]
            info = self.channel().request(root, 'probe')
            self.assertEqual([reply['pid'] for reply in replies], [info['pid'], info['pid']])
            self.assertEqual(sum(reply.get('startCommandAccepted') is True for reply in replies), 1)
            self.assertTrue(info['workers'])
            self.assertEqual(info['executionMode'], 'blocked')
            unit = 'relay-console-' + hashlib.sha256((str(os.getuid()) + '|' + str(root.resolve())).encode()).hexdigest()[:24]
            manager = '--system' if os.getuid() == 0 else '--user'
            state = subprocess.run(['/usr/bin/systemctl', manager, 'show', unit,
                '--property=MainPID,InvocationID,ControlGroup,Type,KillMode'], capture_output=True, text=True, timeout=5)
            self.assertEqual(state.returncode, 0, state.stderr)
            identity = dict(line.split('=', 1) for line in state.stdout.splitlines() if '=' in line)
            self.assertEqual(identity['MainPID'], str(info['pid']))
            self.assertRegex(identity['InvocationID'], r'^[a-f0-9]{32}$')
            self.assertEqual(identity['Type'], 'exec')
            self.assertEqual(identity['KillMode'], 'control-group')
            access = self.channel().request(root, 'access')
            status, session = self.http(info['origin'], '/api/session', {'bootstrap': access['url'].split('#bootstrap=')[1]})
            self.assertEqual(status, 200)
            status, actual = self.http(info['origin'], '/api/state', token=session['token'])
            self.assertEqual(status, 200)
            self.assertEqual(actual['projects'], [])
            self.assertEqual(actual['executionMode'], 'blocked')
            self.shutdown_and_wait(root)
            deadline = time.monotonic() + 5
            group = Path('/sys/fs/cgroup') / identity['ControlGroup'].lstrip('/')
            while group.exists() and time.monotonic() < deadline:
                time.sleep(.03)
            self.assertFalse(group.exists(), 'Collected fixture unit must release its cgroup')
            from root_lock import RootLock
            lock = RootLock(root)
            lock.close()
            with self.assertRaises((OSError, TimeoutError)):
                self.channel().request(root, 'probe', timeout=.15)

    def test_instance_refuses_unknown_operation(self):
        with tempfile.TemporaryDirectory() as root:
            with self.assertRaises(ValueError):
                self.channel().request(root, 'run-shell')

    @unittest.skipUnless(hasattr(os, 'getuid') and os.getuid() == 0, 'Actual foreign-UID probe requires root')
    def test_instance_rejects_different_uid(self):
        with tempfile.TemporaryDirectory(prefix='relay-uid-') as directory, ExitStack() as cleanup:
            root = Path(directory) / 'runtime'
            child = self.launch(root)
            cleanup.callback(self.cleanup_child, child, root)
            self.ready(child, root)
            address = self.channel().address(root)
            code = ('import os,socket;'
                'os.setgroups([]);os.setgid(65534);os.setuid(65534);'
                's=socket.socket(socket.AF_UNIX);s.settimeout(2);'
                's.connect(bytes.fromhex(__import__("sys").argv[1]));'
                '\ntry:\n s.sendall(b"access\\n");data=s.recv(8192)\nexcept OSError:\n data=b""'
                '\nassert b"bootstrap" not in data and b"url" not in data')
            result = subprocess.run([sys.executable, '-I', '-c', code, address.hex()],
                capture_output=True, timeout=5)
            self.assertEqual(result.returncode, 0, result.stderr.decode())
            self.assertEqual(self.channel().request(root, 'probe')['pid'], child.pid)


if __name__ == '__main__':
    unittest.main()
