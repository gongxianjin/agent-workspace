"""Synthetic stream envelopes only; not a paid native Claude protocol verification."""
import importlib
import json
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


def record(value):
    return (json.dumps(value, ensure_ascii=False) + '\n').encode('utf-8')


def stream(text, message='synthetic-message'):
    return b''.join(record({'type': 'stream_event', 'event': event}) for event in (
        {'type': 'message_start', 'message': {'id': message, 'role': 'assistant'}},
        {'type': 'content_block_start', 'index': 0, 'content_block': {'type': 'text', 'text': ''}},
        {'type': 'content_block_delta', 'index': 0, 'delta': {'type': 'text_delta', 'text': text}},
        {'type': 'message_stop'}))


class TelemetryTests(unittest.TestCase):
    def setUp(self):
        self.assertTrue((ROOT / 'p1/telemetry.py').is_file(), 'Live output needs a bounded safe projection, not raw stdout')
        self.module = importlib.import_module('telemetry')

    def test_byte_split_utf8_public_text_and_partial_snapshot_dedup(self):
        parser = self.module.ClaudeStreamParser()
        raw = stream('你好，正在检查。') + record({'type': 'assistant', 'message': {'id': 'synthetic-message', 'role': 'assistant',
            'content': [{'type': 'text', 'text': '你好，正在检查。'}]}}) + record({'type': 'result', 'result': 'must not replay'})
        events = []
        for value in raw:
            events.extend(parser.feed(bytes([value])))
        events.extend(parser.finish())
        self.assertEqual(''.join(row['text'] for row in events if row['kind'] == 'text'), '你好，正在检查。')

    def test_thinking_tool_arguments_raw_result_and_stderr_are_not_public(self):
        parser = self.module.ClaudeStreamParser()
        raw = record({'type': 'assistant', 'message': {'id': 'synthetic-final', 'role': 'assistant', 'content': [
            {'type': 'thinking', 'thinking': 'hidden-thinking', 'signature': 'hidden-signature'},
            {'type': 'tool_use', 'name': 'write_project', 'input': {'text': 'private-file-body'}},
            {'type': 'text', 'text': 'public-only'}]}})
        raw += record({'type': 'user', 'message': {'content': 'user-private'}})
        raw += record({'type': 'result', 'result': 'private-result'})
        raw += b'not-json-private\n' + record({'type': 'system', 'message': 'private-system'})
        events = parser.feed(raw) + parser.finish()
        rendered = json.dumps(events)
        for private in ('hidden-thinking', 'hidden-signature', 'private-file-body', 'user-private', 'private-result', 'not-json-private', 'private-system'):
            self.assertNotIn(private, rendered)
        self.assertIn('public-only', rendered)

    def test_known_secrets_and_credential_patterns_are_redacted_across_records_and_eof(self):
        secret = 'synthetic-capability-do-not-publish'
        parser = self.module.ClaudeStreamParser(secrets=[secret])
        raw = stream('before ' + secret[:10], 'a') + stream(secret[10:] + ' Bearer synthetic-credential after', 'b')
        events = parser.feed(raw) + parser.finish()
        text = ''.join(row['text'] for row in events if row['kind'] == 'text')
        self.assertNotIn(secret, text)
        self.assertNotIn('synthetic-credential', text)
        self.assertIn('[REDACTED]', text)
        parser = self.module.ClaudeStreamParser(secrets=[secret])
        events = parser.feed(stream('prefix ' + secret[:12])) + parser.finish()
        self.assertNotIn(secret[:12], ''.join(row.get('text', '') for row in events))

    def test_oversized_deep_malformed_and_gap_input_never_reemits_raw_bytes(self):
        parser = self.module.ClaudeStreamParser()
        events = parser.feed(b'x' * (512 * 1024 + 1) + b'\n')
        events += parser.feed(b'[' * 2000 + b'0' + b']' * 2000 + b'\n')
        events += parser.feed(stream('safe-before'))
        events += parser.gap()
        events += parser.feed(stream('unsafe-after-gap')) + parser.finish()
        rendered = json.dumps(events)
        self.assertNotIn('unsafe-after-gap', rendered)
        self.assertTrue(any(event.get('code') == 'stream_gap' for event in events))
        self.assertLess(len(rendered), 4096)

    def test_authorization_credentials_keep_whitespace_and_value_private(self):
        parser = self.module.ClaudeStreamParser()
        events = []
        for number, text in enumerate(('Authorization:', ' ', 'synthetic-header-token', '\npublic next line')):
            events += parser.feed(stream(text, str(len(events))))
        events += parser.finish()
        rendered = ''.join(row.get('text', '') for row in events)
        self.assertNotIn('synthetic-header-token', rendered)
        self.assertIn('public next line', rendered)

    def test_multibyte_text_event_limit_and_snapshot_mismatch_do_not_replay(self):
        parser = self.module.ClaudeStreamParser()
        events = parser.feed(stream('中' * 5000))
        events += parser.feed(record({'type': 'assistant', 'message': {'id': 'synthetic-message', 'role': 'assistant',
            'content': [{'type': 'text', 'text': 'changed snapshot must not replay'}]}})) + parser.finish()
        self.assertEqual(''.join(row.get('text', '') for row in events), '中' * 5000)
        self.assertTrue(all(len(row.get('text', '').encode()) <= 2048 for row in events))
        self.assertTrue(any(row.get('code') == 'snapshot_mismatch' for row in events))

    def test_host_event_projection_never_accepts_raw_arguments_or_exception_strings(self):
        with tempfile.TemporaryDirectory(prefix='relay-host-events-') as directory:
            store = self.module.TelemetryStore(Path(directory))
            scope = {'projectId': 'a' * 32, 'attemptId': 'b' * 32, 'epoch': 1, 'runId': None, 'inputHash': 'c' * 64}
            store.begin(scope)
            try:
                store.append(scope, {'kind': 'stage', 'stage': 'preparing'})
            except ValueError:
                self.fail('Telemetry needs a strict host-stage whitelist, not raw records')
            store.append(scope, {'kind': 'tool', 'tool': 'write_project', 'phase': 'started'})
            store.append(scope, {'kind': 'tool', 'tool': 'write_project', 'phase': 'finished', 'outcome': 'error'})
            for event in ({'kind': 'tool', 'tool': 'unknown-command', 'phase': 'started'},
                          {'kind': 'tool', 'tool': 'write_project', 'phase': 'finished', 'outcome': 'error', 'error': 'private'},
                          {'kind': 'text', 'text': '中' * 1000}):
                with self.assertRaises(ValueError):
                    store.append(scope, event)
            self.assertEqual(len(store.read(scope, 0)['events']), 3)

    def test_process_metadata_rejects_nonfinite_and_boolean_times_before_persistence(self):
        with tempfile.TemporaryDirectory(prefix='relay-invalid-clock-') as directory:
            store = self.module.TelemetryStore(Path(directory))
            scope = {'projectId': 'a' * 32, 'attemptId': 'b' * 32, 'epoch': 1, 'runId': None, 'inputHash': 'c' * 64}
            store.begin(scope)
            for invalid in (float('nan'), float('inf'), -float('inf'), True):
                with self.subTest(value=repr(invalid)), self.assertRaises(ValueError):
                    store.append(scope, {'kind': 'process', 'pid': 123, 'startedAt': invalid})
                with self.subTest(deadline=repr(invalid)), self.assertRaises(ValueError):
                    store.append(scope, {'kind': 'process', 'waitStartedAt': 1000, 'timeoutAt': invalid})
            self.assertEqual(store.stats(scope)['events'], 0)

    def test_storage_is_scoped_sealed_bounded_and_independent_of_project_revision(self):
        with tempfile.TemporaryDirectory(prefix='relay-tail-') as directory:
            store = self.module.TelemetryStore(Path(directory))
            scope = {'projectId': 'a' * 32, 'attemptId': 'b' * 32, 'epoch': 1, 'runId': None, 'inputHash': 'c' * 64}
            store.begin(scope)
            for _ in range(600):
                store.append(scope, {'kind': 'text', 'text': 'x' * 2000})
            page = store.read(scope, 0)
            self.assertTrue(page['gap'])
            self.assertLessEqual(len(page['events']), 200)
            self.assertLessEqual(len(json.dumps(page).encode()), 128 * 1024 + 2048)
            tail = store.read(scope, page['nextCursor'])
            self.assertTrue(page['hasMore'])
            self.assertGreater(tail['nextCursor'], page['nextCursor'])
            self.assertLessEqual(store.stats(scope)['events'], 512)
            self.assertLessEqual(store.stats(scope)['bytes'], 256 * 1024)
            store.seal(scope)
            self.assertFalse(store.append(scope, {'kind': 'text', 'text': 'late'}))
            other = {**scope, 'epoch': 2}
            with self.assertRaises(ValueError):
                store.read(other, 0)
            restored = self.module.TelemetryStore(Path(directory))
            self.assertTrue(restored.read(scope, 0)['sealed'])

    def test_process_timing_metadata_survives_tail_eviction_and_reopen(self):
        with tempfile.TemporaryDirectory(prefix='relay-timing-') as directory:
            store = self.module.TelemetryStore(Path(directory))
            scope = {'projectId': 'a' * 32, 'attemptId': 'b' * 32, 'epoch': 1, 'runId': None, 'inputHash': 'c' * 64}
            store.begin(scope)
            store.append(scope, {'kind': 'process', 'pid': 123, 'startedAt': 1000})
            store.append(scope, {'kind': 'process', 'waitStartedAt': 1100, 'timeoutAt': 2100})
            for _ in range(520):
                store.append(scope, {'kind': 'text', 'text': 'synthetic'})
            metadata = self.module.TelemetryStore(Path(directory)).read(scope, 0).get('metadata', {})
            self.assertEqual(metadata.get('pid'), 123, 'Actual PID/deadline cannot disappear when public text rotates')
            self.assertEqual(metadata['timeoutAt'], 2100)

    def test_retention_keeps_active_tail_and_only_recent_twenty_sealed_attempts(self):
        with tempfile.TemporaryDirectory(prefix='relay-retain-') as directory:
            store = self.module.TelemetryStore(Path(directory))
            def scope(number):
                return {'projectId': 'a' * 32, 'attemptId': format(number, '032x'), 'epoch': 1, 'runId': None, 'inputHash': 'c' * 64}
            active = scope(1)
            store.begin(active)
            for number in range(2, 25):
                item = scope(number)
                store.begin(item); store.seal(item)
            self.assertFalse(store.read(active, 0)['sealed'])
            self.assertIsNone(store.find('a' * 32, attempt_id=format(2, '032x')))
            self.assertIsNotNone(store.find('a' * 32, attempt_id=format(24, '032x')))


if __name__ == '__main__':
    unittest.main()
