"""Run ONE project-work-v1 Manager dispatch within one credential lifetime.

Extends live_manager_closure.py for the project-work flow:
- stdin credential is the framed AW-GATEWAY-SECRETS/1 (key + model token),
  matching read_gateway_secrets; both stay in memory only.
- gateway identity pinned to the .221 ubuntu ordinary user stage-13 facts.
- GUI flow: type gateway config path, type design.md path, click the
  project-work dispatch button (one business dispatch only).
- Store inspector checks the native-project/1 evidence chain, the artifact
  package digest, and that no token bytes appear in any persisted record.
Never provisions credentials, writes secrets to disk, or puts them in argv/env.
"""
from __future__ import annotations

import argparse
import contextlib
import hashlib
import importlib.util
import json
import os
from pathlib import Path, PurePosixPath
import re
import sqlite3
import subprocess
import sys
import time

sys.path.insert(0, str(Path(__file__).resolve().parent))
from live_manager_closure import (ClosureCheckError, load_module,
                                  read_channel, require, write_json_new,
                                  _require_owned_window_ready, find_manager_window)

ALT_VK = 0x12  # VK_MENU
KEYEVENTF_KEYUP = 0x0002


def wait_for_owned_foreground_robust(ui, hwnd, pid, timeout=15, poll_interval=0.1):
    """Foreground guard that survives an idle review gap.

    Same ownership invariants as live_manager_closure's guard (owned pid,
    visible, >=720x540, exact title, and foreground == hwnd before any
    input is sent). The only addition: when the OS denies the raise because
    this driver process lost foreground-eligibility during the gap (the
    2026-11-12 13:32 HUMAN_FOREGROUND_REQUIRED incident), restore
    eligibility the documented way — one synthetic ALT tap via SendInput
    makes the calling process the last input recipient — then re-issue
    SetForegroundWindow. Every input still requires GetForegroundWindow()
    == hwnd afterwards; nothing is bypassed.
    """
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        _require_owned_window_ready(ui, hwnd, pid)
        if int(ui.user.GetForegroundWindow() or 0) == int(hwnd):
            return True
        extra = ui.ctypes.pointer(ui.ctypes.c_ulong(0))
        try:
            ui.send_inputs([
                ui.INPUT(type=1, ki=ui.KEYBDINPUT(ALT_VK, 0, 0, 0, extra)),
                ui.INPUT(type=1, ki=ui.KEYBDINPUT(ALT_VK, 0, KEYEVENTF_KEYUP, 0, extra)),
            ])
        except AttributeError:
            pass  # stub without INPUT structures: raise-only fallback below
        ui.user.SetForegroundWindow(hwnd)
        time.sleep(poll_interval)
    raise ClosureCheckError("HUMAN_FOREGROUND_REQUIRED")


GATEWAY_MAGIC = b"AW-GATEWAY-SECRETS/1\n"
PINNED_HOST = "172.28.8.221"
PINNED_USER = "ubuntu"
PINNED_UID = 1000
PINNED_GID = 1000


@contextlib.contextmanager
def stdin_secrets_lease(stream):
    """Read the framed secrets (key + model token) once, zero them after."""
    raw = bytearray(stream.read(65537))
    try:
        require(raw.startswith(GATEWAY_MAGIC), "framed_gateway_secrets_required")
        offset = len(GATEWAY_MAGIC)

        def read_field(offset):
            length = int.from_bytes(raw[offset:offset + 4], "big")
            require(length > 0, "empty_secret_field")
            start, end = offset + 4, offset + 4 + length
            field = bytes(raw[start:end])
            require(len(field) == length, "truncated_secret_field")
            return field, end

        key, offset = read_field(offset)
        token, offset = read_field(offset)
        require(offset == len(raw), "trailing_bytes_in_secrets_frame")
        require(0 < len(key) <= 65536, "gateway_key_bound")
        require(0 < len(token) <= 16384, "model_token_bound")
        yield key, token
    finally:
        raw[:] = b"\x00" * len(raw)


def bounded_json(path):
    with Path(path).open("rb") as stream:
        raw = stream.read(65537)
    require(len(raw) <= 65536, "configuration_oversized")
    return json.loads(raw)


def manifest_json(path):
    """Package manifests list every bundled file (thousands of entries); the
    driver re-hashes each pinned file itself, so a wider read bound is safe."""
    with Path(path).open("rb") as stream:
        raw = stream.read(1 << 24)
    require(len(raw) <= (1 << 24) - 1, "package_manifest_oversized")
    return json.loads(raw)


