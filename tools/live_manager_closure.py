"""Run one Manager dispatch within one externally supplied credential lifetime.

Only the Store admits signed events and releases assignments. The inspector below
checks that Store's persisted admission and cross-references; it does not establish
an independent HMAC trust root. Local test fixtures are never live-run evidence.
No credentials are acquired by discovery, written to disk, or put in argv/env.
"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import re
import shlex
import sqlite3
import subprocess
import sys
import time


class ClosureCheckError(ValueError):
    pass


def require(condition, reason):
    if not condition:
        raise ClosureCheckError(reason)


def write_json_new(path, value):
    with Path(path).open("x", encoding="utf-8", newline="\n") as stream:
        json.dump(value, stream, ensure_ascii=True, indent=2)
        stream.write("\n")


def run_closure(evidence, credential, prepare, manager, dispatch, verify):
    """Bound preparation, actual GUI dispatch and Store inspection by a lease.

    Network/GUI adapters are injected so lifecycle failures can be tested locally.
    prepare/dispatch must raise on failure. verify must inspect the fresh Store.
    Context manager cleanup errors always prevent a closure PASS.
    """
    evidence = Path(evidence)
    evidence.mkdir(parents=True, exist_ok=False)
    result = {
        "schema": "manager-worker-closure/1",
        "status": "BLOCKED_OR_FAILED",
        "dispatch": "NOT_RUN",
        "store": "NOT_RUN",
        "credential_cleanup": "NOT_CONFIRMED",
    }
    stage = "credential_acquire"
    body_error = None
    body_stage = None
    key = None
    try:
        with credential() as key:
            try:
                require(isinstance(key, (bytes, bytearray)) and 0 < len(key) <= 65536,
                        "credential_unavailable_or_oversized")
                stage = "remote_prepare"
                prepared = prepare(key, evidence)
                stage = "manager_open"
                with manager(key, prepared, evidence) as session:
                    stage = "manager_dispatch"
                    dispatch(session)
                    result["dispatch"] = "GUI_INPUT_SENT"
                    stage = "store_acceptance"
                    result["store"] = verify(session)
                    facts = result["store"]
                    require(isinstance(facts, dict)
                            and facts.get("status") == "PASS_STORE_ADMISSION"
                            and bool(facts.get("run_id"))
                            and facts.get("used_slots") == 0,
                            "store_admission_missing")
                    stage = "manager_close"
                require(stage == "manager_close", "manager_context_suppressed_failure")
            except Exception as exc:
                body_error, body_stage = exc, stage
                raise
            finally:
                stage = "credential_cleanup"
        result["credential_cleanup"] = "PASS"
        # Also fail closed if a supplied context manager suppressed a body error.
        require(body_error is None, "context_suppressed_failure")
        result["status"] = "PASS_MANAGER_WORKER_CLOSURE"
    except Exception as exc:
        result["failed_stage"] = body_stage if exc is body_error else stage
        result["error_type"] = type(exc).__name__
        if isinstance(exc, ClosureCheckError) and str(exc) == "HUMAN_FOREGROUND_REQUIRED":
            result["reason"] = "HUMAN_FOREGROUND_REQUIRED"
        if exc is body_error:
            result["credential_cleanup"] = "PASS"
        # Never serialize exception messages, private keys or adapter return data.
    finally:
        if isinstance(key, bytearray):
            key[:] = b"\x00" * len(key)
        write_json_new(evidence / "result.json", result)
    return result


def canonical(value):
    return json.dumps(value, sort_keys=True, ensure_ascii=False,
                      separators=(",", ":"), allow_nan=False).encode("utf-8")


def digest(value):
    return hashlib.sha256(canonical(value)).hexdigest()


def verify_envelope_macs(connection, protocol_key):
    """Recompute HMAC-SHA256 over every persisted signed envelope (outbox +
    inbox) using the protocol preimage (envelope minus "mac", canonical JSON)
    -- the Manager-side scheme from src/core/protocol.rs sign()/verify().
    Raises ClosureCheckError("mac_value_mismatch:<message_id>") on any
    mismatch. Key bytes stay in the caller's frame; nothing is persisted."""
    import hmac as hmac_module
    verified = 0
    for table in ("outbox", "inbox"):
        for row in connection.execute(f"SELECT body FROM {table} ORDER BY id"):
            envelope = json.loads(row[0])
            mac_hex = envelope.get("mac", "")
            if len(mac_hex) != 64 or any(c not in "0123456789abcdef" for c in mac_hex):
                raise ClosureCheckError(f"mac_value_malformed:{table}")
            preimage = canonical({k: v for k, v in envelope.items() if k != "mac"})
            computed = hmac_module.new(protocol_key, preimage, hashlib.sha256).hexdigest()
            if not hmac_module.compare_digest(computed, mac_hex):
                raise ClosureCheckError(f"mac_value_mismatch:{envelope.get('message_id', table)}")
            verified += 1
    return verified


