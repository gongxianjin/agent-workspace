"""Real Linux HTTP/locking checks; never invokes a model or simulates readiness."""
import http.client
import importlib
import json
import os
from pathlib import Path
import subprocess
from unittest.mock import patch
import sys
import tempfile
import threading
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


class RootLockTests(unittest.TestCase):
    def test_kernel_lock_excludes_second_process_and_releases(self):
        try:
            from root_lock import RootLock
        except ImportError as error:
            self.fail(f'Cross-platform root ownership is unavailable: {error}')
        with tempfile.TemporaryDirectory(prefix='relay-lock-') as directory:
            script = ('import sys;sys.path.insert(0,sys.argv[1]);'
                      'from root_lock import RootLock,OwnedRoot;'
                      '\ntry:\n lock=RootLock(sys.argv[2]);lock.close()'
                      '\nexcept OwnedRoot:\n sys.exit(73)')
            lock = RootLock(directory)
            try:
                result = subprocess.run([sys.executable, '-I', '-B', '-c', script,
                    str(ROOT / 'p1'), directory], capture_output=True, timeout=10)
                self.assertEqual(result.returncode, 73, result.stderr.decode())
            finally:
                lock.close()
            result = subprocess.run([sys.executable, '-I', '-B', '-c', script,
                str(ROOT / 'p1'), directory], capture_output=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr.decode())

    @unittest.skipUnless(sys.platform == 'linux', 'Native empty-file flock contract')
    def test_empty_lock_contender_preserves_bytes_and_owner(self):
        from root_lock import RootLock
        with tempfile.TemporaryDirectory(prefix='relay-empty-lock-') as directory:
            root = Path(directory)
            lock = RootLock(root)
            path = root / 'service.lock'
            inode = path.stat().st_ino
            script = ('import sys;sys.path.insert(0,sys.argv[1]);'
                'from root_lock import RootLock,OwnedRoot;'
                '\ntry:\n lock=RootLock(sys.argv[2])'
                '\nexcept OwnedRoot:\n sys.exit(73)'
                '\nelse:\n lock.close();sys.exit(99)')
            try:
                self.assertEqual(path.read_bytes(), b'')
                for _ in range(2):
                    result = subprocess.run([sys.executable, '-I', '-B', '-c', script,
                        str(ROOT / 'p1'), directory], capture_output=True, timeout=5)
                    self.assertEqual(result.returncode, 73, result.stderr.decode())
                    self.assertEqual(path.stat().st_ino, inode)
                    self.assertEqual(path.read_bytes(), b'')
            finally:
                lock.close()
            replacement = RootLock(root)
            replacement.close()
            self.assertEqual(path.stat().st_ino, inode)
            self.assertEqual(path.read_bytes(), b'')

    @unittest.skipUnless(sys.platform == 'linux', 'Private Linux runtime root')
    def test_publicly_readable_root_is_rejected_before_initialization(self):
        from root_lock import RootLock
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'runtime'
            root.mkdir(mode=0o755)
            root.chmod(0o755)
            with self.assertRaises(ValueError):
                RootLock(root)
            self.assertEqual(list(root.iterdir()), [], 'Reject before writing any service or project state')
            root.chmod(0o700)
            lock = RootLock(root)
            lock.close()

    @unittest.skipUnless(sys.platform == 'linux', 'POSIX filesystem ownership')
    def test_lock_rejects_symlink_without_touching_target(self):
        try:
            from root_lock import RootLock
        except ImportError as error:
            self.fail(f'Cross-platform root ownership is unavailable: {error}')
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory) / 'runtime'
            root.mkdir()
            outside = Path(directory) / 'outside'
            outside.write_bytes(b'untouched')
            (root / 'service.lock').symlink_to(outside)
            with self.assertRaises((ValueError, OSError)):
                RootLock(root)
            self.assertEqual(outside.read_bytes(), b'untouched')