def file_identity(path):
    with Path(path).open("rb") as stream:
        return {"bytes": Path(path).stat().st_size,
                "sha256": hashlib.file_digest(stream, "sha256").hexdigest()}


def prepare_existing(args, key, evidence):
    """Read-only checks against the staged .221 deployment and package."""
    evidence.mkdir(parents=True, exist_ok=True)  # open("x") below needs the parent
    gateway_path = Path(args.gateway).resolve()
    gateway = bounded_json(gateway_path)
    require(gateway["schema"] == "native-ssh-gateway/1"
            and gateway["host"] == PINNED_HOST and gateway["port"] == 22
            and gateway["user"] == PINNED_USER and gateway["worker_id"] == "B",
            "pinned_linux221_identity_required")
    controller_path = Path(gateway["controller_config"]).resolve()
    controller = bounded_json(controller_path)
    require(controller["schema"] == "ao-reason-config/1" and controller["mode"] == "real"
            and controller["role"] == "controller" and controller["deployment"] == gateway["deployment"],
            "controller_gateway_mismatch")
    state = Path(controller["state_dir"]).resolve()
    require(not state.exists(), "fresh_controller_state_required")
    package = Path(args.package).resolve()
    manifest = manifest_json(args.package_manifest)
    if "package_dir" in manifest:
        require(Path(manifest["package_dir"]).resolve() == package, "package_manifest_path_mismatch")
    pinned_files = manifest.get("required_files") or manifest.get("files") or {}
    package_facts = {}
    for label, relative in (("manager", "agentranch.exe"), ("worker_cli", "aw-cli.exe"),
                            ("helper", "tools/paramiko_gateway.py"), ("python", "runtime/python/python.exe")):
        identity = file_identity(package / relative)
        pinned = pinned_files[label] if label in pinned_files else pinned_files[relative]
        require(identity == {"bytes": pinned["bytes"], "sha256": pinned["sha256"]}, "package_identity_mismatch")
        package_facts[label] = identity
    known_hosts = Path(gateway["known_hosts"]).resolve()
    require(known_hosts.is_file(), "existing_known_hosts_required")
    helper = load_module(Path(__file__).with_name("paramiko_gateway.py"), "project_closure_helper")
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
                raw_cfg = stream.read(65537)
            require(len(raw_cfg) <= 65536, "remote_config_oversized")
            worker = json.loads(raw_cfg)
            endpoint_path = str(PurePosixPath(gateway["remote_worker"]).parent / "model-endpoint.json")
            with sftp.open(endpoint_path, "rb") as stream:
                raw_endpoint = stream.read(65537)
            endpoint = json.loads(raw_endpoint)
        require(worker["schema"] == "native-worker-config/1"
                and worker["deployment"] == gateway["deployment"] and worker["worker_id"] == "B"
                and worker["artifact_sha256"] == gateway["artifact_sha256"],
                "remote_worker_config_mismatch")
        require(endpoint["schema"] == "model-endpoint/1"
                and endpoint["base_url"].startswith("https://")
                and endpoint["certificate_pem"].startswith("-----BEGIN CERTIFICATE-----"),
                "remote_model_endpoint_config_mismatch")
        import ssl
        import socket
        host_port = re.match(r"https://([^/:]+):(\d+)", endpoint["base_url"])
        require(host_port is not None, "endpoint_url_unparseable")
        ctx = ssl.create_default_context()
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        with socket.create_connection((host_port.group(1), int(host_port.group(2))), timeout=15) as sock:
            with ctx.wrap_socket(sock) as tls:
                der = tls.getpeercert(binary_form=True)
        require(hashlib.sha256(der).hexdigest().lower()
                == hashlib.sha256(ssl.PEM_cert_to_DER_cert(endpoint["certificate_pem"])).hexdigest().lower(),
                "pinned_certificate_changed")
        for name in ("remote_worker", "remote_config", "remote_library_path"):
            require(re.fullmatch(r"/[A-Za-z0-9/_.-]+", gateway[name]) is not None
                    and all(part not in (".", "..", "") for part in gateway[name].split("/")[1:]),
                    "unsafe_remote_path")
        worker_path = re.sub(r"[^A-Za-z0-9/_.-]", "", gateway["remote_worker"])
        library_path = gateway["remote_library_path"]
        library_file = library_path + "/libsqlite3.so.0"
        command = (
            'set -eu; test "$(id -u)" = 1000; test "$(id -g)" = 1000; test "$(id -un)" = ubuntu; '
            f'test "$(sha256sum {worker_path} | cut -d\' \' -f1)" = "{gateway["artifact_sha256"]}"; '
            f'test "$(wc -c < {worker_path})" -eq {int(gateway["artifact_bytes"])}; '
            f'test "$(sha256sum {library_file} | cut -d\' \' -f1)" = "{gateway["remote_library_sha256"]}"; '
            f"LD_LIBRARY_PATH={library_path} /lib64/ld-linux-x86-64.so.2 --list {worker_path} >/dev/null; "
            f"LD_LIBRARY_PATH={library_path} {worker_path} --identity"
        )
        stdin, stdout, stderr = client.exec_command(command, timeout=30, get_pty=False)
        stdin.close()
        code, output, error = read_channel(stdout.channel)
        require(code == 0, "remote_readonly_preflight_failed")
        require(b'"binary":"aw-cli"' in output, "remote_cli_identity_missing")
        transport = client.get_transport()
        host_key = transport.get_remote_server_key()
        write_json_new(evidence / "preflight.json", {
            "schema": "manager-worker-readonly-preflight/1", "status": "PASS_READONLY_PREFLIGHT",
            "host": gateway["host"], "user": gateway["user"], "uid": PINNED_UID, "gid": PINNED_GID,
            "known_hosts": file_identity(known_hosts), "host_key_type": host_key.get_name(),
            "host_key_sha256": hashlib.sha256(host_key.asbytes()).hexdigest(), "package": package_facts,
            "source_sha256": manifest["source_sha256"], "artifact_sha256": gateway["artifact_sha256"],
            "model_endpoint_certificate_pinned": True,
            "remote_mutation": False, "wait_kind": "PARAMIKO_CHANNEL_ONLY",
            "stdout_sha256": hashlib.sha256(output).hexdigest(),
            "stderr_sha256": hashlib.sha256(error).hexdigest(),
        })
    finally:
        client.close()
    doc = Path(args.document).resolve()
    require(doc.is_file(), "design_document_missing")
    facts = file_identity(doc)
    write_json_new(evidence / "document.json", {
        "schema": "confirmed-design-document/1", "path": str(doc), **facts})
    return {"gateway_path": gateway_path, "gateway": gateway, "database": state / "controller.sqlite",
            "package": package, "evidence": evidence, "document": doc, "document_sha256": facts["sha256"]}


