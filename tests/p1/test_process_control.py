"""Platform process boundary: real Windows job, Linux refusal until owned adapter exists."""
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


class ProcessControlTests(unittest.TestCase):
    def test_platform_facade_import_does_not_load_other_os_backend(self):
        code = ('import sys,json;sys.path.insert(0,sys.argv[1]);'
            'import process_control;'
            'print(json.dumps({"winjobLoaded":"winjob" in sys.modules}))')
        result = subprocess.run([sys.executable, '-I', '-B', '-c', code, str(ROOT / 'p1')],
            capture_output=True, text=True, timeout=5)
        self.assertEqual(result.returncode, 0, 'The platform process boundary must import without OS side effects: ' + result.stderr)
        self.assertFalse(json.loads(result.stdout)['winjobLoaded'])

    @unittest.skipUnless(sys.platform == 'linux', 'Native Linux consumer import boundary')
    def test_linux_console_and_execution_consumers_do_not_load_windows_backend(self):
        code = ('import importlib,json,sys;sys.path.insert(0,sys.argv[1]);'
            'importlib.import_module(sys.argv[2]);'
            'print(json.dumps({"windowsBackendLoaded":"winjob" in sys.modules}))')
        for name in ('environment', 'reason_mcp', 'runtime', 'checks', 'server'):
            with self.subTest(module=name):
                result = subprocess.run([sys.executable, '-I', '-B', '-c', code, str(ROOT / 'p1'), name],
                    capture_output=True, text=True, timeout=5)
                self.assertEqual(result.returncode, 0, result.stderr)
                self.assertFalse(json.loads(result.stdout)['windowsBackendLoaded'],
                    'A Linux consumer must not load the Windows backend during import')

    @unittest.skipUnless(sys.platform == 'linux', 'Native Linux detection process boundary')
    def test_linux_dependency_and_client_checks_refuse_without_owned_adapter(self):
        from environment import dependency_health, probe_client, RuntimePaths
        from process_control import ExecutionUnavailable
        with tempfile.TemporaryDirectory(prefix='relay-probe-guard-') as directory:
            executable = Path(sys.executable)
            paths = RuntimePaths(*([executable] * 6))
            for operation in (lambda: probe_client(executable, ['--version']),
                    lambda: dependency_health(Path(directory), paths)):
                with self.subTest(operation=operation), self.assertRaises(ExecutionUnavailable):
                    operation()

    @unittest.skipUnless(sys.platform == 'linux', 'Actual Linux fail-closed process boundary')
    def test_linux_without_owned_adapter_never_launches_target(self):
        try:
            from process_control import JobProcess, ExecutionUnavailable
        except ImportError as error:
            self.fail('Native process boundary is missing: ' + str(error))
        with tempfile.TemporaryDirectory(prefix='relay-no-adapter-') as directory:
            marker = Path(directory) / 'target-ran'
            code = 'from pathlib import Path;Path(' + repr(str(marker)) + ').write_text("forbidden")'
            with self.assertRaises(ExecutionUnavailable):
                JobProcess([sys.executable, '-I', '-c', code], directory)
            self.assertFalse(marker.exists(), 'Missing cgroup ownership must never fall back to Popen')

    @unittest.skipUnless(os.name == 'nt', 'Actual Windows Job Object preservation')
    def test_windows_facade_preserves_owned_job_contract(self):
        try:
            from process_control import JobProcess
        except ImportError as error:
            self.fail('Native process boundary is missing: ' + str(error))
        with tempfile.TemporaryDirectory(prefix='relay-facade-') as directory:
            child = JobProcess([sys.executable, '-I', '-c', 'import sys;print("job-boundary");sys.exit(7)'], directory)
            try:
                result = child.wait(5)
            finally:
                child.close()
            self.assertEqual(result['exitCode'], 7)
            self.assertTrue(result['treeStopped'])
            self.assertIn('job-boundary', result['stdout'])
            self.assertFalse(result['timedOut'])


if __name__ == '__main__':
    unittest.main()
