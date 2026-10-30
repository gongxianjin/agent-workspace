"""Platform identity and executable validation; never executes candidate files."""
import os
from pathlib import Path
import struct
import sys
import tempfile
import unittest

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / 'p1'))


def elf_header(machine):
    header = bytearray(64)
    header[:7] = b'\x7fELF\x02\x01\x01'
    struct.pack_into('<HHI', header, 16, 3, machine, 1)
    struct.pack_into('<H', header, 52, 64)
    return bytes(header)


class PlatformIdentityTests(unittest.TestCase):
    def module(self):
        try:
            import platform_info
            return platform_info
        except ImportError as error:
            self.fail('Native platform validation is missing: ' + str(error))

    def test_supported_architectures_are_explicit(self):
        target = self.module().target
        for system, machine, expected in (
                ('Windows', 'AMD64', 'win32-x64'),
                ('Linux', 'x86_64', 'linux-x64'),
                ('Linux', 'amd64', 'linux-x64'),
                ('Linux', 'aarch64', 'linux-arm64'),
                ('Linux', 'arm64', 'linux-arm64')):
            with self.subTest(system=system, machine=machine):
                self.assertEqual(target(system, machine), expected)
        for system, machine in (('Linux', 'armv7l'), ('Windows', 'ARM64'), ('Darwin', 'arm64')):
            with self.subTest(system=system, machine=machine):
                with self.assertRaises(ValueError):
                    target(system, machine)

    def test_elf_header_rejects_wrong_architecture_and_truncation(self):
        validate = self.module().executable_header
        self.assertTrue(validate(elf_header(62), 'linux-x64'))
        self.assertTrue(validate(elf_header(183), 'linux-arm64'))
        self.assertFalse(validate(elf_header(183), 'linux-x64'))
        self.assertFalse(validate(elf_header(62), 'linux-arm64'))
        self.assertFalse(validate(elf_header(62)[:24], 'linux-x64'))
        self.assertFalse(validate(b'MZ' + bytes(62), 'linux-x64'))
        for index, value in ((4, 1), (5, 2), (6, 0)):
            header = bytearray(elf_header(62))
            header[index] = value
            self.assertFalse(validate(header, 'linux-x64'))
        self.assertTrue(validate(b'MZ', 'win32-x64'))
        self.assertFalse(validate(b'#!', 'win32-x64'))

    @unittest.skipUnless(sys.platform == 'linux', 'Native POSIX permission and link checks')
    def test_native_candidate_requires_permission_and_returns_resolved_target(self):
        validate = self.module().native_executable
        # The candidate is never run. /tmp is a root-owned sticky ancestor, not a mutable candidate link.
        with tempfile.TemporaryDirectory(prefix='relay-native-') as directory:
            root = Path(directory)
            binary = root / 'claude-native'
            binary.write_bytes(elf_header(62))
            binary.chmod(0o600)
            self.assertIsNone(validate(binary, 'linux-x64'))
            binary.chmod(0o700)
            self.assertEqual(validate(binary, 'linux-x64'), binary.resolve())
            self.assertIsNone(validate(binary, 'linux-arm64'))
            link = root / 'claude'
            link.symlink_to(binary.name)
            self.assertEqual(validate(link, 'linux-x64'), binary.resolve())
            binary.chmod(0o720)
            self.assertIsNone(validate(link, 'linux-x64'))
            binary.chmod(0o700)
            root.chmod(0o777)
            self.assertIsNone(validate(link, 'linux-x64'))
            root.chmod(0o700)
            self.assertIsNone(validate(Path('claude-native'), 'linux-x64'))

    @unittest.skipUnless(sys.platform == 'linux', 'Native symlink validation')
    def test_link_chain_rejects_dangling_cycle_and_writable_alias_parent(self):
        validate = self.module().native_executable
        with tempfile.TemporaryDirectory(prefix='relay-native-link-') as directory:
            root = Path(directory)
            binary = root / 'actual'
            binary.write_bytes(elf_header(62))
            binary.chmod(0o700)
            dangling = root / 'missing'
            dangling.symlink_to('absent')
            self.assertIsNone(validate(dangling, 'linux-x64'))
            cycle = root / 'cycle'
            cycle.symlink_to('cycle')
            self.assertIsNone(validate(cycle, 'linux-x64'))
            aliases = root / 'aliases'
            aliases.mkdir(mode=0o700)
            alias = aliases / 'claude'
            alias.symlink_to(binary)
            self.assertEqual(validate(alias, 'linux-x64'), binary.resolve())
            aliases.chmod(0o777)
            self.assertIsNone(validate(alias, 'linux-x64'))
            aliases.chmod(0o700)

    @unittest.skipUnless(sys.platform == 'linux', 'Actual native Linux executable inspection')
    def test_running_python_is_native_without_launching_it_again(self):
        module = self.module()
        self.assertEqual(module.native_executable(Path(sys.executable), module.target()),
            Path(sys.executable).resolve())


if __name__ == '__main__':
    unittest.main()