@unittest.skipUnless(sys.platform == 'linux', 'Native Linux control console')
class LinuxConsoleTests(unittest.TestCase):
    def setUp(self):
        try:
            self.server = importlib.import_module('server')
        except ImportError as error:
            self.fail(f'The control server cannot import on Linux: {error}')
        self.temp = tempfile.TemporaryDirectory(prefix='relay-console-')
        self.addCleanup(self.temp.cleanup)
        self.app = self.server.Application(Path(self.temp.name), port=0, preview_port=0)
        self.thread = threading.Thread(target=self.app.serve)
        self.thread.start()
        self.addCleanup(self.close_app)
        self.token = None

    def close_app(self):
        self.app.close()
        self.thread.join(5)
        self.assertFalse(self.thread.is_alive())

    def request(self, path, body=None, *, token=True, origin=None, host=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.app.port, timeout=8)
        headers = {'Origin': self.app.origin if origin is None else origin}
        if token and self.token:
            headers['Authorization'] = 'Bearer ' + self.token
        if host is not None:
            headers['Host'] = host
        raw = None
        if body is not None:
            raw = json.dumps(body).encode()
            headers['Content-Type'] = 'application/json'
        try:
            connection.request('POST' if body is not None else 'GET', path, raw, headers)
            response = connection.getresponse()
            return response.status, dict(response.getheaders()), response.read()
        finally:
            connection.close()

    def login(self):
        status, _, raw = self.request('/api/session', {'bootstrap': self.app.issue_bootstrap()})
        self.assertEqual(status, 200)
        self.token = json.loads(raw)['token']

    def test_real_http_auth_persistence_and_separate_preview(self):
        self.assertIsNone(self.app.engine, 'Unverified Linux execution must not start an engine')
        status, headers, raw = self.request('/', token=False)
        self.assertEqual(status, 200)
        self.assertIn(b'<html', raw.lower())
        self.assertEqual(self.request('/api/state', token=False)[0], 403)
        self.assertEqual(self.request('/api/state', host='attacker.example')[0], 403)
        self.assertEqual(self.request('/api/state', origin='http://attacker.example')[0], 403)
        bootstrap = self.app.issue_bootstrap()
        status, _, raw = self.request('/api/session', {'bootstrap': bootstrap})
        self.assertEqual(status, 200)
        self.token = json.loads(raw)['token']
        self.assertEqual(self.request('/api/session', {'bootstrap': bootstrap})[0], 403)
        status, _, raw = self.request('/api/projects', {'name': 'Linux实际项目'})
        self.assertEqual(status, 200)
        project = json.loads(raw)
        status, _, raw = self.request('/api/state')
        state = json.loads(raw)
        self.assertEqual(status, 200)
        self.assertEqual(state['platform'], 'linux-x64')
        self.assertEqual(state['executionMode'], 'blocked')
        self.assertFalse(state['workers'])
        self.assertEqual(state['projects'][0]['name'], 'Linux实际项目')
        self.assertNotEqual(self.app.port, self.app.preview_port)
        product = self.app.store.root / 'projects' / project['id'] / 'product/index.html'
        product.write_text('<!doctype html><p id="probe">native Linux preview</p>', encoding='utf-8')
        connection = http.client.HTTPConnection('127.0.0.1', self.app.preview_port, timeout=8)
        try:
            connection.request('GET', '/' + project['id'] + '/index.html')
            response = connection.getresponse()
            self.assertEqual(response.status, 200)
            self.assertIn('sandbox allow-scripts', response.getheader('Content-Security-Policy'))
            self.assertIn(b'native Linux preview', response.read())
        finally:
            connection.close()
        self.app.request_shutdown()
        self.assertTrue(self.app.shutdown_requested.is_set())

    def test_unavailable_cgroup_discovery_preserves_authenticated_blocked_console(self):
        # Break caught: host discovery aborts console startup before selecting an owned unit.
        from process_control import JobProcess, ExecutionUnavailable
        from root_lock import RootLock
        for error in (ValueError('Unified owned cgroup required'), OSError('Fixture proc metadata unavailable')):
            with self.subTest(error=type(error).__name__), tempfile.TemporaryDirectory() as directory:
                root = Path(directory) / 'runtime'
                with patch('linux_process_entry.current_group', side_effect=error):
                    app = self.server.Application(root, port=0, preview_port=0)
                thread = threading.Thread(target=app.serve)
                thread.start()
                connection = http.client.HTTPConnection('127.0.0.1', app.port, timeout=5)
                try:
                    connection.request('POST', '/api/session', json.dumps({'bootstrap': app.issue_bootstrap()}),
                        {'Origin': app.origin, 'Content-Type': 'application/json'})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    token = json.loads(response.read())['token']
                    connection.request('GET', '/api/state', headers={
                        'Origin': app.origin, 'Authorization': 'Bearer ' + token})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    state = json.loads(response.read())
                    self.assertFalse(state['workers'])
                    self.assertEqual(state['executionMode'], 'blocked')
                    self.assertIsNone(app.engine)
                    self.assertIsNone(app.process_context)
                    with self.assertRaises(ExecutionUnavailable):
                        JobProcess([sys.executable, '-I', '-c', 'raise SystemExit(99)'], root)
                    self.assertFalse((root / 'process-ownership').exists())
                finally:
                    connection.close()
                    app.close()
                    thread.join(5)
                self.assertFalse(thread.is_alive())
                lock = RootLock(root)
                lock.close()

    def test_state_platform_does_not_depend_on_setup_implementation(self):
        self.login()
        class CachedSetup:
            def public(self):
                return {'canExecute': False}
        self.app.setup = CachedSetup()
        status, _, raw = self.request('/api/state')
        self.assertEqual(status, 200)
        self.assertEqual(json.loads(raw)['platform'], 'linux-x64')

    def test_linux_setup_cannot_confirm_or_dispatch_unverified_execution(self):
        self.login()
        status, _, raw = self.request('/api/setup')
        setup = json.loads(raw)
        self.assertEqual(status, 200)
        self.assertFalse(setup['canExecute'])
        self.assertEqual(setup['platform'], 'linux-x64')
        self.assertIn('execution', setup['blockers'])
        self.assertEqual(self.request('/api/setup/confirm-existing', {'confirmed': True})[0], 400)
        self.assertEqual(self.request('/api/setup/browser', {'confirmed': True})[0], 409)
        _, _, raw = self.request('/api/projects', {'name': '安全门禁'})
        project = json.loads(raw)
        response = self.request('/api/command', {'projectId': project['id'], 'commandId': 'no-model',
            'expectedRevision': project['revision'], 'kind': 'generate', 'payload': {}})
        self.assertEqual(response[0], 409)
        current = self.app.store.get(project['id'])
        self.assertEqual(current['revision'], project['revision'])
        self.assertIsNone(current['activeAttempt'])
        self.assertFalse((ROOT / 'userdata').exists(), 'Setup must not write in the installation root')


if __name__ == '__main__':
    unittest.main()
