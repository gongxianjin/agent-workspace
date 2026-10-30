"""Real systemd owned-process adapter contracts. No model clients or downloads."""
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


def run_owned_case(directory, case):
    sys.path.insert(0, str(ROOT / 'p1'))
    from process_control import JobProcess
    result = {}
    try:
        try:
            from linux_process import HostContext
        except ImportError:
            HostContext = None
        def exercise():
            if case == 'roundtrip':
                source = b'input-utf8-\xe6\xb5\x8b\xe8\xaf\x95'
                code = 'import os,sys;print(os.environ["RELAY_FIXTURE"]);sys.stdout.buffer.write(sys.stdin.buffer.read());print("error-byte",file=sys.stderr);sys.exit(7)'
                child = JobProcess([sys.executable, '-I', '-c', code], directory,
                    env=dict(os.environ, RELAY_FIXTURE='env-ok'), input_data=source)
                try:
                    data = child.wait(5)
                    result.update(available=True, process=data)
                finally:
                    child.close()
            elif case == 'runtime-stream':
                sys.path.insert(0, str(Path(__file__).parent))
                from test_runtime import RuntimeTests
                RuntimeTests().assert_inflight_stream_revoked()
                result.update(available=True, runtimeStreamRevoked=True)
            elif case == 'mcpstdio':
                from environment import RuntimePaths
                from runtime import cli_command
                console = Path(sys.executable).resolve()
                paths = RuntimePaths(console, console, directory / 'unused-node', directory / 'unused-reason',
                    directory / 'unused-package.json', directory / 'unused-browser')
                # Only the generated Python MCP command runs; no client, reason or model endpoint is called.
                cli_command('Claude Code', directory, 'synthetic-only', 'http://127.0.0.1:1/mcp',
                    'synthetic-only', paths=paths)
                config = json.loads((directory / 'relay-mcp.json').read_text())['mcpServers']['relay']
                requests = [{'jsonrpc': '2.0', 'id': 1, 'method': 'initialize'},
                    {'jsonrpc': '2.0', 'id': 2, 'method': 'tools/list'}]
                source = ('\n'.join(json.dumps(request) for request in requests) + '\n').encode()
                child = JobProcess([config['command'], *config['args']], directory,
                    env=config['env'], input_data=source)
                try:
                    result.update(available=True, process=child.wait(8),
                        nativeConsole=Path(config['command']) == console)
                finally:
                    child.close()
                    (directory / 'relay-mcp.json').write_text('{"mcpServers":{}}')
            elif case == 'polling':
                import linux_process as owned
                original = owned.properties
                late_queries = []
                start = time.monotonic()
                def measured_properties(unit):
                    if unit.startswith('relay-child-') and time.monotonic() - start >= 2:
                        late_queries.append(time.monotonic())
                    return original(unit)
                with patch.object(owned, 'properties', measured_properties):
                    child = JobProcess([sys.executable, '-I', '-c', 'import time;time.sleep(4)'], directory)
                    try:
                        data = child.wait(6)
                    finally:
                        child.close()
                result.update(available=True, process=data, lateManagerQueries=len(late_queries))
            elif case == 'prebind':
                import linux_process as owned
                identifier = uuid.UUID('12345678123456781234567812345678')
                failure_seen = recovered = reopened = False
                with socket.socket(socket.AF_UNIX, socket.SOCK_STREAM) as reserved:
                    reserved.bind('\0relay-process-' + identifier.hex)
                    with patch.object(owned.uuid, 'uuid4', return_value=identifier):
                        try:
                            JobProcess([sys.executable, '-I', '-c', 'from pathlib import Path;Path("should-not-run").touch()'], directory)
                        except OSError:
                            failure_seen = True
                try:
                    child = JobProcess([sys.executable, '-I', '-c', 'print("recovered")'], directory)
                    try:
                        data = child.wait(3)
                        recovered = data['treeStopped'] and data['exitCode'] == 0
                    finally:
                        child.close()
                except Exception:
                    pass
                owned.active_context().close() if recovered else None
                if recovered:
                    with owned.HostContext(directory / 'ownership'):
                        reopened = True
                result.update(available=True, failureSeen=failure_seen, recovered=recovered,
                    reopened=reopened, targetRan=(directory / 'should-not-run').exists())
            elif case in ('pregrant', 'incompletegroup'):
                import linux_process as owned
                original_send, original_properties = owned.send_frame, owned.properties
                grant_attempted = [False]
                missing_group = [False]
                def grant_barrier(channel, kind, payload):
                    if kind == b'G':
                        grant_attempted[0] = True
                        child = next(iter(owned.active_context().children))
                        if case == 'pregrant':
                            (directory / 'child-identity.json').write_text(json.dumps({'unit': child.unit, 'group': child.group}))
                            threading.Event().wait(60)
                            raise RuntimeError('Fixture barrier unexpectedly released')
                        child.invocation = child.group = None
                        raise OSError('Fixture launch grant not sent')
                    return original_send(channel, kind, payload)
                def incomplete_properties(unit):
                    data = original_properties(unit)
                    if (case == 'incompletegroup' and grant_attempted[0] and unit.startswith('relay-child-')
                            and not missing_group[0] and data.get('InvocationID')):
                        missing_group[0] = True
                        return dict(data, ControlGroup='')
                    return data
                failed = recovered = False
                with patch.object(owned, 'send_frame', grant_barrier), patch.object(owned, 'properties', incomplete_properties):
                    try:
                        JobProcess([sys.executable, '-I', '-c', 'from pathlib import Path;Path("should-not-run").touch()'], directory)
                    except OSError:
                        failed = True
                if case == 'incompletegroup':
                    try:
                        child = JobProcess([sys.executable, '-I', '-c', 'print("recovered")'], directory)
                        try:
                            data = child.wait(3)
                            recovered = data['treeStopped'] and data['exitCode'] == 0
                        finally:
                            child.close()
                    except Exception:
                        pass
                    result.update(available=True, failed=failed, missingGroupInjected=missing_group[0],
                        recovered=recovered, targetRan=(directory / 'should-not-run').exists())
            elif case == 'observer':
                entered, release = threading.Event(), threading.Event()
                def stalled_observer(chunk):
                    if chunk:
                        entered.set()
                        release.wait(60)
                code = 'import signal,time;signal.signal(signal.SIGTERM,signal.SIG_IGN);print("ready",flush=True);time.sleep(60)'
                child = JobProcess([sys.executable, '-I', '-c', code], directory, stdout_observer=stalled_observer)
                try:
                    if not entered.wait(3):
                        raise RuntimeError('Observer fixture did not receive target output')
                    started = time.monotonic()
                    data = child.wait(.15)
                    result.update(available=True, process=data, waitSeconds=time.monotonic() - started)
                finally:
                    release.set()
                    child.close()
            elif case == 'unknown':
                child = JobProcess([sys.executable, '-I', '-c', 'import time;time.sleep(60)'], directory)
                invocation = child.invocation
                child.invocation = 'mismatched-fixture-identity'
                child.close()
                rejected = False
                second = None
                try:
                    second = JobProcess([sys.executable, '-I', '-c', 'from pathlib import Path;Path("should-not-run").write_text("bad")'], directory)
                except Exception:
                    rejected = True
                finally:
                    if second:
                        second.close()
                    child.invocation = invocation
                    child.stop()
                result.update(available=True, rejected=rejected, targetRan=(directory / 'should-not-run').exists())
            elif case in ('descendant', 'timeout', 'flood', 'hostdeath'):
                if case == 'descendant':
                    grandchild = 'import os,time;from pathlib import Path;os.setsid();p=Path("beat");\nwhile True:\n p.write_text(str(time.time_ns()));time.sleep(.01)'
                    code = 'import subprocess,sys;subprocess.Popen([sys.executable,"-I","-c",' + repr(grandchild) + ']);sys.exit(7)'
                elif case == 'flood':
                    code = 'import os;\nwhile True:os.write(1,b"x"*65536)'
                else:
                    code = 'import signal,time;from pathlib import Path;signal.signal(signal.SIGTERM,signal.SIG_IGN);Path("beat").write_text("ready");time.sleep(60)'
                child = JobProcess([sys.executable, '-I', '-c', code], directory,
                    output_limit=16384 if case == 'flood' else 8388608)
                try:
                    if case != 'flood':
                        deadline = time.monotonic() + 3
                        while not (directory / 'beat').exists() and time.monotonic() < deadline:
                            time.sleep(.01)
                        if not (directory / 'beat').exists():
                            raise RuntimeError('Target fixture not ready')
                    if case == 'hostdeath':
                        (directory / 'child-identity.json').write_text(json.dumps({'unit': child.unit, 'group': child.group}))
                        time.sleep(60)
                    result.update(available=True, process=child.wait(.15 if case != 'flood' else 5))
                    if case != 'flood':
                        before = (directory / 'beat').read_bytes()
                        time.sleep(.1)
                        result['heartbeatStopped'] = before == (directory / 'beat').read_bytes()
                finally:
                    child.close()
            else:
                raise ValueError('Unknown fixture case')
        if HostContext:
            with HostContext(directory / 'ownership'):
                exercise()
        else:
            exercise()
    except Exception as error:
        result = {'available': False, 'category': type(error).__name__, 'message': str(error)}
    (directory / 'case-result.json').write_text(json.dumps(result), encoding='utf-8')


