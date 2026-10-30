"""Real local control operations with narrowly injected handler/kernel failures."""
from contextlib import ExitStack
import errno
import json
from pathlib import Path
import socket
import subprocess
import sys
import tempfile
import time
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


@unittest.skipUnless(sys.platform == 'linux', 'Native Linux same-UID local channel')
class LinuxInstanceRecoveryTests(unittest.TestCase):
    def test_probe_reuses_owner_without_transferring_capability(self):
        from linux_instance import InstanceChannel, request
        with tempfile.TemporaryDirectory(prefix='relay-channel-owner-') as directory, ExitStack() as cleanup:
            operations = []
            def handle(operation):
                operations.append(operation)
                return {'ok': True, 'pid': 12345}
            channel = InstanceChannel(directory, handle)
            cleanup.callback(channel.close)
            for _ in range(2):
                reply = request(directory, 'probe')
                self.assertEqual(reply, {'ok': True, 'pid': 12345})
            self.assertEqual(operations, ['probe', 'probe'])
            self.assertEqual(list(Path(directory).iterdir()), [])

    def test_fixed_protocol_denies_raw_unknown_request_and_duplicate_owner(self):
        from linux_instance import InstanceChannel, address, request
        with tempfile.TemporaryDirectory(prefix='relay-channel-fixed-') as directory, ExitStack() as cleanup:
            operations = []
            def handle(operation):
                operations.append(operation)
                return {'ok': True}
            channel = InstanceChannel(directory, handle)
            cleanup.callback(channel.close)
            with self.assertRaises(ValueError):
                request(directory, 'execute arbitrary command')
            with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as raw:
                raw.settimeout(2)
                raw.connect(address(directory))
                raw.sendall(b'execute arbitrary command\n')
                self.assertEqual(json.loads(raw.recv(8192)),
                    {'ok': False, 'error': 'Local control operation denied'})
            self.assertEqual(operations, [])
            with self.assertRaises(OSError):
                duplicate = InstanceChannel(directory, handle)
                cleanup.callback(duplicate.close)
            self.assertEqual(request(directory, 'probe'), {'ok': True})
            self.assertEqual(operations, ['probe'])

    def test_closed_or_other_scope_denies_then_allows_clean_rebind(self):
        from linux_instance import InstanceChannel, request
        with tempfile.TemporaryDirectory(prefix='relay-channel-scope-') as directory, ExitStack() as cleanup:
            channel = InstanceChannel(directory, lambda operation: {'ok': True})
            cleanup.callback(channel.close)
            with self.assertRaises((OSError, TimeoutError)):
                request(Path(directory) / 'other', 'probe', timeout=.15)
            self.assertEqual(request(directory, 'probe'), {'ok': True})
            channel.close()
            with self.assertRaises((OSError, TimeoutError)):
                request(directory, 'probe', timeout=.15)
            replacement = InstanceChannel(directory, lambda operation: {'ok': True, 'replacement': True})
            cleanup.callback(replacement.close)
            self.assertEqual(request(directory, 'probe'), {'ok': True, 'replacement': True})

    def test_handler_failure_denies_one_request_without_losing_normal_stop(self):
        from linux_instance import InstanceChannel, request
        with tempfile.TemporaryDirectory(prefix='relay-channel-recovery-') as directory, ExitStack() as cleanup:
            operations = []
            def handle(operation):
                operations.append(operation)
                if operation == 'access':
                    raise KeyError('synthetic-private-value-must-not-be-returned')
                return {'ok': True}
            channel = InstanceChannel(directory, handle)
            cleanup.callback(channel.close)
            with self.assertRaises(ValueError) as caught:
                request(directory, 'access', timeout=.5)
            self.assertNotIn('synthetic-private-value', str(caught.exception))
            self.assertEqual(request(directory, 'probe', timeout=.5), {'ok': True})
            self.assertEqual(request(directory, 'shutdown', timeout=.5), {'ok': True})
            self.assertEqual(operations, ['access', 'probe', 'shutdown'])

    def test_transient_accept_failure_recovers_actual_channel(self):
        from linux_instance import InstanceChannel, request
        accept = socket.socket.accept
        failures = []
        def transient(connection):
            if not failures:
                failures.append(True)
                raise OSError(errno.EMFILE, 'synthetic descriptor pressure')
            return accept(connection)
        with tempfile.TemporaryDirectory(prefix='relay-channel-accept-') as directory, ExitStack() as cleanup:
            with patch.object(socket.socket, 'accept', transient):
                channel = InstanceChannel(directory, lambda operation: {'ok': True})
                cleanup.callback(channel.close)
                self.assertEqual(request(directory, 'probe', timeout=1), {'ok': True})
                self.assertEqual(request(directory, 'shutdown', timeout=1), {'ok': True})
            self.assertTrue(failures)

    def test_dead_channel_stops_host_instead_of_leaving_http_and_lock_alive(self):
        from root_lock import RootLock
        with tempfile.TemporaryDirectory(prefix='relay-channel-death-') as directory:
            root = Path(directory) / 'runtime'
            code = ('import sys;sys.path.insert(0,sys.argv[1]);'
                'import linux_instance;linux_instance.InstanceChannel._serve=lambda self:None;'
                'import linux_entry;sys.argv=["linux_entry","serve","--root",sys.argv[2],"--port","0","--preview-port","0"];'
                'sys.exit(linux_entry.main())')
            child = subprocess.Popen([sys.executable, '-I', '-B', '-c', code, str(ROOT / 'p1'), str(root)],
                stdout=subprocess.PIPE, stderr=subprocess.PIPE, text=True)
            try:
                try:
                    stdout, stderr = child.communicate(timeout=3)
                except subprocess.TimeoutExpired:
                    self.fail('Control channel died but HTTP host still retains ownership')
                self.assertNotEqual(child.returncode, 0)
                self.assertIn('startup_failed', stderr)
                self.assertNotIn('Traceback', stderr)
                self.assertNotIn('bootstrap=', stdout + stderr)
                lock = RootLock(root)
                lock.close()
            finally:
                if child.poll() is None:
                    child.terminate()
                    try:
                        child.communicate(timeout=5)
                    except subprocess.TimeoutExpired:
                        child.kill()
                        child.communicate(timeout=5)


if __name__ == '__main__':
    unittest.main()
