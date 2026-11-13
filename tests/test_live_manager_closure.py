"""Local harness tests; none of these fixtures are cross-host evidence.

Breaks caught: early credential cleanup, cleanup failure hidden by success,
old evidence overwritten, and an empty/released-only Store treated as closure.
Network and HWND boundaries are substituted; SQLite and evidence files are real.
"""
from __future__ import annotations

import contextlib
import importlib.util
import hashlib
import hmac
import io
import json
import subprocess
import sys
from pathlib import Path
import sqlite3
import tempfile
import types
import unittest
from unittest import mock

ROOT = Path(__file__).resolve().parents[1]
DRIVER = ROOT / "tools" / "live_manager_closure.py"


def load_driver(test):
    test.assertTrue(DRIVER.is_file(), "credential-lifetime closure driver is not implemented")
    spec = importlib.util.spec_from_file_location("live_manager_closure", DRIVER)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


class DriverEntryTests(unittest.TestCase):
    def test_stdin_key_is_zeroed_when_lease_closes(self):
        driver = load_driver(self)
        self.assertTrue(callable(getattr(driver, "stdin_key_lease", None)), "memory-only stdin lease is missing")
        with driver.stdin_key_lease(io.BytesIO(b"synthetic-not-a-key")) as key:
            self.assertEqual(key, b"synthetic-not-a-key")
        self.assertEqual(key, bytearray(len(b"synthetic-not-a-key")))

    def test_empty_stdin_exits_nonzero_without_loading_gateway_or_gui(self):
        load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / "new"
            process = subprocess.run([sys.executable, "-I", "-B", str(DRIVER), "run",
                "--evidence-dir", str(out), "--gateway", str(Path(temp) / "missing-gateway.json"),
                "--package", str(Path(temp) / "missing-package"), "--key-stdin"],
                input=b"", capture_output=True, timeout=10, check=False)
            self.assertEqual(process.returncode, 2, process.stdout)
            result = json.loads((out / "result.json").read_text(encoding="utf-8"))
        self.assertEqual(result["failed_stage"], "credential_acquire")
        self.assertEqual(result["dispatch"], "NOT_RUN")
        self.assertNotIn(b"Traceback", process.stderr)

    def test_oversized_stdin_is_rejected_before_preparation(self):
        driver = load_driver(self)
        self.assertTrue(callable(getattr(driver, "stdin_key_lease", None)), "bounded stdin lease is missing")
        with self.assertRaises(ValueError):
            with driver.stdin_key_lease(io.BytesIO(b"k" * 65537)):
                self.fail("oversized key must not enter lease")


class ForegroundFixture:
    """Only the Win32 boundary is simulated; drive_manager and its queue are real."""
    class RECT:
        left = top = 0
        right, bottom = 1672, 1256
    class ctypes:
        @staticmethod
        def byref(value): return value

    def __init__(self, foreground=999, becomes_foreground_at=None):
        self.now = 0.0
        self.foreground = foreground
        self.becomes_foreground_at = becomes_foreground_at
        self.pid, self.title, self.visible = 123, "agentranch 工作台", True
        self.width, self.height = 1672, 1256
        self.input_events = []
        self.user = self

    def owned_pid(self, hwnd): return self.pid
    def window_title(self, hwnd): return self.title
    def IsWindowVisible(self, hwnd): return self.visible
    def GetWindowRect(self, hwnd, rect):
        rect.right, rect.bottom = self.width, self.height
        return True
    def GetForegroundWindow(self):
        if self.becomes_foreground_at is not None and self.now >= self.becomes_foreground_at:
            self.foreground = 7
        return self.foreground
    def SetForegroundWindow(self, hwnd): return 1  # TRUE alone must not permit input.
    def sleep(self, seconds): self.now += seconds
    def click(self, *args): self.input_events.append(("click", self.now, self.foreground))
    def hotkey_ctrl_a(self, *args): self.input_events.append(("select", self.now, self.foreground))
    def unicode_type(self, *args): self.input_events.append(("type", self.now, self.foreground))
    def capture(self, *args): return {"fixture": True}


