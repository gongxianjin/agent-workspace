"""Authenticated real HTTP inbox reads and receipt recovery; no model process."""
from concurrent.futures import ThreadPoolExecutor
import http.client
import json
from pathlib import Path
import sys
import tempfile
import threading
from types import SimpleNamespace
import unittest
import uuid
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
from server import Application


class GuidanceHTTPTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-guidance-http-')
        self.addCleanup(self.temp.cleanup)
        self.app = Application(Path(self.temp.name), port=0, workers=False)
        self.thread = threading.Thread(target=self.app.serve, daemon=True)
        self.thread.start()
        self.addCleanup(self.app.close)
        self.token = self.call('POST', '/api/session', {'bootstrap': self.app.bootstrap}, auth=False)[1]['token']
        self.project = self.call('POST', '/api/projects', {'name': '消息HTTP项目'})[1]
        self.path = '/api/agents/thread?project=' + self.project['id'] + '&role=architect'

    def call(self, method, path, value=None, auth=True, headers=None):
        connection = http.client.HTTPConnection('127.0.0.1', self.app.port, timeout=3)
        request_headers = {'Origin': self.app.origin, 'Content-Type': 'application/json'}
        if auth:
            request_headers['Authorization'] = 'Bearer ' + self.token
        request_headers.update(headers or {})
        try:
            connection.request(method, path, json.dumps(value).encode() if value is not None else None, request_headers)
            response = connection.getresponse()
            return response.status, json.loads(response.read())
        finally:
            connection.close()

    def body(self, command_id='message-http'):
        current = self.app.store.get(self.project['id'])
        return {'projectId': current['id'], 'commandId': command_id, 'expectedRevision': current['revision'],
            'kind': 'agent_message', 'payload': {'role': 'architect', 'text': '<script>private-chat-marker</script>',
                'intent': 'clarification', 'targetAttemptId': None}}

    def test_thread_requires_auth_exact_origin_host_and_bounded_query(self):
        self.assertEqual(self.call('GET', self.path, auth=False)[0], 403)
        for headers in ({'Origin': 'https://bad.invalid'}, {'Host': 'localhost:' + str(self.app.port)}, {'Sec-Fetch-Site': 'cross-site'}):
            with self.subTest(headers=headers):
                self.assertEqual(self.call('GET', self.path, headers=headers)[0], 403)
        self.assertEqual(self.call('GET', self.path)[0], 200)
        for suffix in ('&role=tester', '&extra=x', '&after=-1', '&after=1.5', '&after=',
                '&after=99999999999999999999999', '&limit=0', '&limit=51', '&limit=true', '&limit=1&limit=2', '&bad', '&x=' + 'a' * 1100):
            with self.subTest(suffix=suffix[:70]):
                self.assertEqual(self.call('GET', self.path + suffix)[0], 400)
        self.assertEqual(self.call('GET', self.path.replace('architect', 'other'))[0], 409)
        self.assertEqual(self.call('GET', self.path.replace(self.project['id'], 'not-an-id'))[0], 400)
        self.assertEqual(self.call('GET', self.path.replace(self.project['id'], 'f' * 32))[0], 409)

    def test_message_saves_literal_thread_and_receipt_without_setup_probe_or_engine_notify(self):
        notifications = []
        engine = SimpleNamespace(notify=lambda: notifications.append('notify'))
        body = self.body()
        with patch.object(self.app, 'engine', engine), patch.object(self.app.setup, 'snapshot', side_effect=AssertionError('Message save must not probe execution readiness')):
            status, state = self.call('POST', '/api/command', body)
        self.assertEqual(status, 200)
        self.assertEqual(notifications, [])
        self.assertIsNone(state['activeAttempt'])
        self.assertNotIn('private-chat-marker', json.dumps(state))
        self.assertEqual(self.call('POST', '/api/command', body), (200, state))
        status, thread = self.call('GET', self.path + '&limit=1&after=0')
        self.assertEqual(status, 200)
        self.assertEqual(thread['messages'][0]['text'], '<script>private-chat-marker</script>')
        self.assertNotIn('nonce', json.dumps(thread))
        receipt = self.call('GET', '/api/receipts?scope=project&project=' + self.project['id'] + '&command=message-http')[1]
        self.assertTrue(receipt['found'])
        self.assertNotIn('private-chat-marker', json.dumps(receipt))
        self.assertEqual(self.call('POST', '/api/command', {**body, 'commandId': 'stale'})[0], 409)
        self.assertEqual(len(self.call('GET', self.path)[1]['messages']), 1)

    def test_read_does_not_wait_for_mutation_lock_and_cannot_start_model(self):
        status, _ = self.call('POST', '/api/command', self.body())
        self.assertEqual(status, 200)
        before = self.app.store.get(self.project['id'])
        with ThreadPoolExecutor(max_workers=1) as pool:
            with self.app.action_lock:
                future = pool.submit(self.call, 'GET', self.path)
                status, thread = future.result(timeout=2)
        self.assertEqual(status, 200)
        self.assertEqual(len(thread['messages']), 1)
        self.assertEqual(self.app.store.get(self.project['id']), before)

    def test_consultation_and_fresh_resume_require_model_readiness_even_with_saved_messages(self):
        self.assertEqual(self.call('POST', '/api/command', self.body())[0], 200)
        question = self.call('GET', self.path)[1]['messages'][0]['id']
        before = self.app.store.get(self.project['id'])
        for kind, payload in [('agent_consult', {'role': 'architect', 'messageIds': [question]}),
                ('agent_resume', {'messageIds': []})]:
            with self.subTest(kind=kind), patch.object(self.app.setup, 'snapshot', side_effect=ValueError('fixture-not-ready')):
                body = {'projectId': before['id'], 'commandId': uuid.uuid4().hex, 'expectedRevision': before['revision'],
                    'kind': kind, 'payload': payload}
                status, result = self.call('POST', '/api/command', body)
                self.assertEqual(status, 409, result)
                self.assertIn('fixture-not-ready', result['error'])
                self.assertEqual(self.app.store.get(before['id']), before)
                self.assertFalse(self.app.store.command_receipt(before['id'], body['commandId'])['found'])

    def test_committed_message_response_failure_uses_read_only_receipt_not_replay(self):
        original = self.app.store.get
        def after_commit(project_id):
            if self.app.store.command_receipt(project_id, 'unknown-message')['found']:
                raise OSError('private-host-marker')
            return original(project_id)
        with patch.object(self.app.store, 'get', after_commit):
            status, value = self.call('POST', '/api/command', self.body('unknown-message'))
        self.assertEqual(status, 500)
        self.assertEqual(value['outcome'], 'unknown')
        self.assertNotIn('private-host-marker', json.dumps(value))
        receipt = self.call('GET', '/api/receipts?scope=project&project=' + self.project['id'] + '&command=unknown-message')[1]
        self.assertTrue(receipt['found'])
        self.assertEqual(len(self.call('GET', self.path)[1]['messages']), 1)
        self.assertNotIn('private-chat-marker', json.dumps(receipt))


if __name__ == '__main__':
    unittest.main()