@contextlib.contextmanager
def manager_session(secrets, prepared, evidence):
    """Owned HWND GUI with the framed secrets on stdin; EOF before window wait."""
    require(os.name == "nt", "windows_manager_required")
    _, token = secrets
    ui_path = Path(__file__).resolve().parents[1] / ".test-scripts/native-manager/config_only_direct_capture.py"
    ui = load_module(ui_path, "project_closure_owned_window")
    ui.user.SetProcessDpiAwarenessContext(ui.ctypes.c_void_p(-4))
    package = prepared["package"]
    env = os.environ.copy()
    env["PATH"] = str(package) + os.pathsep + env.get("PATH", "")
    frame = GATEWAY_MAGIC + len(secrets[0]).to_bytes(4, "big") + secrets[0] \
        + len(token).to_bytes(4, "big") + token
    process, hwnd = None, None
    with (evidence / "manager.stdout.log").open("xb") as output, (evidence / "manager.stderr.log").open("xb") as error:
        try:
            process = subprocess.Popen([str(package / "agentranch.exe"), "--real",
                "--gateway-key-stdin", "--state-dir", str(evidence / "manager-state")],
                cwd=str(package), env=env, stdin=subprocess.PIPE, stdout=output, stderr=error)
            process.stdin.write(frame)
            process.stdin.close()
            hwnd = find_manager_window(ui, process.pid)
            initial = ui.capture(hwnd, process.pid, evidence / "window-initial.png")
            write_json_new(evidence / "manager-window.json", {
                "pid": process.pid, "hwnd": hwnd, "initial": initial,
                "visual_review": "REQUIRED_BEFORE_COORDINATE_ACTIONS",
                "gateway_path": str(prepared["gateway_path"]),
                "document_path": str(prepared["document"]),
                "control": "Use action subcommand after inspecting each screenshot. One dispatch only.",
            })
            (evidence / "controls").mkdir(exist_ok=False)
            yield {**prepared, "process": process, "hwnd": hwnd, "ui": ui, "token": token}
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