class ManagerWindowTests(unittest.TestCase):
    def test_tiny_helper_and_other_process_window_are_not_selected(self):
        driver = load_driver(self)
        self.assertTrue(callable(getattr(driver, "find_manager_window", None)), "main-window selection is missing")
        class Rect:
            left = top = right = bottom = 0
        class Types:
            BOOL = HWND = LPARAM = object
        class Ctypes:
            @staticmethod
            def WINFUNCTYPE(*args): return lambda function: function
            @staticmethod
            def byref(value): return value
        class User:
            def EnumWindows(self, callback, unused):
                for hwnd in (1, 2, 3):
                    if not callback(hwnd, 0): break
                return True
            def IsWindowVisible(self, hwnd): return True
            def GetWindowRect(self, hwnd, rect):
                rect.left = rect.top = 0
                rect.right, rect.bottom = (6, 6) if hwnd == 1 else (1672, 1256)
                return True
        class UI:
            RECT = Rect
            wintypes = Types()
            ctypes = Ctypes()
            user = User()
            @staticmethod
            def owned_pid(hwnd): return 999 if hwnd == 2 else 123
            @staticmethod
            def window_title(hwnd): return "agentranch 工作台" if hwnd != 1 else "helper"
        self.assertEqual(driver.find_manager_window(UI(), 123, timeout=1), 3)

    def test_foreground_gate_fails_closed_without_owned_foreground(self):
        driver = load_driver(self)
        ui = ForegroundFixture()
        ui.SetForegroundWindow = lambda hwnd: 0
        with mock.patch.object(driver.time, "monotonic", side_effect=lambda: ui.now), \
             mock.patch.object(driver.time, "sleep", side_effect=ui.sleep):
            with self.assertRaisesRegex(driver.ClosureCheckError, "HUMAN_FOREGROUND_REQUIRED"):
                driver.wait_for_owned_foreground(ui, 7, 123, timeout=0.03, poll_interval=0.001)
        self.assertEqual(ui.input_events, [])
        self.assertLessEqual(ui.now, 0.031)

    def test_foreground_gate_allows_only_owned_actual_foreground(self):
        driver = load_driver(self)
        ui = ForegroundFixture(foreground=7)
        def forbidden(hwnd):
            raise AssertionError("activation must not be needed when already foreground")
        ui.SetForegroundWindow = forbidden
        self.assertTrue(driver.wait_for_owned_foreground(ui, 7, 123, timeout=0.03, poll_interval=0.001))

    def test_input_actions_never_reach_ui_without_actual_foreground(self):
        driver = load_driver(self)
        for action in ("click", "type-gateway", "dispatch"):
            with self.subTest(action=action), tempfile.TemporaryDirectory() as temp:
                root = Path(temp)
                (root / "controls").mkdir()
                (root / "controls/command-0001.json").write_text(
                    json.dumps({"action": action, "x": 1, "y": 1}), encoding="utf-8")
                database = root / "controller.sqlite"
                with contextlib.closing(sqlite3.connect(database)) as db:
                    db.execute("CREATE TABLE runs(id TEXT)")
                ui = ForegroundFixture()
                session = {"evidence": root, "ui": ui, "process": types.SimpleNamespace(pid=123, poll=lambda: None),
                           "hwnd": 7, "database": database, "gateway_path": root / "gateway.json"}
                with mock.patch.object(driver.time, "monotonic", side_effect=lambda: ui.now), \
                     mock.patch.object(driver.time, "sleep", side_effect=ui.sleep):
                    with self.assertRaisesRegex(driver.ClosureCheckError, "HUMAN_FOREGROUND_REQUIRED"):
                        driver.drive_manager(session, control_timeout=20)
                self.assertEqual(ui.input_events, [], "non-foreground input must never reach the adapter")
                self.assertLessEqual(ui.now, 15.1, "foreground wait must be bounded")

    def test_gate_rechecks_pid_title_visibility_and_size_before_input(self):
        driver = load_driver(self)
        for field, value in (("pid", 999), ("title", "helper"), ("visible", False), ("width", 6), ("height", 6)):
            with self.subTest(field=field):
                ui = ForegroundFixture(foreground=7)
                setattr(ui, field, value)
                with self.assertRaises(driver.ClosureCheckError):
                    driver.wait_for_owned_foreground(ui, 7, 123, timeout=0.03, poll_interval=0.001)
                self.assertEqual(ui.input_events, [])

    def test_delayed_actual_foreground_allows_input_after_readiness(self):
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            (root / "controls").mkdir()
            (root / "controls/command-0001.json").write_text(
                json.dumps({"action": "dispatch", "x": 1, "y": 1}), encoding="utf-8")
            database = root / "controller.sqlite"
            with contextlib.closing(sqlite3.connect(database)) as db:
                db.execute("CREATE TABLE runs(id TEXT)")
            ui = ForegroundFixture(becomes_foreground_at=0.2)
            session = {"evidence": root, "ui": ui, "process": types.SimpleNamespace(pid=123, poll=lambda: None),
                       "hwnd": 7, "database": database}
            with mock.patch.object(driver.time, "monotonic", side_effect=lambda: ui.now), \
                 mock.patch.object(driver.time, "sleep", side_effect=ui.sleep):
                driver.drive_manager(session, control_timeout=2)
            self.assertEqual(ui.input_events, [("click", 0.2, 7)])