def inspect_store(database, deployment, worker_id, artifact_sha256=None, protocol_key=None):
    """Read one consistent snapshot without creating/updating a controller DB.

    This is acceptance of a trusted, fresh Manager-owned Store, not an importer
    or digest verifier. handled inbox references are required for both phases.
    With protocol_key (32 bytes, hex-decoded from the staged key file) every
    persisted envelope's MAC VALUE is additionally recomputed and compared --
    closing the length-only gap this tool previously documented.
    """
    database = Path(database).resolve()
    if not database.is_file():
        raise FileNotFoundError(database)
    facts = {"status": "NOT_ADMITTED", "used_slots": None}
    connection = sqlite3.connect(database.as_uri() + "?mode=ro", uri=True, timeout=3)
    connection.row_factory = sqlite3.Row
    try:
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN")
        if protocol_key is not None:
            require(len(protocol_key) == 32, "protocol_key_must_be_32_bytes")
            facts["envelopes_mac_verified"] = verify_envelope_macs(connection, protocol_key)
        identity = connection.execute("SELECT value FROM metadata WHERE key='deployment'").fetchone()
        require(identity is not None and identity[0] == deployment, "deployment_mismatch")
        facts["used_slots"] = connection.execute(
            "SELECT count(*) FROM assignments WHERE node=? AND released=0", (worker_id,)
        ).fetchone()[0]
        rows = connection.execute("SELECT id,body,status,attempt FROM runs").fetchall()
        require(len(rows) == 1, "expected_exactly_one_dispatched_run")
        row = rows[0]
        run = json.loads(row["body"])
        require(run["id"] == row["id"] and run["attempt"] == row["attempt"], "run_row_mismatch")
        facts.update(run_id=run["id"], attempt=run["attempt"], epoch=run["epoch"], session=run["session"])
        require(row["status"] == run["status"] == "done", "run_not_done")
        require(bool(run["session"]) and run["detail"].get("owner_admission") == "admitted", "owner_not_admitted")
        assignments = connection.execute("SELECT * FROM assignments").fetchall()
        require(len(assignments) == 1, "expected_exactly_one_assignment")
        assignment = assignments[0]
        require(assignment["run"] == run["id"] and assignment["attempt"] == run["attempt"]
                and assignment["node"] == worker_id and assignment["released"] == 1
                and facts["used_slots"] == 0, "assignment_not_released")
        binding_row = connection.execute("SELECT * FROM owner_bindings WHERE run=? AND attempt=?",
                                         (run["id"], run["attempt"])).fetchone()
        evidence = connection.execute("SELECT * FROM owner_evidence WHERE run=? AND attempt=?",
                                      (run["id"], run["attempt"])).fetchone()
        require(binding_row is not None and evidence is not None, "owner_rows_missing")
        binding = json.loads(binding_row["expected_json"])
        execution = binding["execution"]
        artifact = run["detail"]["worker_artifact_sha256"]
        require(artifact_sha256 is None or artifact == artifact_sha256, "artifact_mismatch")
        require(execution == {"deployment": deployment, "run_id": run["id"], "attempt": run["attempt"],
                              "epoch": run["epoch"], "session": run["session"], "artifact_sha256": artifact},
                "execution_mismatch")
        require(binding_row["epoch"] == evidence["epoch"] == run["epoch"]
                and evidence["session"] == run["session"]
                and binding_row["node"] == binding["owner"]["node_id"] == worker_id
                and binding_row["address"] == assignment["address"], "owner_route_mismatch")
        binding_sha = digest({"schema": "owner-binding-reference/1", "binding": binding})
        require(binding_sha == binding_row["binding_sha256"] == evidence["binding_sha256"], "binding_digest_mismatch")
        bundle = json.loads(evidence["bundle_json"])
        record = bundle["record"]
        require(record["schema"] == "linux-worker-owner-evidence/1", "record_schema_mismatch")
        for name in ("execution", "owner", "request_sha256", "owner_process", "process"):
            require(record[name] == binding[name], "record_binding_mismatch")
        require(digest(record) == bundle["record_sha256"] == evidence["record_sha256"], "record_digest_mismatch")
        require(digest(bundle) == evidence["bundle_sha256"], "bundle_digest_mismatch")
        report = json.loads(evidence["report_json"])
        verification = json.loads(evidence["verification_json"])
        require(run["detail"].get("report") == report and run["detail"].get("verification") == verification,
                "run_report_mismatch")
        require(report == {"schema": "native-sum/1", "session": run["session"], "inputs": [1, 2, 3], "sum": 6},
                "sum_report_mismatch")
        require(verification == {"passed": True, "state": "verified", "details": {
            "source": "native-sum-verifier", "artifact_sha256": artifact}}, "verification_mismatch")
        require(digest({"schema": "owner-report-binding/1", "execution": execution,
                        "report": report, "verification": verification}) == evidence["report_sha256"],
                "report_digest_mismatch")
        wait = record["direct_wait"]
        require(wait["wait_completed"] is True and wait["method"] in ("child_wait", "child_try_wait")
                and wait["raw_wait_status"] == wait["numeric_exit"] == wait["exit_code"] == 0
                and wait["termination_signal"] is None, "native_wait_missing")
        for stream in ("stdout", "stderr"):
            item = record[stream]
            prefix = bytes.fromhex(bundle[stream + "_prefix_hex"])
            require(item["eof"] is True and item["error"] is None
                    and len(prefix) == item["prefix_len"] == min(item["total_read_bytes"], 4096)
                    and item["truncated"] == (item["total_read_bytes"] > len(prefix))
                    and hashlib.sha256(prefix).hexdigest() == item["prefix_sha256"], "stream_incomplete")
        require(json.loads(bytes.fromhex(bundle["stdout_prefix_hex"])) == report, "stdout_report_mismatch")
        cleanup = record["cleanup"]
        process = record["process"]
        require(process["pid"] > 0 and process["start_ticks"] > 0
                and process["pid"] == process["pgid"]
                and process["parent_pid"] == record["owner_process"]["pid"]
                and process["pid"] != process["parent_pid"], "process_identity_mismatch")
        require(all(cleanup[name] is True for name in (
            "proc_scan_complete", "process_group_scan_complete", "cgroup_scan_complete"))
            and cleanup["process_group_members"] == cleanup["cgroup_members"] == 0
            and cleanup["error"] is None and cleanup["pgid"] == process["pgid"]
            and cleanup["cgroup_path"] == process["cgroup_path"], "cleanup_not_empty")
        reference = {"schema": "owner-evidence-reference/1", "binding_sha256": binding_sha,
                     **{name: evidence[name] for name in ("bundle_sha256", "record_sha256", "report_sha256")}}
        for phase, source, received_at in (
            ("launch", binding_row["source_message_id"], binding_row["registered_at"]),
            ("terminal", evidence["source_message_id"], evidence["admitted_at"]),
        ):
            inbox = connection.execute("SELECT * FROM inbox WHERE id=?", (source,)).fetchone()
            require(inbox is not None and inbox["handled"] == 1
                    and inbox["received_at"] == received_at, "handled_signed_event_missing")
            event = json.loads(inbox["body"])
            require(event["schema"] == "ao-reason-native/1" and event["message_id"] == source
                    and event["deployment"] == deployment and event["from"] == worker_id and event["to"] == "A"
                    and event["kind"] == "report" and event["run_id"] == run["id"]
                    and event["attempt"] == run["attempt"] and event["epoch"] == run["epoch"]
                    and len(bytes.fromhex(event["mac"])) == 32
                    and event["sent_at"] <= received_at + 45 < event["expires_at"] + 45
                    and 0 < event["expires_at"] - event["sent_at"] <= 300,
                    "event_identity_mismatch")
            payload = event["payload"]
            require(payload["phase"] == phase and payload["session"] == run["session"], "event_phase_mismatch")
            if phase == "launch":
                require(payload["schema"] == "owner-evidence-launch/1" and payload["binding"] == binding
                        and run["detail"]["owner_launch"] == payload, "launch_binding_mismatch")
            else:
                require(payload["schema"] == "owner-evidence-report/1" and payload["report"] == report
                        and payload["verification"] == verification
                        and payload["owner_evidence"] == {"reference": reference, "bundle_json": evidence["bundle_json"]},
                        "terminal_evidence_mismatch")
                proof = payload["exit_proof"]
                require(proof == run["detail"]["exit_proof"] and proof == {
                    "run_id": run["id"], "attempt": run["attempt"], "epoch": run["epoch"],
                    "session": run["session"], "empty": True, "observer_alive": True,
                    "not_started": False, "owner_evidence": reference}, "exit_proof_mismatch")
        starts = connection.execute("SELECT body FROM outbox WHERE json_extract(body,'$.kind')='start'").fetchall()
        require(len(starts) == 1, "signed_start_missing")
        start = json.loads(starts[0][0])
        require(start["schema"] == "ao-reason-native/1" and start["deployment"] == deployment
                and start["from"] == "A" and start["to"] == worker_id and start["run_id"] == run["id"]
                and start["attempt"] == run["attempt"] and start["epoch"] == run["epoch"]
                and start["payload"]["worker_artifact_sha256"] == artifact
                and start["payload"]["worker_id"] == worker_id and start["payload"]["template"] == "sum-v1"
                and len(bytes.fromhex(start["mac"])) == 32
                and digest(start["payload"]) == binding["request_sha256"], "signed_start_mismatch")
        facts.update(status="PASS_STORE_ADMISSION", artifact_sha256=artifact,
                     owner_admission="admitted", assignment_released=True,
                     binding_sha256=binding_sha, bundle_sha256=evidence["bundle_sha256"],
                     native_wait_completed=True, stdout_eof=True, stderr_eof=True,
                     process_group_members=0, cgroup_members=0,
                     signature_authority=(
                         "independent HMAC-SHA256 recheck over all persisted envelopes"
                         if protocol_key is not None else
                         "Manager Store handled admission; no independent HMAC recheck"))
    except ClosureCheckError as exc:
        facts["reason"] = str(exc)  # Only our fixed non-secret reason codes.
    except (KeyError, TypeError, ValueError, sqlite3.Error):
        facts["reason"] = "store_incomplete_or_malformed"
    finally:
        connection.close()
    return facts


