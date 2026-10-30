"""Real SQLite Agent inbox contracts; fixtures never invoke a paid model."""
from concurrent.futures import ThreadPoolExecutor
from contextlib import closing
import json
from pathlib import Path
import sqlite3
import sys
import tempfile
import threading
import unittest
import uuid

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from state import Store, Rejected


class GuidanceTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-guidance-')
        self.addCleanup(self.temp.cleanup)
        self.store = Store(Path(self.temp.name))
        self.project = self.store.create_project('角色消息测试')

    def command(self, kind, payload, command_id=None, store=None, revision=None):
        store = store or self.store
        state = store.get(self.project['id'])
        return store.command(state['id'], command_id or uuid.uuid4().hex,
            state['revision'] if revision is None else revision, kind, payload)

    def message(self, text='private-message-marker', role='architect', intent='clarification', target=None, **kw):
        return self.command('agent_message', {'role': role, 'text': text, 'intent': intent, 'targetAttemptId': target}, **kw)

    def thread(self, role='architect', **kw):
        return self.store.agent_thread(self.project['id'], role, **kw)

    def start(self):
        self.command('generate', {})
        return self.store.claim_attempt(self.project['id'])

    def test_saved_message_is_durable_read_only_to_scheduler_and_replay_is_exact(self):
        before = self.store.get(self.project['id'])
        saved = self.message(command_id='one-message')
        replay = self.message(command_id='one-message', revision=before['revision'])
        self.assertEqual(replay, saved)
        self.assertIsNone(saved['activeAttempt'])
        self.assertEqual(saved['phase'], 'draft')
        self.assertNotIn('private-message-marker', json.dumps(saved))
        self.assertEqual(saved['agentInbox']['architect']['queued'], 1)
        with self.assertRaises(Rejected):
            self.message('changed', command_id='one-message', revision=before['revision'])
        other = Store(self.store.root, recover=False)
        rows = other.agent_thread(self.project['id'], 'architect')['messages']
        self.assertEqual(len(rows), 1)
        self.assertEqual(rows[0]['text'], 'private-message-marker')
        self.assertEqual(rows[0]['status'], 'queued')
        self.assertNotIn('nonce', json.dumps(rows))
        self.assertNotIn('private-message-marker', json.dumps(self.store.command_receipt(self.project['id'], 'one-message')))
        self.assertEqual(other.get(self.project['id']), saved)

    def test_concurrent_store_cas_and_exact_replay_never_duplicate_message(self):
        other = Store(self.store.root, recover=False)
        gate = threading.Barrier(2)
        def submit(store):
            gate.wait(timeout=3)
            return self.message(command_id='same', revision=1, store=store)
        with ThreadPoolExecutor(max_workers=2) as pool:
            results = [f.result(timeout=8) for f in [pool.submit(submit, self.store), pool.submit(submit, other)]]
        self.assertEqual(results[0], results[1])
        self.assertEqual(len(self.thread()['messages']), 1)
        with self.assertRaises(Rejected):
            self.message(command_id='different', revision=1, store=other)
        self.assertFalse(other.command_receipt(self.project['id'], 'different')['found'])

    def test_receive_is_not_acknowledgement_and_explicit_receipts_require_order_nonce_and_scope(self):
        attempt = self.start()
        self.message(target=attempt['id'])
        self.message('other-role', role='tester')
        delivery = self.store.receive_guidance(attempt['id'])
        self.assertEqual(len(delivery['messages']), 1)
        item = delivery['messages'][0]
        self.assertEqual(item['text'], 'private-message-marker')
        self.assertEqual(item['status'], 'queued')
        self.assertTrue(item['nonce'])
        self.assertEqual(self.store.receive_guidance(attempt['id'])['messages'][0]['nonce'], item['nonce'])
        self.assertEqual(self.thread()['messages'][0]['status'], 'queued')
        self.assertEqual(self.store.guidance_control(attempt['id'])['pending'], 1)
        for message_id, nonce, status in [(item['id'], 'wrong', 'received'),
                ('f' * 32, item['nonce'], 'received'), (item['id'], item['nonce'], 'applied')]:
            with self.subTest(status=status, id=message_id), self.assertRaises(Rejected):
                self.store.acknowledge_guidance(attempt['id'], message_id, nonce, status, '公开回执')
        self.store.acknowledge_guidance(attempt['id'], item['id'], item['nonce'], 'received', '收到说明')
        self.assertEqual(self.thread()['messages'][0]['status'], 'received')
        self.store.acknowledge_guidance(attempt['id'], item['id'], item['nonce'], 'applied', '在原批准范围内采用说明')
        self.assertEqual(self.thread()['messages'][0]['status'], 'applied')
        self.assertEqual(self.store.guidance_control(attempt['id'])['pending'], 0)
        self.assertEqual(self.thread('tester')['messages'][0]['status'], 'queued')
        self.assertEqual(self.store.get(self.project['id'])['activeAttempt']['inputHash'], attempt['inputHash'])
        with self.assertRaises(Rejected):
            self.store.acknowledge_guidance(attempt['id'], item['id'], item['nonce'], 'received', '不能倒退')

    def test_scope_change_never_applies_until_user_resolves_then_agent_receives_again(self):
        attempt = self.start()
        self.message('申请更改范围', intent='scope_change', target=attempt['id'])
        item = self.store.receive_guidance(attempt['id'])['messages'][0]
        self.assertEqual(item['status'], 'needs_confirmation')
        self.assertTrue(self.store.guidance_control(attempt['id'])['needsConfirmation'])
        with self.assertRaises(Rejected):
            self.store.acknowledge_guidance(attempt['id'], item['id'], item['nonce'], 'applied', '不能自行批准')
        self.command('agent_resolve', {'messageId': item['id'], 'decision': 'clarification', 'text': '仅解释原设计，不扩展范围'})
        resolved = self.store.receive_guidance(attempt['id'])['messages'][0]
        self.assertEqual(resolved['status'], 'queued')
        self.assertNotEqual(resolved['nonce'], item['nonce'])
        with self.assertRaises(Rejected):
            self.store.acknowledge_guidance(attempt['id'], item['id'], item['nonce'], 'received', '旧凭据')
        self.store.acknowledge_guidance(attempt['id'], item['id'], resolved['nonce'], 'received', '重新收到')
        self.store.acknowledge_guidance(attempt['id'], item['id'], resolved['nonce'], 'needs_confirmation', '仍与批准冲突')
        self.command('agent_resolve', {'messageId': item['id'], 'decision': 'supersede', 'text': '撤回此说明'})
        self.assertEqual(self.thread()['messages'][0]['status'], 'superseded')
        self.assertEqual(self.store.guidance_control(attempt['id'])['pending'], 0)
        self.assertEqual(self.store.get(self.project['id'])['activeAttempt']['inputHash'], attempt['inputHash'])

    def test_stopped_restarted_or_context_changed_messages_expire_and_old_attempt_cannot_ack(self):
        self.message()
        self.command('edit', {'requirement': '新需求使原上下文失效'})
        self.assertEqual(self.thread()['messages'][0]['status'], 'expired')
        attempt = self.start()
        self.message(target=attempt['id'])
        item = self.store.receive_guidance(attempt['id'])['messages'][0]
        self.command('stop', {})
        self.assertEqual(self.thread()['messages'][-1]['status'], 'expired')
        with self.assertRaises(Rejected):
            self.store.acknowledge_guidance(attempt['id'], item['id'], item['nonce'], 'received', '迟到')
        # A fresh root recovery also expires undelivered messages when no CLI was active.
        fresh = self.store.create_project('未执行线程')
        self.store.command(fresh['id'], 'save-before-restart', 1, 'agent_message',
            {'role': 'tester', 'text': '重启前说明', 'intent': 'clarification', 'targetAttemptId': None})
        recovered = Store(self.store.root)
        self.assertEqual(recovered.agent_thread(fresh['id'], 'tester')['messages'][0]['status'], 'expired')

    def test_wrong_role_project_or_closed_target_rejects_without_receipt(self):
        attempt = self.start()
        for role, target in [('tester', attempt['id']), ('architect', 'f' * 32)]:
            with self.subTest(role=role), self.assertRaises(Rejected):
                self.message(role=role, target=target)
        other = self.store.create_project('另一项目')
        with self.assertRaises(Rejected):
            self.store.command(other['id'], 'cross', 1, 'agent_message',
                {'role': 'architect', 'text': '不串项目', 'intent': 'clarification', 'targetAttemptId': attempt['id']})
        self.store.fail_attempt(attempt['id'], '明确夹具结束', confirmed=True)
        with self.assertRaises(Rejected):
            self.message(target=attempt['id'])
        with self.assertRaises(Rejected):
            self.store.receive_guidance(attempt['id'])
        self.assertEqual(self.thread()['messages'], [])

    def test_thread_is_paged_role_scoped_and_read_only_with_bounded_inputs(self):
        for i in range(4):
            self.message('消息' + str(i), role='tester' if i == 1 else 'architect')
        before = self.store.get(self.project['id'])
        first = self.thread(limit=2)
        self.assertEqual([row['text'] for row in first['messages']], ['消息0', '消息2'])
        self.assertTrue(first['hasMore'])
        second = self.thread(after=first['nextCursor'], limit=2)
        self.assertEqual([row['text'] for row in second['messages']], ['消息3'])
        self.assertFalse(second['hasMore'])
        for kw in ({'after': -1}, {'after': True}, {'limit': 0}, {'limit': 51}, {'after': '0'}):
            with self.subTest(kw=kw), self.assertRaises(Rejected):
                self.thread(**kw)
        for role in ('other', None, []):
            with self.subTest(role=role), self.assertRaises(Rejected):
                self.thread(role=role)
        self.assertEqual(self.store.get(self.project['id']), before)

    def test_invalid_or_overfull_inbox_rejects_atomically(self):
        for value in ('', 'x' * 4001, '\ud800', 'bad\x00text'):
            with self.subTest(value=repr(value)), self.assertRaises(Rejected):
                self.message(value)
        for i in range(100):
            self.message('未解决' + str(i))
        before = self.store.get(self.project['id'])
        with self.assertRaises(Rejected):
            self.message('超出上限')
        self.assertEqual(len(self.thread(limit=50)['messages']), 50)
        self.assertEqual(self.store.get(self.project['id']), before)
        row = self.thread()['messages'][0]
        self.command('agent_resolve', {'messageId': row['id'], 'decision': 'supersede', 'text': '撤回'})
        self.message('空出一个位置')
        self.assertEqual(self.store.get(self.project['id'])['agentInbox']['architect']['queued'], 100)

    def test_receipt_summary_limits_and_invalid_nonce_reject_without_status_change(self):
        attempt = self.start()
        self.message(target=attempt['id'])
        item = self.store.receive_guidance(attempt['id'])['messages'][0]
        for nonce, summary in [('非ASCII', '收到'), (item['nonce'], 'x' * 2001), (item['nonce'], '\ud800')]:
            with self.subTest(nonce=nonce[:8], summary=repr(summary[:10])), self.assertRaises(Rejected):
                self.store.acknowledge_guidance(attempt['id'], item['id'], nonce, 'received', summary)
        self.assertEqual(self.thread()['messages'][0]['status'], 'queued')
        receipt = self.store.acknowledge_guidance(attempt['id'], item['id'], item['nonce'], 'received', 'x' * 2000)
        self.assertEqual(receipt['status'], 'received')

    def test_unknown_process_allows_saving_explanation_but_never_receiving_or_executing(self):
        attempt = self.start()
        blocked = self.store.fail_attempt(attempt['id'], '进程树状态未知', confirmed=False)
        saved = self.message('先保存说明，不恢复未知进程')
        self.assertTrue(saved['leaseUnknown'])
        self.assertEqual(saved['phase'], 'blocked')
        self.assertIsNone(saved['activeAttempt'])
        self.assertEqual(saved['epoch'], blocked['epoch'])
        with self.assertRaises(Rejected):
            self.store.receive_guidance(attempt['id'])
        with self.assertRaises(Rejected):
            self.command('generate', {})

    def test_message_and_receipt_rollback_together_on_sql_failure(self):
        with closing(sqlite3.connect(self.store.db)) as db, db:
            db.execute("CREATE TRIGGER fail_guidance BEFORE INSERT ON commands BEGIN SELECT RAISE(ABORT, 'fixture write failure'); END")
        with self.assertRaises(sqlite3.DatabaseError):
            self.message(command_id='rollback')
        self.assertEqual(self.thread()['messages'], [])
        self.assertFalse(self.store.command_receipt(self.project['id'], 'rollback')['found'])
        self.assertEqual(self.store.get(self.project['id']), self.project)


if __name__ == '__main__':
    unittest.main()