def drive_manager(session, control_timeout=900):
    """Sequential queue: type gateway config, type design doc, dispatch once."""
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
            require(action in ("click", "type-gateway", "type-document", "capture", "dispatch", "abort"),
                    "unsupported_gui_action")
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
                wait_for_owned_foreground_robust(ui, hwnd, process.pid)
                ui.click(hwnd, process.pid, x, y)
            elif action in ("type-gateway", "type-document"):
                wait_for_owned_foreground_robust(ui, hwnd, process.pid)
                ui.hotkey_ctrl_a(hwnd, process.pid)
                ui.unicode_type(hwnd, process.pid,
                                str(session["gateway_path"] if action == "type-gateway" else session["document"]))
            image = ui.capture(hwnd, process.pid, evidence / (path.stem + ".png"))
            write_json_new(evidence / (path.stem + "-result.json"), {"action": action, "screenshot": image})
            handled.add(path.name)
            if action == "dispatch":
                return
        time.sleep(0.2)
    raise ClosureCheckError("gui_control_deadline_exceeded")


def inspect_manager_diag(state_root, run_id):
    """Gate: the append_diag fix must show all three events for this run.

    Real-mode managers never created --state-dir at startup, so the first
    diag lines (gateway_configured, dispatch_result) were silently dropped
    before the fix (sessions -41/-43 logged only artifact_materialized;
    session -44 with the fix logged all three). This check turns that
    regression class into a harness failure instead of silent line loss.

    Line format written by append_diag: "{ts}\\t{event}\\t{detail}\\n".
    """
    facts = {"status": "NOT_CONFIRMED", "reason": "manager_diag_missing"}
    path = Path(state_root) / "manager-diag.log"
    if not path.is_file():
        return facts
    events = set()
    try:
        for line in path.read_text(encoding="utf-8", errors="replace").splitlines():
            parts = line.split("\t")
            if len(parts) != 3:
                continue
            _ts, event, detail = parts
            if event == "gateway_configured":
                events.add("gateway_configured")
            elif event in ("dispatch_result", "artifact_materialized"):
                if f"run_id={run_id}" in detail:
                    events.add(event)
            elif event == "artifact_error":
                if f"run_id={run_id}" in detail:
                    facts["reason"] = f"manager_diag_artifact_error:{detail}"
    except OSError as exc:
        facts["reason"] = f"manager_diag_unreadable:{type(exc).__name__}"
        return facts
    for required in ("gateway_configured", "dispatch_result", "artifact_materialized"):
        if required not in events:
            facts["reason"] = f"manager_diag_event_missing:{required}"
            return facts
    facts.pop("reason", None)
    facts.update(status="PASS_MANAGER_DIAG_SURVIVAL", events=sorted(events))
    return facts


def scan_state_for_token(root, token):
    """No token bytes in any file under the manager state directory."""
    hits = []
    for path in Path(root).rglob("*"):
        if path.is_file() and not path.is_symlink():
            try:
                if token in path.read_bytes():
                    hits.append(str(path))
            except OSError:
                hits.append(f"unreadable:{path}")
    return hits


