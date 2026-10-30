"""Typed read-only consultation and cooperative handoff host paths."""
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest
from unittest.mock import patch

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'tests/p1'))
sys.path.insert(0, str(ROOT / 'p1'))
import runtime
from test_agent_runtime import Mailboxes


class ConsultRuntimeTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-consult-runtime-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.workspace = self.root / 'workspace'
        (self.workspace / 'product').mkdir(parents=True)
        (self.workspace / 'product/index.html').write_text('<p>read-only fixture</p>', encoding='utf-8')
        self.runner = runtime.RoleRunner(Mailboxes(), self.root / 'evidence')
        self.attempt = {'id': 'fixture-consult', 'projectId': 'fixture', 'runId': None,
            'epoch': 1, 'inputHash': 'fixture', 'role': 'developer', 'kind': 'consult'}

    def run_scenario(self, scenario, control=None):
        class PaidBoundary:
            def __init__(self, *args, **kwargs):
                pass
            def wait(self, timeout):
                scenario()
                return {'pid': 123, 'exitCode': 0, 'treeStopped': True, 'timedOut': False,
                    'outputLimited': False, 'stdout': '', 'stderr': ''}
            def close(self):
                pass
        with patch.object(runtime, 'JobProcess', PaidBoundary):
            return self.runner.run(self.attempt, 'Claude Code', self.workspace, {}, control=control)

    def protocol(self):
        task = self.runner.dispatch('receive_task', {})
        self.runner.dispatch('send_ack', {'nonce': task['nonce'], 'summary': 'explicit fixture only'})
        return task['nonce']

    def test_consultation_is_read_only_and_cannot_mint_normal_result_or_check(self):
        def scenario():
            nonce = self.protocol()
            self.assertEqual(self.runner.dispatch('read_project', {'relativePath': 'product/index.html'})['text'], '<p>read-only fixture</p>')
            for name, args in [('write_project', {'relativePath': 'product/late.txt', 'text': 'not allowed'}),
                    ('run_check', {'checkId': 'inventory'}), ('run_check', {'checkId': 'browser'}),
                    ('send_result', {'nonce': nonce, 'result': {'summary': 'not a task result'}})]:
                with self.assertRaises(ValueError):
                    self.runner.dispatch(name, args)
            reply = self.runner.dispatch('send_consultation', {'nonce': nonce, 'summary': '公开咨询回复'})
            self.assertEqual(reply['kind'], 'CONSULTATION')
        result = self.run_scenario(scenario)
        self.assertEqual(result['consultation'], {'summary': '公开咨询回复'})
        self.assertTrue(result['consultationMessageId'])
        self.assertNotIn('result', result)
        self.assertEqual(result['checks'], [])
        self.assertFalse((self.workspace / 'product/late.txt').exists())

    @unittest.skipUnless(os.name == 'nt', 'Native owned-process fixture uses the verified Windows Job adapter')
    def test_actual_owned_python_child_exchanges_consultation_over_mcp_http_and_is_drained(self):
        def python_instead_of_paid_cli(client, workspace, prompt, endpoint, capability, **kwargs):
            code = '''import json,sys,urllib.request
endpoint,token=sys.argv[1:]
opener=urllib.request.build_opener(urllib.request.ProxyHandler({}))
def call(name,arguments):
 req=urllib.request.Request(endpoint,data=json.dumps({'name':name,'arguments':arguments}).encode(),headers={'Authorization':'Bearer '+token,'Content-Type':'application/json'})
 with opener.open(req,timeout=5) as response: return json.load(response)
task=call('receive_task',{})
assert 'error' not in call('send_ack',{'nonce':task['nonce'],'summary':'real Python fixture, not model'})
assert call('read_project',{'relativePath':'product/index.html'})['text']=='<p>read-only fixture</p>'
assert 'error' in call('write_project',{'relativePath':'product/late.txt','text':'denied'})
assert call('send_consultation',{'nonce':task['nonce'],'summary':'实际受控Python咨询夹具'})['kind']=='CONSULTATION'
'''
            return [sys.executable, '-I', '-B', '-X', 'utf8', '-c', code, endpoint, capability], b''
        with patch.object(runtime, 'cli_command', python_instead_of_paid_cli):
            result = self.runner.run(self.attempt, 'Claude Code', self.workspace, {})
        self.assertEqual(result['consultation'], {'summary': '实际受控Python咨询夹具'})
        self.assertTrue(result['treeStopped'])
        self.assertTrue(result['handlersDrained'])
        self.assertGreater(result['pid'], 0)
        self.assertEqual(result['exitCode'], 0)
        self.assertFalse((self.workspace / 'product/late.txt').exists())
        with self.assertRaises(ValueError):
            self.runner.dispatch('read_project', {'relativePath': 'product/index.html'})

    def test_handoff_requires_user_request_and_prevents_next_side_effect_without_marking_done(self):
        requested = {'value': False}
        control = {'status': lambda: {'pending': 0, 'needsConfirmation': False, 'handoffRequested': requested['value']}}
        self.attempt.update(id='fixture-handoff', kind='develop')
        def scenario():
            nonce = self.protocol()
            with self.assertRaises(ValueError):
                self.runner.dispatch('send_handoff', {'nonce': nonce, 'summary': '不能自行交接'})
            self.runner.dispatch('write_project', {'relativePath': 'product/current.txt', 'text': 'saved before handoff'})
            requested['value'] = True
            blocked = self.runner.dispatch('write_project', {'relativePath': 'product/late.txt', 'text': 'not executed'})
            self.assertIn('operationExecuted', blocked, 'A requested handoff must block the next write, not execute it')
            self.assertIs(blocked['operationExecuted'], False)
            self.assertTrue(blocked['control']['handoffRequested'])
            self.assertIs(self.runner.dispatch('send_result', {'nonce': nonce, 'result': {}})['operationExecuted'], False)
            reply = self.runner.dispatch('send_handoff', {'nonce': nonce, 'summary': '公开交接：只保存了current，任务未完成'})
            self.assertEqual(reply['kind'], 'HANDOFF')
        result = self.run_scenario(scenario, control)
        self.assertEqual(result['terminal']['kind'], 'handoff')
        self.assertNotIn('result', result)
        self.assertFalse((self.workspace / 'product/late.txt').exists())


if __name__ == '__main__':
    unittest.main()