class PreflightBoundaryTests(unittest.TestCase):
    def test_remote_output_is_drained_concurrently_and_channel_is_closed(self):
        driver = load_driver(self)
        self.assertTrue(callable(getattr(driver, "read_channel", None)), "bounded concurrent channel drain is missing")
        class Channel:
            closed = False
            out = [b"out"]
            err = [b"err"]
            def recv_ready(self): return bool(self.out)
            def recv_stderr_ready(self): return bool(self.err)
            def recv(self, count): return self.out.pop(0)
            def recv_stderr(self, count): return self.err.pop(0)
            def exit_status_ready(self): return not self.out and not self.err
            def recv_exit_status(self): return 0
            def close(self): self.closed = True
        channel = Channel()
        self.assertEqual(driver.read_channel(channel, timeout=1), (0, b"out", b"err"))
        self.assertTrue(channel.closed)

    def test_readonly_preflight_pins_host_and_never_records_credentials(self):
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            package, evidence = root / "package", root / "evidence"
            package.mkdir(); evidence.mkdir()
            pinned = {}
            for name, relative in (("manager", "agentranch.exe"), ("worker_cli", "aw-cli.exe"),
                                   ("helper", "tools/paramiko_gateway.py"), ("python", "runtime/python/python.exe")):
                path = package / relative
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b"fixture-binary")
                pinned[name] = {"bytes": 14, "sha256": hashlib.sha256(b"fixture-binary").hexdigest()}
            manifest = root / "manifest.json"
            manifest.write_text(json.dumps({"package_dir": str(package), "source_sha256": "cd" * 32,
                                           "required_files": pinned}), encoding="utf-8")
            known = root / "known_hosts"
            known.write_text("synthetic-host-key-line", encoding="utf-8")
            controller = {"schema": "ao-reason-config/1", "mode": "real", "role": "controller",
                          "deployment": "live-test", "state_dir": "never-created-state"}
            (root / "controller.json").write_text(json.dumps(controller), encoding="utf-8")
            gateway = {"schema": "native-ssh-gateway/1", "deployment": "live-test", "worker_id": "B",
                "host": "172.28.8.210", "port": 22, "user": "awt1104094b2a0cea", "known_hosts": str(known),
                "controller_config": "controller.json", "remote_worker": "/home/worker/aw-cli",
                "remote_config": "/home/worker/worker.json", "artifact_sha256": "ab" * 32,
                "artifact_bytes": 123, "remote_library_path": "/home/worker/lib",
                "remote_library_sha256": "ef" * 32, "remote_library_bytes": 456}
            gateway_path = root / "gateway.json"
            gateway_path.write_text(json.dumps(gateway), encoding="utf-8")
            remote_config = json.dumps({"schema": "native-worker-config/1", "deployment": "live-test",
                "worker_id": "B", "artifact_sha256": "ab" * 32,
                "cgroup_root": "/sys/fs/cgroup/user.slice/user-1003.slice/user@1003.service"}).encode()
            class Sftp:
                def __enter__(self): return self
                def __exit__(self, *args): return False
                def open(self, path, mode):
                    self_path = "/home/worker/worker.json"
                    if (path, mode) != (self_path, "rb"): raise AssertionError("unexpected remote file access")
                    return io.BytesIO(remote_config)
            class Channel:
                out = [b'{"binary":"aw-cli"}\nowner-exec --config FILE\n']
                def recv_ready(self): return bool(self.out)
                def recv_stderr_ready(self): return False
                def recv(self, size): return self.out.pop(0)
                def exit_status_ready(self): return not self.out
                def recv_exit_status(self): return 0
                def close(self): pass
            class HostKey:
                def get_name(self): return "ssh-ed25519"
                def asbytes(self): return b"synthetic-host-key"
            class Transport:
                def get_remote_server_key(self): return HostKey()
            class Client:
                closed = False
                def load_host_keys(self, value): self.known = value
                def set_missing_host_key_policy(self, value): self.policy = value
                def connect(self, **kwargs): self.connection = kwargs
                def open_sftp(self): return Sftp()
                def exec_command(self, command, **kwargs):
                    self.command = command
                    return io.BytesIO(), types.SimpleNamespace(channel=Channel()), io.BytesIO()
                def get_transport(self): return Transport()
                def close(self): self.closed = True
            class RejectPolicy: pass
            client = Client()
            paramiko = types.SimpleNamespace(SSHClient=lambda: client, RejectPolicy=RejectPolicy)
            helper = types.SimpleNamespace(_load_private_key=lambda value: "synthetic-parsed-key")
            args = types.SimpleNamespace(gateway=gateway_path, package=package, package_manifest=manifest)
            with mock.patch.dict(sys.modules, {"paramiko": paramiko}), mock.patch.object(driver, "load_module", return_value=helper):
                result = driver.prepare_existing(args, bytearray(b"synthetic-private-material"), evidence)
            self.assertEqual(result["database"], root / "never-created-state/controller.sqlite")
            self.assertEqual(client.known, str(known))
            self.assertIsInstance(client.policy, RejectPolicy)
            self.assertIs(client.connection["allow_agent"], False)
            self.assertIs(client.connection["look_for_keys"], False)
            self.assertEqual(client.connection["username"], "awt1104094b2a0cea")
            self.assertTrue(client.closed)
            self.assertNotIn("mkdir", client.command)
            self.assertNotIn("authorized_keys", client.command)
            self.assertFalse((root / "never-created-state").exists())
            recorded = (evidence / "preflight.json").read_text(encoding="utf-8")
            self.assertNotIn("synthetic-private-material", recorded)
            self.assertEqual(json.loads(recorded)["wait_kind"], "PARAMIKO_CHANNEL_ONLY")

    def test_preflight_output_bound_prevents_hanging_or_unbounded_capture(self):
        driver = load_driver(self)
        self.assertTrue(callable(getattr(driver, "read_channel", None)), "bounded channel drain is missing")
        class Channel:
            closed = False
            def recv_ready(self): return True
            def recv_stderr_ready(self): return False
            def recv(self, count): return b"x" * 65536
            def exit_status_ready(self): return False
            def close(self): self.closed = True
        channel = Channel()
        with self.assertRaises(ValueError):
            driver.read_channel(channel, timeout=1)
        self.assertTrue(channel.closed)

    def test_duplicate_run_is_rejected_before_any_gui_dispatch_click(self):
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            evidence = Path(temp)
            controls = evidence / "controls"
            controls.mkdir()
            (controls / "command-0001.json").write_text(json.dumps({"action": "dispatch", "x": 20, "y": 20}), encoding="utf-8")
            database = evidence / "controller.sqlite"
            admitted_store(database)
            class Process:
                pid = 123
                def poll(self): return None
            class Rect:
                left = top = 0
                right = bottom = 100
            class User:
                def GetWindowRect(self, *args): return True
            class UI:
                RECT = Rect
                user = User()
                class ctypes:
                    @staticmethod
                    def byref(value): return value
                def click(self, *args):
                    raise AssertionError("duplicate run must not click dispatch")
            with self.assertRaises(ValueError):
                driver.drive_manager({"evidence": evidence, "ui": UI(), "process": Process(),
                                      "hwnd": 1, "database": database}, control_timeout=1)


