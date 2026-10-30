"""Actual Windows CMD launcher checks, no models and no existing service restart."""
import http.client
import json
import os
from pathlib import Path
import socket
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from winjob import JobProcess


class StartupTests(unittest.TestCase):
    def launch(self, directory, *arguments):
        return JobProcess([os.environ.get('COMSPEC', 'C:/Windows/System32/cmd.exe'), '/d', '/c', 'call', str(ROOT / 'start-workbench.cmd'), *arguments], directory)

    def test_launcher_starts_from_unrelated_directory_and_owns_process_tree(self):
        with tempfile.TemporaryDirectory(prefix='relay-launch-') as directory:
            ready = Path(directory) / 'ready.json'
            child = self.launch(directory, '--no-browser', '--no-workers', '--port', '0', '--root', 'runtime', '--ready-file', 'ready.json')
            try:
                deadline = time.monotonic() + 15
                while not ready.exists() and time.monotonic() < deadline and child.poll() is None:
                    time.sleep(.05)
                if not ready.exists():
                    result = child.wait(1)
                    self.fail('CMD must locate server/Python from any working directory: ' + result['stdout'] + result['stderr'])
                launch = json.loads(ready.read_text(encoding='utf-8'))
                # Never print or persist the bootstrap URL in public test output.
                connection = http.client.HTTPConnection(launch['origin'].removeprefix('http://'))
                connection.request('GET', '/')
                response = connection.getresponse()
                self.assertEqual(response.status, 200)
                self.assertIn(b'live.js', response.read())
                connection.close()
                self.assertTrue(child.stop())
                self.assertTrue(child.tree_stopped())
            finally:
                child.close()
                ready.write_text('{"stopped":true}', encoding='utf-8')

    def test_windowless_bootstrap_reopens_owned_instance_and_exits_cleanly(self):
        self.assertTrue((ROOT / 'p1/bootstrap.py').exists(), 'Double-click needs a windowless owned host')
        import http.client
        from instance import request
        with tempfile.TemporaryDirectory(prefix='接力 startup ') as directory:
            root = Path(directory) / 'userdata'
            ready = Path(directory) / 'ready.json'
            pythonw = Path(sys.executable).with_name('pythonw.exe')
            child = JobProcess([str(pythonw), '-I', '-X', 'utf8', str(ROOT / 'p1/bootstrap.py'),
                '--root', str(root), '--no-workers', '--no-browser', '--ready-file', str(ready)], directory)
            try:
                deadline = time.monotonic() + 15
                while not ready.exists() and time.monotonic() < deadline and child.poll() is None:
                    time.sleep(.05)
                self.assertTrue(ready.exists(), 'Owned pythonw host must become ready')
                launch = json.loads(ready.read_text(encoding='utf-8'))
                self.assertEqual(request(root, 'probe'), {'ok': True})
                second = JobProcess([str(pythonw), '-I', '-X', 'utf8', str(ROOT / 'p1/bootstrap.py'),
                    '--root', str(root), '--no-workers', '--no-browser'], directory)
                try:
                    duplicate = second.wait(8)
                finally:
                    second.close()
                self.assertEqual(duplicate['exitCode'], 0)
                self.assertIsNone(child.poll(), 'Reopen must not restart or kill the owner')
                port = int(launch['origin'].rsplit(':', 1)[1])
                connection = http.client.HTTPConnection('127.0.0.1', port)
                connection.request('POST', '/api/session', json.dumps({'bootstrap': launch['url'].split('bootstrap=')[1]}),
                    {'Origin': launch['origin'], 'Content-Type': 'application/json'})
                response = connection.getresponse()
                token = json.loads(response.read())['token']
                connection.close()
                connection = http.client.HTTPConnection('127.0.0.1', port)
                connection.request('POST', '/api/shutdown', '{}', {'Origin': launch['origin'],
                    'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token})
                response = connection.getresponse()
                self.assertEqual(response.status, 200)
                response.read(); connection.close()
                result = child.wait(12)
                self.assertEqual(result['exitCode'], 0)
                self.assertTrue(result['treeStopped'])
            finally:
                child.close()
                if ready.exists():
                    ready.write_text('{"stopped":true}', encoding='utf-8')

    def test_detached_launcher_returns_while_owned_host_keeps_serving(self):
        with tempfile.TemporaryDirectory(prefix='relay-detached-') as directory:
            ready = Path(directory) / 'ready.json'
            child = self.launch(directory, '--background', '--no-browser', '--no-workers', '--root', 'userdata', '--ready-file', 'ready.json')
            try:
                deadline = time.monotonic() + 15
                while not ready.exists() and time.monotonic() < deadline and child.poll() is None:
                    time.sleep(.05)
                # The CMD process may exit before pythonw has finished importing.
                while not ready.exists() and time.monotonic() < deadline:
                    time.sleep(.05)
                self.assertTrue(ready.exists(), 'Detached launcher must use the owned windowless host')
                self.assertEqual(child.poll(), 0, 'CMD must return without waiting for the service')
                launch = json.loads(ready.read_text(encoding='utf-8'))
                connection = http.client.HTTPConnection(launch['origin'].removeprefix('http://'), timeout=5)
                try:
                    connection.request('POST', '/api/session', json.dumps({'bootstrap': launch['url'].split('bootstrap=')[1]}),
                        {'Origin': launch['origin'], 'Content-Type': 'application/json'})
                    response = connection.getresponse()
                    token = json.loads(response.read())['token']
                    connection.request('POST', '/api/shutdown', '{}', {'Origin': launch['origin'],
                        'Content-Type': 'application/json', 'Authorization': 'Bearer ' + token})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    response.read()
                finally:
                    connection.close()
                result = child.wait(12)
                self.assertFalse(result['timedOut'])
                self.assertTrue(result['treeStopped'])
            finally:
                child.close()
                if ready.exists():
                    ready.write_text('{"stopped":true}', encoding='utf-8')

    def test_startup_failure_leaves_bounded_safe_diagnostic(self):
        with tempfile.TemporaryDirectory(prefix='relay-error-') as directory:
            with socket.socket() as occupied:
                occupied.bind(('127.0.0.1', 0)); occupied.listen()
                child = JobProcess([sys.executable, '-I', '-X', 'utf8', str(ROOT / 'p1/bootstrap.py'),
                    '--root', str(Path(directory) / 'userdata'), '--no-browser', '--no-workers',
                    '--port', str(occupied.getsockname()[1])], directory)
                try:
                    result = child.wait(10)
                finally:
                    child.close()
                self.assertNotEqual(result['exitCode'], 0)
                diagnostic = Path(directory) / 'userdata/startup-error.json'
                self.assertTrue(diagnostic.is_file(), 'pythonw errors need a visible diagnostic, not print')
                raw = diagnostic.read_text(encoding='utf-8')
                self.assertLess(len(raw), 2048)
                self.assertNotIn('bootstrap=', raw)
                self.assertNotIn('Traceback', raw)
                diagnostic = json.loads(raw)
                self.assertEqual(diagnostic['code'], 'startup_failed')
                self.assertEqual(diagnostic.get('stage'), 'initialize')
                self.assertEqual(diagnostic['category'], 'os_error')
                self.assertIsInstance(diagnostic['winerror'], int)

    def test_reopen_timeout_keeps_owner_lock_and_reports_only_safe_stage(self):
        from server import OwnedRoot, RootLock
        with tempfile.TemporaryDirectory(prefix='relay-reopen-diagnostic-') as directory:
            root = Path(directory) / 'userdata'
            owner = RootLock(root)
            try:
                child = JobProcess([sys.executable, '-I', '-X', 'utf8', str(ROOT / 'p1/bootstrap.py'),
                    '--root', str(root), '--no-browser', '--no-workers'], directory)
                try:
                    result = child.wait(12)
                finally:
                    child.close()
                self.assertEqual(result['exitCode'], 1)
                self.assertTrue(result['treeStopped'])
                self.assertFalse(result['timedOut'])
                raw = (root / 'startup-error.json').read_text(encoding='utf-8')
                diagnostic = json.loads(raw)
                self.assertEqual(diagnostic.get('stage'), 'reopen')
                self.assertEqual(diagnostic['category'], 'timeout')
                self.assertIsNone(diagnostic['errno'])
                self.assertIsNone(diagnostic['winerror'])
                self.assertEqual(set(diagnostic), {'code', 'message', 'stage', 'category', 'errno', 'winerror'})
                self.assertLess(len(raw.encode('utf-8')), 2048)
                self.assertNotIn(str(root), raw)
                self.assertNotIn('Traceback', raw)
                self.assertNotIn('No matching owned', raw)
                with self.assertRaises(OwnedRoot):
                    RootLock(root)
            finally:
                owner.close()

    def test_empty_lock_contender_recognizes_owner_without_writing_lock_bytes(self):
        import msvcrt
        from server import RootLock
        with tempfile.TemporaryDirectory(prefix='relay-lock-initialize-') as directory:
            directory = Path(directory)
            root = directory / 'userdata'
            root.mkdir()
            script = directory / 'contender.py'
            script.write_text('''from pathlib import Path
import json
import sys
sys.path.insert(0, sys.argv[1])
from server import RootLock, OwnedRoot
try:
    lock = RootLock(Path(sys.argv[2]))
except Exception as error:
    print(json.dumps({'recognizedAsOwnedRoot': isinstance(error, OwnedRoot),
        'exception': type(error).__name__, 'errno': getattr(error, 'errno', None)}))
else:
    lock.close()
    raise SystemExit('Unexpected duplicate owner')
''', encoding='utf-8')
            with (root / 'service.lock').open('a+b') as owner:
                # Windows byte locks can own a region beyond EOF; file contents are not ownership.
                msvcrt.locking(owner.fileno(), msvcrt.LK_NBLCK, 1)
                try:
                    child = JobProcess([sys.executable, '-I', '-X', 'utf8', str(script),
                        str(ROOT / 'p1'), str(root)], directory)
                    try:
                        result = child.wait(12)
                    finally:
                        child.close()
                    self.assertEqual(result['exitCode'], 0, result['stderr'])
                    self.assertTrue(result['treeStopped'])
                    self.assertFalse(result['timedOut'])
                    record = json.loads(result['stdout'])
                    self.assertTrue(record['recognizedAsOwnedRoot'], record)
                    self.assertEqual((root / 'service.lock').stat().st_size, 0)
                finally:
                    owner.seek(0)
                    msvcrt.locking(owner.fileno(), msvcrt.LK_UNLCK, 1)
            replacement = RootLock(root)
            replacement.close()

    def test_simultaneous_windowless_launches_leave_only_one_owned_host(self):
        import concurrent.futures
        import threading
        from instance import request
        with tempfile.TemporaryDirectory(prefix='接力 simultaneous ') as directory:
            root = Path(directory) / 'userdata'
            ready = [Path(directory) / f'ready-{index}.json' for index in range(2)]
            barrier = threading.Barrier(2)
            def start(index):
                barrier.wait(timeout=5)
                return JobProcess([str(Path(sys.executable).with_name('pythonw.exe')), '-I', '-X', 'utf8',
                    str(ROOT / 'p1/bootstrap.py'), '--root', str(root), '--no-browser', '--ready-file', str(ready[index])], directory)
            children = []
            try:
                with concurrent.futures.ThreadPoolExecutor(max_workers=2) as pool:
                    children = list(pool.map(start, range(2)))
                deadline = time.monotonic() + 15
                while time.monotonic() < deadline:
                    owners = [index for index, path in enumerate(ready) if path.is_file()]
                    if len(owners) == 1 and children[1 - owners[0]].poll() is not None:
                        break
                    time.sleep(.05)
                self.assertEqual(len(owners), 1, 'Simultaneous launches must publish only one owned HTTP host')
                owner = owners[0]
                contender_exit = children[1 - owner].poll()
                diagnostic = {}
                if contender_exit != 0 and (root / 'startup-error.json').is_file():
                    saved = json.loads((root / 'startup-error.json').read_text(encoding='utf-8'))
                    diagnostic = {key: saved.get(key) for key in ('code', 'stage', 'category', 'errno', 'winerror')}
                self.assertEqual(contender_exit, 0, json.dumps(diagnostic))
                self.assertIsNone(children[owner].poll())
                self.assertEqual(request(root, 'probe'), {'ok': True})
                launch = json.loads(ready[owner].read_text(encoding='utf-8'))
                connection = http.client.HTTPConnection(launch['origin'].removeprefix('http://'), timeout=5)
                try:
                    headers = {'Origin': launch['origin'], 'Content-Type': 'application/json'}
                    connection.request('POST', '/api/session', json.dumps({'bootstrap': launch['url'].split('bootstrap=')[1]}), headers)
                    response = connection.getresponse()
                    token = json.loads(response.read())['token']
                    headers['Authorization'] = 'Bearer ' + token
                    connection.request('GET', '/api/state', headers=headers)
                    response = connection.getresponse()
                    state = json.loads(response.read())
                    self.assertEqual(state['projects'], [])
                    self.assertTrue(state['workers'], 'Actual empty Engine starts, without any queued model work')
                    connection.request('POST', '/api/shutdown', '{}', headers)
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    response.read()
                finally:
                    connection.close()
                for child in children:
                    result = child.wait(12)
                    self.assertEqual(result['exitCode'], 0)
                    self.assertTrue(result['treeStopped'])
            finally:
                for child in children:
                    child.close()
                for path in ready:
                    if path.exists():
                        path.write_text('{"stopped":true}', encoding='utf-8')

    def test_existing_ready_file_is_preserved_and_failed_host_releases_ownership(self):
        from server import RootLock
        from instance import request
        with tempfile.TemporaryDirectory(prefix='接力 ready failure ') as directory:
            root = Path(directory) / 'userdata'
            ready = Path(directory) / 'ready.json'
            ready.write_text('synthetic existing handoff must not be overwritten', encoding='utf-8')
            child = JobProcess([sys.executable, '-I', '-X', 'utf8', str(ROOT / 'p1/bootstrap.py'),
                '--root', str(root), '--no-workers', '--no-browser', '--ready-file', str(ready)], directory)
            try:
                result = child.wait(12)
            finally:
                child.close()
            self.assertNotEqual(result['exitCode'], 0)
            self.assertFalse(result['timedOut'])
            self.assertTrue(result['treeStopped'])
            self.assertEqual(ready.read_text(encoding='utf-8'), 'synthetic existing handoff must not be overwritten')
            lock = RootLock(root)
            lock.close()
            with self.assertRaises((OSError, TimeoutError)):
                request(root, 'probe', timeout=.15)
            self.assertNotIn('bootstrap=', (root / 'startup-error.json').read_text(encoding='utf-8'))

    def test_launcher_returns_nonzero_on_occupied_port(self):
        with tempfile.TemporaryDirectory(prefix='relay-launch-denied-') as directory:
            with socket.socket() as occupied:
                occupied.bind(('127.0.0.1', 0))
                occupied.listen()
                child = self.launch(directory, '--no-browser', '--no-workers', '--port', str(occupied.getsockname()[1]), '--root', 'runtime')
                try:
                    result = child.wait(15)
                finally:
                    child.close()
                self.assertFalse(result['timedOut'], 'A scripted startup failure must not pause indefinitely')
                self.assertTrue(result['treeStopped'])
                self.assertNotEqual(result['exitCode'], 0, 'The CMD launcher must preserve the failed server exit code')


if __name__ == '__main__':
    unittest.main()