def inspect_project_store(database, deployment, worker_id, artifact_sha256, document_sha256, state_root, token):
    """Check the project-work-v1 evidence chain and the artifact materialization."""
    database = Path(database).resolve()
    if not database.is_file():
        return {"status": "NOT_ADMITTED", "reason": "controller_database_missing"}
    facts = {"status": "NOT_ADMITTED", "used_slots": None}
    connection = sqlite3.connect(database.as_uri() + "?mode=ro", uri=True, timeout=3)
    connection.row_factory = sqlite3.Row
    try:
        connection.execute("PRAGMA query_only=ON")
        connection.execute("BEGIN")
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
        facts.update(run_id=run["id"], template=run.get("template"), attempt=run["attempt"])
        require(run.get("template") == "project-work-v1", "project_run_template_mismatch")
        require(row["status"] == run["status"] == "done", "run_not_done")
        require(bool(run["session"]) and run["detail"].get("owner_admission") == "admitted", "owner_not_admitted")
        assignments = connection.execute("SELECT * FROM assignments").fetchall()
        require(len(assignments) == 1, "expected_exactly_one_assignment")
        assignment = assignments[0]
        require(assignment["run"] == run["id"] and assignment["attempt"] == run["attempt"]
                and assignment["node"] == worker_id and assignment["released"] == 1
                and facts["used_slots"] == 0, "assignment_not_released")
        report = run["detail"].get("report") or {}
        require(report.get("schema") == "native-project/1", "project_report_schema_mismatch")
        require(report.get("session") == run["session"], "project_report_session_mismatch")
        require(report.get("document_sha256") == document_sha256, "document_binding_mismatch")
        model = report.get("model", {})
        require(bool(model.get("model")) and bool(model.get("request_sha256"))
                and bool(model.get("response_sha256")) and int(model.get("elapsed_ms", -1)) >= 0
                and model.get("status") == 200, "model_call_evidence_missing")
        facts["model_evidence"] = {k: model[k] for k in ("model", "request_sha256", "response_sha256", "elapsed_ms", "status")}
        artifact = run["detail"].get("artifact") or report.get("artifact")
        require(isinstance(artifact, dict) and artifact.get("schema") == "project-artifact-package/1",
                "artifact_package_missing")
        require(artifact.get("document_sha256") == document_sha256, "package_document_binding_mismatch")
        files = artifact.get("files") or []
        require(len(files) >= 1, "artifact_files_empty")
        artifact_dir = Path(run["detail"].get("artifact_dir", "")) if run["detail"].get("artifact_dir") else None
        require(artifact_dir is not None and artifact_dir.is_dir(), "artifact_dir_missing")
        seen = set()
        for item in files:
            name = item.get("path", "")
            require(re.fullmatch(r"[A-Za-z0-9._/-]+", name) and ".." not in name and not name.startswith("/"),
                    "artifact_path_unsafe")
            require(name not in seen, "artifact_path_duplicate")
            seen.add(name)
            target = (artifact_dir / name).resolve()
            require(str(target).startswith(str(artifact_dir.resolve()) + os.sep), "artifact_escape")
            raw = target.read_bytes()
            require(len(raw) == item.get("size"), "artifact_size_mismatch")
            require(hashlib.sha256(raw).hexdigest() == item.get("sha256"), "artifact_digest_mismatch")
            require("content_hex" in item and bytes.fromhex(item["content_hex"]) == raw,
                    "artifact_content_hex_mismatch")
        require("index.html" in seen, "artifact_index_missing")
        html = (artifact_dir / "index.html").read_text(encoding="utf-8", errors="strict")
        # design.md defines the card selectors as runtime contract entries
        # ("结果：[data-results-list]、[data-card][data-id=...]"): data-card
        # is created by the renderer at runtime (article.dataset.card in the
        # referenced js), not present in the static index.html text. Mount
        # markers must sit in index.html itself; renderer markers may be
        # satisfied by any js file the page references. Static-text check
        # only (run ea3aed16, session -41): no browser is dragged into the
        # acceptance harness.
        js_text = "\n".join((artifact_dir / n).read_text(encoding="utf-8", errors="strict")
                            for n in seen if n.endswith(".js"))
        static_markers = ("data-search-input", "data-category-list", "data-results-list")
        renderer_markers = ("data-card",)
        for marker in static_markers:
            require(marker in html, f"stable_selector_missing:{marker}")
        for marker in renderer_markers:
            # The renderer may write the attribute either as an HTML string
            # ("data-card") or via the DOM API (dataset.card, camelCase) —
            # both produce the queryable [data-card] at runtime.
            parts = marker.split("-")[1:]
            dom_api = "dataset." + parts[0] + "".join(p.capitalize() for p in parts[1:])
            require(marker in html or marker in js_text or dom_api in js_text,
                    f"stable_selector_missing:{marker}")
        facts["artifact_dir"] = str(artifact_dir)
        facts["artifact_files"] = sorted(seen)
        # Token secrecy: no token bytes anywhere in the persisted manager state.
        hits = scan_state_for_token(state_root, token)
        require(not hits, "token_bytes_found_in_state:" + ",".join(hits[:5]))
        facts["token_absent_from_state"] = True
        facts.update(status="PASS_PROJECT_STORE_ADMISSION", artifact_sha256=artifact_sha256,
                     assignment_released=True, document_sha256=document_sha256)
    except ClosureCheckError as exc:
        facts["reason"] = str(exc)
    except (KeyError, TypeError, ValueError, sqlite3.Error, OSError) as exc:
        facts["reason"] = f"store_incomplete_or_malformed:{type(exc).__name__}"
    finally:
        connection.close()
    return facts


