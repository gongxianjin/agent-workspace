"""Persistent key setup tests; simulated SSH/SFTP is not Linux B evidence."""
from __future__ import annotations

import base64
import contextlib
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import stat
import tempfile
import types
import unittest
from unittest import mock

import paramiko

ROOT = Path(__file__).resolve().parents[1]
SCRIPT = ROOT / "tools/linux_b_key_setup.py"


def load_setup(test):
    test.assertTrue(SCRIPT.is_file(), "persistent SSH key setup entrypoint is missing")
    spec = importlib.util.spec_from_file_location("linux_b_key_setup", SCRIPT)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class KeyBundleTests(unittest.TestCase):
    def test_saved_private_key_matches_public_key_and_can_sign_after_reload(self):
        setup = load_setup(self)
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "key"
            record = setup.generate(directory)
            saved = paramiko.RSAKey.from_private_key_file(str(directory / "ordinary-user-rsa"))
            public_text = (directory / "ordinary-user-rsa.pub").read_text(encoding="ascii").strip()
            self.assertEqual(public_text, f"{saved.get_name()} {saved.get_base64()}")
            public = paramiko.RSAKey(data=base64.b64decode(public_text.split()[1]))
            signature = saved.sign_ssh_data(b"independent-reload-check")
            self.assertTrue(public.verify_ssh_sig(b"independent-reload-check", paramiko.Message(signature.asbytes())))
            self.assertFalse(public.verify_ssh_sig(b"wrong-message", paramiko.Message(signature.asbytes())))
            self.assertEqual(record["public_key_sha256"], hashlib.sha256(public_text.encode("ascii")).hexdigest())
            persisted = json.loads((directory / "authorization.json").read_text(encoding="utf-8"))
            self.assertEqual(persisted["authorization_status"], "NOT_INSTALLED")
            self.assertFalse(persisted["remote_login_verified"])
            self.assertNotIn("BEGIN RSA PRIVATE KEY", json.dumps(persisted))

    def test_existing_bundle_is_not_replaced(self):
        setup = load_setup(self)
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "key"
            setup.generate(directory)
            before = {p.name: p.read_bytes() for p in directory.iterdir()}
            with self.assertRaises(FileExistsError):
                setup.generate(directory)
            self.assertEqual(before, {p.name: p.read_bytes() for p in directory.iterdir()})


class AppendTests(unittest.TestCase):
    def test_append_preserves_existing_authorizations_and_is_idempotent(self):
        setup = load_setup(self)
        for old in (b"ssh-ed25519 EXISTING old-key\n", b"ssh-ed25519 EXISTING old-key"):
            with self.subTest(trailing_newline=old.endswith(b"\n")):
                sftp = RemoteFiles(old)
                line = "ssh-rsa NEW test-key"
                self.assertEqual(setup.install_public_key(sftp, line), "APPENDED")
                self.assertEqual(sftp.data, old + (b"" if old.endswith(b"\n") else b"\n") + line.encode() + b"\n")
                once = sftp.data
                self.assertEqual(setup.install_public_key(sftp, line), "ALREADY_PRESENT")
                self.assertEqual(sftp.data, once)
                self.assertEqual(sftp.append_count, 1)

    def test_symlink_is_rejected_before_writing_authorizations(self):
        setup = load_setup(self)
        sftp = RemoteFiles(b"untouched\n")
        sftp.file_mode = stat.S_IFLNK | 0o600
        with self.assertRaises(ValueError):
            setup.install_public_key(sftp, "ssh-rsa NEW")
        self.assertEqual(sftp.data, b"untouched\n")
        self.assertEqual(sftp.append_count, 0)


