"""Native asset metadata contracts. ELF stand-ins are never run as clients/browsers."""
import hashlib
import importlib
import json
import os
from pathlib import Path
import shutil
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


@unittest.skipUnless(sys.platform == 'linux', 'Native ELF ownership and inventory validation')
class NativeEnvironmentTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory(prefix='relay-native-assets-')
        self.addCleanup(self.temp.cleanup)
        self.root = Path(self.temp.name)
        self.module = importlib.import_module('native_environment') if (ROOT / 'p1/native_environment.py').exists() else None

    def resolver(self):
        self.assertIsNotNone(self.module, 'Native dependency resolution is missing; no Windows fallback permitted')
        return self.module.resolve_native_environment

    def manifest(self):
        from platform_info import target
        assets = self.root / 'assets'
        assets.mkdir()
        package = assets / 'playwright/package.json'
        package.parent.mkdir()
        package.write_text('{"name":"playwright","version":"fixture-metadata-only","main":"index.js"}')
        (package.parent / 'index.js').write_text('// Metadata fixture only; not a functioning browser package.\n')
        binary = self.root / 'native-elf-stand-in'
        shutil.copyfile(Path(sys.executable).resolve(), binary)
        binary.chmod(0o700)
        entry = lambda p: {'path': str(p), 'sha256': hashlib.sha256(p.read_bytes()).hexdigest()}
        data = {'format': 1, 'platform': target(), 'assetRoots': [str(assets)],
            'files': {key: entry(package if key == 'playwright' else binary)
                for key in ('pythonConsole', 'pythonWindowed', 'node', 'reason', 'playwright', 'chromium', 'claude')},
            'inventory': [entry(package), entry(package.parent / 'index.js')]}
        self.save(data)
        return data

    def save(self, data):
        (self.root / 'native-runtime.json').write_text(json.dumps(data), encoding='utf-8')

    def test_native_manifest_returns_exact_pinned_paths_without_candidate_execution(self):
        # Break caught: manufacturing Windows paths or ignoring the selected native manifest.
        data = self.manifest()
        resolved = self.resolver()(self.root)
        self.assertEqual(resolved.paths.pythonConsole, self.root / 'native-elf-stand-in')
        self.assertEqual(resolved.paths.playwright, self.root / 'assets/playwright/package.json')
        self.assertEqual(resolved.claude, self.root / 'native-elf-stand-in')
        self.assertEqual(set(data['files']), {'pythonConsole', 'pythonWindowed', 'node', 'reason', 'playwright', 'chromium', 'claude'})
        self.assertEqual(len(list(self.root.iterdir())), 3, 'Resolution must not create setup data or run candidates')

    def test_missing_changed_and_wrong_platform_manifest_never_falls_back(self):
        # Break caught: resolving unrelated PATH clients when explicit pinned assets are invalid.
        resolve = self.resolver()
        with self.assertRaises(ValueError):
            resolve(self.root)
        data = self.manifest()
        data['platform'] = 'win32-x64'
        self.save(data)
        with self.assertRaises(ValueError):
            resolve(self.root)
        from platform_info import target
        data['platform'] = target()
        data['files']['node']['sha256'] = '0' * 64
        self.save(data)
        with self.assertRaises(ValueError):
            resolve(self.root)

    def test_inventory_changed_missing_or_added_asset_is_rejected(self):
        # Break caught: verifying package.json while executing unchecked JS/library siblings.
        data = self.manifest()
        resolve = self.resolver()
        script = self.root / 'assets/playwright/index.js'
        original = script.read_bytes()
        script.write_text('// changed')
        with self.assertRaises(ValueError):
            resolve(self.root)
        script.write_bytes(original)
        extra = script.with_name('unchecked.js')
        extra.write_text('// unchecked asset')
        with self.assertRaises(ValueError):
            resolve(self.root)
        extra.unlink()
        script.unlink()
        with self.assertRaises(ValueError):
            resolve(self.root)

    def test_wrong_elf_architecture_and_writable_executable_are_rejected(self):
        # Break caught: accepting MZ/scripts/another architecture or stranger-writable binaries.
        data = self.manifest()
        resolve = self.resolver()
        binary = self.root / 'native-elf-stand-in'
        binary.chmod(0o722)
        with self.assertRaises(ValueError):
            resolve(self.root)
        binary.chmod(0o700)
        raw = bytearray(binary.read_bytes())
        raw[18:20] = b'\x03\x00'  # ELF EM_386, never accepted for linux-x64/arm64.
        binary.write_bytes(raw)
        for entry in data['files'].values():
            if entry['path'] == str(binary):
                entry['sha256'] = hashlib.sha256(raw).hexdigest()
        self.save(data)
        with self.assertRaises(ValueError):
            resolve(self.root)

    def test_linked_inventory_and_missing_playwright_root_are_rejected(self):
        # Break caught: require() escaping an inventoried asset root through a link.
        data = self.manifest()
        resolve = self.resolver()
        script = self.root / 'assets/playwright/index.js'
        outside = self.root / 'outside.js'
        script.rename(outside)
        script.symlink_to(outside)
        with self.assertRaises(ValueError):
            resolve(self.root)
        script.unlink()
        outside.rename(script)
        data['assetRoots'] = []
        self.save(data)
        with self.assertRaises(ValueError):
            resolve(self.root)

    def test_discovery_accepts_only_native_trusted_candidate_and_never_executes_script(self):
        # Break caught: executing a shell/npm wrapper or searching relative PATH entries.
        self.assertIsNotNone(self.module, 'Native discovery is missing')
        directory = self.root / '.local/bin'
        directory.mkdir(parents=True)
        candidate = directory / 'claude'
        shutil.copyfile(Path(sys.executable).resolve(), candidate)
        candidate.chmod(0o700)
        self.assertEqual(self.module.discover_native_claude({'HOME': str(self.root), 'PATH': '.'}), candidate)
        candidate.write_text('#!/bin/sh\ntouch SHOULD_NOT_RUN\n')
        self.assertIsNone(self.module.discover_native_claude({'HOME': str(self.root), 'PATH': '.'}))
        self.assertFalse((self.root / 'SHOULD_NOT_RUN').exists())

    def setup_status(self):
        from linux_setup import LinuxSetup
        try:
            return LinuxSetup(self.root)
        except TypeError as error:
            self.fail('Linux setup cannot inspect an explicit native manifest: ' + str(error))

    def test_linux_setup_public_reads_do_not_resolve_assets_or_execute_candidates(self):
        # Break caught: a GET/public read starts dependency detection or trusts file existence.
        setup = self.setup_status()
        self.manifest()
        status = setup.public()
        self.assertEqual(status['nativeAssets'], 'unchecked')
        self.assertIsNone(setup.paths)
        self.assertFalse(status['canExecute'])
        self.assertEqual(set(self.root.iterdir()), {
            self.root / 'assets', self.root / 'native-elf-stand-in', self.root / 'native-runtime.json'})

    def test_linux_setup_metadata_success_never_proves_dependencies_sandbox_or_auth(self):
        # 部署版语义：测试进程不在受管 systemd 单元内，也无真实可执行依赖，
        # 因此即使清单校验通过也必须 fail-closed（execution blocker 保持）。
        self.manifest()
        setup = self.setup_status()
        status = setup.recheck()
        self.assertEqual(status['nativeAssets'], 'verified')
        self.assertEqual(setup.paths.pythonConsole, self.root / 'native-elf-stand-in')
        self.assertIn(status['dependencies'], ('unchecked', 'unhealthy', 'missing'))
        self.assertEqual(status['login'], 'unknown')
        self.assertFalse(status['canExecute'])
        self.assertIn('execution', status['blockers'])
        with self.assertRaises(ValueError):
            setup.confirm_existing()
        with self.assertRaises(ValueError):
            setup.snapshot()
        self.assertEqual(len(list(self.root.iterdir())), 3)

    def test_linux_setup_snapshot_revalidates_changed_assets_and_clears_cached_paths(self):
        # Break caught: a cached successful manifest inspection survives changed executable bytes.
        self.manifest()
        setup = self.setup_status()
        self.assertEqual(setup.recheck()['nativeAssets'], 'verified')
        (self.root / 'native-elf-stand-in').write_bytes(b'changed, not an executable')
        with self.assertRaises(ValueError):
            setup.snapshot()
        self.assertIsNone(setup.paths)
        self.assertEqual(setup.public()['nativeAssets'], 'invalid')
        self.assertFalse(setup.public()['canExecute'])

    def test_linux_execution_seam_retains_pinned_claude_and_reason_bytes_without_activation(self):
        # Break caught: LinuxSetup discards the manifest's CLI or treats metadata as authority.
        data = self.manifest()
        claude = self.root / 'pinned-native-claude'
        shutil.copyfile(self.root / 'native-elf-stand-in', claude)
        claude.chmod(0o700)
        data['files']['claude']['path'] = str(claude)
        self.save(data)
        setup = self.setup_status()
        self.assertFalse(setup.recheck()['canExecute'])
        candidate = getattr(setup, 'candidate', None)
        self.assertIsNotNone(candidate, 'Pinned native Claude is discarded by setup')
        self.assertEqual(candidate.executable, claude)
        self.assertIs(candidate.paths, setup.paths)
        self.assertEqual(candidate.reason_sha256, data['files']['reason']['sha256'])
        atomic = getattr(setup, 'execution_snapshot', None)
        self.assertTrue(callable(atomic), 'Linux lacks the guarded atomic activation seam')
        with self.assertRaises(ValueError):
            atomic()
        self.assertFalse(setup.public()['canExecute'])
        # A changed CLI invalidates the entire cached candidate on authorization.
        claude.write_bytes(b'changed CLI bytes, never executed')
        with self.assertRaises(ValueError):
            atomic()
        self.assertIsNone(setup.candidate)
        self.assertIsNone(setup.paths)
        self.assertEqual(setup.public()['nativeAssets'], 'invalid')

    def test_linux_setup_invalid_manifest_never_uses_discovered_candidate_or_leaks_paths(self):
        # Break caught: invalid explicit config falls back to another CLI or publishes local paths.
        data = self.manifest()
        data['files']['reason']['sha256'] = '0' * 64
        self.save(data)
        setup = self.setup_status()
        status = setup.recheck()
        self.assertEqual(status['nativeAssets'], 'invalid')
        self.assertIsNone(setup.paths)
        self.assertFalse(status['canExecute'])
        self.assertNotIn(str(self.root), json.dumps(status))


if __name__ == '__main__':
    unittest.main()
