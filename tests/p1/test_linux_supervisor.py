"""Actual systemd249 cgroup lifecycle; Python fixture programs only, never model clients."""
from contextlib import ExitStack
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]


@unittest.skipUnless(sys.platform == 'linux', 'Native systemd/cgroup integration')
class LinuxSupervisorTests(unittest.TestCase):
    def systemctl(self, *args):
        return subprocess.run(['/usr/bin/systemctl', '--system' if os.getuid() == 0 else '--user', *args],
            capture_output=True, text=True, timeout=12)

    def start(self, directory, code, host=None):
        unit = 'relay-lifecycle-test-' + uuid.uuid4().hex + '.service'
        report = directory / (unit + '.json')
        command = ['/usr/bin/systemd-run', '--system' if os.getuid() == 0 else '--user', '--quiet', '--unit', unit,
            '--property=Type=exec', '--property=KillMode=control-group', '--property=TimeoutStopSec=1s',
            '--property=WorkingDirectory=' + str(directory), '--property=UMask=0077']
        if host:
            command += ['--property=BindsTo=' + host, '--property=After=' + host]
        command += [sys.executable, '-I', '-B', str(ROOT / 'p1/linux_supervisor.py'),
            '--result', str(report), '--', sys.executable, '-I', '-B', '-c', code]
        result = subprocess.run(command, capture_output=True, text=True, timeout=12)
        self.assertEqual(result.returncode, 0, result.stderr)
        return unit, report

    def cleanup(self, unit):
        self.systemctl('stop', unit)
        self.systemctl('reset-failed', unit)

    def property(self, unit, name):
        result = self.systemctl('show', unit, '--property=' + name, '--value')
        self.assertEqual(result.returncode, 0, result.stderr)
        return result.stdout.strip()

    def wait_file(self, path):
        deadline = time.monotonic() + 6
        while time.monotonic() < deadline:
            if path.exists() and path.stat().st_size:
                return
            time.sleep(.02)
        self.fail('Real fixture did not produce ' + path.name)

    def assert_group_stopped(self, path):
        try:
            events = (path / 'cgroup.events').read_text()
        except FileNotFoundError:
            if path.exists():
                raise
            return
        self.assertIn('populated 0', events.splitlines())

    def test_stop_proof_handles_collection_without_accepting_populated_or_unreadable_group(self):
        from unittest.mock import patch
        with tempfile.TemporaryDirectory(prefix='relay-cgroup-proof-') as directory:
            group = Path(directory) / 'group'
            self.assert_group_stopped(group)
            group.mkdir()
            with self.assertRaises(FileNotFoundError):
                self.assert_group_stopped(group)
            events = group / 'cgroup.events'
            events.write_text('populated 1\nfrozen 0\n')
            with self.assertRaises(AssertionError):
                self.assert_group_stopped(group)
            events.write_text('populated 0\nfrozen 0\n')
            self.assert_group_stopped(group)
            read_text = Path.read_text
            def collect(path, *args, **kwargs):
                if path == events:
                    events.unlink()
                    group.rmdir()
                return read_text(path, *args, **kwargs)
            with patch.object(Path, 'read_text', collect):
                self.assert_group_stopped(group)
            self.assertFalse(group.exists())

    def test_parent_exit_does_not_end_unit_while_setsid_child_writes(self):
        with tempfile.TemporaryDirectory(prefix='relay-cgroup-') as directory, ExitStack() as cleanup:
            root = Path(directory)
            heartbeat = "import os,time;from pathlib import Path;os.setsid();p=Path('beat');\nwhile True:\n p.write_text(str(time.time_ns()));time.sleep(.02)"
            code = 'import subprocess,sys;subprocess.Popen([sys.executable,"-I","-c",' + repr(heartbeat) + ']);sys.exit(7)'
            unit, report = self.start(root, code)
            cleanup.callback(self.cleanup, unit)
            self.wait_file(root / 'beat')
            time.sleep(.15)
            self.assertEqual(self.property(unit, 'ActiveState'), 'active')
            self.assertFalse(report.exists(), 'Cannot record tree exit while descendants still write')
            group = self.property(unit, 'ControlGroup')
            self.assertTrue((Path('/sys/fs/cgroup') / group.lstrip('/')).is_dir())
            result = self.systemctl('stop', unit)
            self.assertEqual(result.returncode, 0, result.stderr)
            before = (root / 'beat').read_bytes()
            time.sleep(.15)
            self.assertEqual((root / 'beat').read_bytes(), before)
            self.assertNotEqual(self.property(unit, 'ActiveState'), 'active')
            group_path = Path('/sys/fs/cgroup') / group.lstrip('/')
            self.assert_group_stopped(group_path)

    def test_normal_exit_has_actual_code_private_report_and_tree_empty(self):
        with tempfile.TemporaryDirectory(prefix='relay-cgroup-normal-') as directory, ExitStack() as cleanup:
            root = Path(directory)
            unit, report = self.start(root, 'import sys;print("fixture output",flush=True);sys.exit(7)')
            cleanup.callback(self.cleanup, unit)
            self.wait_file(report)
            data = json.loads(report.read_text())
            self.assertEqual(data['exitCode'], 7)
            self.assertTrue(data['descendantsStopped'])
            self.assertEqual(report.stat().st_mode & 0o777, 0o600)
            self.assertEqual(data['unit'], unit)
            self.assertTrue(data['invocationId'])

    def test_sigterm_ignored_is_killed_by_owned_group_stop(self):
        with tempfile.TemporaryDirectory(prefix='relay-cgroup-term-') as directory, ExitStack() as cleanup:
            root = Path(directory)
            code = 'import signal,time;from pathlib import Path;signal.signal(signal.SIGTERM,signal.SIG_IGN);Path("ready").write_text("ready");time.sleep(60)'
            unit, _ = self.start(root, code)
            cleanup.callback(self.cleanup, unit)
            self.wait_file(root / 'ready')
            group = self.property(unit, 'ControlGroup')
            start = time.monotonic()
            self.assertEqual(self.systemctl('stop', unit).returncode, 0)
            self.assertLess(time.monotonic() - start, 5)
            path = Path('/sys/fs/cgroup') / group.lstrip('/')
            self.assert_group_stopped(path)

    def test_killed_host_stops_bound_child_unit(self):
        with tempfile.TemporaryDirectory(prefix='relay-cgroup-host-') as directory, ExitStack() as cleanup:
            root = Path(directory)
            host = 'relay-lifecycle-host-' + uuid.uuid4().hex + '.service'
            command = ['/usr/bin/systemd-run', '--system' if os.getuid() == 0 else '--user', '--quiet', '--unit', host,
                '--property=Type=exec', '--property=KillMode=control-group',
                sys.executable, '-I', '-c', 'import time;time.sleep(60)']
            result = subprocess.run(command, capture_output=True, text=True, timeout=10)
            self.assertEqual(result.returncode, 0, result.stderr)
            cleanup.callback(self.cleanup, host)
            code = 'import time;from pathlib import Path;p=Path("beat");\nwhile True:\n p.write_text(str(time.time_ns()));time.sleep(.02)'
            child, _ = self.start(root, code, host=host)
            cleanup.callback(self.cleanup, child)
            self.wait_file(root / 'beat')
            group = self.property(child, 'ControlGroup')
            self.assertEqual(self.systemctl('kill', '--signal=SIGKILL', '--kill-who=main', host).returncode, 0)
            deadline = time.monotonic() + 5
            while time.monotonic() < deadline and self.property(child, 'ActiveState') in ('active', 'deactivating'):
                time.sleep(.03)
            self.assertNotIn(self.property(child, 'ActiveState'), ('active', 'deactivating'))
            before = (root / 'beat').read_bytes()
            time.sleep(.15)
            self.assertEqual((root / 'beat').read_bytes(), before)
            path = Path('/sys/fs/cgroup') / group.lstrip('/')
            self.assert_group_stopped(path)


if __name__ == '__main__':
    unittest.main()