class ClosureLifecycleTests(unittest.TestCase):
    def test_credential_lease_covers_prepare_dispatch_store_check_and_manager_close(self):
        driver = load_driver(self)
        events = []
        active = {"credential": False, "manager": False}

        @contextlib.contextmanager
        def credential():
            active["credential"] = True
            events.append("credential-open")
            try:
                yield bytearray(b"synthetic-not-a-private-key")
            finally:
                events.append("credential-cleanup")
                active["credential"] = False

        def prepare(key, evidence):
            self.assertTrue(active["credential"])
            events.append("prepare")
            return "prepared"

        @contextlib.contextmanager
        def manager(key, prepared, evidence):
            self.assertEqual(prepared, "prepared")
            active["manager"] = True
            events.append("manager-open")
            try:
                yield object()
            finally:
                self.assertTrue(active["credential"], "credential ended before Manager closed")
                events.append("manager-close")
                active["manager"] = False

        def dispatch(session):
            self.assertTrue(active["credential"] and active["manager"])
            events.append("dispatch")

        def verify(session):
            self.assertTrue(active["credential"] and active["manager"])
            events.append("store-check")
            return {"status": "PASS_STORE_ADMISSION", "run_id": "run-1", "used_slots": 0}

        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / "new-evidence"
            result = driver.run_closure(out, credential, prepare, manager, dispatch, verify)
            persisted = json.loads((out / "result.json").read_text(encoding="utf-8"))
        self.assertEqual(events, ["credential-open", "prepare", "manager-open", "dispatch", "store-check", "manager-close", "credential-cleanup"])
        self.assertEqual(result["status"], "PASS_MANAGER_WORKER_CLOSURE")
        self.assertEqual(result, persisted)
        self.assertFalse(active["credential"] or active["manager"])

    def test_foreground_timeout_records_only_fixed_manual_action_reason(self):
        driver = load_driver(self)
        @contextlib.contextmanager
        def credential():
            yield bytearray(b"synthetic")
        @contextlib.contextmanager
        def manager(*args):
            yield object()
        def dispatch(session):
            raise driver.ClosureCheckError("HUMAN_FOREGROUND_REQUIRED")
        with tempfile.TemporaryDirectory() as temp:
            result = driver.run_closure(Path(temp) / "new", credential, lambda *args: None,
                manager, dispatch, lambda session: self.fail("no Store check without dispatch"))
        self.assertEqual(result.get("reason"), "HUMAN_FOREGROUND_REQUIRED")
        self.assertEqual(result["dispatch"], "NOT_RUN")

    def test_remote_failure_is_redacted_and_still_closes_credential_lease(self):
        driver = load_driver(self)
        closed = []

        @contextlib.contextmanager
        def credential():
            try:
                yield bytearray(b"synthetic")
            finally:
                closed.append(True)

        def fail(*args):
            raise RuntimeError("synthetic-private-material-must-not-be-recorded")

        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp) / "new"
            result = driver.run_closure(out, credential, fail, fail, fail, fail)
            serialized = (out / "result.json").read_text(encoding="utf-8")
        self.assertEqual(closed, [True])
        self.assertEqual(result["status"], "BLOCKED_OR_FAILED")
        self.assertEqual(result["failed_stage"], "remote_prepare")
        self.assertEqual(result["error_type"], "RuntimeError")
        self.assertNotIn("synthetic-private-material", serialized)
        self.assertEqual(result["dispatch"], "NOT_RUN")

    def test_cleanup_failure_cannot_preserve_a_pass_status(self):
        driver = load_driver(self)

        @contextlib.contextmanager
        def credential():
            try:
                yield bytearray(b"synthetic")
            finally:
                raise OSError("synthetic-secret-not-for-logs")

        @contextlib.contextmanager
        def manager(*args):
            yield object()

        with tempfile.TemporaryDirectory() as temp:
            result = driver.run_closure(Path(temp) / "new", credential,
                lambda *args: None, manager, lambda session: None,
                lambda session: {"status": "PASS_STORE_ADMISSION", "run_id": "r", "used_slots": 0})
        self.assertEqual(result["status"], "BLOCKED_OR_FAILED")
        self.assertEqual(result["failed_stage"], "credential_cleanup")
        self.assertEqual(result["store"]["status"], "PASS_STORE_ADMISSION")

    def test_manager_context_cannot_suppress_failure_into_closure_success(self):
        driver = load_driver(self)
        @contextlib.contextmanager
        def credential():
            yield bytearray(b"synthetic")
        @contextlib.contextmanager
        def manager(*args):
            try:
                yield object()
            except RuntimeError:
                pass
        def dispatch(session):
            raise RuntimeError("dispatch failed")
        with tempfile.TemporaryDirectory() as temp:
            result = driver.run_closure(Path(temp) / "new", credential, lambda *args: None,
                manager, dispatch, lambda session: self.fail("verification must not run"))
        self.assertEqual(result["status"], "BLOCKED_OR_FAILED")
        self.assertEqual(result["dispatch"], "NOT_RUN")
        self.assertEqual(result["store"], "NOT_RUN")

    def test_manager_close_failure_cannot_preserve_store_pass(self):
        driver = load_driver(self)
        key = bytearray(b"synthetic")
        @contextlib.contextmanager
        def credential():
            yield key
        @contextlib.contextmanager
        def manager(*args):
            try:
                yield object()
            finally:
                raise RuntimeError("manager shutdown failed")
        with tempfile.TemporaryDirectory() as temp:
            result = driver.run_closure(Path(temp) / "new", credential, lambda *args: None,
                manager, lambda session: None,
                lambda session: {"status": "PASS_STORE_ADMISSION", "run_id": "r", "used_slots": 0})
        self.assertEqual(result["status"], "BLOCKED_OR_FAILED")
        self.assertEqual(result["failed_stage"], "manager_close")
        self.assertEqual(key, bytearray(len(b"synthetic")))

    def test_zero_slots_without_store_admission_does_not_mark_closure_pass(self):
        driver = load_driver(self)
        @contextlib.contextmanager
        def credential():
            yield bytearray(b"synthetic")
        @contextlib.contextmanager
        def manager(*args):
            yield object()
        with tempfile.TemporaryDirectory() as temp:
            result = driver.run_closure(Path(temp) / "new", credential, lambda *args: None,
                manager, lambda session: None,
                lambda session: {"status": "NOT_ADMITTED", "used_slots": 0})
        self.assertEqual(result["status"], "BLOCKED_OR_FAILED")
        self.assertEqual(result["failed_stage"], "store_acceptance")

    def test_existing_evidence_is_rejected_before_acquiring_credentials(self):
        driver = load_driver(self)
        def forbidden(*args):
            self.fail("existing evidence must be rejected before side effects")
        with tempfile.TemporaryDirectory() as temp:
            out = Path(temp)
            marker = out / "preserve.txt"
            marker.write_text("old evidence", encoding="utf-8")
            with self.assertRaises(FileExistsError):
                driver.run_closure(out, forbidden, forbidden, forbidden, forbidden, forbidden)
            self.assertEqual(marker.read_text(encoding="utf-8"), "old evidence")
            self.assertEqual(list(out.iterdir()), [marker])

    def test_missing_credentials_do_not_run_remote_or_manager(self):
        driver = load_driver(self)
        @contextlib.contextmanager
        def absent():
            yield bytearray()
        def forbidden(*args):
            self.fail("missing credentials must not reach any remote or GUI boundary")
        with tempfile.TemporaryDirectory() as temp:
            result = driver.run_closure(Path(temp) / "new", absent, forbidden, forbidden, forbidden, forbidden)
        self.assertEqual(result["status"], "BLOCKED_OR_FAILED")
        self.assertEqual(result["failed_stage"], "credential_acquire")


