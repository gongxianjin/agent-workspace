"""First-use browser preparation; synthetic network/archive, real disk and threads."""
import hashlib
import importlib
import io
import json
from pathlib import Path
import sys
import tempfile
import threading
import time
import unittest
import zipfile
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


def browser_zip(files=None):
    stream = io.BytesIO()
    with zipfile.ZipFile(stream, 'w') as archive:
        for name, raw in (files or {'chrome-win64/chrome.exe': b'MZsynthetic browser',
                'chrome-win64/resources.pak': b'synthetic resource'}).items():
            archive.writestr(name, raw)
    return stream.getvalue()


class BrowserSetupTests(unittest.TestCase):
    def setUp(self):
        self.module = importlib.import_module('browser_setup') if (ROOT / 'p1/browser_setup.py').exists() else None
        self.assertIsNotNone(self.module, 'First-use browser preparation must be explicit, pinned and local')
        self.temp = tempfile.TemporaryDirectory(prefix='接力 browser ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def descriptor(self, raw):
        return {**self.module.BROWSER, 'sha256': hashlib.sha256(raw).hexdigest(), 'bytes': len(raw)}

    def wait(self, manager):
        deadline = time.monotonic() + 5
        while manager.public()['status'] in ('downloading', 'verifying', 'extracting', 'checking') and time.monotonic() < deadline:
            time.sleep(.01)
        manager.close()
        return manager.public()

    def manager(self, raw, **kwargs):
        # Only external download bytes and browser execution are fixtures; validation/install are real.
        return self.module.BrowserSetup(self.root, required=True, descriptor=self.descriptor(raw),
            fetch=lambda url, destination, progress, cancelled: self.fetch(raw, destination, progress),
            check=lambda executable: executable.read_bytes() == b'MZsynthetic browser', **kwargs)

    @staticmethod
    def fetch(raw, destination, progress):
        destination.write_bytes(raw)
        progress(len(raw))

    def test_network_download_checks_each_bounded_range_and_combines_actual_bytes(self):
        from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
        raw = bytes(range(256)) * 128
        requested = []
        mode = {'bad': False}
        class Handler(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass
            def do_GET(self):
                value = self.headers.get('Range')
                requested.append(value)
                if not value:
                    self.send_response(400); self.end_headers(); return
                start, end = map(int, value.removeprefix('bytes=').split('-'))
                part = raw[start:end+1]
                self.send_response(206)
                self.send_header('Content-Range', f'bytes {start}-{end}/{len(raw) + int(mode["bad"])}')
                self.send_header('Content-Length', str(len(part)))
                self.end_headers()
                self.wfile.write(part)
        server = ThreadingHTTPServer(('127.0.0.1', 0), Handler)
        thread = threading.Thread(target=server.serve_forever)
        thread.start()
        self.addCleanup(thread.join)
        self.addCleanup(server.server_close)
        self.addCleanup(server.shutdown)
        url = f'http://127.0.0.1:{server.server_port}/synthetic-only'
        destination = self.root / 'download.zip'
        progress = []
        # Only trust-policy/descriptor boundary is replaced, not transport or range validation.
        with patch.object(self.module, 'allowed_url', side_effect=lambda candidate: candidate == url), patch.dict(self.module.BROWSER, bytes=len(raw)):
            try:
                self.module.download(url, destination, progress.append, threading.Event())
            except Exception as error:
                self.fail('The download must support bounded official byte ranges: ' + type(error).__name__)
            self.assertEqual(destination.read_bytes(), raw)
            self.assertEqual(progress[-1], len(raw))
            self.assertEqual(len(requested), 8)
            self.assertTrue(all(requested))
            mode['bad'] = True
            with self.assertRaises(ValueError):
                self.module.download(url, self.root / 'bad.zip', lambda count: None, threading.Event())
            self.assertFalse((self.root / 'bad.zip').exists())
        for candidate in ('http://cdn.playwright.dev/a', 'https://bad.invalid/a',
                'https://cdn.playwright.dev:444/a', 'https://user:pass@cdn.playwright.dev/a'):
            self.assertFalse(self.module.allowed_url(candidate))

    def test_release_browser_is_pinned_to_complete_official_archive(self):
        self.assertEqual(self.module.BROWSER['bytes'], 181196793,
            'First use must pin the verified complete archive, not an unknown download size')
        self.assertEqual(self.module.BROWSER['sha256'],
            '08476e6fe550ccbc0d11d1ab030292fc4e32d4987400c77b2d1905a6bd13b4f6')

    def test_reads_and_unconfirmed_start_never_download_or_create_userdata(self):
        manager = self.manager(browser_zip())
        self.assertEqual(manager.public()['status'], 'missing')
        with self.assertRaises(ValueError):
            manager.start(False)
        self.assertFalse((self.root / 'userdata').exists())
        self.assertEqual(manager.public()['downloadedBytes'], 0)

    def test_confirmed_prepare_installs_whole_archive_then_reports_ready(self):
        raw = browser_zip()
        manager = self.manager(raw)
        manager.start(True)
        state = self.wait(manager)
        self.assertEqual(state['status'], 'ready')
        executable = self.module.browser_executable(self.root)
        self.assertEqual(executable.read_bytes(), b'MZsynthetic browser')
        self.assertTrue((executable.parent / 'resources.pak').is_file())
        self.assertEqual(state['downloadedBytes'], len(raw))
        self.assertEqual(self.module.verified_browser(self.root), executable)
        self.assertNotIn(str(self.root), json.dumps(state))
        self.assertFalse(list((self.root / 'userdata/browser').glob('.prepare-*')))

    def test_corruption_or_traversal_is_not_installed_or_executed(self):
        for raw, corrupt in ((browser_zip(), True), (browser_zip({'../outside.exe': b'MZbad'}), False)):
            with self.subTest(corrupt=corrupt):
                checks = []
                manager = self.manager(raw)
                manager.check = lambda executable: checks.append(executable) or True
                if corrupt:
                    manager.descriptor = {**manager.descriptor, 'sha256': '0' * 64}
                manager.start(True)
                self.assertEqual(self.wait(manager)['status'], 'failed')
                self.assertEqual(checks, [], 'Unverified content must never reach execution')
                self.assertFalse(self.module.browser_executable(self.root).exists())
                self.assertFalse((self.root / 'outside.exe').exists())

    def test_failed_health_check_and_existing_unknown_target_are_not_overwritten(self):
        raw = browser_zip()
        manager = self.manager(raw)
        manager.check = lambda executable: False
        manager.start(True)
        self.assertEqual(self.wait(manager)['status'], 'failed')
        target = self.module.browser_executable(self.root)
        target.parent.mkdir(parents=True)
        target.write_bytes(b'unknown existing data')
        manager = self.manager(raw)
        with self.assertRaises(ValueError):
            manager.start(True)
        self.assertEqual(target.read_bytes(), b'unknown existing data')

    def test_duplicate_prepare_is_rejected_and_cancellation_does_not_install(self):
        entered, release = threading.Event(), threading.Event()
        raw = browser_zip()
        manager = self.manager(raw)
        def fetch(url, destination, progress, cancelled):
            entered.set()
            release.wait(3)
            self.fetch(raw, destination, progress)
        manager.fetch = fetch
        manager.start(True)
        self.assertTrue(entered.wait(2))
        with self.assertRaises(ValueError):
            manager.start(True)
        release.set()
        manager.close()
        self.assertNotEqual(manager.public()['status'], 'ready')
        self.assertFalse(self.module.browser_executable(self.root).exists())

    def test_download_failure_reports_safe_tls_category_until_explicit_retry(self):
        import ssl
        from urllib.error import URLError
        raw = browser_zip()
        calls = []
        manager = self.manager(raw)
        def fetch(url, destination, progress, cancelled):
            calls.append(url)
            if len(calls) == 1:
                progress(17)
                raise URLError(ssl.SSLEOFError(8, 'synthetic-secret https://user:password@invalid/ local-path'))
            self.fetch(raw, destination, progress)
        manager.fetch = fetch
        manager.start(True)
        state = self.wait(manager)
        self.assertEqual(state['status'], 'failed')
        self.assertEqual(state.get('stage'), 'download')
        self.assertEqual(state.get('failureCode'), 'tls_error')
        self.assertIn('TLS', state['message'])
        self.assertNotIn('synthetic-secret', json.dumps(state))
        self.assertNotIn('password', json.dumps(state))
        self.assertNotIn(str(self.root), json.dumps(state))
        self.assertEqual(len(calls), 1, 'A failed prepare must never start another attempt by itself')
        self.assertEqual(state['downloadedBytes'], 17)
        self.assertFalse(self.module.browser_executable(self.root).exists())
        self.assertFalse(list((self.root / 'userdata/browser').glob('.prepare-*')))
        manager.start(True)
        ready = self.wait(manager)
        self.assertEqual(ready['status'], 'ready')
        self.assertIsNone(ready.get('failureCode'))
        self.assertEqual(ready.get('stage'), 'ready')
        self.assertEqual(len(calls), 2)

    def test_failed_preparation_distinguishes_transport_storage_and_validation(self):
        import errno
        import ssl
        from urllib.error import HTTPError, URLError
        cases = (
            ('download', URLError(TimeoutError('synthetic-secret')), 'network_timeout'),
            ('download', TimeoutError('synthetic-secret'), 'network_timeout'),
            ('download', URLError(ssl.SSLCertVerificationError(1, 'synthetic-secret')), 'tls_error'),
            ('download', URLError(OSError(10054, 'synthetic-secret')), 'network_error'),
            ('download', HTTPError('https://user:secret@invalid/', 403, 'synthetic-secret', {}, io.BytesIO()), 'http_error'),
            ('download', OSError(errno.ENOSPC, 'synthetic-secret'), 'disk_full'),
            ('download', PermissionError(errno.EACCES, 'synthetic-secret'), 'permission_denied'),
            ('download', ValueError('synthetic-secret'), 'download_invalid'),
            ('verify', None, 'archive_integrity'),
            ('extract', None, 'archive_invalid'),
            ('check', None, 'health_failed'),
            ('check', OSError(errno.EIO, 'synthetic-secret'), 'storage_error'),
            ('check', RuntimeError('synthetic-secret'), 'health_failed'),
        )
        for _, error, _ in cases:
            if isinstance(error, HTTPError):
                self.addCleanup(error.close)
        for stage, error, expected in cases:
            with self.subTest(stage=stage, code=expected):
                raw = browser_zip({'../outside.exe': b'MZbad'}) if stage == 'extract' else browser_zip()
                manager = self.manager(raw)
                if stage == 'verify':
                    manager.descriptor['sha256'] = '0' * 64
                if stage == 'download':
                    def fetch(url, destination, progress, cancelled):
                        raise error
                    manager.fetch = fetch
                if stage == 'check':
                    def check(executable):
                        if error:
                            raise error
                        return False
                    manager.check = check
                manager.start(True)
                state = self.wait(manager)
                self.assertEqual(state['status'], 'failed')
                self.assertEqual(state.get('stage'), stage)
                self.assertEqual(state.get('failureCode'), expected)
                self.assertNotIn('synthetic-secret', json.dumps(state))
                self.assertNotIn('user:secret', json.dumps(state))
                self.assertFalse(self.module.browser_executable(self.root).exists())
                self.assertFalse(list((self.root / 'userdata/browser').glob('.prepare-*')))

    def test_cancellation_has_its_own_category_without_an_install(self):
        raw = browser_zip()
        manager = self.manager(raw)
        def fetch(url, destination, progress, cancelled):
            cancelled.set()
            raise ValueError('synthetic-secret')
        manager.fetch = fetch
        manager.start(True)
        state = self.wait(manager)
        self.assertEqual(state['status'], 'failed')
        self.assertEqual(state.get('stage'), 'download')
        self.assertEqual(state.get('failureCode'), 'cancelled')
        self.assertNotIn('synthetic-secret', json.dumps(state))
        self.assertFalse(self.module.browser_executable(self.root).exists())

    def test_http_range_rejection_remains_a_safe_http_failure(self):
        from urllib.error import HTTPError
        class RefusingOpener:
            def open(self, request, timeout):
                raise HTTPError(request.full_url, 403, 'synthetic-secret', {}, io.BytesIO())
        manager = self.manager(browser_zip())
        manager.fetch = self.module.download
        with patch.object(self.module, 'build_opener', return_value=RefusingOpener()):
            manager.start(True)
            state = self.wait(manager)
        self.assertEqual(state['status'], 'failed')
        self.assertEqual(state.get('failureCode'), 'http_error')
        self.assertNotIn('synthetic-secret', json.dumps(state))
        self.assertFalse(self.module.browser_executable(self.root).exists())

    @unittest.skipUnless(sys.platform == 'win32', 'Windows MAX_PATH placement guidance')
    def test_overlong_browser_destination_is_rejected_before_partial_extraction(self):
        relative = 'chrome-win64/' + 'nested/' * 35 + 'resource.pak'
        raw = browser_zip({'chrome-win64/chrome.exe': b'MZsynthetic browser', relative: b'resource'})
        archive = self.root / 'long.zip'
        archive.write_bytes(raw)
        target = self.root / 'extracted'
        try:
            self.module.extract(archive, target, threading.Event())
        except Exception as error:
            self.assertIsInstance(error, ValueError, 'A long path needs a controlled placement error, not a raw filesystem failure')
        else:
            self.fail('Reject browser destinations exceeding the supported Windows path length')
        self.assertFalse(target.exists(), 'Check all member paths before creating a partial browser tree')
        manager = self.manager(raw)
        manager.start(True)
        state = self.wait(manager)
        self.assertEqual(state['status'], 'failed')
        self.assertEqual(state.get('failureCode'), 'path_too_long')
        self.assertIn('短', state['message'])
        self.assertNotIn(str(self.root), json.dumps(state))
        self.assertFalse(self.module.browser_executable(self.root).exists())

    def test_installed_resource_tampering_is_rejected(self):
        manager = self.manager(browser_zip())
        manager.start(True)
        self.assertEqual(self.wait(manager)['status'], 'ready')
        (self.module.browser_executable(self.root).parent / 'resources.pak').write_bytes(b'tampered')
        with self.assertRaises(ValueError):
            self.module.verified_browser(self.root)


if __name__ == '__main__':
    unittest.main()
