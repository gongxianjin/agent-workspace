"""Real role handlers and SQLite; only paid CLI/reason use explicit fixtures."""
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
import runtime
from reason_mcp import mcp_dispatch
from state import Store, Rejected


class Mailboxes:
    def __init__(self):
        self.messages = []

    def register(self, name):
        return 'fixture/' + name

    def send(self, sender, recipient, body):
        identifier = 'fixture-' + str(len(self.messages))
        self.messages.append({'id': identifier, 'sender': 'fixture/' + sender, 'recipient': recipient, 'body': body})
        return identifier

    def listen(self, name):
        return [item for item in self.messages if item['recipient'] == 'fixture/' + name]


class AgentRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-agent-runtime-')
        self.addCleanup(self.temp.cleanup)
        self.store = Store(Path(self.temp.name))
        self.project = self.store.create_project('真实工具安全点')
        self.command('upload', {'name': 'fixture.md', 'text': '# fixture approved scope'})
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
        self.transport = Mailboxes()
        self.runner = runtime.RoleRunner(self.transport, self.store.root / 'evidence')
        self.control = {
            'status': lambda: self.store.guidance_control(self.attempt['id']),
            'receive': lambda: self.store.receive_guidance(self.attempt['id']),
            'acknowledge': lambda **kw: self.store.acknowledge_guidance(self.attempt['id'],
                kw['messageId'], kw['nonce'], kw['status'], kw['summary'])}

    def command(self, kind, payload=None):
        state = self.store.get(self.project['id'])
        return self.store.command(state['id'], uuid.uuid4().hex, state['revision'], kind, payload or {})

    def message(self, intent='clarification'):
        self.command('agent_message', {'role': 'developer', 'text': 'private-guidance-fixture',
            'intent': intent, 'targetAttemptId': self.attempt['id']})

    def start_protocol(self):
        task = self.runner.dispatch('receive_task', {})
        self.runner.dispatch('send_ack', {'nonce': task['nonce'], 'summary': 'explicit fixture ACK, not model evidence'})
        return task['nonce']

    def result(self, nonce):
        return self.runner.dispatch('send_result', {'nonce': nonce, 'result': {'summary': 'fixture', 'artifacts': []}})

    def run_scenario(self, scenario, process=None):
        outer = self
        class PaidBoundary:
            def __init__(boundary, *args, **kwargs):
                outer.stdout_observer = kwargs.get('stdout_observer')
            def wait(boundary, timeout):
                scenario()
                return {'pid': 123, 'exitCode': 0, 'treeStopped': True, 'timedOut': False,
                    'outputLimited': False, 'stdout': '', 'stderr': '', **(process or {})}
            def stop(boundary):
                return True
            def close(boundary):
                pass
        with patch.object(runtime, 'JobProcess', PaidBoundary):
            try:
                return self.runner.run(self.attempt, 'Claude Code', self.workspace, {}, control=self.control)
            except TypeError as error:
                if "unexpected keyword argument 'control'" in str(error):
                    self.fail('RoleRunner must enforce its attempt-scoped guidance adapter at real host tool boundaries')
                raise

    def test_inflight_write_finishes_then_guidance_blocks_next_write_and_result_until_applied(self):
        entered, release = threading.Event(), threading.Event()
        original = runtime.ProjectFiles.write
        responses, errors = [], []
        def held(files, relative, source):
            if relative == 'product/current.txt':
                entered.set()
                if not release.wait(4):
                    raise RuntimeError('Fixture did not release current write')
            return original(files, relative, source)
        def scenario():
            nonce = self.start_protocol()
            def write():
                try:
                    responses.append(self.runner.dispatch('write_project', {'relativePath': 'product/current.txt', 'text': 'current write completes'}))
                except Exception as error:
                    errors.append(error)
            writer = threading.Thread(target=write)
            writer.start()
            try:
                self.assertTrue(entered.wait(2))
                self.message()
                self.assertFalse((self.workspace / 'product/current.txt').exists())
            finally:
                release.set()
                writer.join(4)
            self.assertFalse(writer.is_alive())
            self.assertEqual(errors, [])
            self.assertEqual((self.workspace / 'product/current.txt').read_text(), 'current write completes')
            self.assertEqual(responses[0]['control']['pending'], 1)
            blocked = self.runner.dispatch('write_project', {'relativePath': 'product/next.txt', 'text': 'must not run yet'})
            self.assertIs(blocked['operationExecuted'], False)
            self.assertFalse((self.workspace / 'product/next.txt').exists())
            self.assertIs(self.result(nonce)['operationExecuted'], False)
            self.assertFalse(self.runner.exchange.finished)
            item = self.runner.dispatch('receive_guidance', {})['messages'][0]
            self.assertEqual(item['status'], 'queued')
            with self.assertRaises(Rejected):
                self.runner.dispatch('acknowledge_guidance', {'messageId': item['id'], 'nonce': 'wrong', 'status': 'received', 'summary': 'wrong nonce'})
            self.runner.dispatch('acknowledge_guidance', {'messageId': item['id'], 'nonce': item['nonce'], 'status': 'received', 'summary': 'explicit received'})
            self.assertIs(self.result(nonce)['operationExecuted'], False)
            self.runner.dispatch('acknowledge_guidance', {'messageId': item['id'], 'nonce': item['nonce'], 'status': 'applied', 'summary': 'explicitly used within approved scope'})
            self.runner.dispatch('write_project', {'relativePath': 'product/next.txt', 'text': 'now permitted'})
            self.assertEqual((self.workspace / 'product/next.txt').read_text(), 'now permitted')
            self.assertEqual(self.result(nonce)['kind'], 'RESULT')
        with patch.object(runtime.ProjectFiles, 'write', held):
            result = self.run_scenario(scenario)
        self.assertTrue(result['treeStopped'])
        self.assertEqual(self.store.agent_thread(self.project['id'], 'developer')['messages'][0]['status'], 'applied')
        self.assertEqual(self.store.get(self.project['id'])['run']['done'], [])

    def test_conflict_blocks_reads_inventory_and_result_but_allows_public_blocker(self):
        def scenario():
            with self.assertRaises(ValueError):
                self.runner.dispatch('receive_guidance', {})
            nonce = self.start_protocol()
            self.message('scope_change')
            for name, arguments in [('read_project', {'relativePath': 'product/missing.txt'}),
                    ('run_check', {'checkId': 'inventory'}),
                    ('send_result', {'nonce': nonce, 'result': {'summary': 'not permitted'}})]:
                reply = self.runner.dispatch(name, arguments)
                self.assertIs(reply['operationExecuted'], False)
                self.assertTrue(reply['control']['needsConfirmation'])
            item = self.runner.dispatch('receive_guidance', {})['messages'][0]
            with self.assertRaises(Rejected):
                self.runner.dispatch('acknowledge_guidance', {'messageId': item['id'], 'nonce': item['nonce'], 'status': 'applied', 'summary': 'cannot self approve'})
            reply = self.runner.dispatch('report_blocker', {'nonce': nonce, 'reason': '公开原因：与批准范围冲突'})
            self.assertEqual(reply['kind'], 'BLOCKED')
            for name, arguments in [('write_project', {'relativePath': 'product/late.txt', 'text': 'late'}),
                    ('receive_guidance', {}), ('send_result', {'nonce': nonce, 'result': {}})]:
                with self.assertRaises(ValueError):
                    self.runner.dispatch(name, arguments)
        result = self.run_scenario(scenario)
        self.assertEqual(result['terminal'], {'kind': 'blocked', 'reason': '公开原因：与批准范围冲突'})
        self.assertNotIn('result', result)
        self.assertNotIn('resultMessageId', result)
        self.assertTrue(result['terminalMessageId'])
        self.assertFalse(any(item['body']['kind'] == 'RESULT' for item in self.transport.messages))

    def test_controlled_terminal_requires_clean_child_tree_and_revokes_after_stop(self):
        for failure in ({'exitCode': 7}, {'treeStopped': False}, {'timedOut': True}, {'outputLimited': True}):
            with self.subTest(failure=failure):
                self.runner = runtime.RoleRunner(self.transport, self.store.root / ('evidence-' + uuid.uuid4().hex))
                def scenario():
                    nonce = self.start_protocol()
                    with self.assertRaises(ValueError):
                        self.runner.dispatch('report_blocker', {'nonce': 'wrong', 'reason': '不能用旧凭据'})
                    self.runner.dispatch('report_blocker', {'nonce': nonce, 'reason': '公开阻塞'})
                with self.assertRaisesRegex(RuntimeError, 'did not finish cleanly'):
                    self.run_scenario(scenario, failure)
                self.assertIsNone(self.runner.last)
                self.assertEqual(self.store.get(self.project['id'])['run']['done'], [])
        self.runner = runtime.RoleRunner(self.transport, self.store.root / 'stop-evidence')
        def stopped():
            nonce = self.start_protocol()
            self.assertTrue(self.runner.stop())
            for name, arguments in [('receive_guidance', {}), ('report_blocker', {'nonce': nonce, 'reason': '迟到'})]:
                with self.assertRaises(ValueError):
                    self.runner.dispatch(name, arguments)
        with self.assertRaisesRegex(RuntimeError, 'did not finish cleanly'):
            self.run_scenario(stopped)

    def test_guidance_delivery_nonce_is_never_published_as_cli_text(self):
        observed = []
        self.runner.telemetry = observed.append
        def scenario():
            nonce = self.start_protocol()
            self.message()
            item = self.runner.dispatch('receive_guidance', {})['messages'][0]
            raw = {'type': 'assistant', 'message': {'id': 'fixture-public-text', 'role': 'assistant',
                'content': [{'type': 'text', 'text': 'delivery credential ' + item['nonce'] + ' must remain private'}]}}
            self.stdout_observer((json.dumps(raw) + '\n').encode())
            self.stdout_observer(b'')
            for status in ('received', 'applied'):
                self.runner.dispatch('acknowledge_guidance', {'messageId': item['id'], 'nonce': item['nonce'], 'status': status, 'summary': 'explicit ' + status})
            self.result(nonce)
            self.assertNotIn(item['nonce'], json.dumps(observed))
            self.assertTrue(any(event.get('kind') == 'text' for event in observed))
        self.run_scenario(scenario)

    def test_guidance_schema_dispatches_only_exact_fixed_control_fields(self):
        def scenario():
            nonce = self.start_protocol()
            self.message()
            def call(identifier, name, arguments):
                return mcp_dispatch({'jsonrpc': '2.0', 'id': identifier, 'method': 'tools/call',
                    'params': {'name': name, 'arguments': arguments}}, self.runner.dispatch)['result']
            reply = call(1, 'receive_guidance', {})
            self.assertFalse(reply.get('isError', False), reply)
            item = json.loads(reply['content'][0]['text'])['messages'][0]
            bad = call(2, 'acknowledge_guidance', {'messageId': item['id'], 'nonce': item['nonce'],
                'status': 'applied', 'summary': 'invalid order', 'extra': 'no extra fields'})
            self.assertTrue(bad['isError'])
            for status in ('received', 'applied'):
                reply = call(3, 'acknowledge_guidance', {'messageId': item['id'], 'nonce': item['nonce'], 'status': status, 'summary': 'explicit ' + status})
                self.assertFalse(reply.get('isError', False), reply)
            self.result(nonce)
        self.run_scenario(scenario)


if __name__ == '__main__':
    unittest.main()
