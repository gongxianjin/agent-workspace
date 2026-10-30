"""Real scheduler/state concurrency; paid CLI/reason are explicit boundary fixtures."""
import copy
import json
from pathlib import Path
import sys
import tempfile
import threading
import unittest
from unittest.mock import patch
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
import engine
from state import Store


class TransportFixture:
    def __init__(self, *args):
        pass
    def start(self):
        pass
    def close(self):
        pass


class EngineTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-engine-')
        self.addCleanup(self.temp.cleanup)
        self.store = Store(Path(self.temp.name))
        self.entered, self.release = threading.Event(), threading.Event()
        self.left = threading.Event()
        self.clean = True
        outer = self

        class RunnerFixture:
            def __init__(self, transport, root):
                self.transport = transport
                self.active = None
            def stop(self):
                return outer.clean
            def run(self, attempt, client, workspace, payload, **kwargs):
                outer.entered.set()
                outer.release.wait(8)
                outer.left.set()
                # Complete protocol metadata, but explicitly NOT a real AI in these tests.
                return {'attemptId': attempt['id'], 'client': client, 'memberId': 'fixture', 'address': 'fixture/member',
                    'taskMessageId': 'fixture-task', 'ackMessageId': 'fixture-ack', 'resultMessageId': 'fixture-result',
                    'pid': 1, 'exitCode': 0, 'treeStopped': True, 'checks': [], 'productManifest': {'files': [], 'sha256': ''},
                    'result': {'markdown': '# fixture design', 'criteria': ['fixture criterion']}}

        self.addCleanup(patch.stopall)
        patch('engine.RoleRunner', RunnerFixture).start()
        patch('engine.ReasonTransport', TransportFixture).start()
        self.worker = engine.Engine(self.store)
        def cleanup():
            self.release.set()
            self.worker.close()
        self.addCleanup(cleanup)

    def command(self, project, kind):
        current = self.store.get(project['id'])
        return self.store.command(project['id'], uuid.uuid4().hex, current['revision'], kind, {})

    def test_stop_does_not_confirm_until_running_host_work_has_left(self):
        project = self.store.create_project('停止屏障')
        self.command(project, 'generate')
        self.worker.notify()
        self.assertTrue(self.entered.wait(3))
        self.command(project, 'stop')
        stopped, result = threading.Event(), []
        def stop():
            result.append(self.worker.stop(project['id']))
            stopped.set()
        thread = threading.Thread(target=stop)
        thread.start()
        try:
            self.assertFalse(stopped.wait(.15), 'Stopping the CLI is not confirmation that host work has left')
        finally:
            self.release.set()
            thread.join(4)
        self.assertTrue(stopped.is_set())
        self.assertEqual(result, [True])
        self.assertTrue(self.left.is_set())
        self.assertEqual(self.store.get(project['id'])['phase'], 'stopping')

    def test_stop_of_queued_project_never_blocks_other_projects(self):
        first = self.store.create_project('正在运行')
        self.command(first, 'generate')
        self.worker.notify()
        self.assertTrue(self.entered.wait(3))
        second = self.store.create_project('排队取消')
        self.command(second, 'generate')
        self.command(second, 'stop')
        self.assertTrue(self.worker.stop(second['id']), 'An unclaimed attempt must not require a nonexistent process proof')
        self.assertEqual(self.store.get(second['id'])['phase'], 'stopped')
        third = self.store.create_project('后续可调度')
        self.command(third, 'generate')
        self.release.set()
        self.worker.notify()
        for _ in range(150):
            if self.store.get(third['id'])['phase'] == 'document':
                break
            threading.Event().wait(.02)
        self.assertEqual(self.store.get(third['id'])['phase'], 'document')
        self.assertFalse(any(p['leaseUnknown'] for p in self.store.list_projects()))

    def test_recovered_unknown_lease_is_not_confirmed_by_an_empty_new_engine(self):
        project = self.store.create_project('恢复屏障')
        self.command(project, 'generate')
        self.store.claim_attempt(project['id'])
        recovered = Store(self.store.root)
        state = recovered.get(project['id'])
        self.assertTrue(state['recoveryRequired'])
        self.assertFalse(self.worker.stop(project['id']), 'No current runner is not proof that an old runner exited')

    def test_unknown_writer_blocks_dispatch_in_other_projects(self):
        first = self.store.create_project('未知写入者')
        self.command(first, 'generate')
        attempt = self.store.claim_attempt(first['id'])
        self.store.fail_attempt(attempt['id'], 'fixture unknown process', confirmed=False)
        second = self.store.create_project('不能抢写')
        self.command(second, 'generate')
        self.worker.notify()
        self.assertFalse(self.entered.wait(.8), 'Unknown global writer must block a second project')
        self.assertEqual(self.store.get(second['id'])['activeAttempt']['status'], 'queued')

    def test_environment_gate_before_claim_preserves_queue_without_spending_retry(self):
        class MissingSetup:
            def snapshot(self):
                raise ValueError('synthetic missing local CLI')
        self.worker.setup = MissingSetup()
        project = self.store.create_project('环境门禁')
        queued = self.command(project, 'generate')
        self.worker.notify()
        self.assertFalse(self.entered.wait(.8))
        state = self.store.get(project['id'])
        self.assertEqual(state['revision'], queued['revision'])
        self.assertEqual(state['activeAttempt']['status'], 'queued')
        self.assertEqual(state['activeAttempt']['retryCount'], 0)

    def test_telemetry_seals_after_authoritative_completion_without_changing_revision(self):
        self.assertIsNotNone(getattr(self.worker, 'telemetry', None), 'Engine needs independently scoped observation, not state log mutations')
        project = self.store.create_project('遥测不派工')
        self.command(project, 'generate')
        self.worker.notify()
        self.assertTrue(self.entered.wait(3))
        scope = self.worker.telemetry.find(project['id'])
        self.assertIsNotNone(scope)
        before = self.store.get(project['id'])
        for _ in range(100):
            self.worker.telemetry.append(scope, {'kind': 'text', 'text': 'synthetic observer text'})
        self.assertEqual(self.store.get(project['id']), before)
        self.release.set()
        for _ in range(150):
            if self.worker.telemetry.read(scope, 0)['sealed']:
                break
            threading.Event().wait(.02)
        self.assertTrue(self.worker.telemetry.read(scope, 0)['sealed'])
        self.assertEqual(self.store.get(project['id'])['phase'], 'document')
        self.assertFalse(self.worker.telemetry.append(scope, {'kind': 'text', 'text': 'late'}))

    def test_unready_environment_does_not_spin_continuously_on_queued_attempt(self):
        import time
        visits = []
        class MissingSetup:
            def snapshot(self):
                visits.append(time.monotonic())
                raise ValueError('synthetic missing')
        self.worker.setup = MissingSetup()
        project = self.store.create_project('门禁不忙转')
        self.command(project, 'generate')
        self.worker.notify()
        threading.Event().wait(.25)
        self.assertLessEqual(len(visits), 2, 'Failed preclaim gate must wait, not immediately wake itself forever')

    def test_real_check_failure_reaches_blocked_state_without_unknown_evidence_mask(self):
        worker = self.worker
        worker.closed.set()
        worker.wake.set()
        worker.thread.join(3)
        self.assertFalse(worker.thread.is_alive())
        expected = 'synthetic snapshot disk full'
        class FailedCheckBoundary:
            def __init__(self, *args):
                pass
            def stop(self):
                return True
            def run(self, attempt, client, workspace, payload, **kwargs):
                (workspace / 'tests/browser.json').write_text(json.dumps({'version': 1, 'cases': [
                    {'name': 'fixture browser', 'steps': [{'op': 'text', 'selector': 'p', 'value': 'fixture'}]}]}), encoding='utf-8')
                try:
                    with patch('checks.shutil.copyfile', side_effect=OSError(expected)):
                        kwargs['checker'](workspace, worker.store.root / 'evidence' / attempt['id'])
                except OSError:
                    pass
                return {'result': {'summary': 'Browser could not run', 'evidenceId': 'no-evidence-issued'}, 'checks': []}
        project = self.store.create_project('原始错误保留')
        def command(kind, payload=None):
            current = self.store.get(project['id'])
            return self.store.command(project['id'], uuid.uuid4().hex, current['revision'], kind, payload or {})
        command('upload', {'name': 'fixture.md', 'text': '# fixture'})
        command('split')
        attempt = self.store.claim_attempt(project['id'])
        tasks = [dict(id='D1', owner='developer', title='fixture', detail='fixture', acceptance=['fixture'], dependsOn=[]),
            dict(id='T1', owner='tester', title='fixture', detail='fixture', acceptance=['fixture'], dependsOn=['D1'])]
        self.store.complete_attempt(attempt['id'], {'tasks': tasks})
        for task in self.store.get(project['id'])['tasks']:
            command('approve', {'taskId': task['id'], 'taskVersion': task['version'], 'documentVersion': task['documentVersion']})
        command('start')
        attempt = self.store.claim_attempt(project['id'])
        self.store.complete_attempt(attempt['id'], {'summary': 'fixture development', 'artifacts': []})
        (self.store.root / 'projects' / project['id'] / 'product/index.html').write_text('<p>fixture</p>', encoding='utf-8')
        self.assertEqual(self.store.get(project['id'])['activeAttempt']['role'], 'tester')
        with patch('engine.RoleRunner', FailedCheckBoundary):
            worker._execute(project['id'])
        blocked = self.store.get(project['id'])
        self.assertEqual(blocked['phase'], 'blocked')
        self.assertIn(expected, blocked['lastError'])
        self.assertNotIn('Unknown evidence', blocked['lastError'])
        self.assertIsNone(blocked['activeAttempt'])
        self.assertFalse(blocked['leaseUnknown'])
        self.assertEqual(worker.checker.issued, {})
        self.assertFalse((self.store.root / 'frozen-tests').exists())
        self.assertEqual(blocked['run']['evidence'], [])

    def test_failed_cleanup_keeps_lease_unknown(self):
        project = self.store.create_project('清理失败')
        self.clean = False
        # A malformed result causes the real state validator to reject completion.
        with patch('engine.payload_for', side_effect=ValueError('fixture dispatch failure')):
            self.command(project, 'generate')
            self.worker.notify()
            deadline = threading.Event()
            for _ in range(100):
                current = self.store.get(project['id'])
                if current['phase'] == 'blocked':
                    break
                deadline.wait(.02)
        self.assertEqual(current['phase'], 'blocked')
        self.assertTrue(current['leaseUnknown'], 'Cleanup uncertainty must not become confirmed=True')


if __name__ == '__main__':
    unittest.main()
