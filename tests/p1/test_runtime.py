"""Adapter/tool boundaries tested without paying for or simulating model replies."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
PATH = ROOT / 'p1/runtime.py'
spec = importlib.util.spec_from_file_location('relay_runtime', PATH) if PATH.exists() else None
runtime = importlib.util.module_from_spec(spec) if spec else None
if spec:
    sys.path.insert(0, str(ROOT / 'p1'))
    spec.loader.exec_module(runtime)


class RuntimeTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(runtime, 'Real CLI adapters and role scopes are required')

    def test_adapters_use_argv_and_never_permission_bypass(self):
        from unittest.mock import patch
        from environment import RuntimePaths
        with tempfile.TemporaryDirectory(prefix='relay-adapter-') as temp:
            root = Path(temp).resolve()
            clients = {name: root / filename for name, filename in
                [('Claude Code', 'claude fixture'), ('Codex', 'codex fixture'), ('BoBot', 'bobot fixture.cjs')]}
            paths = RuntimePaths(root / 'console python', root / 'windowed python', root / 'node fixture',
                root / 'reason fixture', root / 'package.json', root / 'browser fixture')
            # Command-policy inputs only: no executable discovery, execution or native readiness claim.
            with patch.dict(runtime.CLIENTS, clients):
                for client in ['Claude Code', 'Codex', 'BoBot']:
                    argv, source = runtime.cli_command(client, root, 'wake up',
                        'http://127.0.0.1:12345/mcp', 'test-only-capability', paths=paths)
                    self.assertIsInstance(argv, list)
                    self.assertEqual(argv[0], str(paths.node if client == 'BoBot' else clients[client]))
                    self.assertTrue(Path(argv[0]).is_absolute())
                    self.assertNotIn('dangerously', ' '.join(argv))
                    self.assertNotIn('bypassPermissions', ' '.join(argv))
                    self.assertIsInstance(source, bytes)
                    config = json.loads((root / 'relay-mcp.json').read_text())['mcpServers']['relay']
                    self.assertEqual(config, {'command': str(paths.pythonConsole),
                        'args': ['-I', '-X', 'utf8', str(runtime.BASE / 'p1/reason_mcp.py')],
                        'env': {'RELAY_MCP_ENDPOINT': 'http://127.0.0.1:12345/mcp',
                            'RELAY_MCP_TOKEN': 'test-only-capability'}})
                    if client == 'Codex':
                        self.assertIn('workspace-write', argv)
                        self.assertIn('approval_policy="never"', argv)
                        self.assertEqual(argv[argv.index('-C') + 1], str(root))
                    else:
                        self.assertEqual(argv[argv.index('--permission-mode') + 1], 'dontAsk')
                        self.assertEqual(argv[argv.index('--tools') + 1], '')
                        self.assertIn('--strict-mcp-config', argv)
                        self.assertEqual(argv[argv.index('--allowedTools') + 1], 'mcp__relay__*')
                    if client == 'BoBot':
                        self.assertEqual(argv[1], str(clients[client]))
                        self.assertIn('mcp__windows-mcp__*', argv)
                        self.assertEqual(json.loads(source)['message']['role'], 'user')

    def test_claude_requests_partial_stream_without_changing_permission_contract(self):
        with tempfile.TemporaryDirectory(prefix='relay-stream-argv-') as temp:
            argv, _ = runtime.cli_command('Claude Code', Path(temp), 'fixture', 'http://127.0.0.1:1/mcp', 'fixture')
            self.assertEqual(argv[argv.index('--output-format') + 1], 'stream-json')
            self.assertIn('--verbose', argv)
            self.assertIn('--include-partial-messages', argv)
            self.assertEqual(argv[argv.index('--permission-mode') + 1], 'dontAsk')
            self.assertEqual(argv[argv.index('--tools') + 1], '')

    def test_tool_events_wrap_actual_host_handler_success_and_failure_without_arguments(self):
        with tempfile.TemporaryDirectory(prefix='relay-tool-observe-') as temp:
            runner = runtime.RoleRunner(None, Path(temp))
            events = []
            runner.telemetry = events.append
            runner.exchange = runtime.Exchange('fixture', None, 'fixture', 1, 'fixture')
            def handler(name, arguments):
                self.assertTrue(events, 'Actual host dispatch must emit start before doing file/check work')
                self.assertEqual(events[-1], {'kind': 'tool', 'tool': name, 'phase': 'started'})
                if name == 'read_project':
                    raise ValueError('synthetic private exception')
                return {'text': 'private tool result'}
            runner._handler = handler
            self.assertEqual(runner.dispatch('write_project', {'text': 'private input'})['text'], 'private tool result')
            with self.assertRaises(ValueError):
                runner.dispatch('read_project', {})
            self.assertEqual([event.get('outcome') for event in events], [None, 'ok', None, 'error'])
            self.assertNotIn('private', json.dumps(events))

    def test_delayed_stdout_callback_cannot_append_text_after_cli_wait_returns(self):
        from unittest.mock import patch
        import threading
        with tempfile.TemporaryDirectory(prefix='relay-late-stream-') as temp:
            runner = runtime.RoleRunner(None, Path(temp) / 'evidence')
            observed, callbacks, messages = [], [], []
            release, finished = threading.Event(), threading.Event()
            runner.telemetry = observed.append
            class TransportFixture:
                def register(self, name):
                    return 'fixture/' + name
                def send(self, sender, recipient, body):
                    identifier = 'fixture-' + str(len(messages))
                    messages.append({'id': identifier, 'sender': 'fixture/' + sender, 'recipient': recipient, 'body': body})
                    return identifier
                def listen(self, name):
                    return [message for message in messages if message['recipient'] == 'fixture/' + name]
            class DelayedBoundary:
                def __init__(self, *args, **kwargs):
                    self.observer = kwargs['stdout_observer']
                def wait(self, timeout):
                    task = runner.dispatch('receive_task', {})
                    runner.dispatch('send_ack', {'nonce': task['nonce'], 'summary': 'synthetic only'})
                    runner.dispatch('send_result', {'nonce': task['nonce'], 'result': {'summary': 'synthetic only'}})
                    def late():
                        release.wait(5)
                        raw = {'type': 'assistant', 'message': {'id': 'late-fixture', 'role': 'assistant',
                            'content': [{'type': 'text', 'text': 'must-not-appear-after-cli-exit'}]}}
                        self.observer((json.dumps(raw) + '\n').encode())
                        finished.set()
                    thread = threading.Thread(target=late)
                    callbacks.append(thread)
                    thread.start()
                    return {'pid': 123, 'exitCode': 0, 'treeStopped': True, 'timedOut': False,
                        'outputLimited': False, 'observationGap': True, 'stdout': '', 'stderr': ''}
                def close(self):
                    pass
            runner.transport = TransportFixture()
            attempt = {'id': 'late-fixture', 'projectId': 'fixture', 'runId': None,
                'epoch': 1, 'inputHash': 'fixture', 'role': 'architect'}
            try:
                with patch.object(runtime, 'JobProcess', DelayedBoundary):
                    runner.run(attempt, 'Claude Code', Path(temp) / 'workspace', {})
                self.assertTrue(any(event.get('stage') == 'cli_exited' for event in observed))
                release.set()
                self.assertTrue(finished.wait(3))
                self.assertFalse(any(event['kind'] == 'text' for event in observed),
                    'The bounded flush may leave a callback alive, but cannot leave its text capability alive')
            finally:
                release.set()
                for thread in callbacks:
                    thread.join(3)

    def test_real_job_flush_revokes_stdout_already_parsing_before_cli_exit(self):
        if sys.platform == 'linux':
            from test_linux_process import LinuxProcessTests
            with tempfile.TemporaryDirectory(prefix='relay-runtime-stream-') as temp:
                outcome = LinuxProcessTests().run_case(Path(temp), 'runtime-stream')
            self.assertTrue(outcome['available'], outcome)
            self.assertTrue(outcome['runtimeStreamRevoked'], outcome)
            return
        self.assert_inflight_stream_revoked()

    def assert_inflight_stream_revoked(self):
        from unittest.mock import patch
        import threading
        from process_control import JobProcess
        with tempfile.TemporaryDirectory(prefix='relay-inflight-stream-') as temp:
            runner = runtime.RoleRunner(None, Path(temp) / 'evidence')
            observed, messages, children = [], [], []
            entered, release, finished = threading.Event(), threading.Event(), threading.Event()
            runner.telemetry = observed.append
            class TransportFixture:
                def register(self, name):
                    return 'fixture/' + name
                def send(self, sender, recipient, body):
                    identifier = 'fixture-' + str(len(messages))
                    messages.append({'id': identifier, 'sender': 'fixture/' + sender, 'recipient': recipient, 'body': body})
                    return identifier
                def listen(self, name):
                    return [message for message in messages if message['recipient'] == 'fixture/' + name]
            class SlowParser(runtime.ClaudeStreamParser):
                def feed(self, data):
                    entered.set()
                    if not release.wait(8):
                        raise RuntimeError('Test did not release the parser')
                    try:
                        return super().feed(data)
                    finally:
                        finished.set()
            class NativeBoundaryFixture:
                def __init__(self, *args, **kwargs):
                    raw = {'type': 'assistant', 'message': {'id': 'inflight', 'role': 'assistant',
                        'content': [{'type': 'text', 'text': 'must never publish after exit'}]}}
                    code = 'import sys; sys.stdout.write(' + repr(json.dumps(raw) + '\n') + '); sys.stdout.flush()'
                    self.child = JobProcess([sys.executable, '-I', '-X', 'utf8', '-c', code], temp,
                        stdout_observer=kwargs['stdout_observer'])
                    children.append(self.child)
                def wait(self, timeout):
                    task = runner.dispatch('receive_task', {})
                    runner.dispatch('send_ack', {'nonce': task['nonce'], 'summary': 'synthetic only'})
                    runner.dispatch('send_result', {'nonce': task['nonce'], 'result': {'summary': 'synthetic only'}})
                    if not entered.wait(3):
                        raise RuntimeError('Real stdout never reached the parser')
                    return self.child.wait(timeout)
                def close(self):
                    self.child.close()
            runner.transport = TransportFixture()
            attempt = {'id': 'inflight-fixture', 'projectId': 'fixture', 'runId': None,
                'epoch': 1, 'inputHash': 'fixture', 'role': 'architect'}
            try:
                with patch.object(runtime, 'JobProcess', NativeBoundaryFixture), patch.object(runtime, 'ClaudeStreamParser', SlowParser):
                    result = runner.run(attempt, 'Claude Code', Path(temp) / 'workspace', {}, timeout=5)
                self.assertTrue(result['treeStopped'])
                self.assertFalse(finished.is_set(), 'The real bounded flush must return without waiting for the parser')
                self.assertTrue(any(event.get('code') == 'stream_gap' for event in observed))
                self.assertTrue(any(event.get('stage') == 'cli_exited' for event in observed))
                release.set()
                self.assertTrue(finished.wait(3))
                for child in children:
                    child.observer_thread.join(3)
                    self.assertFalse(child.observer_thread.is_alive())
                self.assertFalse(any(event['kind'] == 'text' for event in observed))
            finally:
                release.set()
                for child in children:
                    child.close()
                    child.observer_thread.join(3)

    def test_correlated_result_and_public_success_cannot_override_bad_process_exit(self):
        from unittest.mock import patch
        for failure in ({'exitCode': 7}, {'treeStopped': False}, {'timedOut': True}, {'outputLimited': True}):
            with self.subTest(failure=failure), tempfile.TemporaryDirectory(prefix='relay-result-gate-') as temp:
                runner = runtime.RoleRunner(None, Path(temp) / 'evidence')
                observed, messages = [], []
                runner.telemetry = observed.append
                class TransportFixture:
                    def register(self, name):
                        return 'fixture/' + name
                    def send(self, sender, recipient, body):
                        identifier = 'fixture-' + str(len(messages))
                        messages.append({'id': identifier, 'sender': 'fixture/' + sender, 'recipient': recipient, 'body': body})
                        return identifier
                    def listen(self, name):
                        return [message for message in messages if message['recipient'] == 'fixture/' + name]
                class FailedNativeFixture:
                    def __init__(self, *args, **kwargs):
                        self.observer = kwargs['stdout_observer']
                    def wait(self, timeout):
                        task = runner.dispatch('receive_task', {})
                        runner.dispatch('send_ack', {'nonce': task['nonce'], 'summary': 'synthetic only'})
                        runner.dispatch('send_result', {'nonce': task['nonce'], 'result': {'summary': 'claims success'}})
                        self.observer((json.dumps({'type': 'assistant', 'message': {'id': 'claims-success', 'role': 'assistant',
                            'content': [{'type': 'text', 'text': 'Finished successfully.'}]}}) + '\n').encode())
                        self.observer(b'')
                        return {'pid': 123, 'exitCode': 0, 'treeStopped': True, 'timedOut': False,
                            'outputLimited': False, 'stdout': '', 'stderr': '', **failure}
                    def close(self):
                        pass
                runner.transport = TransportFixture()
                attempt = {'id': 'failed-fixture', 'projectId': 'fixture', 'runId': None,
                    'epoch': 1, 'inputHash': 'fixture', 'role': 'architect'}
                with patch.object(runtime, 'JobProcess', FailedNativeFixture), self.assertRaisesRegex(RuntimeError, 'did not finish cleanly'):
                    runner.run(attempt, 'Claude Code', Path(temp) / 'workspace', {})
                self.assertTrue(any(event['kind'] == 'text' for event in observed))
                self.assertIsNone(runner.last, 'Public success and a real protocol result cannot grant completion')
                self.assertFalse((Path(temp) / 'evidence/failed-fixture/summary.json').exists())

    def test_detected_claude_path_is_used_instead_of_global_candidate(self):
        with tempfile.TemporaryDirectory(prefix='relay-snapshot-') as temp:
            detected = Path(temp) / 'detected-claude.exe'
            detected.write_bytes(b'MZsynthetic')
            try:
                argv, _ = runtime.cli_command('Claude Code', Path(temp), 'fixture', 'http://127.0.0.1:1/mcp',
                    'synthetic', executable=detected)
            except TypeError:
                self.fail('Detection and execution must use the same trusted path snapshot')
            self.assertEqual(Path(argv[0]), detected)

    def test_other_clients_keep_their_own_provider_configuration(self):
        inherited = {'PATH': 'local', 'ANTHROPIC_AUTH_TOKEN': 'fixture-secret', 'ANTHROPIC_BASE_URL': 'fixture-endpoint',
            'CLAUDE_CODE_MESSAGING_TOKEN': 'fixture-session', 'CLAUDE_CODE_SESSION_ID': 'fixture-parent'}
        bobot = runtime.client_environment('BoBot', inherited)
        self.assertNotIn('ANTHROPIC_BASE_URL', bobot)
        self.assertNotIn('ANTHROPIC_AUTH_TOKEN', bobot)
        self.assertNotIn('CLAUDE_CODE_MESSAGING_TOKEN', bobot)
        self.assertEqual(bobot['PATH'], 'local')
        self.assertEqual(inherited['ANTHROPIC_AUTH_TOKEN'], 'fixture-secret')
        claude = runtime.client_environment('Claude Code', inherited)
        self.assertIn('ANTHROPIC_AUTH_TOKEN', claude)
        self.assertNotIn('CLAUDE_CODE_SESSION_ID', claude)
        with tempfile.TemporaryDirectory(prefix='relay-codex-config-') as temp:
            argv, _ = runtime.cli_command('Codex', Path(temp), 'probe', 'http://127.0.0.1:1/mcp', 'fixture')
            self.assertNotIn('--ignore-user-config', argv, 'Existing provider settings must not be discarded')

    def test_mcp_uses_console_python_even_when_host_is_windowed(self):
        from unittest.mock import patch
        console = Path(sys.executable).with_name('python.exe')
        with tempfile.TemporaryDirectory(prefix='relay-windowed-mcp-') as temp:
            with patch.object(sys, 'executable', str(console.with_name('pythonw.exe'))):
                runtime.cli_command('Claude Code', Path(temp), 'fixture', 'http://127.0.0.1:1/mcp', 'fixture')
            config = json.loads(Path(temp, 'relay-mcp.json').read_text())
            self.assertEqual(Path(config['mcpServers']['relay']['command']), console,
                'Windowless host must not break MCP stdin/stdout')

    def test_real_windowless_host_keeps_mcp_console_stdio_bidirectional(self):
        if sys.platform == 'linux':
            from test_linux_process import LinuxProcessTests
            LinuxProcessTests().test_generated_mcp_config_roundtrips_real_native_console_stdio()
            return
        from winjob import JobProcess
        with tempfile.TemporaryDirectory(prefix='接力 mcp ') as temp:
            directory = Path(temp)
            output = directory / 'stdio-result.json'
            code = f'''import json,sys
from pathlib import Path
sys.path.insert(0, {str(ROOT / 'p1')!r})
from runtime import cli_command
from winjob import JobProcess
cli_command('Claude Code', Path({temp!r}), 'fixture', 'http://127.0.0.1:1/mcp', 'synthetic-only')
config=json.loads(Path({temp!r}, 'relay-mcp.json').read_text())['mcpServers']['relay']
requests=[{{'jsonrpc':'2.0','id':1,'method':'initialize'}},{{'jsonrpc':'2.0','id':2,'method':'tools/list'}}]
source=('\\n'.join(json.dumps(r) for r in requests)+'\\n').encode()
child=JobProcess([config['command'],*config['args']], {temp!r}, env=config['env'], input_data=source)
try:
 result=child.wait(8)
finally:
 child.close()
Path({str(output)!r}).write_text(json.dumps({{'exitCode':result['exitCode'],'treeStopped':result['treeStopped'],
 'replies':[json.loads(line) for line in result['stdout'].splitlines()]}}),encoding='utf-8')
'''
            host = JobProcess([str(Path(sys.executable).with_name('pythonw.exe')), '-I', '-X', 'utf8', '-c', code], directory)
            try:
                result = host.wait(12)
            finally:
                host.close()
            self.assertEqual(result['exitCode'], 0)
            self.assertTrue(result['treeStopped'])
            report = json.loads(output.read_text(encoding='utf-8'))
            self.assertEqual(report['exitCode'], 0)
            self.assertTrue(report['treeStopped'])
            self.assertEqual([reply['id'] for reply in report['replies']], [1, 2])
            self.assertEqual(report['replies'][0]['result']['serverInfo']['name'], 'relay-scoped')
            self.assertIn('send_ack', [tool['name'] for tool in report['replies'][1]['result']['tools']])

    def test_tester_cannot_write_product_or_evidence(self):
        with tempfile.TemporaryDirectory(prefix='relay-scope-') as temp:
            files = runtime.ProjectFiles(Path(temp), 'tester')
            for name in ['product/index.html', 'evidence/pass.json', '../secret']:
                with self.subTest(name=name), self.assertRaises(ValueError):
                    files.write(name, 'not allowed')
            files.write('tests/browser.json', '{"cases":[]}')
            self.assertEqual(files.read('tests/browser.json'), '{"cases":[]}')

    def test_developer_can_write_product_but_not_tester_specs(self):
        with tempfile.TemporaryDirectory(prefix='relay-scope-') as temp:
            files = runtime.ProjectFiles(Path(temp), 'developer')
            files.write('product/index.html', '<p>hello</p>')
            self.assertEqual(files.read('product/index.html'), '<p>hello</p>')
            with self.assertRaises(ValueError):
                files.write('tests/browser.json', 'changed expectations')
            with self.assertRaises(ValueError):
                runtime.ProjectFiles(Path(temp), 'architect').write('product/x.txt', 'x')

    def test_frozen_tester_specs_cannot_be_rewritten(self):
        with tempfile.TemporaryDirectory(prefix='relay-frozen-') as temp:
            files = runtime.ProjectFiles(Path(temp), 'tester')
            files.write('tests/browser.json', '{"cases":[]}')
            self.assertTrue(callable(getattr(files, 'freeze', None)), 'Retests need a frozen specification capability')
            files.freeze()
            self.assertEqual(files.read('tests/browser.json'), '{"cases":[]}')
            with self.assertRaises(ValueError):
                files.write('tests/browser.json', 'lowered expectations')

    def test_stop_before_cli_start_cannot_be_reset_by_run(self):
        with tempfile.TemporaryDirectory(prefix='relay-cancel-') as temp:
            runner = runtime.RoleRunner(None, Path(temp) / 'evidence')
            self.assertTrue(runner.stop())
            failure = None
            try:
                runner.run({'id': 'cancelled', 'projectId': 'fixture', 'runId': None, 'epoch': 1,
                    'inputHash': 'fixture', 'role': 'developer'}, 'Claude Code', Path(temp) / 'workspace', {})
            except Exception as error:
                failure = error
            self.assertIsInstance(failure, RuntimeError, 'Cancellation must be checked before any transport operation')
            self.assertIn('cancelled', str(failure))
            self.assertFalse((Path(temp) / 'workspace').exists())

    def test_stopping_freezes_host_file_capability_before_return(self):
        with tempfile.TemporaryDirectory(prefix='relay-revoked-') as temp:
            runner = runtime.RoleRunner(None, Path(temp) / 'evidence')
            self.assertTrue(callable(getattr(runner, 'dispatch', None)), 'Host tools need an explicit revocation barrier')
            # dispatch without a live exchange must be denied, not revive file access.
            runner.stop()
            with self.assertRaises((ValueError, RuntimeError)):
                runner.dispatch('write_project', {'relativePath': 'product/late.txt', 'text': 'late'})
            self.assertFalse((Path(temp) / 'product/late.txt').exists())

    def test_stop_waits_for_an_inflight_python_file_handler(self):
        import threading
        with tempfile.TemporaryDirectory(prefix='relay-host-drain-') as temp:
            runner = runtime.RoleRunner(None, Path(temp) / 'evidence')
            entered, release, stopped = threading.Event(), threading.Event(), threading.Event()
            runner.exchange = runtime.Exchange('fixture', None, 'fixture-attempt', 1, 'fixture')
            def file_handler(name, arguments):
                entered.set()
                release.wait(3)
                Path(temp, 'late.txt').write_text('completed before stop returns')
            runner._handler = file_handler
            writer = threading.Thread(target=lambda: runner.dispatch('write_project', {}))
            stopper = threading.Thread(target=lambda: (runner.stop(), stopped.set()))
            writer.start()
            self.assertTrue(entered.wait(2))
            stopper.start()
            try:
                self.assertFalse(stopped.wait(.1))
            finally:
                release.set(); writer.join(3); stopper.join(3)
            self.assertTrue(stopped.is_set())
            with self.assertRaises(ValueError):
                runner.dispatch('write_project', {})

    def test_first_actual_browser_check_freezes_all_tester_writes_before_result(self):
        from unittest.mock import patch
        from checks import CheckRunner
        with tempfile.TemporaryDirectory(prefix='relay-first-freeze-') as temp:
            root = Path(temp)
            workspace = root / 'workspace'
            (workspace / 'product').mkdir(parents=True)
            (workspace / 'product/index.html').write_text('<!doctype html><p>fixture product</p>', encoding='utf-8')
            runner = runtime.RoleRunner(None, root / 'evidence')
            messages, denied = [], []
            # Only the paid native CLI/reason boundary is replaced; host tools and Chromium are real.
            class TransportFixture:
                def register(self, name):
                    return 'fixture/' + name
                def send(self, sender, recipient, body):
                    message_id = 'fixture-message-' + str(len(messages))
                    messages.append({'id': message_id, 'sender': 'fixture/' + sender, 'recipient': recipient, 'body': body})
                    return message_id
                def listen(self, name):
                    return [message for message in messages if message['recipient'] == 'fixture/' + name]
            class NativeBoundaryFixture:
                def __init__(self, *args, **kwargs):
                    pass
                def wait(self, timeout):
                    task = runner.dispatch('receive_task', {})
                    nonce = task['nonce']
                    runner.dispatch('send_ack', {'nonce': nonce, 'summary': 'explicit fixture ACK'})
                    runner.dispatch('write_project', {'relativePath': 'tests/acceptance.md', 'text': 'Before first actual check'})
                    runner.dispatch('write_project', {'relativePath': 'tests/browser.json', 'text': json.dumps({'version': 1, 'cases': [{'name': 'actual Chromium fixture', 'steps': [{'op': 'text', 'selector': 'p', 'value': 'fixture product'}]}]})})
                    evidence = runner.dispatch('run_check', {'checkId': 'browser'})
                    for relative in ('tests/acceptance.md', 'tests/browser.json'):
                        try:
                            runner.dispatch('write_project', {'relativePath': relative, 'text': 'must not change frozen bytes'})
                        except ValueError:
                            denied.append(relative)
                    runner.dispatch('send_result', {'nonce': nonce, 'result': {'summary': 'fixture role, actual browser', 'evidenceId': evidence['id']}})
                    return {'pid': 123, 'exitCode': 0, 'treeStopped': True, 'timedOut': False, 'outputLimited': False, 'stdout': '', 'stderr': ''}
                def close(self):
                    pass
            runner.transport = TransportFixture()
            checker = CheckRunner()
            attempt = {'id': 'fixture-first-freeze', 'projectId': 'fixture', 'runId': None, 'epoch': 1, 'inputHash': 'fixture', 'role': 'tester'}
            with patch.object(runtime, 'JobProcess', NativeBoundaryFixture):
                result = runner.run(attempt, 'Claude Code', workspace, {}, checker=checker.run)
            self.assertEqual(denied, ['tests/acceptance.md', 'tests/browser.json'], 'First actual check must revoke writes before returning to the tester')
            self.assertEqual(checker.verify(result['result']['evidenceId'], workspace)['exitCode'], 0)

    def test_snapshot_hash_changes_with_real_product_bytes(self):
        with tempfile.TemporaryDirectory(prefix='relay-hash-') as temp:
            root = Path(temp)
            (root / 'a.txt').write_text('one')
            first = runtime.tree_manifest(root)
            self.assertEqual(first['files'][0]['path'], 'a.txt')
            (root / 'a.txt').write_text('two')
            self.assertNotEqual(runtime.tree_manifest(root)['sha256'], first['sha256'])


if __name__ == '__main__':
    unittest.main()