@unittest.skipUnless(sys.platform == 'linux', 'Native systemd owned-process integration')
class LinuxProcessTests(unittest.TestCase):
    def systemctl(self, *args):
        return subprocess.run(['/usr/bin/systemctl', '--system' if os.getuid() == 0 else '--user', *args],
            capture_output=True, text=True, timeout=12)

    def run_case(self, directory, case):
        unit = 'relay-process-test-' + uuid.uuid4().hex + '.service'
        command = ['/usr/bin/systemd-run', '--system' if os.getuid() == 0 else '--user', '--quiet', '--collect',
            '--unit', unit, '--property=Type=exec', '--property=KillMode=control-group',
            '--property=TimeoutStopSec=2s', '--property=UMask=0077',
            sys.executable, '-I', '-B', str(Path(__file__)), '--owned-case', str(directory), case]
        started = subprocess.run(command, capture_output=True, text=True, timeout=12)
        self.assertEqual(started.returncode, 0, started.stderr)
        try:
            deadline = time.monotonic() + 18
            report = directory / 'case-result.json'
            while time.monotonic() < deadline:
                identity = directory / 'child-identity.json'
                if case in ('hostdeath', 'pregrant') and identity.exists():
                    child = json.loads(identity.read_text())
                    self.assertEqual(self.systemctl('kill', '--signal=SIGKILL', '--kill-who=main', unit).returncode, 0)
                    group = Path('/sys/fs/cgroup') / child['group'].lstrip('/')
                    while time.monotonic() < deadline:
                        if not group.exists() or 'populated 0' in (group / 'cgroup.events').read_text():
                            return {'emptyAfterHostDeath': True,
                                'targetRan': (directory / 'should-not-run').exists()}
                        time.sleep(.03)
                    self.fail('Child survived owned host death')
                if report.exists() and report.stat().st_size:
                    return json.loads(report.read_text())
                time.sleep(.03)
            self.fail('Owned host fixture did not finish')
        finally:
            self.systemctl('stop', unit)
            self.systemctl('reset-failed', unit)

    def test_owned_host_roundtrips_stdin_environment_exit_and_proves_tree_stopped(self):
        # Break caught: refusing all Linux processes, losing stdin/env or reporting parent-only exit.
        with tempfile.TemporaryDirectory(prefix='relay-owned-') as directory:
            root = Path(directory)
            outcome = self.run_case(root, 'roundtrip')
            self.assertTrue(outcome['available'], outcome)
            data = outcome['process']
            self.assertEqual(data['exitCode'], 7)
            self.assertTrue(data['treeStopped'])
            self.assertFalse(data['timedOut'])
            self.assertIn('env-ok', data['stdout'])
            self.assertIn('input-utf8-测试', data['stdout'])
            self.assertIn('error-byte', data['stderr'])
            records = list((root / 'ownership').rglob('*.json'))
            self.assertTrue(records, 'Owned launch must leave a private durable ledger')
            for record in records:
                self.assertEqual(record.stat().st_mode & 0o777, 0o600)
                self.assertNotIn('input-utf8', record.read_text())

    def test_runtime_revokes_inflight_stream_after_real_owned_child_exit(self):
        with tempfile.TemporaryDirectory(prefix='relay-owned-runtime-') as directory:
            outcome = self.run_case(Path(directory), 'runtime-stream')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['runtimeStreamRevoked'], outcome)

    def test_generated_mcp_config_roundtrips_real_native_console_stdio(self):
        with tempfile.TemporaryDirectory(prefix='relay-owned-mcp-') as directory:
            outcome = self.run_case(Path(directory), 'mcpstdio')
            self.assertTrue(outcome['available'], outcome)
            data = outcome['process']
            self.assertEqual(data['exitCode'], 0, data)
            self.assertTrue(data['treeStopped'], data)
            self.assertFalse(data['timedOut'], data)
            self.assertFalse(data['outputLimited'], data)
            self.assertEqual(data['stderr'], '')
            replies = [json.loads(line) for line in data['stdout'].splitlines()]
            self.assertEqual([reply['id'] for reply in replies], [1, 2])
            self.assertEqual(replies[0]['result']['serverInfo']['name'], 'relay-scoped')
            self.assertIn('send_ack', [tool['name'] for tool in replies[1]['result']['tools']])
            self.assertTrue(outcome['nativeConsole'], outcome)

    def test_parent_exit_does_not_release_setsid_descendant_until_timeout_stop(self):
        # Break caught: using parent exit as tree-stop evidence.
        with tempfile.TemporaryDirectory(prefix='relay-owned-descendant-') as directory:
            outcome = self.run_case(Path(directory), 'descendant')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['process']['timedOut'])
            self.assertTrue(outcome['process']['treeStopped'])
            self.assertTrue(outcome['heartbeatStopped'])

    def test_timeout_escalates_ignored_term_and_stops_owned_group(self):
        # Break caught: a timeout that leaves a writer alive.
        with tempfile.TemporaryDirectory(prefix='relay-owned-timeout-') as directory:
            outcome = self.run_case(Path(directory), 'timeout')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['process']['timedOut'])
            self.assertTrue(outcome['process']['treeStopped'])
            self.assertTrue(outcome['heartbeatStopped'])

    def test_output_limit_stops_real_writer_and_bounds_collected_bytes(self):
        # Break caught: unbounded target output or marking overflow successful.
        with tempfile.TemporaryDirectory(prefix='relay-owned-flood-') as directory:
            outcome = self.run_case(Path(directory), 'flood')
            self.assertTrue(outcome['available'], outcome)
            data = outcome['process']
            self.assertTrue(data['outputLimited'])
            self.assertTrue(data['treeStopped'])
            self.assertLessEqual(len(data['stdout'].encode()) + len(data['stderr'].encode()), 16384)

    def test_unsettled_child_ownership_blocks_new_target_in_same_host(self):
        # Break caught: dropping an unknown child from memory and starting another writer.
        with tempfile.TemporaryDirectory(prefix='relay-owned-unknown-') as directory:
            outcome = self.run_case(Path(directory), 'unknown')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['rejected'])
            self.assertFalse(outcome['targetRan'])

    def test_long_wait_bounds_real_manager_queries_after_initial_settle_window(self):
        # Break caught: spawning systemctl at20ms throughout a model-length wait.
        with tempfile.TemporaryDirectory(prefix='relay-owned-polling-') as directory:
            outcome = self.run_case(Path(directory), 'polling')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['process']['treeStopped'])
            self.assertEqual(outcome['process']['exitCode'], 0)
            self.assertLessEqual(outcome['lateManagerQueries'], 14)

    def test_pre_submission_socket_failure_settles_ledger_and_allows_reopen(self):
        # Break caught: permanently poisoning ownership when no manager request was sent.
        with tempfile.TemporaryDirectory(prefix='relay-owned-prebind-') as directory:
            outcome = self.run_case(Path(directory), 'prebind')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['failureSeen'])
            self.assertTrue(outcome['recovered'])
            self.assertTrue(outcome['reopened'])
            self.assertFalse(outcome['targetRan'])
            records = [json.loads(p.read_text()) for p in (Path(directory) / 'ownership').glob('*.json')]
            self.assertTrue(any(row.get('launched') is False for row in records))
            self.assertTrue(all(row['status'] == 'stopped' for row in records))

    def test_pregrant_incomplete_group_is_repolled_before_owned_stop(self):
        # Break caught: permanently adopting an empty cgroup from startup metadata.
        with tempfile.TemporaryDirectory(prefix='relay-owned-incomplete-') as directory:
            outcome = self.run_case(Path(directory), 'incompletegroup')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['failed'])
            self.assertTrue(outcome['missingGroupInjected'])
            self.assertTrue(outcome['recovered'])
            self.assertFalse(outcome['targetRan'])

    def test_host_sigkill_before_grant_never_launches_target_and_stops_wrapper(self):
        # Break caught: launching a target before grant or relying on Python cleanup.
        with tempfile.TemporaryDirectory(prefix='relay-owned-pregrant-') as directory:
            outcome = self.run_case(Path(directory), 'pregrant')
            self.assertTrue(outcome['emptyAfterHostDeath'])
            self.assertFalse(outcome['targetRan'])

    def test_stalled_observer_does_not_block_timeout_or_stop_proof(self):
        # Break caught: synchronously invoking an observer on the process reader/stop path.
        with tempfile.TemporaryDirectory(prefix='relay-owned-observer-') as directory:
            outcome = self.run_case(Path(directory), 'observer')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['process']['treeStopped'])
            self.assertTrue(outcome['process']['timedOut'])
            self.assertTrue(outcome['process']['observationGap'])
            self.assertLess(outcome['waitSeconds'], 7)

    def test_host_sigkill_stops_bound_target_without_python_cleanup(self):
        # Break caught: relying on host finally instead of manager ownership.
        with tempfile.TemporaryDirectory(prefix='relay-owned-hostdeath-') as directory:
            outcome = self.run_case(Path(directory), 'hostdeath')
            self.assertTrue(outcome['emptyAfterHostDeath'])


if __name__ == '__main__':
    if sys.argv[1:2] == ['--owned-case']:
        run_owned_case(Path(sys.argv[2]), sys.argv[3])
    else:
        unittest.main()