class AuthorizationTests(unittest.TestCase):
    def test_verified_result_requires_ordinary_user_identity_not_just_install(self):
        setup = load_setup(self)
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "key"
            setup.generate(directory)
            known = Path(temp) / "known_hosts"
            known.write_text("test-placeholder", encoding="ascii")
            root, ordinary = Client(True), Client(False)
            ordinary.identity = b"999\n1004\nawt1104094b2a0cea\n/home/awt1104094b2a0cea\n"
            with mock.patch.object(setup.paramiko, "SSHClient", side_effect=[root, ordinary]):
                result = setup.authorize(directory, Path(temp) / "attempt", known, "synthetic-root-password")
            self.assertEqual(result["status"], "BLOCKED_OR_FAILED")
            self.assertEqual(result["failed_stage"], "ordinary_identity")
            self.assertEqual(result["installation"], "APPENDED")
            self.assertFalse(result["remote_login_verified"])
            self.assertTrue(root.closed and ordinary.closed)
            self.assertIn((directory / "ordinary-user-rsa.pub").read_bytes().strip(), root.sftp.data)
            # Failure preserves the authorized key for reuse, rather than revoking it.
            self.assertEqual(root.sftp.append_count, 1)

    def test_strict_ordinary_key_login_and_public_only_result(self):
        setup = load_setup(self)
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "key"
            setup.generate(directory)
            known = Path(temp) / "known_hosts"
            known.write_text("test-placeholder", encoding="ascii")
            root, ordinary = Client(True), Client(False)
            with mock.patch.object(setup.paramiko, "SSHClient", side_effect=[root, ordinary]):
                result = setup.authorize(directory, Path(temp) / "attempt", known, "synthetic-root-password")
            self.assertEqual(result["status"], "PASS_ORDINARY_USER_LOGIN")
            self.assertTrue(result["remote_login_verified"])
            for client in (root, ordinary):
                self.assertEqual(client.known, str(known.resolve()))
                self.assertIsInstance(client.policy, paramiko.RejectPolicy)
                self.assertFalse(client.connection["allow_agent"])
                self.assertFalse(client.connection["look_for_keys"])
                self.assertTrue(client.closed)
            self.assertEqual(root.connection["username"], "root")
            self.assertEqual(ordinary.connection["username"], "awt1104094b2a0cea")
            self.assertNotIn("password", ordinary.connection)
            self.assertIsInstance(ordinary.connection["pkey"], paramiko.RSAKey)
            raw = (Path(temp) / "attempt/result.json").read_text(encoding="utf-8")
            self.assertNotIn("synthetic-root-password", raw)
            self.assertNotIn("BEGIN RSA PRIVATE KEY", raw)
            self.assertEqual(result["wait_kind"], "PARAMIKO_CHANNEL_ONLY")

    def test_empty_admin_password_never_connects_and_failure_is_recorded(self):
        setup = load_setup(self)
        with tempfile.TemporaryDirectory() as temp:
            directory = Path(temp) / "key"
            setup.generate(directory)
            with mock.patch.object(setup.paramiko, "SSHClient", side_effect=AssertionError("must not connect")):
                result = setup.authorize(directory, Path(temp) / "attempt", Path(temp) / "absent", "")
            self.assertEqual(result["status"], "BLOCKED_OR_FAILED")
            self.assertEqual(result["failed_stage"], "admin_credential")
            self.assertFalse(result["remote_login_verified"])
            self.assertEqual(result["installation"], "NOT_RUN")
            self.assertTrue((Path(temp) / "attempt/result.json").is_file())


class RemoteFiles:
    """Network boundary: retains actual bytes, append writes, attrs and close."""
    def __init__(self, data):
        self.data = data
        self.file_mode = stat.S_IFREG | 0o600
        self.append_count = 0

    def lstat(self, path):
        if path == "/home/awt1104094b2a0cea/.ssh":
            mode = stat.S_IFDIR | 0o700
        elif path == "/home/awt1104094b2a0cea/.ssh/authorized_keys":
            mode = self.file_mode
        else:
            raise AssertionError("unexpected remote path")
        return types.SimpleNamespace(st_mode=mode, st_uid=1003, st_gid=1004)

    def open(self, path, mode):
        if path != "/home/awt1104094b2a0cea/.ssh/authorized_keys":
            raise AssertionError("unexpected remote file")
        if mode == "rb":
            return io.BytesIO(self.data)
        if mode != "ab":
            raise AssertionError("authorization must be append-only")
        self.append_count += 1
        owner = self
        class Append(io.BytesIO):
            def close(self):
                if not self.closed:
                    owner.data += self.getvalue()
                super().close()
        return Append()

    def close(self):
        pass


class Client:
    def __init__(self, root):
        self.root = root
        self.closed = False
        self.sftp = RemoteFiles(b"ssh-ed25519 EXISTING old-key\n")
        self.identity = b"0\n" if root else b"1003\n1004\nawt1104094b2a0cea\n/home/awt1104094b2a0cea\n"

    def load_host_keys(self, path):
        self.known = path

    def set_missing_host_key_policy(self, policy):
        self.policy = policy

    def connect(self, **kwargs):
        self.connection = kwargs

    def exec_command(self, command, timeout):
        expected = "id -u" if self.root else 'id -u; id -g; id -un; printf "%s\\n" "$HOME"'
        if command != expected:
            raise AssertionError("unexpected remote command")
        output = io.BytesIO(self.identity)
        output.channel = types.SimpleNamespace(recv_exit_status=lambda: 0, close=lambda: None)
        return io.BytesIO(), output, io.BytesIO()

    def open_sftp(self):
        return self.sftp

    def close(self):
        self.closed = True


if __name__ == "__main__":
    unittest.main()
