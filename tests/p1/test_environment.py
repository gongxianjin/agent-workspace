"""Portable path contracts; synthetic files, no Claude or install commands executed."""
import hashlib
import importlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest
from dataclasses import replace

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


class EnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.module = importlib.import_module('environment') if (ROOT / 'p1/environment.py').exists() else None
        self.assertIsNotNone(self.module, 'Portable dependency resolution must not depend on this developer machine')
        self.temp = tempfile.TemporaryDirectory(prefix='接力 portable ')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def bundle(self):
        paths = {'pythonConsole': 'vendor/python/python.exe', 'pythonWindowed': 'vendor/python/pythonw.exe',
            'node': 'vendor/node/node.exe', 'reason': 'vendor/reason/reason-network.exe',
            'playwright': 'vendor/playwright/package.json', 'chromium': 'vendor/chromium/chrome.exe'}
        files = {}
        for key, relative in paths.items():
            target = self.root / relative
            target.parent.mkdir(parents=True, exist_ok=True)
            raw = ('synthetic dependency ' + key).encode()
            target.write_bytes(raw)
            files[key] = {'path': relative, 'sha256': hashlib.sha256(raw).hexdigest()}
        manifest = {'format': 1, 'platform': 'win32-x64', 'files': files}
        (self.root / 'portable.json').write_text(json.dumps(manifest), encoding='utf-8')
        return manifest

    def test_portable_dependencies_survive_relocation_without_system_path(self):
        self.bundle()
        moved = self.root / '移动后的 工作台'
        original = self.root / 'original'
        original.mkdir()
        for path in list(self.root.iterdir()):
            if path != original:
                shutil.move(str(path), original / path.name)
        original.rename(moved)
        paths = self.module.resolve_environment(moved)
        for name in ('pythonConsole', 'pythonWindowed', 'node', 'reason', 'playwright', 'chromium'):
            self.assertTrue(getattr(paths, name).is_file())
            self.assertTrue(getattr(paths, name).is_relative_to(moved))

    def test_missing_changed_and_outside_dependency_are_rejected(self):
        manifest = self.bundle()
        target = self.root / manifest['files']['node']['path']
        target.write_text('tampered', encoding='utf-8')
        with self.assertRaisesRegex(ValueError, 'integrity'):
            self.module.resolve_environment(self.root)
        target.unlink()
        with self.assertRaises(ValueError):
            self.module.resolve_environment(self.root)
        manifest['files']['node']['path'] = '../outside.exe'
        (self.root / 'portable.json').write_text(json.dumps(manifest), encoding='utf-8')
        with self.assertRaises(ValueError):
            self.module.resolve_environment(self.root)

    def test_native_claude_is_found_without_npm_or_path_and_cmd_is_not_executed(self):
        native = self.root / '.local/bin/claude.exe'
        native.parent.mkdir(parents=True)
        native.write_bytes(b'MZsynthetic executable candidate')
        env = {'USERPROFILE': str(self.root), 'PATH': ''}
        self.assertEqual(self.module.discover_claude(env), native.resolve())
        native.rename(native.with_suffix('.cmd'))
        self.assertIsNone(self.module.discover_claude(env))

    def test_npm_executable_fallback_and_absolute_path_only(self):
        npm = self.root / 'npm/node_modules/@anthropic-ai/claude-code/bin/claude.exe'
        npm.parent.mkdir(parents=True)
        npm.write_bytes(b'MZsynthetic')
        self.assertEqual(self.module.discover_claude({'APPDATA': str(self.root), 'PATH': '.'}), npm.resolve())
        npm.write_text('not an exe')
        self.assertIsNone(self.module.discover_claude({'APPDATA': str(self.root), 'PATH': ''}))

    def test_fixed_probe_commands_gate_missing_old_and_unknown_clients_without_model(self):
        detector = getattr(self.module, 'SetupStatus', None)
        self.assertTrue(callable(detector), 'Onboarding needs cached bounded non-model probes')
        # A temporary setup-data root must not inherit or write release consent.
        paths = self.module.source_or_bundle(ROOT)
        calls = []
        def probe(executable, argv):
            calls.append(tuple(argv))
            if argv == ['--version']:
                return {'exitCode': 0, 'stdout': '2.1.307 (Claude Code)'}
            if argv == ['--help']:
                return {'exitCode': 0, 'stdout': '--tools --permission-mode --strict-mcp-config --mcp-config --allowedTools --output-format stream-json --verbose --include-partial-messages'}
            return {'exitCode': 0, 'stdout': '{"syntheticUnknown":true,"token":"never-publish"}'}
        candidate = Path(sys.executable)
        setup = detector(self.root, paths=paths, discover=lambda: candidate, probe=probe)
        self.assertFalse(setup.public()['canExecute'])
        state = setup.recheck()
        self.assertEqual(state['login'], 'unknown')
        self.assertFalse(state['canExecute'])
        self.assertNotIn('never-publish', json.dumps(state))
        self.assertEqual(calls, [('--version',), ('--help',), ('auth', 'status', '--json')])
        self.assertEqual(setup.public(), state, 'Read-only polling must not run a command')
        self.assertEqual(len(calls), 3)
        setup.confirm_existing()
        self.assertEqual(setup.snapshot(), candidate.resolve())
        state = setup.recheck()
        self.assertFalse(state['canExecute'], 'Explicit unknown-provider consent is scoped to one detection')
        setup = detector(self.root, paths=paths, discover=lambda: None, probe=probe)
        self.assertEqual(setup.recheck()['client'], 'missing')
        self.assertEqual(len(calls), 6, 'Missing CLI must not trigger fallback commands or models')

    def test_incompatible_cli_cannot_be_unlocked_as_unknown_provider(self):
        detector = getattr(self.module, 'SetupStatus', None)
        self.assertTrue(callable(detector), 'Old clients need an explicit compatibility gate')
        setup = detector(self.root, paths=self.module.source_or_bundle(ROOT), discover=lambda: Path(sys.executable),
            probe=lambda exe, argv: {'exitCode': 0, 'stdout': '0.1.0' if argv == ['--version'] else '--help'})
        self.assertEqual(setup.recheck()['compatibility'], 'unsupported')
        with self.assertRaises(ValueError):
            setup.confirm_existing()
        with self.assertRaises(ValueError):
            setup.snapshot()

    def test_existing_but_broken_browser_dependency_does_not_report_healthy(self):
        paths = replace(self.module.source_or_bundle(ROOT), node=Path(sys.executable))
        setup = self.module.SetupStatus(self.root, paths=paths, discover=lambda: None)
        state = setup.recheck()
        self.assertNotEqual(state['dependencies'], 'healthy', 'Existing files are not proof that the fixed browser checker can run')
        self.assertFalse(state['canExecute'])

    def test_healthy_dependencies_require_actual_owned_browser_assertions(self):
        setup = self.module.SetupStatus(self.root, paths=self.module.source_or_bundle(ROOT), discover=lambda: None)
        self.assertEqual(setup.recheck()['dependencies'], 'healthy')
        self.assertFalse(setup.public()['canExecute'], 'Local dependency health must not imply Claude is connected')

    def test_total_detection_output_budget_stops_before_authentication_probe(self):
        auth_calls = []
        def probe(executable, argv):
            if argv == ['--version']:
                return {'exitCode': 0, 'stdout': '2.1.307', 'outputBytes': 70000}
            if argv == ['--help']:
                return {'exitCode': 0, 'stdout': ' '.join(self.module.REQUIRED_FLAGS) + ' stream-json', 'outputBytes': 70000}
            auth_calls.append(argv)
            return {'exitCode': 0, 'stdout': '{}'}
        setup = self.module.SetupStatus(self.root, paths=self.module.source_or_bundle(ROOT),
            discover=lambda: Path(sys.executable), probe=probe)
        state = setup.recheck()
        self.assertFalse(state['canExecute'])
        self.assertEqual(state['compatibility'], 'unknown')
        self.assertIsNone(state['version'], 'A failed detection must not retain a partial ready-looking result')
        self.assertEqual(auth_calls, [], 'Exhausting the budget must prevent any further client command')

    def test_documented_unauthenticated_exit_is_blocked_and_unknown_schema_never_green(self):
        detector = getattr(self.module, 'SetupStatus', None)
        self.assertTrue(callable(detector))
        self.assertEqual(detector.login_status({'exitCode': 1, 'stdout': '{}'}), 'logged_out')
        self.assertEqual(detector.login_status({'exitCode': 0, 'stdout': '{"unexpected":"schema"}'}), 'unknown')
        self.assertEqual(detector.login_status({'exitCode': 7, 'stdout': '{}'}), 'unknown')
        self.assertEqual(detector.login_status({'exitCode': 1, 'stdout': 'not JSON'}), 'unknown')

    def test_portable_tree_with_missing_manifest_cannot_fall_back_to_developer_paths(self):
        (self.root / 'vendor').mkdir()
        with self.assertRaisesRegex(ValueError, 'manifest'):
            self.module.source_or_bundle(self.root)

    def test_first_use_bundle_opens_without_browser_but_keeps_other_dependencies_pinned(self):
        manifest = self.bundle()
        browser = self.root / manifest['files'].pop('chromium')['path']
        browser.unlink()
        manifest.update(format=2, browser={'mode': 'first-use', 'version': '145.0.7632.6', 'revision': '1208'})
        (self.root / 'portable.json').write_text(json.dumps(manifest), encoding='utf-8')
        try:
            paths = self.module.resolve_environment(self.root)
        except ValueError as error:
            self.fail('A valid first-use bundle must open before downloading a browser: ' + str(error))
        self.assertEqual(paths.chromium, self.root / 'userdata/browser/chromium-1208/chrome-win64/chrome.exe')
        self.assertFalse(paths.chromium.exists(), 'Opening the workbench must not download the browser')
        self.assertFalse((self.root / 'userdata').exists())
        paths.node.write_bytes(b'tampered node')
        with self.assertRaisesRegex(ValueError, 'integrity'):
            self.module.resolve_environment(self.root)

    def test_first_use_setup_is_read_only_and_requires_verified_browser_before_health(self):
        from unittest.mock import patch
        manifest = self.bundle()
        (self.root / manifest['files'].pop('chromium')['path']).unlink()
        manifest.update(format=2, browser={'mode': 'first-use', 'version': '145.0.7632.6', 'revision': '1208'})
        (self.root / 'portable.json').write_text(json.dumps(manifest), encoding='utf-8')
        setup = self.module.SetupStatus(self.root, discover=lambda: None)
        self.assertIn('browser', setup.public(), 'First-use preparation must be visible before CLI detection')
        self.assertEqual(setup.public()['browser']['status'], 'missing')
        self.assertFalse((self.root / 'userdata').exists())
        with patch.object(self.module, 'dependency_health', side_effect=AssertionError('Unverified browser reached execution')):
            state = setup.recheck()
            self.assertEqual(state['dependencies'], 'missing')
        self.assertFalse(state['canExecute'])
        self.assertIn('浏览器', state['message'])
        # An executable alone, without its verified install receipt, must not run.
        setup.paths.chromium.parent.mkdir(parents=True)
        setup.paths.chromium.write_bytes(b'MZsynthetic unknown executable')
        with patch.object(self.module, 'dependency_health', side_effect=AssertionError('Unknown browser reached execution')):
            self.assertNotEqual(setup.recheck()['dependencies'], 'healthy')

    def test_absent_manifest_does_not_silently_use_global_dependencies(self):
        with self.assertRaisesRegex(ValueError, 'manifest'):
            self.module.resolve_environment(self.root)


if __name__ == '__main__':
    unittest.main()
