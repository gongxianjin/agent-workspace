"""Offline release assembly behavior using synthetic dependency archives."""
import hashlib
import importlib.util
from html.parser import HTMLParser
import json
from pathlib import Path
import sys
import tempfile
import unittest
import zipfile

ROOT = Path(__file__).resolve().parents[2]


class PackagingTests(unittest.TestCase):
    def setUp(self):
        script = ROOT / 'tools/build_portable.py'
        self.assertTrue(script.is_file(), 'A portable release must be assembled by an audited allowlist builder')
        spec = importlib.util.spec_from_file_location('portable_builder', script)
        self.builder = importlib.util.module_from_spec(spec)
        spec.loader.exec_module(self.builder)
        self.temp = tempfile.TemporaryDirectory(prefix='relay-assembly-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)

    def archive(self, name, files):
        path = self.root / (name + '.zip')
        with zipfile.ZipFile(path, 'w') as output:
            for relative, raw in files.items():
                output.writestr(relative, raw)
        return path

    def test_extract_rejects_traversal_without_writing_outside(self):
        path = self.archive('malicious', {'../outside.txt': 'must not write'})
        with self.assertRaises(ValueError):
            self.builder.extract_archive(path, self.root / 'extract')
        self.assertFalse((self.root / 'outside.txt').exists())

    def test_archive_hash_is_checked_before_extraction(self):
        path = self.archive('dependency', {'safe.txt': 'fixture'})
        with self.assertRaisesRegex(ValueError, 'SHA256'):
            self.builder.verify_archive(path, '0' * 64)
        self.builder.verify_archive(path, hashlib.sha256(path.read_bytes()).hexdigest())

    def test_complete_offline_bundle_requires_licensed_hash_pinned_archives(self):
        source = self.root / 'source'
        (source / 'p1').mkdir(parents=True)
        (source / 'index.html').write_text('owned interface')
        names = {'pythonConsole': 'python.exe', 'pythonWindowed': 'pythonw.exe', 'node': 'node.exe',
            'reason': 'reason.exe', 'playwright': 'package.json', 'chromium': 'chrome.exe'}
        archive = self.archive('vendor', {**{name: 'synthetic' for name in names.values()}, 'LICENSE': 'synthetic permission'})
        lock = {'format': 1, 'platform': 'win32-x64', 'archives': [{'name': 'fixture', 'archive': archive.name,
            'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(), 'source': 'https://example.invalid/fixture',
            'license': 'fixture-only', 'destination': 'vendor'}], 'paths': {k: 'vendor/' + v for k, v in names.items()}}
        (self.root / 'dependencies.json').write_text(json.dumps(lock))
        target = self.root / 'release'
        self.builder.build(source, self.root, target)
        self.assertTrue((target / 'vendor/LICENSE').exists())
        self.assertTrue((target / 'portable.json').exists())
        self.assertFalse(any(p.name.startswith('stage-') for p in target.iterdir()))

    def test_first_use_bundle_omits_browser_and_npm_but_keeps_pinned_runtime_and_notices(self):
        source = self.root / 'source'
        (source / 'p1').mkdir(parents=True)
        (source / 'index.html').write_text('owned interface')
        names = {'pythonConsole': 'python.exe', 'pythonWindowed': 'pythonw.exe', 'node': 'node.exe',
            'reason': 'reason.exe', 'playwright': 'package.json'}
        archive = self.archive('first-use', {**{name: 'synthetic' for name in names.values()},
            'LICENSE': 'synthetic permission', 'node_modules/npm/index.js': 'must not ship'})
        notice = self.root / 'NOTICE.txt'
        notice.write_text('additional version-specific notice')
        lock = {'format': 2, 'platform': 'win32-x64',
            'browser': {'mode': 'first-use', 'version': '145.0.7632.6', 'revision': '1208'},
            'archives': [{'name': 'fixture', 'archive': archive.name,
                'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(), 'source': 'https://example.invalid/fixture',
                'license': 'fixture-only', 'destination': 'vendor', 'include': [*names.values(), 'LICENSE']}],
            'paths': {k: 'vendor/' + v for k, v in names.items()},
            'documents': [{'file': notice.name, 'destination': 'licenses/reason/NOTICE.txt',
                'sha256': hashlib.sha256(notice.read_bytes()).hexdigest()}]}
        (self.root / 'dependencies.json').write_text(json.dumps(lock))
        target = self.root / 'first-use-release'
        try:
            self.builder.build(source, self.root, target)
        except ValueError as error:
            self.fail('First-use release assembly must support the approved browser choice: ' + str(error))
        manifest = json.loads((target / 'portable.json').read_text())
        self.assertEqual(manifest['format'], 2)
        self.assertEqual(manifest['browser'], lock['browser'])
        self.assertNotIn('chromium', manifest['files'])
        self.assertFalse((target / 'userdata').exists())
        self.assertFalse((target / 'vendor/node_modules').exists())
        self.assertEqual((target / 'licenses/reason/NOTICE.txt').read_bytes(), notice.read_bytes())
        self.assertTrue(any(entry['path'] == 'licenses/reason/NOTICE.txt' for entry in manifest['inventory']))
        sys.path.insert(0, str(ROOT / 'p1'))
        from environment import resolve_environment
        paths = resolve_environment(target)
        self.assertTrue(paths.node.is_file())
        self.assertFalse(paths.chromium.exists())
        (target / 'licenses/reason/NOTICE.txt').write_text('tampered')
        with self.assertRaisesRegex(ValueError, 'integrity'):
            resolve_environment(target)

    def distribution_fixture(self):
        source = self.root / 'source'
        (source / 'p1').mkdir(parents=True)
        (source / 'index.html').write_text('owned interface')
        raw = 'Synthetic downstream document; not a license approval.\n仅测试当前文档。\n'.encode('utf-8')
        document = source / 'DISTRIBUTION-TERMS.txt'
        document.write_bytes(raw)
        lock = {'format': 2, 'platform': 'win32-x64',
            'browser': {'mode': 'first-use', 'version': '145.0.7632.6', 'revision': '1208'},
            'archives': [], 'paths': {},
            'distributionTerms': {'path': 'DISTRIBUTION-TERMS.txt', 'sha256': hashlib.sha256(raw).hexdigest()}}
        (self.root / 'dependencies.json').write_text(json.dumps(lock), encoding='utf-8')
        return source, document, raw, lock

    def test_distribution_terms_are_byte_pinned_in_inventory_and_activate_only_unaccepted_gate(self):
        source, document, raw, lock = self.distribution_fixture()
        (source / 'userdata').mkdir()
        (source / 'userdata/distribution-consent.json').write_text('synthetic old acceptance must never ship')
        target = self.root / 'release'
        self.builder.build(source, self.root, target)
        manifest = json.loads((target / 'portable.json').read_text(encoding='utf-8'))
        self.assertEqual(manifest.get('distributionTerms'), lock['distributionTerms'])
        self.assertEqual((target / 'DISTRIBUTION-TERMS.txt').read_bytes(), raw)
        self.assertIn({'path': 'DISTRIBUTION-TERMS.txt', 'sha256': hashlib.sha256(raw).hexdigest()}, manifest['inventory'])
        self.assertFalse((target / 'userdata').exists())
        sys.path.insert(0, str(ROOT / 'p1'))
        from distribution import DistributionTerms
        state = DistributionTerms(target).public()
        self.assertEqual(state, {'required': True, 'accepted': False, 'sha256': hashlib.sha256(raw).hexdigest(), 'text': raw.decode('utf-8')})

    def test_declared_distribution_document_is_required_and_checked_before_output(self):
        source, document, raw, lock = self.distribution_fixture()
        for name, value in [('missing', None), ('changed', b'not the pinned text'), ('empty', b'')]:
            with self.subTest(name=name):
                if document.exists():
                    document.unlink()
                if value is not None:
                    document.write_bytes(value)
                target = self.root / name
                with self.assertRaises(ValueError):
                    self.builder.build(source, self.root, target)
                self.assertFalse(target.exists(), 'Invalid terms must fail before any release files are written')

    def test_distribution_metadata_cannot_select_outside_file_or_invalid_text(self):
        source, document, raw, lock = self.distribution_fixture()
        for name, entry in [('outside', {'path': '../outside.txt', 'sha256': 'a' * 64}),
                ('bad-hash', {'path': 'DISTRIBUTION-TERMS.txt', 'sha256': 1}),
                ('extra-key', {**lock['distributionTerms'], 'accepted': True}), ('null', None)]:
            with self.subTest(name=name):
                lock['distributionTerms'] = entry
                (self.root / 'dependencies.json').write_text(json.dumps(lock), encoding='utf-8')
                with self.assertRaises(ValueError):
                    self.builder.build(source, self.root, self.root / name)
        for name, raw in [('oversized', b'x' * (65536 + 1)), ('invalid-utf8', b'\xff\xfe')]:
            with self.subTest(name=name):
                document.write_bytes(raw)
                lock['distributionTerms'] = {'path': 'DISTRIBUTION-TERMS.txt', 'sha256': hashlib.sha256(raw).hexdigest()}
                (self.root / 'dependencies.json').write_text(json.dumps(lock), encoding='utf-8')
                with self.assertRaises(ValueError):
                    self.builder.build(source, self.root, self.root / name)

    def test_selected_runtime_never_extracts_excluded_deep_npm_tree(self):
        source = self.root / 'source'
        (source / 'p1').mkdir(parents=True)
        (source / 'index.html').write_text('fixture')
        archive = self.archive('selective', {'node/node.exe': b'MZsynthetic', 'node/LICENSE': 'fixture license',
            'node/node_modules/' + 'excluded-package/' * 25 + 'index.js': 'must never extract'})
        lock = {'format': 1, 'platform': 'win32-x64', 'archives': [{'name': 'node', 'archive': archive.name,
            'sha256': hashlib.sha256(archive.read_bytes()).hexdigest(), 'source': 'https://example.invalid/node',
            'license': 'fixture-only', 'subdir': 'node', 'destination': 'vendor/node',
            'include': ['node.exe', 'LICENSE']}], 'paths': {}}
        (self.root / 'dependencies.json').write_text(json.dumps(lock))
        target = self.root / 'selected-release'
        try:
            self.builder.build(source, self.root, target)
        except OSError as error:
            self.fail('Excluded npm members must never be written to staging: ' + type(error).__name__)
        self.assertEqual((target / 'vendor/node/node.exe').read_bytes(), b'MZsynthetic')
        self.assertFalse((target / 'vendor/node/node_modules').exists())
        self.assertFalse(any(path.name.startswith('stage-') for path in target.iterdir()))

    def test_released_interface_has_every_local_script_it_loads(self):
        class Scripts(HTMLParser):
            def __init__(self):
                super().__init__()
                self.sources = []

            def handle_starttag(self, tag, attrs):
                if tag == 'script':
                    source = dict(attrs).get('src')
                    if source:
                        self.sources.append(source)

        target = self.root / 'release'
        target.mkdir()
        self.builder.copy_sources(ROOT, target)
        page = Scripts()
        page.feed((target / 'index.html').read_text(encoding='utf-8'))
        self.assertTrue(page.sources, 'The actual interface must load its script dependencies')
        for relative in page.sources:
            self.assertNotIn('://', relative)
            script = target / relative
            self.assertTrue(script.is_file(), 'Released interface loads a missing script: ' + relative)
            self.assertEqual(script.read_bytes(), (ROOT / relative).read_bytes())

    def test_released_static_sample_link_has_its_page_script_and_styles(self):
        class Resources(HTMLParser):
            def __init__(self):
                super().__init__()
                self.links = []

            def handle_starttag(self, tag, attrs):
                attrs = dict(attrs)
                relative = attrs.get('src') if tag == 'script' else attrs.get('href')
                if tag in ('a', 'script', 'link') and relative and not relative.startswith('#') and '://' not in relative:
                    self.links.append(relative)

        target = self.root / 'release'
        target.mkdir()
        self.builder.copy_sources(ROOT, target)
        page = Resources()
        page.feed((target / 'index.html').read_text(encoding='utf-8'))
        self.assertIn('demo/index.html', page.links)
        sample = target / 'demo/index.html'
        self.assertTrue(sample.is_file(), 'The preserved file:// sample link must work in the actual release')
        resources = Resources()
        resources.feed(sample.read_text(encoding='utf-8'))
        self.assertEqual(set(resources.links), {'../index.html', 'demo.css', 'demo.js'})
        for relative in resources.links:
            self.assertTrue((sample.parent / relative).is_file(), 'The sample must retain its linked local resources: ' + relative)
        for relative in ('index.html', 'demo.css', 'demo.js'):
            self.assertEqual((target / 'demo' / relative).read_bytes(), (ROOT / 'demo' / relative).read_bytes())
        self.assertEqual({path.name for path in (target / 'demo').iterdir()}, {'index.html', 'demo.css', 'demo.js'})

    def test_release_includes_standalone_user_guide_and_first_run_instructions(self):
        source = self.root / 'source'
        (source / 'p1').mkdir(parents=True)
        documents = {'START-HERE.txt': '先解压整个文件夹，再双击 start-workbench.cmd。\n',
            'USER-GUIDE.html': '<!doctype html><html lang="zh-CN"><meta charset="utf-8"><title>使用手册</title><p>首次准备与日常使用</p></html>'}
        for name, content in documents.items():
            (source / name).write_bytes(content.encode('utf-8'))
        target = self.root / 'release'
        target.mkdir()
        self.builder.copy_sources(source, target)
        for name in documents:
            self.assertTrue((target / name).is_file(), 'Recipients need instructions inside the extracted package: ' + name)
            self.assertEqual((target / name).read_bytes(), (source / name).read_bytes())

    def test_source_copy_excludes_userdata_evidence_and_credentials(self):
        source = self.root / 'source'
        (source / 'p1').mkdir(parents=True)
        (source / 'p1/server.py').write_text('# owned source')
        (source / 'p1/environment.py').write_text('# owned source')
        for relative in ('userdata/credentials.json', 'dist/runtime/token.txt', '.claude/config.json', 'p1/__pycache__/secret.pyc'):
            path = source / relative
            path.parent.mkdir(parents=True, exist_ok=True)
            path.write_text('SYNTHETIC-SECRET-MUST-NOT-SHIP')
        (source / 'index.html').write_text('fixture interface')
        target = self.root / 'release'
        target.mkdir()
        self.builder.copy_sources(source, target)
        self.assertEqual((target / 'index.html').read_text(), 'fixture interface')
        self.assertTrue((target / 'p1/server.py').exists())
        self.assertFalse((target / 'userdata').exists())
        self.assertFalse((target / 'dist').exists())
        self.assertFalse((target / '.claude').exists())
        self.assertFalse(any('SYNTHETIC-SECRET' in p.read_text() for p in target.rglob('*') if p.is_file()))


if __name__ == '__main__':
    unittest.main()