def admitted_store(database, protocol_key=b"\x11" * 32):
    """Hand-shaped local DB fixture, not signed/remote execution evidence.

    Envelopes carry REAL HMAC-SHA256 signatures over the protocol preimage
    (envelope minus "mac", canonical JSON), so acceptance checks can verify
    MAC values, not just lengths. protocol_key default is a public fixture
    constant, never a production secret."""
    encode = lambda value: json.dumps(value, sort_keys=True, ensure_ascii=False, separators=(",", ":"))
    sha = lambda value: hashlib.sha256(encode(value).encode("utf-8")).hexdigest()
    mac = lambda value: hmac.new(protocol_key, encode({k: v for k, v in value.items() if k != "mac"}).encode("utf-8"), hashlib.sha256).hexdigest()
    artifact = "ab" * 32
    execution = {"deployment": "live-test", "run_id": "run-1", "attempt": 1,
                 "epoch": "epoch-1", "session": "session-1", "artifact_sha256": artifact}
    start_payload = {"template": "sum-v1", "worker_id": "B", "worker_artifact_sha256": artifact}
    binding = {"execution": execution, "owner": {"node_id": "B", "instance_id": "fixture-instance",
        "boot_id": "fixture-boot", "owner_handle": "fixture-owner"}, "request_sha256": sha(start_payload),
        "owner_process": {"pid": 100, "start_ticks": 1000},
        "process": {"pid": 200, "parent_pid": 100, "start_ticks": 2000, "pgid": 200,
                    "cgroup_path": "workers/fixture-instance/fixture-owner"}}
    report = {"schema": "native-sum/1", "session": "session-1", "inputs": [1, 2, 3], "sum": 6}
    verification = {"passed": True, "state": "verified", "details": {
        "source": "native-sum-verifier", "artifact_sha256": artifact}}
    stdout = encode(report).encode("utf-8") + b"\n"
    streams = {}
    for name, raw in (("stdout", stdout), ("stderr", b"diagnostic\n")):
        streams[name] = {"prefix_len": len(raw), "total_read_bytes": len(raw), "truncated": False,
                         "eof": True, "error": None, "prefix_sha256": hashlib.sha256(raw).hexdigest()}
    record = {"schema": "linux-worker-owner-evidence/1", **binding,
        "direct_wait": {"wait_completed": True, "method": "child_try_wait", "raw_wait_status": 0,
                        "exit_code": 0, "termination_signal": None, "numeric_exit": 0}, **streams,
        "cleanup": {"proc_scan_complete": True, "process_group_scan_complete": True,
                    "cgroup_scan_complete": True, "pgid": 200,
                    "cgroup_path": "workers/fixture-instance/fixture-owner",
                    "process_group_members": 0, "cgroup_members": 0, "error": None},
        "evidence_refs": {"record": "owners/fixture-instance/fixture-owner/record.json",
                          "stdout": "owners/fixture-instance/fixture-owner/stdout.bin",
                          "stderr": "owners/fixture-instance/fixture-owner/stderr.bin"}}
    bundle = {"record": record, "record_sha256": sha(record), "stdout_prefix_hex": stdout.hex(),
              "stderr_prefix_hex": b"diagnostic\n".hex()}
    reference = {"schema": "owner-evidence-reference/1",
        "binding_sha256": sha({"schema": "owner-binding-reference/1", "binding": binding}),
        "bundle_sha256": sha(bundle), "record_sha256": sha(record),
        "report_sha256": sha({"schema": "owner-report-binding/1", "execution": execution,
                              "report": report, "verification": verification})}
    proof = {"run_id": "run-1", "attempt": 1, "epoch": "epoch-1", "session": "session-1",
             "empty": True, "observer_alive": True, "not_started": False, "owner_evidence": reference}
    launch = {"schema": "owner-evidence-launch/1", "phase": "launch", "session": "session-1", "binding": binding}
    terminal = {"schema": "owner-evidence-report/1", "phase": "terminal", "session": "session-1",
                "report": report, "verification": verification,
                "owner_evidence": {"reference": reference, "bundle_json": encode(bundle)}, "exit_proof": proof}
    base = {"schema": "ao-reason-native/1", "deployment": "live-test", "run_id": "run-1", "attempt": 1,
            "epoch": "epoch-1", "sent_at": 100, "expires_at": 400}
    events = []
    for body in ({**base, "message_id": "launch-1", "from": "B", "to": "A", "kind": "report", "payload": launch},
                 {**base, "message_id": "terminal-1", "from": "B", "to": "A", "kind": "report", "payload": terminal}):
        events.append({**body, "mac": mac(body)})
    start_body = {**base, "message_id": "start-1", "from": "A", "to": "B", "kind": "start", "payload": start_payload}
    start = {**start_body, "mac": mac(start_body)}
    run = {"id": "run-1", "attempt": 1, "epoch": "epoch-1", "session": "session-1", "status": "done",
           "detail": {"owner_admission": "admitted", "worker_artifact_sha256": artifact,
                      "owner_launch": launch, "report": report, "verification": verification, "exit_proof": proof}}
    with contextlib.closing(sqlite3.connect(database)) as db, db:
        db.executescript("""
            CREATE TABLE metadata(key TEXT PRIMARY KEY,value TEXT);
            INSERT INTO metadata VALUES('deployment','live-test');
            CREATE TABLE runs(id TEXT,body TEXT,status TEXT,attempt INTEGER);
            CREATE TABLE assignments(run TEXT,attempt INTEGER,node TEXT,address TEXT,released INTEGER);
            CREATE TABLE inbox(id TEXT,body TEXT,handled INTEGER,received_at INTEGER);
            CREATE TABLE outbox(id TEXT,destination TEXT,body TEXT,sent INTEGER);
            CREATE TABLE owner_bindings(run TEXT,attempt INTEGER,epoch TEXT,binding_sha256 TEXT,
                expected_json TEXT,source_message_id TEXT,registered_at INTEGER,node TEXT,address TEXT);
            CREATE TABLE owner_evidence(run TEXT,attempt INTEGER,epoch TEXT,session TEXT,binding_sha256 TEXT,
                bundle_sha256 TEXT,record_sha256 TEXT,report_sha256 TEXT,bundle_json TEXT,report_json TEXT,
                verification_json TEXT,source_message_id TEXT,admitted_at INTEGER);
        """)
        db.execute("INSERT INTO runs VALUES(?,?,?,?)", ("run-1", encode(run), "done", 1))
        db.execute("INSERT INTO assignments VALUES(?,?,?,?,?)", ("run-1", 1, "B", "172.28.8.210", 1))
        db.executemany("INSERT INTO inbox VALUES(?,?,?,?)", [(e["message_id"], encode(e), 1, 101) for e in events])
        db.execute("INSERT INTO outbox VALUES(?,?,?,?)", ("start-1", "172.28.8.210", encode(start), 0))
        db.execute("INSERT INTO owner_bindings VALUES(?,?,?,?,?,?,?,?,?)", ("run-1", 1, "epoch-1",
            reference["binding_sha256"], encode(binding), "launch-1", 101, "B", "172.28.8.210"))
        db.execute("INSERT INTO owner_evidence VALUES(?,?,?,?,?,?,?,?,?,?,?,?,?)", ("run-1", 1, "epoch-1", "session-1",
            reference["binding_sha256"], reference["bundle_sha256"], reference["record_sha256"], reference["report_sha256"],
            encode(bundle), encode(report), encode(verification), "terminal-1", 101))


