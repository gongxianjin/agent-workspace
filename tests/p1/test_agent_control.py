"""Transactional controlled-turn settlement; no model or process is launched."""
import copy
from pathlib import Path
import sys
import tempfile
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from state import Store, Rejected


class AgentControlTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-agent-control-')
        self.addCleanup(self.temp.cleanup)
        self.store = Store(Path(self.temp.name))
        self.project = self.store.create_project('停止证明和提交屏障')
        self.command('generate')
        self.attempt = self.store.claim_attempt(self.project['id'])
        self.terminal = {'kind': 'blocked', 'reason': '公开阻塞：需要用户说明'}
        self.proof = {'attemptId': self.attempt['id'], 'pid': 123, 'exitCode': 0,
            'treeStopped': True, 'handlersDrained': True, 'checksStopped': True,
            'taskMessageId': 'fixture-task', 'ackMessageId': 'fixture-ack', 'terminalMessageId': 'fixture-blocked'}

    def command(self, kind, payload=None):
        state = self.store.get(self.project['id'])
        return self.store.command(state['id'], uuid.uuid4().hex, state['revision'], kind, payload or {})

    def message(self):
        self.command('agent_message', {'role': 'architect', 'text': 'private-guidance-remains',
            'intent': 'clarification', 'targetAttemptId': self.attempt['id']})

    def settle(self, terminal=None, proof=None):
        method = getattr(self.store, 'settle_controlled_attempt', None)
        self.assertTrue(callable(method), 'Controlled turns need a Store transaction distinct from normal completion')
        return method(self.attempt['id'], terminal or self.terminal, self.proof if proof is None else proof)

    def test_clean_block_is_not_completion_and_keeps_a_bound_suspended_context(self):
        self.message()
        before = self.store.get(self.project['id'])
        state = self.settle()
        self.assertEqual(state['phase'], 'blocked')
        self.assertIsNone(state['activeAttempt'])
        self.assertFalse(state['leaseUnknown'])
        self.assertEqual(state['agentControl']['status'], 'blocked')
        self.assertTrue(state['agentControl']['resumable'])
        self.assertEqual(state['agentControl']['attemptId'], self.attempt['id'])
        self.assertEqual(state['agentControl']['reason'], self.terminal['reason'])
        self.assertEqual(state['lastAttempt']['inputHash'], self.attempt['inputHash'])
        self.assertIsNone(state['generatedDocument'])
        self.assertEqual(state['tasks'], before['tasks'])
        self.assertNotIn('private-guidance-remains', str(state))
        self.assertEqual(self.store.agent_thread(state['id'], 'architect')['messages'][0]['status'], 'queued')
        with self.assertRaises(Rejected):
            self.store.receive_guidance(self.attempt['id'])
        with self.assertRaises(Rejected):
            self.store.complete_attempt(self.attempt['id'], {'markdown': '# late', 'criteria': ['late']})
        with self.assertRaises(Rejected):
            self.settle()

    def test_restart_or_stop_invalidates_suspended_resume_without_unknown_unlock(self):
        self.settle()
        recovered = Store(self.store.root)
        state = recovered.get(self.project['id'])
        self.assertFalse(state['agentControl']['resumable'], 'Restart cannot retain an unverified suspended resume capability')
        self.assertTrue(state['recoveryRequired'])
        self.assertTrue(state['leaseUnknown'])
        self.assertEqual(state['agentControl']['status'], 'expired')
        state = self.command('stop')
        self.assertFalse(state['agentControl']['resumable'])
        self.assertTrue(state['leaseUnknown'])

    def test_bad_process_or_handler_proof_cannot_grant_a_resumable_block(self):
        for bad in ({}, {'treeStopped': False}, {'handlersDrained': False}, {'checksStopped': False},
                {'exitCode': 7}, {'exitCode': False}, {'attemptId': 'foreign'}, {'terminalMessageId': ''}):
            with self.subTest(bad=bad):
                proof = {} if not bad else {**self.proof, **bad}
                before = self.store.get(self.project['id'])
                with self.assertRaises(Rejected):
                    self.settle(proof=proof)
                self.assertEqual(self.store.get(self.project['id']), before)
        failed = self.store.fail_attempt(self.attempt['id'], '树或检查器未停稳', confirmed=False)
        self.assertTrue(failed['leaseUnknown'])
        self.assertFalse(failed.get('agentControl', {}).get('resumable', False))

    def test_late_or_wrong_typed_terminal_cannot_settle_after_stop(self):
        for terminal in ({'kind': 'RESULT', 'reason': 'not a blocker'}, {'kind': 'blocked', 'reason': ''},
                {'kind': 'blocked', 'reason': 'x' * 2001}, {'kind': 'blocked', 'reason': 'ok', 'result': {}}):
            with self.subTest(terminal=terminal), self.assertRaises(Rejected):
                self.settle(terminal=terminal)
        self.command('stop')
        before = self.store.get(self.project['id'])
        with self.assertRaises(Rejected):
            self.settle()
        self.assertEqual(self.store.get(self.project['id']), before)

    def test_message_arriving_after_result_is_preserved_without_advancing_workflow(self):
        self.message()
        proof = {k: v for k, v in self.proof.items() if k != 'terminalMessageId'}
        proof['resultMessageId'] = 'fixture-result-before-guidance'
        try:
            state = self.store.complete_attempt(self.attempt['id'], {'markdown': '# stale result', 'criteria': ['stale']}, process_proof=proof)
        except TypeError:
            self.fail('Normal completion must atomically recheck guidance after host quiescence')
        self.assertEqual(state['phase'], 'blocked')
        self.assertIsNone(state['generatedDocument'])
        self.assertIsNone(state['activeAttempt'])
        self.assertTrue(state['agentControl']['resumable'])
        self.assertEqual(self.store.agent_thread(state['id'], 'architect')['messages'][0]['status'], 'queued')
        self.assertEqual(state['agentInbox']['architect']['queued'], 1)

    def test_pending_message_cannot_be_completed_without_host_quiescence_proof(self):
        self.message()
        before = self.store.get(self.project['id'])
        with self.assertRaises(Rejected):
            self.store.complete_attempt(self.attempt['id'], {'markdown': '# cannot commit', 'criteria': ['not done']})
        self.assertEqual(self.store.get(self.project['id']), before)


if __name__ == '__main__':
    unittest.main()
