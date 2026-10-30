"""Real path propagation and state; process boundaries never invoke a model."""
import json
from dataclasses import replace
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
import engine
import environment
from environment import RuntimePaths
from reason_mcp import ReasonTransport
import runtime
from state import Store


class NativeWiringTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-path-wiring-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.paths = RuntimePaths(**{key: self.root / ('selected-' + key)
            for key in RuntimePaths.__dataclass_fields__})
        self.claude = self.root / 'selected-claude'

    def test_selected_paths_reach_actual_mcp_json_without_weakening_claude_argv(self):
        # Break caught: selected native Python discarded in favor of imported PATHS.
        try:
            argv, source = runtime.cli_command('Claude Code', self.root, 'public task',
                'http://127.0.0.1:1/mcp', 'fixture-only', executable=self.claude, paths=self.paths)
        except TypeError as error:
            self.fail('Explicit runtime paths are not accepted: ' + str(error))
        config = json.loads((self.root / 'relay-mcp.json').read_text())['mcpServers']['relay']
        self.assertEqual(config['command'], str(self.paths.pythonConsole))
        self.assertEqual(config['args'][:3], ['-I', '-X', 'utf8'])
        self.assertEqual(argv[0], str(self.claude))
        self.assertEqual(source, b'public task')
        self.assertEqual(argv[argv.index('--tools') + 1], '')
        self.assertEqual(argv[argv.index('--permission-mode') + 1], 'dontAsk')
        self.assertEqual(argv[argv.index('--allowedTools') + 1], 'mcp__relay__*')
        self.assertIn('--strict-mcp-config', argv)
        self.assertNotIn('bypass', ' '.join(argv).lower())

    def test_selected_node_reaches_script_client_without_mutating_module_defaults(self):
        # Break caught: a script client still starts the global Windows Node.
        original = runtime.PATHS
        try:
            argv, _ = runtime.cli_command('BoBot', self.root, 'fixture',
                'http://127.0.0.1:1/mcp', 'fixture-only', paths=self.paths)
        except TypeError as error:
            self.fail('Explicit runtime paths are not accepted: ' + str(error))
        self.assertEqual(argv[0], str(self.paths.node))
        self.assertIs(runtime.PATHS, original)

    def worker(self, store, setup):
        worker = engine.Engine(store, setup=setup)
        # Exercise _execute deterministically without the background queue racing it.
        worker.closed.set()
        worker.wake.set()
        worker.thread.join(3)
        self.assertFalse(worker.thread.is_alive())
        self.addCleanup(worker.close)
        return worker

    def queued(self, store):
        project = store.create_project('path wiring fixture')
        return store.command(project['id'], uuid.uuid4().hex, project['revision'], 'generate', {})

    def test_real_engine_runner_mcp_checker_and_reason_use_post_snapshot_paths(self):
        # Break caught: setup succeeds but Engine/RoleRunner/CheckRunner keep stale globals.
        store = Store(self.root / 'data')
        queued = self.queued(store)
        selected, claude, observed = self.paths, self.claude, {}

        class SetupBoundary:
            def execution_snapshot(self):
                observed['beforeClaim'] = store.get(queued['id'])['activeAttempt']['status']
                return environment.ExecutionSnapshot(claude, selected, 'a' * 64)

        class ReasonBoundary(ReasonTransport):
            def start(self):
                observed['reason'] = self.exe
            def register(self, name):
                return 'fixture/' + name
            def send(self, *args):
                return 'fixture-message'

        class NoCliBoundary:
            def __init__(self, argv, workspace, **kwargs):
                observed['argv'] = argv
                observed['mcp'] = json.loads((Path(workspace) / 'relay-mcp.json').read_text())
                raise RuntimeError('Fixture stops before starting any CLI; not model execution')

        worker = self.worker(store, SetupBoundary())
        with patch('engine.ReasonTransport', ReasonBoundary), patch('runtime.JobProcess', NoCliBoundary):
            worker._execute(queued['id'])
        self.assertEqual(observed['beforeClaim'], 'queued')
        self.assertEqual(observed.get('reason'), selected.reason)
        self.assertIs(worker.checker.paths, selected)
        self.assertEqual(observed['argv'][0], str(claude))
        self.assertEqual(observed['mcp']['mcpServers']['relay']['command'], str(selected.pythonConsole))
        state = store.get(queued['id'])
        self.assertIsNone(state['activeAttempt'])
        self.assertEqual(state['phase'], 'blocked')
        self.assertIn('Fixture stops before starting any CLI', state['lastError'])
        with store._connect() as db:
            self.assertEqual(db.execute('SELECT status FROM attempts WHERE id=?',
                (queued['activeAttempt']['id'],)).fetchone()[0], 'failed')
        self.assertFalse(state['leaseUnknown'])
        config = self.root / 'data/projects' / queued['id'] / 'relay-mcp.json'
        self.assertEqual(json.loads(config.read_text()), {'mcpServers': {}})

    def selection(self, paths=None, reason_hash='a' * 64):
        selection = getattr(environment, 'ExecutionSnapshot', None)
        self.assertTrue(callable(selection), 'Atomic execution selection is missing')
        return selection(self.claude, paths or self.paths, reason_hash)

    def test_engine_consumes_one_atomic_selection_without_rereading_mutable_setup(self):
        # Break caught: a recheck changes setup.paths between authorization and dispatch.
        selected = self.selection()
        store = Store(self.root / 'data')
        queued = self.queued(store)
        class SetupBoundary:
            def execution_snapshot(self):
                return selected
            def snapshot(self):
                raise AssertionError('Legacy split snapshot must not authorize Engine')
            @property
            def paths(self):
                raise AssertionError('Mutable paths reread after atomic selection')
        class StopBeforeDaemon(ReasonTransport):
            def start(self):
                raise RuntimeError('Fixture stops before daemon execution')
        worker = self.worker(store, SetupBoundary())
        with patch('engine.ReasonTransport', StopBeforeDaemon):
            worker._execute(queued['id'])
        self.assertIs(worker.checker.paths, selected.paths)
        state = store.get(queued['id'])
        self.assertIsNone(state['activeAttempt'])
        self.assertIn('Fixture stops before daemon execution', state['lastError'])

    def test_incomplete_explicit_setup_cannot_fall_back_or_claim_attempt(self):
        # Break caught: successful legacy snapshot with paths=None silently selects globals.
        store = Store(self.root / 'data')
        queued = self.queued(store)
        claude = self.claude
        class IncompleteSetup:
            paths = None
            def snapshot(self):
                return claude
            def execution_snapshot(self):
                return None
        class StopBeforeDaemon:
            def __init__(self, *args):
                raise RuntimeError('Fixture prevents accidental process execution')
        worker = self.worker(store, IncompleteSetup())
        before = store.get(queued['id'])
        with patch('engine.ReasonTransport', StopBeforeDaemon):
            worker._execute(queued['id'])
        self.assertEqual(store.get(queued['id']), before)
        self.assertIsNone(worker.transport)

    def test_windows_execution_selection_keeps_path_snapshot_compatibility(self):
        # Break caught: changing detection after selection mixes Claude and runtime paths.
        self.paths.reason.write_bytes(b'fixture reason bytes, never executed')
        setup = environment.SetupStatus(self.root, paths=self.paths, discover=lambda: None)
        self.addCleanup(setup.close)
        atomic = getattr(setup, 'execution_snapshot', None)
        self.assertTrue(callable(atomic), 'Windows setup lacks an atomic execution snapshot')
        with self.assertRaises(ValueError):
            atomic()
        setup.candidate = Path(sys.executable)
        setup.state['canExecute'] = True  # Explicit capability fixture, not real readiness.
        selected = atomic()
        self.assertEqual(setup.snapshot(), Path(sys.executable))
        self.assertEqual(selected.executable, Path(sys.executable))
        self.assertIs(selected.paths, self.paths)
        import hashlib
        self.assertEqual(selected.reason_sha256, hashlib.sha256(b'fixture reason bytes, never executed').hexdigest())
        setup.paths = None
        setup.candidate = self.claude
        self.assertIs(selected.paths, self.paths)
        self.assertEqual(selected.executable, Path(sys.executable))

    def test_reason_generation_change_is_rejected_before_claim_including_same_path_bytes(self):
        # Break caught: one daemon is reused after selected path OR pinned bytes change.
        for changed_path in (False, True):
            with self.subTest(changed_path=changed_path):
                root = self.root / str(changed_path)
                store = Store(root / 'data')
                first = self.queued(store)
                selected = self.selection()
                class SetupBoundary:
                    def execution_snapshot(self):
                        return selected
                class ReasonBoundary(ReasonTransport):
                    def start(self):
                        pass  # No daemon/model: only lifecycle and real state are under test.
                class StopBeforeRole:
                    def __init__(self, *args, **kwargs):
                        self.executable = None
                    def stop(self):
                        return True
                    def run(self, *args, **kwargs):
                        raise RuntimeError('Fixture stops before any CLI')
                worker = self.worker(store, SetupBoundary())
                with patch('engine.ReasonTransport', ReasonBoundary), patch('engine.RoleRunner', StopBeforeRole):
                    worker._execute(first['id'])
                    old_transport = worker.transport
                    self.assertIsNotNone(old_transport)
                    paths = replace(self.paths, reason=self.root / 'changed-reason') if changed_path else self.paths
                    selected = self.selection(paths, 'b' * 64)
                    second = self.queued(store)
                    before = store.get(second['id'])
                    worker._execute(second['id'])
                self.assertEqual(store.get(second['id']), before)
                self.assertIs(worker.transport, old_transport)

    def test_failed_reason_start_is_cleared_only_after_confirmed_owned_stop(self):
        # Break caught: failed start leaves a non-None transport reused without start().
        store = Store(self.root / 'data')
        queued = self.queued(store)
        selected = self.selection()
        class SetupBoundary:
            def execution_snapshot(self):
                return selected
        class OwnedBoundary:
            def poll(self):
                return 1
            def stop(self):
                return True
            def tree_stopped(self):
                return True
            def close(self):
                pass
        worker = self.worker(store, SetupBoundary())
        with patch('reason_mcp.JobProcess', return_value=OwnedBoundary()), patch('reason_mcp.time.sleep'):
            worker._execute(queued['id'])
        state = store.get(queued['id'])
        self.assertIsNone(worker.transport)
        self.assertIsNone(state['activeAttempt'])
        self.assertIn('Dedicated reason daemon failed to start', state['lastError'])
        self.assertFalse(state['leaseUnknown'])

    def test_failed_reason_start_with_unknown_tree_retains_lease_and_blocks_other_project(self):
        # Break caught: close() returning None is treated as proof, discarding an owned daemon.
        store = Store(self.root / 'data')
        queued = self.queued(store)
        selected = self.selection()
        class SetupBoundary:
            def execution_snapshot(self):
                return selected
        class UnknownOwnedBoundary:
            settled = False
            def poll(self):
                return 1
            def stop(self):
                return self.settled
            def tree_stopped(self):
                return self.settled
            def close(self):
                pass
        process = UnknownOwnedBoundary()
        # The test finally establishes fixture stop before the real Engine cleanup.
        self.addCleanup(lambda: setattr(process, 'settled', True))
        worker = self.worker(store, SetupBoundary())
        with patch('reason_mcp.JobProcess', return_value=process), patch('reason_mcp.time.sleep'):
            worker._execute(queued['id'])
        self.assertIsNotNone(worker.transport)
        self.assertIs(worker.transport.daemon, process)
        self.assertTrue(store.get(queued['id'])['leaseUnknown'])
        second = self.queued(store)
        before = store.get(second['id'])
        worker._execute(second['id'])
        self.assertEqual(store.get(second['id']), before)
        process.settled = True

    def test_ambiguous_reason_process_construction_never_reports_confirmed_stop(self):
        # Break caught: constructor can submit a process then raise before returning a handle.
        store = Store(self.root / 'data')
        queued = self.queued(store)
        selected = self.selection()
        class SetupBoundary:
            def execution_snapshot(self):
                return selected
        worker = self.worker(store, SetupBoundary())
        with patch('reason_mcp.JobProcess', side_effect=RuntimeError('Fixture ambiguous process creation')):
            worker._execute(queued['id'])
        state = store.get(queued['id'])
        # No process was really launched by this fixture; test only the unknown outcome policy.
        self.assertIsNotNone(worker.transport)
        self.assertTrue(state['leaseUnknown'])
        self.assertFalse(worker.stop(queued['id']))
        # Avoid treating teardown itself as an owned stop proof in this fixture-only case.
        worker.transport = None

    def test_snapshot_rejection_does_not_read_paths_or_change_real_queued_state(self):
        # Break caught: path access/attempt claim happens before the setup authority check.
        store = Store(self.root / 'data')
        queued = self.queued(store)
        class RejectedSetup:
            @property
            def paths(self):
                raise AssertionError('Paths read before setup snapshot passed')
            def execution_snapshot(self):
                raise ValueError('Changed native manifest')
        worker = self.worker(store, RejectedSetup())
        before = store.get(queued['id'])
        worker._execute(queued['id'])
        self.assertEqual(store.get(queued['id']), before)
        self.assertIsNone(worker.transport)


if __name__ == '__main__':
    unittest.main()
