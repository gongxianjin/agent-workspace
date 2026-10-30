"""Serial consultation and explicit fresh-attempt resume using real SQLite/files."""
import copy
from pathlib import Path
import sys
import tempfile
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from state import Store, Rejected
from runtime import tree_manifest


class AgentConsultTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-agent-consult-')
        self.addCleanup(self.temp.cleanup)
        self.store = Store(Path(self.temp.name))
        self.project = self.store.create_project('串行咨询恢复')
        self.command('upload', {'name': 'fixture.md', 'text': '# fixture'})
        self.command('split')
        attempt = self.store.claim_attempt(self.project['id'])
        self.store.complete_attempt(attempt['id'], {'tasks': [
            {'id': 'D1', 'owner': 'developer', 'title': 'fixture', 'detail': 'fixture', 'acceptance': ['fixture'], 'dependsOn': []},
            {'id': 'T1', 'owner': 'tester', 'title': 'fixture', 'detail': 'fixture', 'acceptance': ['fixture'], 'dependsOn': ['D1']}]})
        for task in self.store.get(self.project['id'])['tasks']:
            self.command('approve', {'taskId': task['id'], 'taskVersion': task['version'], 'documentVersion': task['documentVersion']})
        self.command('start')
        self.attempt = self.store.claim_attempt(self.project['id'])
        self.workspace = self.store.root / 'projects' / self.project['id']

    def command(self, kind, payload=None):
        current = self.store.get(self.project['id'])
        return self.store.command(current['id'], uuid.uuid4().hex, current['revision'], kind, payload or {})

    def message(self, role, text='fixture question', intent='clarification'):
        self.command('agent_message', {'role': role, 'text': text, 'intent': intent, 'targetAttemptId': None})
        return self.store.agent_thread(self.project['id'], role)['messages'][-1]['id']

    def proof(self, attempt=None, key='terminalMessageId'):
        attempt = attempt or self.attempt
        return {'attemptId': attempt['id'], 'pid': 123, 'exitCode': 0,
            'treeStopped': True, 'handlersDrained': True, 'checksStopped': True,
            'taskMessageId': 'fixture-task', 'ackMessageId': 'fixture-ack', key: 'fixture-terminal',
            'productManifest': tree_manifest(self.workspace / 'product'), 'testManifest': tree_manifest(self.workspace / 'tests')}

    def block(self):
        return self.store.settle_controlled_attempt(self.attempt['id'],
            {'kind': 'blocked', 'reason': '需要说明'}, self.proof())

    def test_handoff_is_only_a_request_until_clean_correlated_terminal(self):
        before = self.store.get(self.project['id'])
        try:
            requested = self.command('agent_handoff')
        except Rejected as error:
            self.fail('An explicit handoff must request a cooperative safe point: ' + str(error))
        self.assertEqual(requested['activeAttempt']['id'], self.attempt['id'])
        self.assertEqual(requested['activeAttempt']['status'], 'running')
        self.assertEqual(requested['agentControl']['status'], 'handoff_requested')
        self.assertFalse(requested['agentControl']['resumable'])
        self.assertTrue(self.store.guidance_control(self.attempt['id'])['handoffRequested'])
        with self.assertRaises(Rejected):
            self.command('agent_consult', {'role': 'architect', 'messageIds': []})
        state = self.store.settle_controlled_attempt(self.attempt['id'],
            {'kind': 'handoff', 'reason': '公开交接：D1尚未完成'}, self.proof())
        self.assertEqual(state['phase'], 'paused')
        self.assertTrue(state['agentControl']['resumable'])
        self.assertEqual(state['run']['done'], [])
        self.assertEqual(state['tasks'], before['tasks'])

    def test_consultation_reply_never_completes_work_and_resume_uses_new_identity(self):
        self.block()
        question = self.message('architect', '请解释原设计')
        before = self.store.get(self.project['id'])
        try:
            queued = self.command('agent_consult', {'role': 'architect', 'messageIds': [question]})
        except Rejected as error:
            self.fail('Quiescent suspended work needs an explicit read-only consultation: ' + str(error))
        self.assertEqual(queued['activeAttempt']['kind'], 'consult')
        self.assertEqual(queued['activeAttempt']['role'], 'architect')
        self.assertEqual(queued['run'], before['run'])
        self.assertNotIn('请解释原设计', str(queued), 'Full state must not become a second chat transcript')
        consult = self.store.claim_attempt(self.project['id'])
        self.assertEqual(consult['input']['consultation']['messages'][0]['text'], '请解释原设计')
        self.assertEqual(self.store.guidance_control(consult['id'])['pending'], 0)
        with self.assertRaises(Rejected):
            self.command('agent_consult', {'role': 'tester', 'messageIds': []})
        with self.assertRaises(Rejected):
            self.store.complete_attempt(consult['id'], {'summary': '不能作为正常RESULT'})
        method = getattr(self.store, 'complete_consultation', None)
        self.assertTrue(callable(method), 'Consultation replies need separate typed settlement')
        paused = method(consult['id'], {'summary': '咨询回复只解释原设计'}, self.proof(consult, 'consultationMessageId'))
        self.assertIsNone(paused['activeAttempt'])
        self.assertEqual(paused['phase'], 'paused')
        self.assertEqual(paused['run'], before['run'])
        self.assertEqual(paused['tasks'], before['tasks'])
        self.assertNotIn('请解释原设计', str(paused))
        replies = self.store.agent_thread(self.project['id'], 'architect')['messages']
        self.assertEqual(replies[-1]['author'], 'agent')
        self.assertEqual(replies[-1]['text'], '咨询回复只解释原设计')
        self.assertEqual(replies[0]['status'], 'answered')
        explanation = self.message('developer', '在原批准范围内说明')
        resumed = self.command('agent_resume', {'messageIds': [explanation]})
        fresh = resumed['activeAttempt']
        self.assertEqual(fresh['kind'], 'develop')
        self.assertEqual(fresh['taskId'], 'D1')
        self.assertNotEqual(fresh['id'], self.attempt['id'])
        self.assertNotEqual(fresh['inputHash'], self.attempt['inputHash'])
        self.assertNotIn('在原批准范围内说明', str(resumed))
        claimed = self.store.claim_attempt(self.project['id'])
        self.assertEqual(claimed['input']['guidance'][0]['text'], '在原批准范围内说明')
        self.assertEqual(resumed['run']['done'], [])
        self.assertEqual(resumed['tasks'], before['tasks'])
        self.assertEqual(self.store.agent_thread(self.project['id'], 'developer')['messages'][0]['targetAttemptId'], fresh['id'])

    def test_global_writer_unknown_and_changed_snapshot_prevent_consult_or_resume(self):
        self.block()
        question = self.message('architect')
        other = self.store.create_project('另一项目排队')
        self.store.command(other['id'], 'other-queued', other['revision'], 'generate', {})
        for kind, payload in [('agent_consult', {'role': 'architect', 'messageIds': [question]}), ('agent_resume', {'messageIds': []})]:
            with self.subTest(kind=kind), self.assertRaises(Rejected):
                self.command(kind, payload)
        current = self.store.get(other['id'])
        self.store.command(other['id'], 'cancel-other', current['revision'], 'stop', {})
        (self.workspace / 'product/changed.txt').write_text('unexpected external edit')
        with self.assertRaises(Rejected):
            self.command('agent_resume', {'messageIds': []})
        self.assertEqual(self.store.get(self.project['id'])['run']['done'], [])

    def test_scope_change_cannot_resume_and_unselected_old_guidance_is_not_auto_delivered(self):
        old = self.message('developer', '旧阶段未采用说明')
        self.block()
        conflict = self.message('developer', '改变批准范围', 'scope_change')
        with self.assertRaises(Rejected):
            self.command('agent_resume', {'messageIds': [conflict]})
        self.command('agent_resolve', {'messageId': conflict, 'decision': 'supersede', 'text': '撤回范围变更'})
        try:
            self.command('agent_resume', {'messageIds': []})
        except Rejected as error:
            self.fail('Safe explicit resume without selected messages must remain available: ' + str(error))
        fresh = self.store.claim_attempt(self.project['id'])
        self.assertNotEqual(fresh['id'], self.attempt['id'])
        self.assertEqual(self.store.receive_guidance(fresh['id'])['messages'], [])
        rows = self.store.agent_thread(self.project['id'], 'developer')['messages']
        self.assertEqual(rows[0]['id'], old)
        self.assertEqual(rows[0]['status'], 'expired')
        with self.assertRaises(Rejected):
            self.store.receive_guidance(self.attempt['id'])

    def test_other_role_scope_conflicts_block_resume_without_changing_suspended_work(self):
        self.block()
        conflicts = [self.message(role, '先确认另一岗位提出的范围变更', 'scope_change') for role in ('architect', 'tester')]
        for conflict in conflicts:
            before = self.store.get(self.project['id'])
            with self.assertRaisesRegex(Rejected, '范围冲突'):
                self.command('agent_resume', {'messageIds': []})
            self.assertEqual(self.store.get(self.project['id']), before)
            self.assertTrue(before['agentControl']['resumable'])
            self.assertIsNone(before['activeAttempt'])
            self.command('agent_resolve', {'messageId': conflict, 'decision': 'supersede', 'text': '明确撤回此范围变更'})
        resumed = self.command('agent_resume', {'messageIds': []})
        self.assertNotEqual(resumed['activeAttempt']['id'], self.attempt['id'])
        self.assertEqual(resumed['activeAttempt']['role'], 'developer')
        self.assertEqual(resumed['run']['done'], [])

    def test_selected_guidance_is_received_only_by_the_fresh_attempt_and_new_nonce(self):
        message = self.message('developer', '新旧投递必须隔离')
        delivery = self.store.receive_guidance(self.attempt['id'])['messages'][0]
        self.store.acknowledge_guidance(self.attempt['id'], message, delivery['nonce'], 'received', '旧阶段收到')
        self.block()
        self.command('agent_resume', {'messageIds': [message]})
        fresh = self.store.claim_attempt(self.project['id'])
        received = self.store.receive_guidance(fresh['id'])['messages'][0]
        self.assertNotEqual(received['nonce'], delivery['nonce'])
        with self.assertRaises(Rejected):
            self.store.acknowledge_guidance(fresh['id'], message, delivery['nonce'], 'received', '旧凭据不能迁移')
        self.store.acknowledge_guidance(fresh['id'], message, received['nonce'], 'received', '新阶段收到')
        self.store.acknowledge_guidance(fresh['id'], message, received['nonce'], 'applied', '在原批准范围内采用')
        self.assertEqual(self.store.guidance_control(fresh['id'])['pending'], 0)

    def test_global_unknown_lease_rejects_consultation_and_resume_even_after_other_stop(self):
        self.block()
        question = self.message('architect')
        other = self.store.create_project('未知旧进程')
        self.store.command(other['id'], uuid.uuid4().hex, other['revision'], 'generate', {})
        claimed = self.store.claim_attempt(other['id'])
        self.store.fail_attempt(claimed['id'], 'unknown fixture', confirmed=False)
        current = self.store.get(other['id'])
        self.store.command(other['id'], uuid.uuid4().hex, current['revision'], 'stop', {})
        for kind, payload in [('agent_consult', {'role': 'architect', 'messageIds': [question]}), ('agent_resume', {'messageIds': []})]:
            with self.subTest(kind=kind), self.assertRaises(Rejected):
                self.command(kind, payload)
        self.assertTrue(self.store.get(other['id'])['leaseUnknown'])
        self.assertEqual(self.store.get(self.project['id'])['run']['done'], [])

    def test_user_resolved_or_superseded_consult_question_cannot_be_answered_as_if_current(self):
        self.block()
        question = self.message('architect', '用户可能在咨询过程中撤回')
        self.command('agent_consult', {'role': 'architect', 'messageIds': [question]})
        consult = self.store.claim_attempt(self.project['id'])
        self.command('agent_resolve', {'messageId': question, 'decision': 'supersede', 'text': '明确撤回'})
        with self.assertRaises(Rejected):
            self.store.complete_consultation(consult['id'], {'summary': '迟到回答不得伪造已回答'}, self.proof(consult, 'consultationMessageId'))
        rows = self.store.agent_thread(self.project['id'], 'architect')['messages']
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['status'], 'superseded')

    def test_handoff_at_result_commit_pauses_without_completing_and_legacy_resume_cannot_skip_work(self):
        self.command('agent_handoff')
        paused = self.store.complete_attempt(self.attempt['id'], {'summary': 'late old result', 'artifacts': []},
            process_proof=self.proof(key='resultMessageId'))
        self.assertEqual(paused['phase'], 'paused')
        self.assertEqual(paused['run']['done'], [])
        with self.assertRaises(Rejected):
            self.command('resume')
        self.command('agent_resume', {'messageIds': []})
        fresh = self.store.claim_attempt(self.project['id'])
        self.assertEqual(fresh['taskId'], 'D1')
        self.assertNotEqual(fresh['id'], self.attempt['id'])

    def test_idle_completed_role_can_consult_and_returns_to_same_phase_without_autorun(self):
        self.command('stop')
        self.store.confirm_stopped(self.project['id'], True)
        question = self.message('tester', '尚未运行也可以明确请求只读咨询')
        before = self.store.get(self.project['id'])
        try:
            self.command('agent_consult', {'role': 'tester', 'messageIds': [question]})
        except Rejected as error:
            self.fail('Inactive roles must not wait forever for a nonexistent next workflow turn: ' + str(error))
        consult = self.store.claim_attempt(self.project['id'])
        result = self.store.complete_consultation(consult['id'], {'summary': '只读解释'}, self.proof(consult, 'consultationMessageId'))
        self.assertEqual(result['phase'], before['phase'])
        self.assertIsNone(result['activeAttempt'])
        self.assertEqual(result['run'], before['run'])
        with self.assertRaises(Rejected):
            self.command('agent_resume', {'messageIds': []})


if __name__ == '__main__':
    unittest.main()
