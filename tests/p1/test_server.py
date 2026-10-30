"""Real loopback HTTP tests. No model calls; queued attempts stay queued."""
import http.client
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import threading
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
path = ROOT / 'p1/server.py'
spec = importlib.util.spec_from_file_location('relay_server', path) if path.exists() else None
server = importlib.util.module_from_spec(spec) if spec else None
if spec:
    spec.loader.exec_module(server)


class ServerTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(server, 'A protected local HTTP service must own real state')
        self.temp = tempfile.TemporaryDirectory(prefix='relay-http-')
        self.addCleanup(self.temp.cleanup)
        self.app = server.Application(Path(self.temp.name), port=0, workers=False)
        from support import ReadySetupFixture
        self.app.setup = ReadySetupFixture()
        self.thread = threading.Thread(target=self.app.serve, daemon=True)
        self.thread.start()
        self.addCleanup(self.app.close)
        self.origin = self.app.origin
        self.token = self.call('POST', '/api/session', {'bootstrap': self.app.bootstrap}, auth=False)[1]['token']
        self.project = self.call('POST', '/api/projects', {'name': 'API测试项目'})[1]

    def call(self, method, path, value=None, auth=True, headers=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.app.port, timeout=10)
        request_headers = {'Origin': self.origin, 'Content-Type': 'application/json'}
        if auth:
            request_headers['Authorization'] = 'Bearer ' + self.token
        request_headers.update(headers or {})
        source = json.dumps(value, ensure_ascii=False).encode('utf-8') if value is not None else None
        connection.request(method, path, source, request_headers)
        response = connection.getresponse()
        raw, status = response.read(), response.status
        connection.close()
        try:
            return status, json.loads(raw)
        except ValueError:
            return status, raw.decode('utf-8', errors='replace')

    def command(self, kind, payload=None, revision=None, command_id=None):
        result = self.call('POST', '/api/command', {'projectId': self.project['id'], 'commandId': command_id or uuid.uuid4().hex,
            'expectedRevision': self.project['revision'] if revision is None else revision, 'kind': kind, 'payload': payload or {}})
        if result[0] == 200:
            self.project = result[1]
        return result

    def test_live_script_is_fixed_order_with_unchanged_origin_policy_and_no_file_selector(self):
        from unittest.mock import patch
        assets = Path(self.temp.name) / 'script-assets'
        assets.mkdir()
        modules = ('live-session.js', 'live-workspace.js', 'live-operations.js', 'live-agents.js', 'live.js')
        for index, name in enumerate(modules):
            (assets / name).write_bytes(('globalThis.releaseOrder.push(' + str(index) + ')').encode())
        (assets / 'private.js').write_bytes(b'PRIVATE-NOT-SERVED')
        expected = b';\n'.join((assets / name).read_bytes() for name in modules)
        with patch.object(server, 'BASE', assets):
            for suffix in ('', '?file=private.js', '?modules=private.js'):
                connection = http.client.HTTPConnection('127.0.0.1', self.app.port, timeout=3)
                try:
                    connection.request('GET', '/live.js' + suffix, headers={'Host': '127.0.0.1:' + str(self.app.port)})
                    response = connection.getresponse()
                    self.assertEqual(response.status, 200)
                    self.assertIn('javascript', response.getheader('Content-Type'))
                    self.assertEqual(response.getheader('Content-Security-Policy'), "default-src 'self'; script-src 'self'; style-src 'self'; img-src 'self' data:; connect-src 'self'; frame-src http://127.0.0.1:" + str(self.app.preview_port) + "; frame-ancestors 'none'; object-src 'none'; base-uri 'none'; form-action 'none'")
                    self.assertEqual(response.read(), expected)
                finally:
                    connection.close()
            for name in (*modules[:-1], 'private.js', '../private.js'):
                self.assertEqual(self.call('GET', '/' + name)[0], 404)
        self.assertEqual(self.app.store.get(self.project['id']), self.project)

    def test_setup_is_cached_protected_and_blocks_model_enqueue_without_mutation(self):
        from environment import SetupStatus, source_or_bundle
        # Isolate setup data from the release root; keep the real pinned runtime paths.
        setup = SetupStatus(Path(self.temp.name), paths=source_or_bundle(ROOT), discover=lambda: None)
        self.app.setup = setup
        status, initial = self.call('GET', '/api/setup')
        self.assertEqual(status, 200)
        self.assertFalse(initial['canExecute'])
        before = self.app.store.get(self.project['id'])
        self.assertEqual(self.command('generate')[0], 409)
        self.assertEqual(self.app.store.get(self.project['id']), before)
        self.assertEqual(self.call('POST', '/api/setup/recheck', {}, auth=False)[0], 403)
        self.assertEqual(self.call('POST', '/api/setup/recheck', {}, headers={'Origin': 'https://bad.invalid'})[0], 403)
        status, checked = self.call('POST', '/api/setup/recheck', {})
        self.assertEqual(status, 200)
        self.assertEqual(checked['client'], 'missing')
        self.assertEqual(self.call('GET', '/api/setup')[1], checked)
        self.assertEqual(self.call('POST', '/api/setup/confirm-existing', {'confirmed': False})[0], 409)
        self.assertEqual(self.command('edit', {'roles': {'architect': 'Codex', 'developer': 'Claude Code', 'tester': 'Claude Code'}})[0], 409)
        self.assertEqual(self.command('upload', {'name': 'reviewed.md', 'text': '# No model needed'})[0], 200)

    def test_distribution_consent_requires_current_document_auth_origin_and_explicit_acceptance(self):
        import hashlib
        from environment import SetupStatus, source_or_bundle
        bundle = Path(self.temp.name) / 'consent-bundle'
        bundle.mkdir()
        raw = b'Synthetic downstream terms; no third-party executable is run.\n'
        (bundle / 'DISTRIBUTION-TERMS.txt').write_bytes(raw)
        checksum = hashlib.sha256(raw).hexdigest()
        (bundle / 'portable.json').write_text(json.dumps({'format': 2, 'distributionTerms': {
            'path': 'DISTRIBUTION-TERMS.txt', 'sha256': checksum}}), encoding='utf-8')
        setup = SetupStatus(bundle, paths=source_or_bundle(ROOT), discover=lambda: None)
        self.app.setup = setup
        self.addCleanup(setup.close)
        before = self.app.store.get(self.project['id'])
        shown = self.call('GET', '/api/setup')[1]['distributionTerms']
        self.assertEqual(shown['text'], raw.decode())
        self.assertFalse(shown['accepted'])
        self.assertEqual(self.command('generate')[0], 409)
        path = '/api/setup/distribution'
        value = {'confirmed': True, 'sha256': checksum}
        self.assertEqual(self.call('POST', path, value, auth=False)[0], 403)
        self.assertEqual(self.call('POST', path, value, headers={'Origin': 'https://bad.invalid'})[0], 403)
        self.assertEqual(self.call('POST', path, {**value, 'confirmed': False})[0], 409)
        self.assertEqual(self.call('POST', path, {**value, 'sha256': '0' * 64})[0], 409)
        self.assertEqual(self.call('POST', path, {**value, 'file': 'outside'})[0], 409)
        self.assertFalse((bundle / 'userdata').exists())
        status, accepted = self.call('POST', path, value)
        self.assertEqual(status, 200, 'Explicit consent to the shown document needs a protected endpoint')
        self.assertTrue(accepted['distributionTerms']['accepted'])
        self.assertFalse(accepted['canExecute'])
        self.assertTrue(self.call('GET', '/api/setup')[1]['distributionTerms']['accepted'])
        self.assertEqual(self.app.store.get(self.project['id']), before, 'Consent cannot advance project authority')
        self.assertEqual(self.command('generate')[0], 409, 'Terms acceptance cannot replace client detection')

    def test_browser_prepare_requires_auth_origin_consent_and_idle_then_polls_real_state(self):
        from environment import SetupStatus, source_or_bundle
        from browser_setup import BrowserSetup
        from test_browser_setup import browser_zip
        import hashlib
        entered, release = threading.Event(), threading.Event()
        raw = browser_zip()
        # Isolate setup data from the release root; keep the real pinned runtime paths.
        setup = SetupStatus(Path(self.temp.name), paths=source_or_bundle(ROOT), discover=lambda: None)
        self.app.setup = setup
        def fetch(url, destination, progress, cancelled):
            entered.set()
            if not release.wait(3):
                raise ValueError('test release timed out')
            destination.write_bytes(raw)
            progress(len(raw))
        setup.browser = BrowserSetup(Path(self.temp.name) / 'bundle', required=True,
            descriptor={**setup.browser.descriptor, 'bytes': len(raw), 'sha256': hashlib.sha256(raw).hexdigest()},
            fetch=fetch, check=lambda executable: executable.read_bytes() == b'MZsynthetic browser')
        self.addCleanup(setup.close)
        path = '/api/setup/browser'
        before = self.app.store.get(self.project['id'])
        self.assertEqual(self.call('POST', path, {'confirmed': True}, auth=False)[0], 403)
        self.assertEqual(self.call('POST', path, {'confirmed': True}, headers={'Origin': 'https://bad.invalid'})[0], 403)
        self.assertEqual(self.call('POST', path, {'confirmed': False})[0], 409)
        self.assertEqual(self.call('POST', path, {'confirmed': True, 'url': 'https://bad.invalid'})[0], 409)
        self.assertFalse(entered.is_set())
        try:
            status, state = self.call('POST', path, {'confirmed': True})
            self.assertEqual(status, 200, 'Explicit first-use browser API must start a bounded preparation')
            self.assertTrue(entered.wait(1))
            self.assertEqual(self.call('GET', '/api/setup')[1]['browser']['status'], 'downloading')
            self.assertEqual(self.call('POST', path, {'confirmed': True})[0], 409)
            self.assertEqual(self.call('POST', '/api/shutdown', {})[0], 409, 'Do not report clean exit during preparation')
            self.assertEqual(self.app.store.get(self.project['id']), before, 'Browser setup cannot mutate project authority')
            self.assertFalse(state['canExecute'])
        finally:
            release.set()
            setup.browser.thread.join(5) if setup.browser.thread else None
        self.assertEqual(self.call('GET', '/api/setup')[1]['browser']['status'], 'ready')
        self.assertEqual(self.call('POST', '/api/shutdown', {})[0], 200)

    def test_telemetry_api_is_read_only_scoped_bounded_and_authoritative(self):
        project_id = self.project['id']
        path = '/api/telemetry?project=' + project_id
        before = self.app.store.get(project_id)
        status, empty = self.call('GET', path)
        self.assertEqual(status, 200, 'Protected telemetry endpoint must not enqueue or initialize an attempt')
        self.assertIsNone(empty['scope'])
        self.assertEqual(self.app.store.get(project_id), before)
        self.assertEqual(self.call('GET', path, auth=False)[0], 403)
        self.assertEqual(self.call('GET', path, headers={'Origin': 'https://bad.invalid'})[0], 403)
        self.command('generate')
        attempt = self.app.store.claim_attempt(project_id)
        scope = {'projectId': project_id, 'attemptId': attempt['id'], 'epoch': attempt['epoch'], 'runId': attempt['runId'], 'inputHash': attempt['inputHash']}
        self.app.telemetry.begin(scope)
        for _ in range(150):
            self.app.telemetry.append(scope, {'kind': 'text', 'text': '<script>not code</script>' * 70})
        before = self.app.store.get(project_id)
        status, page = self.call('GET', path)
        self.assertEqual(status, 200)
        self.assertEqual(page['scope'], scope)
        self.assertEqual(page['authority']['phase'], before['phase'])
        self.assertLessEqual(len(json.dumps(page).encode()), 128 * 1024)
        self.assertTrue(page['hasMore'])
        suffix = '&attempt=' + attempt['id'] + '&epoch=' + str(attempt['epoch']) + '&cursor=' + str(page['nextCursor'])
        status, tail = self.call('GET', path + suffix)
        self.assertEqual(status, 200)
        self.assertGreater(tail['nextCursor'], page['nextCursor'])
        self.assertEqual(self.call('GET', path + suffix.replace('epoch=1', 'epoch=9'))[0], 400)
        self.assertEqual(self.call('GET', path + '&cursor=9')[0], 400)
        other = self.app.store.create_project('隔离')
        self.assertEqual(self.call('GET', '/api/telemetry?project=' + other['id'] + suffix)[0], 400)
        self.assertEqual(self.app.store.get(project_id), before)
        self.app.store.fail_attempt(attempt['id'], 'synthetic authority failure', confirmed=True)
        self.app.telemetry.seal(scope)
        final = self.call('GET', path + '&attempt=' + attempt['id'] + '&epoch=1&cursor=0')[1]
        self.assertEqual(final['authority']['phase'], 'blocked')
        self.assertTrue(final['sealed'])
        self.assertEqual(final['metadata']['role'], attempt['role'])

    def test_disconnected_telemetry_client_does_not_retry_response_or_fail_request_thread(self):
        import socket
        import struct
        from unittest.mock import patch
        entered, release, finished = threading.Event(), threading.Event(), threading.Event()
        errors = []
        read = self.app.read_telemetry
        process = self.app.http.process_request_thread
        def gated_read(query):
            entered.set()
            if not release.wait(3):
                raise RuntimeError('Telemetry fixture was not released')
            return read(query)
        def observed_process(*args):
            try:
                process(*args)
            finally:
                finished.set()
        def record_error(*args):
            errors.append(type(sys.exception()).__name__)
        before = self.app.store.get(self.project['id'])
        with patch.object(self.app, 'read_telemetry', gated_read), \
                patch.object(self.app.http, 'process_request_thread', observed_process), \
                patch.object(self.app.http, 'handle_error', record_error):
            try:
                with socket.create_connection(('127.0.0.1', self.app.port), timeout=3) as connection:
                    request = (f'GET /api/telemetry?project={self.project["id"]} HTTP/1.1\r\n'
                        f'Host: 127.0.0.1:{self.app.port}\r\nAuthorization: Bearer {self.token}\r\n\r\n')
                    connection.sendall(request.encode('ascii'))
                    self.assertTrue(entered.wait(3), 'Request must reach the actual telemetry reader before disconnect')
                    # Native linger=0 sends a real TCP reset, not a simulated writer exception.
                    linger_format = 'HH' if os.name == 'nt' else 'ii'
                    connection.setsockopt(socket.SOL_SOCKET, socket.SO_LINGER, struct.pack(linger_format, 1, 0))
            finally:
                release.set()
            self.assertTrue(finished.wait(3), 'Disconnected request must finish without another keep-alive read')
        self.assertEqual(errors, [], 'A lost response connection must not trigger a second 500 response or an unhandled thread error')
        self.assertEqual(self.call('GET', '/api/state')[0], 200)
        self.assertEqual(self.app.store.get(self.project['id']), before)

    def test_telemetry_storage_error_still_returns_500_without_mutating_state(self):
        from unittest.mock import patch
        before = self.app.store.get(self.project['id'])
        with patch.object(self.app.telemetry, 'find', side_effect=OSError('synthetic local storage failure')):
            status, body = self.call('GET', '/api/telemetry?project=' + self.project['id'])
        self.assertEqual(status, 500)
        self.assertEqual(body, {'error': 'Local operation failed; inspect local evidence'})
        self.assertEqual(self.app.store.get(self.project['id']), before)

    def test_reopen_mints_short_lived_single_use_bootstrap_without_revoking_old_session(self):
        import time
        issue = getattr(self.app, 'issue_bootstrap', None)
        self.assertTrue(callable(issue), 'Local owner must be able to reopen without restarting the running engine')
        fresh = issue()
        self.assertNotEqual(fresh, self.app.bootstrap)
        self.assertEqual(self.call('POST', '/api/session', {'bootstrap': fresh}, auth=False)[0], 200)
        self.assertEqual(self.call('POST', '/api/session', {'bootstrap': fresh}, auth=False)[0], 403)
        self.assertEqual(self.call('GET', '/api/state')[0], 200)
        from unittest.mock import patch
        expired = issue()
        with patch.object(server.time, 'time', return_value=time.time() + 61):
            self.assertEqual(self.call('POST', '/api/session', {'bootstrap': expired}, auth=False)[0], 403)
        self.assertEqual(self.call('GET', '/api/session', auth=False)[0], 404)

    def test_shutdown_is_explicit_authenticated_and_refuses_queued_work(self):
        self.assertEqual(self.call('POST', '/api/shutdown', {}, auth=False)[0], 403)
        self.assertEqual(self.command('generate')[0], 200)
        self.assertEqual(self.call('POST', '/api/shutdown', {})[0], 409)
        self.assertEqual(self.command('stop')[0], 200)
        self.assertEqual(self.call('POST', '/api/shutdown', {})[0], 200)
        self.assertTrue(self.app.shutdown_requested.is_set())
        self.assertEqual(self.call('POST', '/api/projects', {'name': 'too late'})[0], 409)

    def test_shutdown_checks_every_project_and_each_unsafe_state(self):
        from unittest.mock import patch
        second = self.app.store.create_project('第二项目的退出门禁')
        baseline = self.app.store.get(second['id'])
        for unsafe in ({'phase': 'paused'}, {'phase': 'stopping'}, {'leaseUnknown': True}, {'recoveryRequired': True},
                {'activeAttempt': {'id': 'synthetic queued', 'status': 'queued'}},
                {'activeAttempt': {'id': 'synthetic running', 'status': 'running'}}):
            with self.subTest(state=unsafe), patch.object(self.app.store, 'list_projects',
                    return_value=[self.project, {**baseline, **unsafe}]):
                self.assertEqual(self.call('POST', '/api/shutdown', {})[0], 409)
                self.assertTrue(self.app.accepting)
                self.assertFalse(self.app.shutdown_requested.is_set())
        class BusyEngine:
            lock = threading.RLock()
            active_project = second['id']
        self.app.engine = BusyEngine()
        try:
            self.assertEqual(self.call('POST', '/api/shutdown', {})[0], 409)
            self.assertTrue(self.app.accepting)
        finally:
            self.app.engine = None
        self.assertEqual(self.call('POST', '/api/shutdown', {})[0], 200)

    def test_reopen_caps_pending_links_and_refuses_new_links_at_session_limit(self):
        import time
        links = [self.app.issue_bootstrap() for _ in range(12)]
        self.assertEqual(len(self.app.bootstraps), 8)
        self.assertEqual(self.call('POST', '/api/session', {'bootstrap': links[0]}, auth=False)[0], 403)
        self.assertEqual(self.call('POST', '/api/session', {'bootstrap': links[-1]}, auth=False)[0], 200)
        with self.app.auth_lock:
            self.app.tokens = {'synthetic-' + str(index): time.time() + 60 for index in range(128)}
        with self.assertRaises(server.Rejected):
            self.app.issue_bootstrap()
        self.assertLessEqual(len(self.app.bootstraps), 8)

    def test_failed_cleanup_does_not_claim_closed_or_release_root(self):
        class FailedEngine:
            def close(self):
                raise RuntimeError('synthetic cleanup failure')
        self.app.engine = FailedEngine()
        try:
            with self.assertRaises(RuntimeError):
                self.app.close()
            self.assertFalse(self.app.closed, 'A failed close must remain retryable and must not report clean shutdown')
            with self.assertRaises(server.OwnedRoot):
                server.Application(self.app.store.root, port=0, workers=False)
        finally:
            self.app.engine = None
            self.app.closed = False

    def test_failed_construction_releases_owned_lock_and_partial_listeners(self):
        from unittest.mock import patch
        import socket
        owned = []
        real_http = server.ThreadingHTTPServer
        def bind_then_fail(address, handler):
            if owned:
                raise OSError('synthetic second listener failure')
            listener = real_http(address, handler)
            owned.append(listener)
            return listener
        root = Path(self.temp.name) / 'failed-construction'
        locks = []
        real_lock = server.RootLock
        def tracked_lock(directory):
            lock = real_lock(directory)
            locks.append(lock)
            return lock
        try:
            with patch.object(server, 'ThreadingHTTPServer', side_effect=bind_then_fail), patch.object(server, 'RootLock', side_effect=tracked_lock):
                try:
                    server.Application(root, port=0, workers=False)
                except OSError:
                    # Hold the traceback alive: resource cleanup cannot rely on GC.
                    with socket.socket() as replacement:
                        try:
                            replacement.bind(owned[0].server_address)
                        except OSError:
                            self.fail('Failed construction left its control listener open')
                    try:
                        lock = real_lock(root)
                    except server.OwnedRoot:
                        self.fail('Failed construction left its data root locked')
                    lock.close()
                else:
                    self.fail('Fixture must fail after opening the control listener')
        finally:
            for listener in owned:
                listener.server_close()
            for lock in locks:
                lock.close()

    def test_early_construction_failures_do_not_keep_data_root_locked(self):
        from unittest.mock import patch
        if os.name == 'nt':
            setup_module, setup_class = server, 'SetupStatus'
        else:
            import linux_setup
            setup_module, setup_class = linux_setup, 'LinuxSetup'
        for module, stage in ((server, 'Store'), (setup_module, setup_class),
                (server, 'TelemetryStore'), (server, 'ThreadingHTTPServer')):
            with self.subTest(stage=stage):
                root = Path(self.temp.name) / ('failed-' + stage)
                with patch.object(module, stage, side_effect=OSError('synthetic construction failure')):
                    with self.assertRaises(OSError) as caught:
                        server.Application(root, port=0, workers=False)
                lock = server.RootLock(root)
                lock.close()
                self.assertIsNotNone(caught.exception)

    def test_worker_construction_failure_closes_both_listeners_and_preview_thread(self):
        if sys.platform == 'linux':
            from test_linux_application import LinuxApplicationTests
            result = LinuxApplicationTests().run_case('enginefailure')
            self.assertEqual(result['caught'], 'enginefailure', result)
            self.assertEqual(result['listenerCount'], 2, result)
            self.assertEqual(result['releasedListeners'], 2, result)
            self.assertEqual(result['previewThreadsAtFailure'], 1, result)
            self.assertTrue(result['previewThreadsStopped'], result)
            self.assertTrue(result['rootReleased'], result)
            self.assertTrue(result['contextReleased'], result)
            self.assertTrue(result['reopenedEngine'], result)
            return
        from unittest.mock import patch
        import engine
        import socket
        listeners = []
        real_http = server.ThreadingHTTPServer
        def tracked_http(*args):
            listener = real_http(*args)
            listeners.append(listener)
            return listener
        root = Path(self.temp.name) / 'failed-worker'
        try:
            with patch.object(server, 'ThreadingHTTPServer', side_effect=tracked_http), patch.object(engine, 'Engine', side_effect=OSError('synthetic worker failure')):
                with self.assertRaises(OSError) as caught:
                    server.Application(root, port=0, workers=True)
            for listener in listeners:
                with socket.socket() as replacement:
                    replacement.bind(listener.server_address)
            lock = server.RootLock(root)
            lock.close()
            self.assertIsNotNone(caught.exception)
        finally:
            for listener in listeners:
                listener.server_close()

    def test_same_root_second_service_cannot_recover_or_mutate_first_service(self):
        self.assertEqual(self.command('generate')[0], 200)
        before = self.app.store.get(self.project['id'])
        second = None
        try:
            second = server.Application(self.app.store.root, port=0, workers=False)
        except (RuntimeError, OSError):
            pass
        else:
            self.fail('Only one service may own a runtime root, even on different ports')
        finally:
            if second:
                second.close()
        self.assertEqual(self.app.store.get(self.project['id']), before)

    def test_stop_command_replay_is_safe_and_does_not_stop_new_work(self):
        self.assertEqual(self.command('generate')[0], 200)
        revision, command_id = self.project['revision'], uuid.uuid4().hex
        first = self.command('stop', revision=revision, command_id=command_id)
        self.assertEqual(first[0], 200)
        self.project = self.app.store.get(self.project['id'])
        self.assertEqual(self.command('generate')[0], 200)
        attempt = self.project['activeAttempt']['id']
        self.assertEqual(self.command('stop', revision=revision, command_id=command_id)[0], 200)
        self.assertEqual(self.app.store.get(self.project['id'])['activeAttempt']['id'], attempt)

    def test_actual_check_artifacts_survive_checker_and_service_restart(self):
        from checks import CheckRunner
        workspace = self.app.store.root / 'projects' / self.project['id']
        (workspace / 'product/index.html').write_text('<!doctype html><p>trusted fixture</p>', encoding='utf-8')
        (workspace / 'tests/browser.json').write_text(json.dumps({'version': 1, 'cases': [{'name': 'actual browser', 'steps': [{'op': 'text', 'selector': 'p', 'value': 'trusted fixture'}]}]}), encoding='utf-8')
        try:
            checker = CheckRunner(index_root=self.app.store.root / 'check-index')
        except TypeError:
            self.fail('Historical artifact access requires a persistent trusted index, not the latest checker object')
        result = checker.run(workspace, self.app.store.root / 'evidence' / 'fixture-attempt')
        self.app.close()
        self.thread.join(3)
        self.app = server.Application(self.app.store.root, port=0, workers=False)
        self.addCleanup(self.app.close)
        self.thread = threading.Thread(target=self.app.serve, daemon=True)
        self.thread.start()
        self.origin = self.app.origin
        self.token = self.call('POST', '/api/session', {'bootstrap': self.app.bootstrap}, auth=False)[1]['token']
        status, report = self.call('GET', '/api/artifacts/' + result['id'] + '/report.json')
        self.assertEqual(status, 200)
        self.assertGreater(report['assertions'], 0)
        self.assertEqual(self.call('GET', '/api/artifacts/' + '0' * 32 + '/report.json')[0], 404)

    def test_auth_host_origin_and_single_use_bootstrap(self):
        self.assertEqual(self.call('GET', '/api/state', auth=False)[0], 403)
        self.assertEqual(self.call('GET', '/api/state', headers={'Host': 'evil.invalid'})[0], 403)
        self.assertEqual(self.call('POST', '/api/projects', {'name': 'x'}, headers={'Origin': 'null'})[0], 403)
        self.assertEqual(self.call('POST', '/api/projects', {'name': 'x'}, headers={'Origin': 'https://evil.invalid'})[0], 403)
        self.assertEqual(self.call('POST', '/api/session', {'bootstrap': self.app.bootstrap}, auth=False)[0], 403)
        status, state = self.call('GET', '/api/state')
        self.assertEqual(status, 200)
        self.assertEqual(len(state['projects']), 1)
        self.assertEqual(set(state['projects'][0]['roles'].values()), {'Claude Code'})

    def test_command_cas_idempotency_and_no_completion_backdoor(self):
        command_id = uuid.uuid4().hex
        status, first = self.command('edit', {'requirement': '真实新需求'}, command_id=command_id)
        self.assertEqual(status, 200)
        self.assertEqual(self.command('edit', {'requirement': '真实新需求'}, revision=1, command_id=command_id), (200, first))
        self.assertEqual(self.command('edit', {'requirement': '过期'}, revision=1)[0], 409)
        self.assertEqual(self.command('test-pass')[0], 409)
        self.assertEqual(self.command('develop-complete')[0], 409)
        self.assertEqual(self.command('start')[0], 409)

    def test_upload_invalid_preserves_bytes_and_rendered_text_is_not_html(self):
        source = '<img src=x onerror=alert(1)>\n这是数据'
        self.assertEqual(self.command('upload', {'name': 'design.md', 'text': source})[0], 200)
        before = self.project
        self.assertEqual(self.command('upload', {'name': '../bad.md', 'text': 'bad'})[0], 409)
        self.assertEqual(self.app.store.get(before['id']), before)
        self.assertEqual(before['uploadedDocument']['text'], source)
        self.assertEqual(self.call('GET', '/api/../../p1/state.py')[0], 404)
        self.assertEqual(self.call('GET', '/p1/state.py')[0], 404)

    def test_request_limit_and_arbitrary_shell_or_root_are_rejected(self):
        self.assertEqual(self.call('POST', '/api/projects', {'name': 'x', 'root': 'C:/Users'})[0], 409)
        self.assertEqual(self.command('shell', {'command': 'whoami'})[0], 409)
        self.assertEqual(self.call('POST', '/api/projects', {'name': 'a' * (600 * 1024)})[0], 413)

    def test_oversized_body_is_rejected_before_body_arrives_and_closes_gracefully(self):
        import socket
        with socket.create_connection(('127.0.0.1', self.app.port), timeout=3) as connection:
            headers = (f'POST /api/projects HTTP/1.1\r\nHost: 127.0.0.1:{self.app.port}\r\n'
                f'Origin: {self.origin}\r\nAuthorization: Bearer {self.token}\r\n'
                'Content-Type: application/json\r\nContent-Length: 614400\r\n\r\n')
            connection.sendall(headers.encode('ascii'))
            response = http.client.HTTPResponse(connection)
            response.begin()
            self.assertEqual(response.status, 413)
            self.assertEqual(response.getheader('Connection'), 'close', 'Rejected unread bodies must never be reused as another request')
            self.assertEqual(json.loads(response.read()), {'error': 'Body exceeds limit', 'outcome': 'rejected'})
            response.close()
        self.assertEqual(len(self.app.store.list_projects()), 1, 'Oversize rejection cannot create a project')

    def test_generated_product_is_separate_origin_and_never_control_route(self):
        product = self.app.store.root / 'projects' / self.project['id'] / 'product/index.html'
        product.write_text('<script>parent.postMessage("product", "*")</script>', encoding='utf-8')
        self.assertEqual(self.call('GET', '/projects/' + self.project['id'] + '/product/index.html')[0], 404)
        connection = http.client.HTTPConnection('127.0.0.1', self.app.preview_port)
        connection.request('GET', '/' + self.project['id'] + '/index.html')
        response = connection.getresponse()
        self.assertEqual(response.status, 200)
        self.assertIn("connect-src 'none'", response.getheader('Content-Security-Policy'))
        self.assertIn('sandbox', response.getheader('Content-Security-Policy'))
        response.read(); connection.close()
        self.assertNotEqual(self.app.preview_port, self.app.port)

    def test_restart_blocks_unfinished_work_and_never_automatically_runs(self):
        self.assertEqual(self.command('generate')[0], 200)
        from state import Store
        recovered = Store(self.app.store.root).get(self.project['id'])
        self.assertEqual(recovered['phase'], 'blocked')
        self.assertTrue(recovered['leaseUnknown'])


if __name__ == '__main__':
    unittest.main()
