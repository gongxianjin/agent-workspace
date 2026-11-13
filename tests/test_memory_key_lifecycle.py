"""Local tests for the authorized temporary Paramiko key lifecycle.

These tests use fake clients and never connect to Linux B. They must not persist
or print private-key material; real cross-host evidence requires a separate run.
"""
from __future__ import annotations

import contextlib
import hashlib
import importlib.util
import io
from pathlib import Path
import unittest

ROOT = Path(__file__).resolve().parents[1]
MODULE_PATH = ROOT / "tools" / "memory_key_lifecycle.py"


def load_module(test):
    test.assertTrue(MODULE_PATH.is_file(), "memory key lifecycle adapter is missing")
    spec = importlib.util.spec_from_file_location("memory_key_lifecycle", MODULE_PATH)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class MemoryKeyLeaseTests(unittest.TestCase):
    def test_generated_key_yields_pem_bytes_and_only_records_public_fingerprint(self):
        module = load_module(self)
        with contextlib.ExitStack() as stack:
            key, record = stack.enter_context(module.generated_key_lease())
            self.assertIsInstance(key, bytearray)
            self.assertIn(b"BEGIN", bytes(key))
            self.assertRegex(record["public_key_sha256"], r"^[0-9a-f]{64}$")
            self.assertNotIn("private", record)
            self.assertNotIn(bytes(key).decode("ascii"), str(record))
        self.assertEqual(key, bytearray(len(key)))

    def test_authorized_key_is_removed_when_body_raises(self):
        module = load_module(self)
        events = []

        class RootClient:
            def exec_command(self, command):
                events.append(("root-command", command))
                return 0, "removed\n", ""

        with self.assertRaisesRegex(RuntimeError, "body-failed"):
            with module.temporary_authorized_key(
                RootClient(),
                user="awt1104094b2a0cea",
                uid=1003,
                gid=1004,
                record=lambda value: events.append(("record", value)),
            ) as lease:
                self.assertTrue(lease["private_key"])
                self.assertTrue(lease["public_key"].startswith("ssh-rsa "))
                raise RuntimeError("body-failed")

        commands = [item[1] for item in events if item[0] == "root-command"]
        self.assertEqual(len(commands), 2)
        self.assertIn("authorized_keys", commands[0])
        self.assertIn("authorized_keys", commands[1])
        self.assertIn("grep -F -x -v", commands[1])

    def test_public_record_contains_no_private_key_bytes(self):
        module = load_module(self)
        seen = []

        class RootClient:
            def exec_command(self, command):
                seen.append(command)
                return 0, "installed\n", ""

        with module.temporary_authorized_key(
            RootClient(),
            user="awt1104094b2a0cea",
            uid=1003,
            gid=1004,
            record=lambda value: seen.append(value),
        ) as lease:
            private_bytes = lease["private_key"]
            public_line = lease["public_key"]
            self.assertNotIn(private_bytes.decode("ascii"), str(seen))
            self.assertIn(hashlib.sha256(public_line.encode("ascii")).hexdigest(), str(seen))

        self.assertNotIn(private_bytes.decode("ascii"), " ".join(item for item in seen if isinstance(item, str)))


if __name__ == "__main__":
    unittest.main()