@contextlib.contextmanager
def stdin_key_lease(stream):
    key = bytearray(stream.read(65537))
    try:
        require(0 < len(key) <= 65536, "credential_unavailable_or_oversized")
        yield key
    finally:
        key[:] = b"\x00" * len(key)


def load_module(path, name):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def bounded_json(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(65537)
    require(len(raw) <= 65536, "configuration_oversized")
    return json.loads(raw)


def file_identity(path):
    with Path(path).open("rb") as stream:
        return {"bytes": Path(path).stat().st_size,
                "sha256": hashlib.file_digest(stream, "sha256").hexdigest()}


def read_channel(channel, timeout=30):
    """Paramiko channel completion only, never Linux owner wait evidence."""
    output, error = bytearray(), bytearray()
    deadline = time.monotonic() + timeout
    try:
        while not channel.exit_status_ready() or channel.recv_ready() or channel.recv_stderr_ready():
            require(time.monotonic() < deadline, "preflight_channel_deadline")
            if channel.recv_ready():
                output.extend(channel.recv(65536))
            if channel.recv_stderr_ready():
                error.extend(channel.recv_stderr(65536))
            require(len(output) <= 65536 and len(error) <= 65536, "preflight_output_bound")
            if not channel.recv_ready() and not channel.recv_stderr_ready():
                time.sleep(0.02)
        return channel.recv_exit_status(), bytes(output), bytes(error)
    finally:
        channel.close()


def prepare_existing(args, key, evidence):
    """Read-only checks only; never creates configs, key files or remote state."""
    gateway_path = Path(args.gateway).resolve()
    gateway = bounded_json(gateway_path)
    require(gateway["schema"] == "native-ssh-gateway/1"
            and gateway["host"] == "172.28.8.210" and gateway["port"] == 22
            and gateway["user"] == "awt1104094b2a0cea" and gateway["worker_id"] == "B",
            "approved_linux_b_identity_required")
    controller_path = (gateway_path.parent / gateway["controller_config"]).resolve()
    controller = bounded_json(controller_path)
    require(controller["schema"] == "ao-reason-config/1" and controller["mode"] == "real"
            and controller["role"] == "controller" and controller["deployment"] == gateway["deployment"],
            "controller_gateway_mismatch")
    state = (controller_path.parent / controller["state_dir"]).resolve()
    require(not state.exists(), "fresh_controller_state_required")
    package = Path(args.package).resolve()
    manifest = bounded_json(args.package_manifest)
    require(Path(manifest["package_dir"]).resolve() == package, "package_manifest_path_mismatch")
    package_facts = {}
    for label, relative in (("manager", "agentranch.exe"), ("worker_cli", "aw-cli.exe"),
                            ("helper", "tools/paramiko_gateway.py"), ("python", "runtime/python/python.exe")):
        identity = file_identity(package / relative)
        pinned = manifest["required_files"][label]
        require(identity == {"bytes": pinned["bytes"], "sha256": pinned["sha256"]}, "package_identity_mismatch")
        package_facts[label] = identity
    known_hosts = Path(gateway["known_hosts"]).resolve()
    require(known_hosts.is_file(), "existing_known_hosts_required")
    helper = load_module(Path(__file__).with_name("paramiko_gateway.py"), "closure_paramiko_helper")
    private_key = helper._load_private_key(bytes(key))
    import paramiko
    client = paramiko.SSHClient()
    try:
        client.load_host_keys(str(known_hosts))
        client.set_missing_host_key_policy(paramiko.RejectPolicy())
        client.connect(hostname=gateway["host"], port=gateway["port"], username=gateway["user"],
                       pkey=private_key, allow_agent=False, look_for_keys=False,
                       timeout=15, auth_timeout=15, banner_timeout=15)
        with client.open_sftp() as sftp:
            with sftp.open(gateway["remote_config"], "rb") as stream:
                raw = stream.read(65537)
            require(len(raw) <= 65536, "remote_config_oversized")
            worker = json.loads(raw)
        require(worker["schema"] == "native-worker-config/1"
                and worker["deployment"] == gateway["deployment"] and worker["worker_id"] == "B"
                and worker["artifact_sha256"] == gateway["artifact_sha256"], "remote_worker_config_mismatch")
        for name in ("remote_worker", "remote_config", "remote_library_path"):
            require(re.fullmatch(r"/[A-Za-z0-9/_.-]+", gateway[name]) is not None
                    and all(part not in (".", "..", "") for part in gateway[name].split("/")[1:]),
                    "unsafe_remote_path")
        cgroup = worker["cgroup_root"]
        require(cgroup.startswith("/sys/fs/cgroup/") and re.fullmatch(r"/[A-Za-z0-9/_.@-]+", cgroup)
                and all(part not in (".", "..", "") for part in cgroup.split("/")[1:]), "unsafe_cgroup_root")
        worker_path = shlex.quote(gateway["remote_worker"])
        library_path = shlex.quote(gateway["remote_library_path"])
        library_file = shlex.quote(gateway["remote_library_path"] + "/libsqlite3.so.0")
        require(re.fullmatch(r"[0-9a-f]{64}", gateway["artifact_sha256"])
                and re.fullmatch(r"[0-9a-f]{64}", gateway["remote_library_sha256"]), "invalid_artifact_digest")
        worker_size, library_size = int(gateway["artifact_bytes"]), int(gateway["remote_library_bytes"])
        require(worker_size > 0 and library_size > 0, "invalid_artifact_size")
        command = (
            "set -eu; test \"$(id -u)\" = 1003; test \"$(id -g)\" = 1004; "
            "test \"$(id -un)\" = awt1104094b2a0cea; "
            f"test \"$(sha256sum {worker_path} | cut -d' ' -f1)\" = {gateway['artifact_sha256']}; "
            f"test \"$(wc -c < {worker_path})\" -eq {worker_size}; "
            f"test \"$(sha256sum {library_file} | cut -d' ' -f1)\" = {gateway['remote_library_sha256']}; "
            f"test \"$(wc -c < {library_file})\" -eq {library_size}; "
            f"test -d {shlex.quote(cgroup)}; test -r {shlex.quote(cgroup + '/cgroup.controllers')}; "
            f"test -w {shlex.quote(cgroup + '/cgroup.procs')}; "
            f"LD_LIBRARY_PATH={library_path} /lib64/ld-linux-x86-64.so.2 --list {worker_path}; "
            f"LD_LIBRARY_PATH={library_path} {worker_path} --identity; "
            f"LD_LIBRARY_PATH={library_path} {worker_path} --help"
        )
        stdin, stdout, stderr = client.exec_command(command, timeout=30, get_pty=False)
        stdin.close()
        code, output, error = read_channel(stdout.channel)
        require(code == 0, "remote_readonly_preflight_failed")
        require(b'"binary":"aw-cli"' in output and b"owner-exec --config FILE" in output,
                "remote_cli_identity_missing")
        transport = client.get_transport()
        host_key = transport.get_remote_server_key()
        write_json_new(evidence / "preflight.json", {
            "schema": "manager-worker-readonly-preflight/1", "status": "PASS_READONLY_PREFLIGHT",
            "host": gateway["host"], "user": gateway["user"], "uid": 1003, "gid": 1004,
            "known_hosts": file_identity(known_hosts), "host_key_type": host_key.get_name(),
            "host_key_sha256": hashlib.sha256(host_key.asbytes()).hexdigest(), "package": package_facts,
            "source_sha256": manifest["source_sha256"], "artifact_sha256": gateway["artifact_sha256"],
            "loader_sha256": gateway["remote_library_sha256"], "cgroup_root": cgroup,
            "remote_mutation": False, "cgroup_child_migration": "NOT_TESTED",
            "native_os_wait_completed": False, "wait_kind": "PARAMIKO_CHANNEL_ONLY",
            "stdout_sha256": hashlib.sha256(output).hexdigest(),
            "stderr_sha256": hashlib.sha256(error).hexdigest(),
        })
    finally:
        client.close()
    return {"gateway_path": gateway_path, "gateway": gateway, "database": state / "controller.sqlite",
            "package": package, "evidence": evidence}


def _require_owned_window_ready(ui, hwnd, pid):
    require(ui.owned_pid(hwnd) == pid, "owned_window_pid_mismatch")
    require(ui.user.IsWindowVisible(hwnd), "owned_window_not_visible")
    rect = ui.RECT()
    require(ui.user.GetWindowRect(hwnd, ui.ctypes.byref(rect)), "window_rect_unavailable")
    require(rect.right - rect.left >= 720 and rect.bottom - rect.top >= 540,
            "owned_window_geometry_changed")
    require(ui.window_title(hwnd) == "agentranch 工作台", "owned_window_title_changed")


def wait_for_owned_foreground(ui, hwnd, pid, timeout=15, poll_interval=0.1):
    """Require a valid owned window to be the actual foreground before input."""
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        _require_owned_window_ready(ui, hwnd, pid)
        if int(ui.user.GetForegroundWindow() or 0) == int(hwnd):
            return True
        ui.user.SetForegroundWindow(hwnd)
        time.sleep(poll_interval)
    raise ClosureCheckError("HUMAN_FOREGROUND_REQUIRED")


def find_manager_window(ui, pid, timeout=15):
    """Skip transient same-PID helper windows; select the visible titled GUI."""
    callback_type = ui.ctypes.WINFUNCTYPE(ui.wintypes.BOOL, ui.wintypes.HWND, ui.wintypes.LPARAM)
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        candidates = []
        @callback_type
        def callback(hwnd, unused):
            if ui.user.IsWindowVisible(hwnd) and ui.owned_pid(hwnd) == pid:
                rect = ui.RECT()
                if ui.user.GetWindowRect(hwnd, ui.ctypes.byref(rect)):
                    width, height = rect.right - rect.left, rect.bottom - rect.top
                    if width >= 720 and height >= 540 and ui.window_title(hwnd) == "agentranch 工作台":
                        candidates.append((width * height, int(hwnd)))
            return True
        ui.user.EnumWindows(callback, 0)
        if candidates:
            return max(candidates)[1]
        time.sleep(0.1)
    raise ClosureCheckError("manager_main_window_not_found")


@contextlib.contextmanager
def manager_session(key, prepared, evidence):
    """Owned HWND only, with stdin EOF before waiting for the actual window."""
    require(os.name == "nt", "windows_manager_required")
    ui_path = Path(__file__).resolve().parents[1] / ".test-scripts/native-manager/config_only_direct_capture.py"
    ui = load_module(ui_path, "closure_owned_window")
    ui.user.SetProcessDpiAwarenessContext(ui.ctypes.c_void_p(-4))
    package = prepared["package"]
    env = os.environ.copy()
    env["PATH"] = str(package) + os.pathsep + env.get("PATH", "")
    process, hwnd = None, None
    with (evidence / "manager.stdout.log").open("xb") as output, (evidence / "manager.stderr.log").open("xb") as error:
        try:
            process = subprocess.Popen([str(package / "agentranch.exe"), "--real",
                "--gateway-key-stdin", "--state-dir", str(evidence / "manager-state")],
                cwd=str(package), env=env, stdin=subprocess.PIPE, stdout=output, stderr=error)
            process.stdin.write(key)
            process.stdin.close()
            hwnd = find_manager_window(ui, process.pid)
            initial = ui.capture(hwnd, process.pid, evidence / "window-initial.png")
            write_json_new(evidence / "manager-window.json", {
                "pid": process.pid, "hwnd": hwnd, "initial": initial,
                "visual_review": "REQUIRED_BEFORE_COORDINATE_ACTIONS",
                "gateway_path": str(prepared["gateway_path"]),
                "control": "Use action subcommand after inspecting each screenshot. No automatic dispatch.",
            })
            (evidence / "controls").mkdir(exist_ok=False)
            yield {**prepared, "process": process, "hwnd": hwnd, "ui": ui}
        finally:
            if process is not None:
                if process.poll() is None and hwnd is not None and ui.owned_pid(hwnd) == process.pid:
                    ui.user.PostMessageW(hwnd, ui.WM_CLOSE, 0, 0)
                forced = False
                try:
                    code = process.wait(timeout=15)
                except subprocess.TimeoutExpired:
                    process.terminate()
                    forced = True
                    code = process.wait(timeout=10)
                write_json_new(evidence / "manager-exit.json", {
                    "pid": process.pid, "exit_code": code, "forced_owned_child_terminate": forced,
                    "windows_manager_wait_completed": True,
                    "linux_owner_wait": "NOT_INFERRED_FROM_WINDOWS_PROCESS_EXIT"})
                require(not forced and code == 0, "manager_shutdown_failed")


def drive_manager(session, control_timeout=600):
    """A control queue lets this session inspect screenshots before each input.

    A dispatch action is unique: it sends one click then returns, allowing the
    same live credential lease to remain active through Store acceptance.
    """
    evidence, ui = session["evidence"], session["ui"]
    process, hwnd = session["process"], session["hwnd"]
    deadline = time.monotonic() + control_timeout
    handled = set()
    while time.monotonic() < deadline:
        require(process.poll() is None, "manager_exited_before_dispatch")
        for path in sorted((evidence / "controls").glob("command-*.json")):
            if path.name in handled:
                continue
            command = bounded_json(path)
            action = command["action"]
            require(action in ("click", "type-gateway", "capture", "dispatch", "abort"), "unsupported_gui_action")
            require(action != "abort", "operator_aborted")
            if action in ("click", "dispatch"):
                x, y = command["x"], command["y"]
                rect = ui.RECT()
                require(ui.user.GetWindowRect(hwnd, ui.ctypes.byref(rect)), "window_rect_unavailable")
                require(type(x) is int and type(y) is int and 0 <= x < rect.right - rect.left
                        and 0 <= y < rect.bottom - rect.top, "click_outside_owned_window")
                if action == "dispatch":
                    require(session["database"].is_file(), "configure_gateway_before_dispatch")
                    with contextlib.closing(sqlite3.connect(session["database"].as_uri() + "?mode=ro", uri=True)) as db:
                        require(db.execute("SELECT count(*) FROM runs").fetchone()[0] == 0,
                                "fresh_store_required_before_dispatch")
                wait_for_owned_foreground(ui, hwnd, process.pid)
                ui.click(hwnd, process.pid, x, y)
            elif action == "type-gateway":
                wait_for_owned_foreground(ui, hwnd, process.pid)
                ui.hotkey_ctrl_a(hwnd, process.pid)
                ui.unicode_type(hwnd, process.pid, str(session["gateway_path"]))
            image = ui.capture(hwnd, process.pid, evidence / (path.stem + ".png"))
            write_json_new(evidence / (path.stem + "-result.json"), {"action": action, "screenshot": image})
            handled.add(path.name)
            if action == "dispatch":
                return
        time.sleep(0.2)
    raise ClosureCheckError("gui_control_deadline_exceeded")


def wait_for_admission(session, timeout=60):
    deadline = time.monotonic() + timeout
    facts = {"status": "NOT_ADMITTED", "reason": "controller_database_missing"}
    while time.monotonic() < deadline:
        if session["database"].is_file():
            gateway = session["gateway"]
            facts = inspect_store(session["database"], gateway["deployment"], gateway["worker_id"], gateway["artifact_sha256"])
            if facts["status"] == "PASS_STORE_ADMISSION":
                break
        require(session["process"].poll() is None, "manager_exited_before_admission")
        time.sleep(0.2)
    session["ui"].capture(session["hwnd"], session["process"].pid, session["evidence"] / "window-terminal.png")
    write_json_new(session["evidence"] / "store-acceptance.json", facts)
    return facts


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="One memory-only key lifetime; no provisioning or key generation")
    run.add_argument("--evidence-dir", required=True)
    run.add_argument("--gateway", required=True)
    run.add_argument("--package", required=True)
    run.add_argument("--package-manifest", default=str(Path(__file__).resolve().parents[1] / "dist/live-manager-package-20261110-01/result.json"))
    run.add_argument("--key-stdin", action="store_true", required=True)
    action = commands.add_parser("action", help="Control only the owned Manager window after visual review")
    action.add_argument("--evidence-dir", required=True)
    action.add_argument("--action", required=True, choices=("click", "type-gateway", "capture", "dispatch", "abort"))
    action.add_argument("--x", type=int)
    action.add_argument("--y", type=int)
    args = parser.parse_args(argv)
    try:
        if args.command == "action":
            control = Path(args.evidence_dir).resolve() / "controls"
            require(control.is_dir(), "live_manager_controls_missing")
            index = len(list(control.glob("command-*.json"))) + 1
            write_json_new(control / f"command-{index:04d}.json", {"action": args.action, "x": args.x, "y": args.y})
            print(json.dumps({"status": "GUI_COMMAND_QUEUED", "sequence": index}))
            return 0
        result = run_closure(Path(args.evidence_dir).resolve(),
            lambda: stdin_key_lease(sys.stdin.buffer),
            lambda key, evidence: prepare_existing(args, key, evidence),
            manager_session, drive_manager, wait_for_admission)
        print(json.dumps(result, ensure_ascii=True))
        return 0 if result["status"] == "PASS_MANAGER_WORKER_CLOSURE" else 2
    except Exception as exc:
        print(json.dumps({"status": "BLOCKED_OR_FAILED", "error_type": type(exc).__name__}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
