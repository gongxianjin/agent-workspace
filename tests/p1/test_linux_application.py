"""Actual Application ownership in a native systemd host; no model/assets/downloads."""
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
from unittest.mock import patch
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


def run_application_case(directory, case):
    from linux_process import active_context
    from process_control import JobProcess
    from root_lock import RootLock, OwnedRoot
    from server import Application
    runtime = directory / 'runtime'
    app = None
    report = {}

    def context_absent():
        try:
            active_context()
        except ValueError:
            return True
        return False

    def call_http(path, body=None, token=None):
        connection = http.client.HTTPConnection('127.0.0.1', app.port, timeout=5)
        headers = {'Origin': app.origin, 'Content-Type': 'application/json'}
        if token:
            headers['Authorization'] = 'Bearer ' + token
        try:
            connection.request('GET' if body is None else 'POST', path,
                None if body is None else json.dumps(body), headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    try:
        if case == 'application':
            app = Application(runtime, port=0, preview_port=0)
            thread = threading.Thread(target=app.serve)
            thread.start()

            report['unauthorized'] = call_http('/api/state')[0]
            _, session = call_http('/api/session', {'bootstrap': app.issue_bootstrap()})
            token = session['token']
            _, state = call_http('/api/state', token=token)
            report.update(workers=state['workers'], mode=state['executionMode'],
                engineAlive=bool(app.engine and app.engine.thread.is_alive()),
                canExecute=app.setup.public()['canExecute'])
            project = app.store.create_project('Native application gate')
            report['commandStatus'] = call_http('/api/command', {'projectId': project['id'],
                'commandId': 'denied-native-model', 'expectedRevision': project['revision'],
                'kind': 'generate', 'payload': {}}, token)[0]
            report['deniedUnchanged'] = app.store.get(project['id']) == project
            queued = app.store.command(project['id'], 'queued-free-fixture', project['revision'], 'generate', {})
            time.sleep(1.1)
            report['queuedUnchanged'] = app.store.get(project['id']) == queued
            report['unrequestedChildren'] = len(list((runtime / 'process-ownership').glob('*.json')))

            def free_child():
                try:
                    child = JobProcess([sys.executable, '-I', '-c',
                        'import sys;sys.stdout.buffer.write(sys.stdin.buffer.read());sys.exit(7)'],
                        directory, input_data=b'native-app-thread-stdin')
                    try:
                        report['process'] = child.wait(5)
                    finally:
                        child.close()
                except Exception as error:
                    report['processError'] = type(error).__name__

            child_thread = threading.Thread(target=free_child)
            child_thread.start()
            child_thread.join(10)
            report['childThreadStopped'] = not child_thread.is_alive()
            app.close()
            thread.join(5)
            report['serverStopped'] = not thread.is_alive()
            report['contextReleased'] = context_absent()
            lock = RootLock(runtime)
            lock.close()
            report['rootReleased'] = True
            app = None
        elif case in ('occupied', 'enginefailure', 'invalidhost', 'unsettled'):
            caught = None
            if case == 'occupied':
                with socket.socket() as occupied:
                    occupied.bind(('127.0.0.1', 0))
                    occupied.listen()
                    try:
                        app = Application(runtime, port=occupied.getsockname()[1], preview_port=0)
                    except OSError:
                        caught = 'occupied'
            elif case == 'enginefailure':
                import server
                listeners, preview_threads = [], []
                existing_threads = set(threading.enumerate())
                real_http = server.ThreadingHTTPServer
                def tracked_http(*args):
                    listener = real_http(*args)
                    listeners.append(listener)
                    return listener
                def fail_engine(*args, **kwargs):
                    preview_threads.extend(set(threading.enumerate()) - existing_threads)
                    raise OSError('fixture engine construction failure')
                with patch.object(server, 'ThreadingHTTPServer', tracked_http), \
                        patch('engine.Engine', fail_engine):
                    try:
                        app = Application(runtime, port=0, preview_port=0)
                    except OSError as error:
                        # Retain the exception while checking real cleanup; GC must not release resources.
                        caught = 'enginefailure'
                        report['listenerCount'] = len(listeners)
                        report['releasedListeners'] = 0
                        for listener in listeners:
                            with socket.socket() as replacement:
                                replacement.bind(listener.server_address)
                            report['releasedListeners'] += 1
                        report['previewThreadsAtFailure'] = len(preview_threads)
                        report['previewThreadsStopped'] = all(not thread.is_alive() for thread in preview_threads)
                        report['constructionError'] = type(error).__name__
            elif case == 'invalidhost':
                with patch.dict(os.environ, INVOCATION_ID='not-the-owned-invocation'):
                    try:
                        app = Application(runtime, port=0, preview_port=0)
                    except ValueError:
                        caught = 'invalidhost'
            else:
                runtime.mkdir(mode=0o700)
                ledger = runtime / 'process-ownership'
                ledger.mkdir(mode=0o700)
                record = ledger / ('a' * 32 + '.json')
                record.write_text('{"status":"unknown","fixture":"must-not-clear"}')
                record.chmod(0o600)
                before = record.read_bytes()
                try:
                    app = Application(runtime, port=0, preview_port=0)
                except ValueError:
                    caught = 'unsettled'
                report['ledgerUnchanged'] = record.read_bytes() == before
            report.update(caught=caught, contextReleased=context_absent())
            if app:
                app.close()
                app = None
            lock = RootLock(runtime)
            lock.close()
            report['rootReleased'] = True
            if caught in ('occupied', 'enginefailure', 'invalidhost'):
                app = Application(runtime, port=0, preview_port=0)
                report['reopenedEngine'] = bool(app.engine and app.engine.thread.is_alive())
                app.close()
                app = None
        elif case == 'read-only-status':
            import linux_process
            from process_control import ExecutionUnavailable
            app = Application(runtime, port=0, preview_port=0)
            thread = threading.Thread(target=app.serve)
            thread.start()
            try:
                _, session = call_http('/api/session', {'bootstrap': app.issue_bootstrap()})
                manager_queries = []
                properties = linux_process.properties
                def counted_properties(unit):
                    manager_queries.append(unit)
                    return properties(unit)
                with patch('linux_process.properties', counted_properties):
                    report['blockedPolls'] = [call_http('/api/state', token=session['token'])[1]['executionMode']
                        for _ in range(12)]
                    # Reporting-only fixture. Real LinuxSetup snapshots remain denied.
                    with patch.object(app.setup, 'public', return_value={'canExecute': True}):
                        report['readyReportPolls'] = [call_http('/api/state', token=session['token'])[1]['executionMode']
                            for _ in range(12)]
                        host = app.process_context
                        report['localFaultModes'] = {}
                        for field, changed in (('closed', True), ('unknown', True),
                                ('pid', -1), ('start', 'fixture-changed-start')):
                            previous = getattr(host, field)
                            try:
                                setattr(host, field, changed)
                                report['localFaultModes'][field] = app.execution_mode()
                            finally:
                                setattr(host, field, previous)
                    report['managerQueries'] = len(manager_queries)
                with patch('linux_process.properties', return_value={
                        'MainPID': str(os.getpid()), 'InvocationID': 'fixture-changed-invocation'}):
                    try:
                        JobProcess([sys.executable, '-I', '-c',
                            'from pathlib import Path;Path("must-not-run").touch()'], directory)
                    except ExecutionUnavailable:
                        report['launchDeniedByManager'] = True
                report['targetRan'] = (directory / 'must-not-run').exists()
                report['executionStillDenied'] = False
                try:
                    app.setup.execution_snapshot()
                except ValueError:
                    report['executionStillDenied'] = True
            finally:
                app.close()
                thread.join(5)
                report['serverStopped'] = not thread.is_alive()
                app = None
        elif case == 'python-dependency-probe':
            from checks import CheckRunner
            from environment import RuntimePaths, dependency_health
            app = Application(runtime, port=0, preview_port=0)
            python = Path(sys.executable)
            # Python --version stands in only for reason's fixed local version boundary.
            # The browser is absent: inspect its invocation boundary, never claim health.
            paths = RuntimePaths(python, python, python, python, directory / 'package.json', python)
            browser_specs = []
            def browser_boundary(checker, workspace, evidence, timeout):
                browser_specs.append(json.loads((workspace / 'tests/browser.json').read_text()))
                return {'exitCode': 0, 'assertions': 1}
            with patch.object(CheckRunner, 'run', browser_boundary):
                report['probeBoundaryResult'] = dependency_health(directory, paths)
            report['browserSpecs'] = browser_specs
        elif case == 'unknown-close':
            app = Application(runtime, port=0, preview_port=0)
            child = JobProcess([sys.executable, '-I', '-c', 'import time;time.sleep(60)'], directory)
            invocation = child.invocation
            child.invocation = 'fixture-unknown-identity'
            child.close()
            try:
                app.close()
            except RuntimeError:
                report['closeDenied'] = True
            report['notClosed'] = not app.closed
            try:
                lock = RootLock(runtime)
            except OwnedRoot:
                report['rootRetained'] = True
            else:
                lock.close()
            other = None
            try:
                other = Application(directory / 'other-runtime', port=0, preview_port=0)
            except ValueError:
                report['otherApplicationDenied'] = True
            finally:
                if other:
                    other.close()
            child.invocation = invocation
            report['fixtureChildStopped'] = child.stop()
            # Keep the failed Application/lock alive until this fixture process exits.
            retained_application = app
            app = None
        elif case == 'no-workers':
            app = Application(runtime, port=0, preview_port=0, workers=False)
            report['noEngine'] = app.engine is None
            report['noContext'] = context_absent()
            try:
                JobProcess([sys.executable, '-I', '-c', 'from pathlib import Path;Path("must-not-run").touch()'], directory)
            except Exception as error:
                report['denied'] = type(error).__name__
            report['targetRan'] = (directory / 'must-not-run').exists()
        else:
            raise ValueError('Unknown application fixture case')
    except Exception as error:
        report['fixtureError'] = type(error).__name__ + ': ' + str(error)
    finally:
        if app:
            app.close()
    (directory / 'application-result.json').write_text(json.dumps(report), encoding='utf-8')


@unittest.skipUnless(sys.platform == 'linux', 'Native systemd Application integration')
class LinuxApplicationTests(unittest.TestCase):
    def run_case(self, case):
        with tempfile.TemporaryDirectory(prefix='relay-native-app-') as directory:
            root = Path(directory)
            unit = 'relay-process-test-' + uuid.uuid4().hex + '.service'
            manager = '--system' if os.getuid() == 0 else '--user'
            started = subprocess.run(['/usr/bin/systemd-run', manager, '--quiet', '--collect',
                '--unit', unit, '--property=Type=exec', '--property=KillMode=control-group',
                '--property=TimeoutStopSec=2s', '--property=UMask=0077',
                sys.executable, '-I', '-B', str(Path(__file__)), '--application-case', directory, case],
                capture_output=True, text=True, timeout=12)
            self.assertEqual(started.returncode, 0, started.stderr)
            try:
                deadline = time.monotonic() + 25
                report = root / 'application-result.json'
                while time.monotonic() < deadline:
                    if report.is_file() and report.stat().st_size:
                        result = json.loads(report.read_text())
                        self.assertNotIn('fixtureError', result, result)
                        records = list((root / 'runtime/process-ownership').glob('*.json'))
                        result['records'] = [json.loads(path.read_text()) for path in records]
                        result['ledgerModes'] = [path.stat().st_mode & 0o777 for path in records]
                        return result
                    time.sleep(.03)
                self.fail('Native application fixture did not finish')
            finally:
                subprocess.run(['/usr/bin/systemctl', manager, 'stop', unit], capture_output=True, timeout=12)
                subprocess.run(['/usr/bin/systemctl', manager, 'reset-failed', unit], capture_output=True, timeout=12)

    def test_owned_application_runs_free_child_on_worker_thread_without_claiming_unready_queue(self):
        # Break caught: Linux platform masking, thread-local context, or metadata granting authority.
        result = self.run_case('application')
        self.assertTrue(result['workers'], result)
        self.assertTrue(result['engineAlive'], result)
        self.assertFalse(result['canExecute'])
        self.assertEqual(result['mode'], 'blocked')
        self.assertEqual(result['unauthorized'], 403)
        self.assertEqual(result['commandStatus'], 409)
        self.assertTrue(result['deniedUnchanged'])
        self.assertTrue(result['queuedUnchanged'])
        self.assertEqual(result['unrequestedChildren'], 0)
        self.assertTrue(result['childThreadStopped'])
        self.assertIn('process', result, result)
        self.assertEqual(result['process']['exitCode'], 7)
        self.assertEqual(result['process']['stdout'], 'native-app-thread-stdin')
        self.assertTrue(result['process']['treeStopped'])
        self.assertTrue(result['serverStopped'])
        self.assertTrue(result['contextReleased'])
        self.assertTrue(result['rootReleased'])
        self.assertTrue(result['records'])
        self.assertTrue(all(row['status'] == 'stopped' for row in result['records']))
        self.assertEqual(set(result['ledgerModes']), {0o600})

    def test_failed_application_start_releases_native_context_and_root_for_reopen(self):
        # Break caught: context/lock leak on listener or Engine construction failure.
        for case in ('occupied', 'enginefailure', 'invalidhost'):
            with self.subTest(case=case):
                result = self.run_case(case)
                self.assertEqual(result['caught'], case, result)
                self.assertTrue(result['contextReleased'])
                self.assertTrue(result['rootReleased'])
                self.assertTrue(result['reopenedEngine'])
                if case == 'enginefailure':
                    self.assertEqual(result.get('listenerCount'), 2, result)
                    self.assertEqual(result.get('releasedListeners'), 2, result)
                    self.assertEqual(result.get('previewThreadsAtFailure'), 1, result)
                    self.assertTrue(result.get('previewThreadsStopped'), result)

    def test_unsettled_application_ledger_is_not_cleared_or_granted_execution(self):
        # Break caught: activating an application over unknown prior ownership.
        result = self.run_case('unsettled')
        self.assertEqual(result['caught'], 'unsettled', result)
        self.assertTrue(result['ledgerUnchanged'])
        self.assertTrue(result['contextReleased'])
        self.assertTrue(result['rootReleased'])

    def test_http_status_has_no_manager_subprocesses_and_launch_still_verifies_manager(self):
        # Break caught: request-driven systemctl calls, stale local liveness, or cached launch authority.
        result = self.run_case('read-only-status')
        self.assertEqual(result['managerQueries'], 0, result)
        self.assertEqual(result['blockedPolls'], ['blocked'] * 12)
        self.assertEqual(result['readyReportPolls'], ['enabled'] * 12)
        self.assertEqual(result['localFaultModes'], {name: 'blocked' for name in ('closed', 'unknown', 'pid', 'start')})
        self.assertTrue(result['launchDeniedByManager'])
        self.assertFalse(result['targetRan'])
        self.assertTrue(result['executionStillDenied'])
        self.assertTrue(result['serverStopped'])
        self.assertEqual(result['records'], [])

    def test_native_python_probe_reaches_browser_boundary_without_windows_only_imports(self):
        # Break caught: requiring _winapi in a real owned native Python dependency probe.
        result = self.run_case('python-dependency-probe')
        self.assertTrue(result['probeBoundaryResult'], result)
        self.assertEqual(len(result['browserSpecs']), 1)
        self.assertEqual(result['browserSpecs'][0]['cases'][0]['steps'][0],
            {'op': 'text', 'selector': '#health', 'value': 'relay-local-health'})
        self.assertEqual(len(result['records']), 2)
        self.assertTrue(all(row['status'] == 'stopped' for row in result['records']))

    def test_unknown_application_child_retains_lock_and_blocks_another_application(self):
        # Break caught: dropping a closed-but-unknown context and releasing its runtime.
        result = self.run_case('unknown-close')
        self.assertTrue(result.get('closeDenied'), result)
        self.assertTrue(result['notClosed'])
        self.assertTrue(result.get('rootRetained'), result)
        self.assertTrue(result.get('otherApplicationDenied'), result)
        self.assertTrue(result['fixtureChildStopped'])

    def test_no_workers_mode_does_not_acquire_context_or_fall_back_to_unowned_process(self):
        result = self.run_case('no-workers')
        self.assertTrue(result['noEngine'])
        self.assertTrue(result['noContext'])
        self.assertEqual(result['denied'], 'ExecutionUnavailable')
        self.assertFalse(result['targetRan'])
        self.assertEqual(result['records'], [])


if __name__ == '__main__':
    if sys.argv[1:2] == ['--application-case']:
        run_application_case(Path(sys.argv[2]), sys.argv[3])
    else:
        unittest.main()
