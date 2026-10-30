"""Real Windows process-tree tests; these children are fixtures, not AI."""
import ctypes
from ctypes import wintypes as w
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import time
import unittest

ROOT = Path(__file__).resolve().parents[2]
PATH = ROOT / 'p1/winjob.py'
spec = importlib.util.spec_from_file_location('relay_winjob', PATH) if PATH.exists() else None
jobs = importlib.util.module_from_spec(spec) if spec else None
if spec:
    spec.loader.exec_module(jobs)


@unittest.skipUnless(os.name == 'nt', 'Windows Job Object contract')
class ProcessTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(jobs, 'A supervised tree must stop before the next writer starts')
        self.temp = tempfile.TemporaryDirectory(prefix='relay-job-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.children = []
        self.addCleanup(self.stop_children)

    def stop_children(self):
        for child in self.children:
            child.close()

    def start(self, code, **options):
        child = jobs.JobProcess([sys.executable, '-I', '-X', 'utf8', '-c', code], self.root, **options)
        self.children.append(child)
        return child

    def wait_file(self, name):
        deadline = time.monotonic() + 5
        while time.monotonic() < deadline:
            target = self.root / name
            if target.exists() and target.stat().st_size:
                return
            time.sleep(.03)
        self.fail(f'Fixture did not write {name}')

    def test_stdin_and_output_are_drained_without_pipe_deadlock(self):
        child = self.start("import sys; print('ready',flush=True); data=sys.stdin.buffer.read(); print(len(data),flush=True)", input_data=b'a' * 300000)
        result = child.wait(5)
        self.assertEqual(result['exitCode'], 0)
        self.assertTrue(result['treeStopped'])
        self.assertIn('300000', result['stdout'])

    def test_stop_kills_child_and_grandchild_and_heartbeat_stays_frozen(self):
        beat = "from pathlib import Path; import time; p=Path('beat');\nwhile True:\n p.write_text(str(time.time())); time.sleep(.02)"
        middle = f"import subprocess,sys,time; subprocess.Popen([sys.executable,'-I','-c',{beat!r}]); time.sleep(60)"
        child = self.start(f"import subprocess,sys,time; subprocess.Popen([sys.executable,'-I','-c',{middle!r}]); time.sleep(60)")
        self.wait_file('beat')
        self.assertTrue(child.stop())
        before = (self.root / 'beat').read_bytes()
        time.sleep(.2)
        self.assertEqual((self.root / 'beat').read_bytes(), before)
        self.assertEqual(child.active_processes(), 0)

    def test_parent_exit_does_not_release_tree_while_child_keeps_writing(self):
        beat = "from pathlib import Path; import time; p=Path('orphan');\nwhile True:\n p.write_text(str(time.time())); time.sleep(.02)"
        child = self.start(f"import subprocess,sys; subprocess.Popen([sys.executable,'-I','-c',{beat!r}])")
        self.wait_file('orphan')
        self.assertFalse(child.tree_stopped())
        result = child.wait(.1)
        self.assertTrue(result['timedOut'])
        self.assertTrue(result['treeStopped'])
        self.assertEqual(child.active_processes(), 0)

    def test_timeout_terminates_running_tree(self):
        child = self.start("import time; print('started',flush=True); time.sleep(60)")
        result = child.wait(.2)
        self.assertTrue(result['timedOut'])
        self.assertTrue(result['treeStopped'])
        self.assertNotEqual(result['exitCode'], 0)

    def test_output_limit_stops_flood_instead_of_unbounded_memory(self):
        child = self.start("import sys;\nwhile True: sys.stdout.write('x'*8192); sys.stdout.flush()", output_limit=32768)
        result = child.wait(5)
        self.assertTrue(result['outputLimited'])
        self.assertLessEqual(len(result['stdout'].encode()), 32768)
        self.assertTrue(result['treeStopped'])

    def test_job_closes_when_supervisor_crashes(self):
        beat = "from pathlib import Path; import time; p=Path('crashbeat');\nwith p.open('ab', buffering=0) as output:\n while True:\n  output.write(b'beat\\n'); time.sleep(.02)"
        owner_code = f"""import importlib.util,os,sys,time
from pathlib import Path
s=importlib.util.spec_from_file_location('wj',{str(PATH)!r}); m=importlib.util.module_from_spec(s);s.loader.exec_module(m)
p=m.JobProcess([sys.executable,'-I','-c',{beat!r}],{str(self.root)!r})
while not Path('crashbeat').exists() or not Path('crashbeat').stat().st_size: time.sleep(.02)
os._exit(7)
"""
        owner = subprocess.run([sys.executable, '-I', '-c', owner_code], cwd=self.root, capture_output=True, timeout=8)
        self.assertEqual(owner.returncode, 7, owner.stderr.decode(errors='replace'))
        self.wait_file('crashbeat')
        time.sleep(.15)
        before = (self.root / 'crashbeat').read_bytes()
        time.sleep(.2)
        self.assertEqual((self.root / 'crashbeat').read_bytes(), before)

    def test_stopped_tree_has_signaled_every_process_and_released_locked_files(self):
        kernel = ctypes.WinDLL('kernel32', use_last_error=True)
        kernel.OpenProcess.argtypes = [w.DWORD, w.BOOL, w.DWORD]
        kernel.OpenProcess.restype = w.HANDLE
        kernel.WaitForSingleObject.argtypes = [w.HANDLE, w.DWORD]
        kernel.WaitForSingleObject.restype = w.DWORD
        kernel.CloseHandle.argtypes = [w.HANDLE]
        holder = """import msvcrt,os,time
from pathlib import Path
f=Path('held.lock').open('w+b'); f.write(b'x'); f.flush(); f.seek(0)
msvcrt.locking(f.fileno(),msvcrt.LK_NBLCK,1)
Path('holder-pid').write_text(str(os.getpid()))
time.sleep(60)
"""
        child = self.start(f"import subprocess,sys,time; subprocess.Popen([sys.executable,'-I','-c',{holder!r}]); time.sleep(60)")
        self.wait_file('holder-pid')
        handle = kernel.OpenProcess(0x100000 | 0x1000, False, int((self.root / 'holder-pid').read_text()))
        self.assertTrue(handle)
        self.addCleanup(kernel.CloseHandle, handle)
        self.assertTrue(child.stop())
        root_signal = kernel.WaitForSingleObject(child.process, 0)
        holder_signal = kernel.WaitForSingleObject(handle, 0)
        self.assertEqual((root_signal, holder_signal), (0, 0), 'Stopped must mean signaled process handles, not just early job accounting')
        (self.root / 'held.lock').unlink()
        self.assertFalse((self.root / 'held.lock').exists())

    def test_stdout_observation_precedes_exit_and_stderr_is_not_observed(self):
        import threading
        observed, chunks = threading.Event(), []
        def observer(data):
            if isinstance(data, bytes):
                chunks.append(data)
                observed.set()
        try:
            child = self.start("import sys,time; print('synthetic-visible',flush=True); print('private-stderr',file=sys.stderr,flush=True); time.sleep(60)", stdout_observer=observer)
        except TypeError:
            self.fail('Real stdout must be observable before wait returns')
        self.assertTrue(observed.wait(3))
        self.assertIsNone(child.poll())
        self.assertNotIn(b'private-stderr', b''.join(chunks))
        self.assertTrue(child.stop())
        self.assertTrue(child.wait(2)['treeStopped'])

    def test_slow_or_broken_observer_cannot_block_pipe_or_tree_exit(self):
        import threading
        entered, release = threading.Event(), threading.Event()
        def slow(data):
            entered.set()
            release.wait(8)
        try:
            child = self.start("import os; os.write(1,b'x'*(2*1024*1024))", stdout_observer=slow)
        except TypeError:
            self.fail('Observation must have a bounded nonblocking queue')
        try:
            self.assertTrue(entered.wait(3))
            before = time.monotonic()
            result = child.wait(5)
            self.assertLess(time.monotonic() - before, 2.5)
            self.assertEqual(result['exitCode'], 0)
            self.assertTrue(result['treeStopped'])
            self.assertFalse(result['outputLimited'])
            self.assertTrue(result['observationGap'])
        finally:
            release.set()
        def broken(data):
            raise RuntimeError('synthetic observer failure')
        other = self.start("print('ok',flush=True)", stdout_observer=broken)
        result = other.wait(3)
        self.assertEqual(result['exitCode'], 0)
        self.assertTrue(result['treeStopped'])
        self.assertTrue(result['observationFailed'])
        self.assertLessEqual(result['observerQueueCapacity'], 64)

    def test_wait_deadline_uses_actual_wait_start_not_queue_or_process_start(self):
        import threading
        try:
            child = self.start('import time; time.sleep(60)', stdout_observer=lambda data: None)
        except TypeError:
            self.fail('Execution metadata must expose the actual timeout boundary')
        threading.Event().wait(.1)
        waited = time.time() * 1000
        result = child.wait(.2)
        self.assertTrue(result['timedOut'])
        self.assertGreaterEqual(result['waitStartedAt'], waited - 20)
        self.assertAlmostEqual(result['timeoutAt'] - result['waitStartedAt'], 200, delta=5)
        self.assertGreater(result['waitStartedAt'], result['startedAt'])

    def test_close_of_successful_process_is_idempotent(self):
        child = self.start("print('ok')")
        self.assertEqual(child.wait(5)['exitCode'], 0)
        child.close()
        child.close()
        self.assertTrue(child.tree_stopped())


if __name__ == '__main__':
    unittest.main()