def wait_for_admission(session, timeout=480):
    deadline = time.monotonic() + timeout
    facts = {"status": "NOT_ADMITTED", "reason": "controller_database_missing"}
    while time.monotonic() < deadline:
        if session["database"].is_file():
            gateway = session["gateway"]
            facts = inspect_project_store(session["database"], gateway["deployment"], gateway["worker_id"],
                                          gateway["artifact_sha256"], session["document_sha256"],
                                          session["evidence"] / "manager-state", session["token"])
            if facts["status"] == "PASS_PROJECT_STORE_ADMISSION":
                break
        require(session["process"].poll() is None, "manager_exited_before_admission")
        time.sleep(1.0)
    session["ui"].capture(session["hwnd"], session["process"].pid, session["evidence"] / "window-terminal.png")
    write_json_new(session["evidence"] / "store-acceptance.json", facts)
    return facts


def run_project_closure(evidence, secrets, prepare, manager, dispatch, verify):
    result = {"schema": "manager-project-closure/1", "status": "BLOCKED_OR_FAILED",
              "dispatch": "NOT_RUN", "store": "NOT_RUN", "credential_cleanup": "NOT_CONFIRMED"}
    stage = "credential_acquire"
    try:
        with secrets() as (key, token):
            stage = "remote_prepare"
            prepared = prepare((key, token), evidence) if prepare.__code__.co_argcount == 2 else prepare(args_ref[0], (key, token), evidence)
            stage = "manager_open"
            with manager((key, token), prepared, evidence) as session:
                stage = "manager_dispatch"
                dispatch(session)
                result["dispatch"] = "GUI_INPUT_SENT"
                stage = "store_acceptance"
                result["store"] = verify(session)
                facts = result["store"]
                require(isinstance(facts, dict)
                        and facts.get("status") == "PASS_PROJECT_STORE_ADMISSION"
                        and bool(facts.get("run_id"))
                        and facts.get("used_slots") == 0,
                        "store_admission_missing")
                stage = "manager_diag"
                result["manager_diag"] = inspect_manager_diag(
                    session["evidence"] / "manager-state", facts["run_id"])
                require(result["manager_diag"]["status"] == "PASS_MANAGER_DIAG_SURVIVAL",
                        "manager_diag_survival_missing")
                stage = "manager_close"
        result["credential_cleanup"] = "PASS"
        result["status"] = "PASS_MANAGER_PROJECT_CLOSURE"
    except Exception as exc:
        result["failed_stage"] = stage
        result["error_type"] = type(exc).__name__
    finally:
        write_json_new(evidence / "result.json", result)
    return result


args_ref = []


def main(argv=None):
    parser = argparse.ArgumentParser(description=__doc__)
    commands = parser.add_subparsers(dest="command", required=True)
    run = commands.add_parser("run", help="One memory-only credential lifetime; one project dispatch")
    run.add_argument("--evidence-dir", required=True)
    run.add_argument("--gateway", required=True)
    run.add_argument("--package", required=True)
    run.add_argument("--package-manifest", required=True)
    run.add_argument("--document", required=True)
    run.add_argument("--key-stdin", action="store_true", required=True)
    action = commands.add_parser("action", help="Control the owned Manager window after visual review")
    action.add_argument("--evidence-dir", required=True)
    action.add_argument("--action", required=True, choices=("click", "type-gateway", "type-document", "capture", "dispatch", "abort"))
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
        args_ref.append(args)
        evidence = Path(args.evidence_dir).resolve()
        result = run_project_closure(evidence,
            lambda: stdin_secrets_lease(sys.stdin.buffer),
            lambda secrets, ev: prepare_existing(args_ref[0], secrets[0], ev),
            manager_session,
            drive_manager,
            wait_for_admission)
        print(json.dumps(result, ensure_ascii=True))
        return 0 if result["status"] == "PASS_MANAGER_PROJECT_CLOSURE" else 2
    except Exception as exc:
        print(json.dumps({"status": "BLOCKED_OR_FAILED", "error_type": type(exc).__name__}), file=sys.stderr)
        return 2


if __name__ == "__main__":
    raise SystemExit(main())
