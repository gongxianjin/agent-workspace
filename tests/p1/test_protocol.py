"""Reason protocol fixtures are captured transport data, never model ACK evidence."""
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PATH = ROOT / 'p1/reason_mcp.py'
spec = importlib.util.spec_from_file_location('relay_protocol', PATH) if PATH.exists() else None
protocol = importlib.util.module_from_spec(spec) if spec else None
if spec:
    sys.path.insert(0, str(ROOT / 'p1'))
    spec.loader.exec_module(protocol)


class ProtocolTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(protocol, 'Reason results need strict parsing and explicit model acknowledgement')

    def test_real_captured_format_preserves_body_sender_and_message_id(self):
        records = json.loads((ROOT / 'dist/p1/reason-contract-sample.json').read_text(encoding='utf-8'))
        raw = next(r['stdout'] for r in records if r['args'] == ['listen', '--as', 'sample-worker', '--wait', '1'])
        messages = protocol.parse_messages(raw)
        self.assertEqual(len(messages), 1)
        self.assertEqual(messages[0]['sender'], '61769/sample-control')
        self.assertEqual(messages[0]['body']['nonce'], 'format-contract-only')
        self.assertTrue(messages[0]['id'].startswith('01'))
        self.assertEqual(protocol.parse_messages('No messages. You are user/agent.\n'), [])

    def test_ambiguous_or_malformed_listen_output_fails_closed(self):
        for raw in ['not a message', '<reason-message from="x" id="a">\nnot-json\n</reason-message>',
                '<reason-message id="a">\n{}\n</reason-message>', '<reason-message from="x" id="a">\n{}']:
            with self.subTest(raw=raw), self.assertRaises(ValueError):
                protocol.parse_messages(raw)

    def test_result_requires_prior_explicit_ack_and_correct_nonce(self):
        guard = protocol.Exchange('project', 'run', 'attempt', 1, 'hash')
        task = guard.task({'kind': 'probe'})
        self.assertNotIn('nonce', guard.wakeup())
        with self.assertRaises(ValueError):
            guard.result(task['nonce'], {'summary': 'unread'})
        with self.assertRaises(ValueError):
            guard.ack('wrong', 'read')
        self.assertFalse(guard.acknowledged)
        ack = guard.ack(task['nonce'], 'received actual message')
        self.assertEqual(ack['kind'], 'ACK')
        self.assertTrue(guard.acknowledged)
        with self.assertRaises(ValueError):
            guard.ack(task['nonce'], 'duplicate')
        result = guard.result(task['nonce'], {'summary': 'done'})
        self.assertEqual(result['inputHash'], 'hash')
        self.assertEqual(result['kind'], 'RESULT')
        with self.assertRaises(ValueError):
            guard.result(task['nonce'], {})

    def test_cancelled_exchange_rejects_late_ack_result(self):
        guard = protocol.Exchange('p', 'r', 'a', 2, 'h')
        nonce = guard.task({})['nonce']
        guard.cancelled = True
        with self.assertRaises(ValueError):
            guard.ack(nonce, 'late')
        with self.assertRaises(ValueError):
            guard.result(nonce, {})

    def test_paths_cannot_escape_or_follow_reparse_points(self):
        with tempfile.TemporaryDirectory(prefix='relay-path-') as temp:
            root = Path(temp)
            (root / 'file.txt').write_text('safe')
            self.assertEqual(protocol.safe_path(root, 'file.txt').read_text(), 'safe')
            for path in ['../secret', 'C:/Windows/win.ini', '/etc/passwd', 'x:stream', '..\\escape', 'a/../../b', 'CON', 'a\\b', '.git/config']:
                with self.subTest(path=path), self.assertRaises(ValueError):
                    protocol.safe_path(root, path)

    def test_mcp_dispatch_validates_tool_and_does_not_evaluate_arguments(self):
        calls = []
        def handler(name, arguments):
            calls.append((name, arguments))
            return {'data': '<script>not executed</script>'}
        reply = protocol.mcp_dispatch({'jsonrpc': '2.0', 'id': 1, 'method': 'tools/call',
            'params': {'name': 'receive_task', 'arguments': {}}}, handler)
        self.assertNotIn('error', reply)
        self.assertEqual(calls, [('receive_task', {})])
        bad = protocol.mcp_dispatch({'jsonrpc': '2.0', 'id': 2, 'method': 'tools/call',
            'params': {'name': 'exec_shell', 'arguments': {'command': 'whoami'}}}, handler)
        self.assertTrue(bad['result']['isError'])
        self.assertEqual(len(calls), 1)


if __name__ == '__main__':
    unittest.main()