class StoreAcceptanceTests(unittest.TestCase):
    def test_complete_admission_is_projected_without_modifying_database(self):
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            database = Path(temp) / "controller.sqlite"
            admitted_store(database)
            before = database.read_bytes()
            result = driver.inspect_store(database, "live-test", "B", "ab" * 32)
            self.assertEqual(database.read_bytes(), before)
        self.assertEqual(result["status"], "PASS_STORE_ADMISSION", result)
        self.assertEqual(result["run_id"], "run-1")
        self.assertEqual(result["used_slots"], 0)
        self.assertTrue(result["native_wait_completed"])

    def test_mac_value_verification_accepts_genuinely_signed_envelopes(self):
        """Acceptance must recompute MAC VALUES (not lengths) when a protocol
        key is provided: a correctly-signed store passes."""
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            database = Path(temp) / "controller.sqlite"
            admitted_store(database, protocol_key=b"\x22" * 32)
            result = driver.inspect_store(database, "live-test", "B", "ab" * 32,
                                          protocol_key=b"\x22" * 32)
        self.assertEqual(result["status"], "PASS_STORE_ADMISSION", result)
        self.assertEqual(result.get("signature_authority"),
                         "independent HMAC-SHA256 recheck over all persisted envelopes")
        self.assertEqual(result.get("envelopes_mac_verified"), 3,
                         "must report the count of envelopes actually MAC-checked")

    def test_mac_value_verification_rejects_tampered_envelope(self):
        """Any tampered field in any persisted envelope must fail acceptance
        even though every digest chain still matches and lengths are valid."""
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            database = Path(temp) / "controller.sqlite"
            admitted_store(database, protocol_key=b"\x22" * 32)
            with contextlib.closing(sqlite3.connect(database)) as db, db:
                db.execute("UPDATE inbox SET body=json_set(body,'$.payload.binding.owner.node_id','C') WHERE id='launch-1'")
            result = driver.inspect_store(database, "live-test", "B", "ab" * 32,
                                          protocol_key=b"\x22" * 32)
        self.assertNotEqual(result["status"], "PASS_STORE_ADMISSION", result)

    def test_mac_value_verification_rejects_wrong_key(self):
        """Signatures made with a different key must fail acceptance."""
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            database = Path(temp) / "controller.sqlite"
            admitted_store(database, protocol_key=b"\x22" * 32)
            result = driver.inspect_store(database, "live-test", "B", "ab" * 32,
                                          protocol_key=b"\x33" * 32)
        self.assertNotEqual(result["status"], "PASS_STORE_ADMISSION", result)

    def test_partial_or_conflicting_store_never_passes(self):
        driver = load_driver(self)
        mutations = [
            "DELETE FROM inbox WHERE id='launch-1'",
            "UPDATE inbox SET handled=0 WHERE id='terminal-1'",
            "UPDATE assignments SET released=0",
            "UPDATE owner_evidence SET session='other-session'",
            "UPDATE owner_evidence SET bundle_sha256='wrong'",
            "UPDATE runs SET body=json_set(body,'$.detail.owner_admission','unknown')",
            "UPDATE runs SET body=json_set(body,'$.detail.report.sum',7)",
            "UPDATE inbox SET body=json_set(body,'$.epoch','other-epoch') WHERE id='terminal-1'",
            "UPDATE outbox SET body=json_set(body,'$.payload.worker_artifact_sha256','wrong')",
        ]
        for mutation in mutations:
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temp:
                database = Path(temp) / "controller.sqlite"
                admitted_store(database)
                with contextlib.closing(sqlite3.connect(database)) as db, db:
                    db.execute(mutation)
                self.assertEqual(driver.inspect_store(database, "live-test", "B")["status"], "NOT_ADMITTED")

    def test_empty_store_with_zero_slots_does_not_count_as_execution(self):
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            database = Path(temp) / "controller.sqlite"
            with contextlib.closing(sqlite3.connect(database)) as connection, connection:
                connection.executescript("""
                    CREATE TABLE metadata(key TEXT PRIMARY KEY, value TEXT NOT NULL);
                    INSERT INTO metadata VALUES('deployment','live-test');
                    CREATE TABLE runs(id TEXT PRIMARY KEY, body TEXT, status TEXT, attempt INTEGER);
                    CREATE TABLE assignments(run TEXT, attempt INTEGER, node TEXT, address TEXT, released INTEGER);
                """)
            result = driver.inspect_store(database, "live-test", "B")
        self.assertEqual(result["status"], "NOT_ADMITTED")
        self.assertEqual(result["used_slots"], 0)
        self.assertEqual(result["reason"], "expected_exactly_one_dispatched_run")

    def test_missing_database_is_not_created_by_read_only_inspection(self):
        driver = load_driver(self)
        with tempfile.TemporaryDirectory() as temp:
            database = Path(temp) / "missing.sqlite"
            with self.assertRaises(FileNotFoundError):
                driver.inspect_store(database, "live-test", "B")
            self.assertFalse(database.exists())


if __name__ == "__main__":
    unittest.main()
