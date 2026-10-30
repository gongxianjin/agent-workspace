"""Trusted browser checker tests: real Node/Chromium, no model replies."""
import importlib.util
import json
import os
from pathlib import Path
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))
path = ROOT / 'p1/checks.py'
spec = importlib.util.spec_from_file_location('relay_checks', path) if path.exists() else None
checks = importlib.util.module_from_spec(spec) if spec else None
if spec:
    spec.loader.exec_module(checks)


class CheckTests(unittest.TestCase):
    def setUp(self):
        self.assertIsNotNone(checks, 'A trusted runner must issue real browser evidence')
        self.temp = tempfile.TemporaryDirectory(prefix='relay-check-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        (self.root / 'product').mkdir()
        (self.root / 'tests').mkdir()
        (self.root / 'product/index.html').write_text('<!doctype html><html><body><input id="search"><p id="result">待核实</p></body></html>', encoding='utf-8')
        self.write_spec([{'op': 'fill', 'selector': '#search', 'value': '公园'}, {'op': 'text', 'selector': '#result', 'value': '待核实'}])
        self.runner = checks.CheckRunner()

    def write_spec(self, steps):
        (self.root / 'tests/browser.json').write_text(json.dumps({'version': 1, 'cases': [{'name': 'real page', 'steps': steps}]}), encoding='utf-8')

    def test_actual_browser_pass_and_artifacts_are_bound_to_bytes(self):
        result = self.runner.run(self.root, self.root / 'evidence')
        self.assertEqual(result['exitCode'], 0)
        self.assertGreaterEqual(result['assertions'], 3)
        self.assertTrue(Path(result['directory'], 'report.json').is_file())
        self.assertTrue(Path(result['directory'], 'case-1.png').is_file())
        self.assertTrue(Path(result['directory'], 'case-1.zip').is_file())
        self.assertEqual(self.runner.verify(result['id'], self.root), result)
        (self.root / 'product/index.html').write_text('<p>changed</p>')
        with self.assertRaises(ValueError):
            self.runner.verify(result['id'], self.root)
        with self.assertRaises(ValueError):
            self.runner.verify('model-invented-evidence', self.root)

    @unittest.skipUnless(os.name == 'nt', 'Windows path-length regression')
    def test_deep_attempt_uses_owned_short_evidence_and_restart_safe_artifacts(self):
        root = self.root / ('deep-' + 'x' * (150 - len(str(self.root)) - 6))
        root.mkdir()
        evidence_dir = root / 'evidence' / ('a' * 32)
        runner = checks.CheckRunner(index_root=root / 'check-index')
        self.assertGreaterEqual(len(str(evidence_dir / ('check-' + 'b' * 32) / 'snapshot/product')), 248)
        result = runner.run(self.root, evidence_dir)
        self.assertEqual(result['exitCode'], 0)
        self.assertGreater(result['assertions'], 0)
        self.assertTrue(Path(result['directory']).is_relative_to(root / 'evidence'))
        for name in ('report.json', 'evidence.json', 'process.json', 'case-1.png', 'case-1.zip'):
            self.assertTrue(checks.resolve_artifact(root, result['id'], name).is_file())
        self.assertEqual(runner.verify(result['id'], self.root), result)

    @unittest.skipUnless(os.name == 'nt', 'Windows path-length regression')
    def test_unusable_evidence_path_is_rejected_before_writes_and_keeps_cause(self):
        evidence_dir = self.root / ('x' * 180) / ('y' * 60)
        with self.assertRaisesRegex(ValueError, 'path_too_long'):
            self.runner.run(self.root, evidence_dir)
        self.assertFalse(evidence_dir.parent.exists(), 'Preflight must not leave partial evidence directories')
        self.assertEqual(self.runner.issued, {})
        self.assertIsNone(self.runner.active)
        with self.assertRaisesRegex(ValueError, 'path_too_long'):
            self.runner.verify('no-evidence-issued', self.root)

    def test_snapshot_failure_is_not_masked_as_unknown_evidence(self):
        from unittest.mock import patch
        with patch.object(checks.shutil, 'copyfile', side_effect=OSError('synthetic snapshot disk full')):
            with self.assertRaisesRegex(OSError, 'snapshot disk full'):
                self.runner.run(self.root, self.root / 'evidence')
        with self.assertRaisesRegex(ValueError, 'snapshot disk full'):
            self.runner.verify('no-evidence-issued', self.root)
        self.assertEqual(self.runner.issued, {})
        self.assertIsNone(self.runner.active)

    def test_success_after_failed_check_clears_failure_without_weakening_hash_checks(self):
        from unittest.mock import patch
        with patch.object(checks.shutil, 'copyfile', side_effect=OSError('synthetic snapshot failure')):
            with self.assertRaises(OSError):
                self.runner.run(self.root, self.root / 'failed-evidence')
        other = self.root / 'other-workspace'
        with self.assertRaisesRegex(ValueError, 'Unknown evidence'):
            self.runner.verify('invented', other)
        result = self.runner.run(self.root, self.root / 'fresh-evidence')
        self.assertEqual(self.runner.verify(result['id'], self.root), result)
        with self.assertRaisesRegex(ValueError, 'Unknown evidence'):
            self.runner.verify('invented', self.root)
        self.write_spec([{'op': 'text', 'selector': '#result', 'value': 'changed test bytes'}])
        with self.assertRaisesRegex(ValueError, 'changed product/test bytes'):
            self.runner.verify(result['id'], self.root)

    @unittest.skipUnless(os.name == 'nt', 'Windows path-length regression')
    def test_nested_snapshot_paths_are_checked_before_any_evidence_writes(self):
        relative = 'nested/' + 'n' * 130 + '/source.html'
        target = self.root / 'product' / relative
        target.parent.mkdir(parents=True)
        target.write_text('<p>nested fixture</p>', encoding='utf-8')
        runner = checks.CheckRunner(index_root=self.root / 'check-index')
        with self.assertRaisesRegex(ValueError, 'path_too_long'):
            runner.run(self.root, self.root / 'evidence' / 'attempt')
        self.assertFalse((self.root / 'evidence').exists())
        self.assertEqual(runner.issued, {})

    @unittest.skipUnless(os.name == 'nt', 'Windows path-length regression')
    def test_windows_path_budget_counts_utf16_units_before_creating_directories(self):
        evidence_dir = self.root / ('\U0001f600' * 80)
        with self.assertRaisesRegex(ValueError, 'path_too_long'):
            self.runner.run(self.root, evidence_dir)
        self.assertFalse(evidence_dir.exists())
        self.assertEqual(self.runner.issued, {})

    def test_checker_uses_explicit_local_browser_dependencies_without_user_caches(self):
        from unittest.mock import patch
        from environment import source_or_bundle
        paths = source_or_bundle(ROOT)
        try:
            runner = checks.CheckRunner(paths=paths)
        except TypeError:
            self.fail('Checker must accept trusted resolved bundle paths, not global user caches')
        environment = dict(__import__('os').environ)
        environment.update(APPDATA='', LOCALAPPDATA='', NODE_PATH='', PLAYWRIGHT_BROWSERS_PATH='0')
        with patch.dict(__import__('os').environ, environment, clear=True):
            result = runner.run(self.root, self.root / 'explicit-evidence')
        self.assertEqual(result['exitCode'], 0)
        self.assertGreater(result['assertions'], 0)

    def test_short_health_deadline_stops_the_real_checker_tree(self):
        try:
            with self.assertRaisesRegex(ValueError, 'interrupted'):
                self.runner.run(self.root, self.root / 'health-evidence', timeout=.01)
        except TypeError:
            self.fail('Local health checks need a bounded deadline on the actual checker process')
        self.assertIsNone(self.runner.active)
        self.assertEqual(self.runner.issued, {})

    def test_assertion_failure_is_not_a_model_reported_pass(self):
        self.write_spec([{'op': 'text', 'selector': '#result', 'value': 'not in page'}])
        result = self.runner.run(self.root, self.root / 'evidence')
        self.assertNotEqual(result['exitCode'], 0)
        self.assertGreater(result['assertions'], 0)

    def test_stop_before_check_start_is_not_reset(self):
        self.assertTrue(self.runner.stop())
        with self.assertRaisesRegex(ValueError, 'cancelled'):
            self.runner.run(self.root, self.root / 'evidence')
        self.assertEqual(self.runner.issued, {})

    def test_empty_skipped_or_executable_spec_is_rejected(self):
        for steps in [[], [{'op': 'skip'}], [{'op': 'eval', 'code': 'process.exit(0)'}], [{'op': 'click', 'selector': 'body'}]]:
            with self.subTest(steps=steps):
                self.write_spec(steps)
                with self.assertRaises(ValueError):
                    self.runner.run(self.root, self.root / 'evidence')

    def test_fixed_liuwa_profile_rejects_missing_features_even_if_model_spec_passes(self):
        try:
            runner = checks.CheckRunner(profile='liuwa-v1')
        except TypeError:
            self.fail('The approved sample requires fixed acceptance independent of model-selected tests')
        result = runner.run(self.root, self.root / 'evidence')
        self.assertNotEqual(result['exitCode'], 0)
        self.assertTrue(any(row['name'].startswith('fixed:') and not row['passed'] for row in result['cases']))

    def test_overflow_is_detected_by_fixed_probe_even_if_model_spec_passes(self):
        (self.root / 'product/index.html').write_text('<!doctype html><style>body{min-width:1400px}</style><p id="result">待核实</p>', encoding='utf-8')
        self.write_spec([{'op': 'text', 'selector': '#result', 'value': '待核实'}])
        result = self.runner.run(self.root, self.root / 'evidence')
        self.assertNotEqual(result['exitCode'], 0)
        report = json.loads(Path(result['directory'], 'report.json').read_text(encoding='utf-8'))
        self.assertTrue(any('overflow' in row.get('error', '') for row in report['cases']))


if __name__ == '__main__':
    unittest.main()
