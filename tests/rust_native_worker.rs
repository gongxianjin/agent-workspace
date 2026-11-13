//! Local CLI boundary tests. These tests never execute Linux, SSH, AO or a model.
use std::{io::Write, process::{Command, Stdio}};
use serde_json::json;

#[test]
fn worker_agent_rejects_caller_supplied_report_without_starting_a_child() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("worker.json");
    std::fs::write(&path, serde_json::to_vec(&json!({
        "schema":"native-worker-config/1", "deployment":"native-test", "worker_id":"B",
        "key_file":"not-read.key", "cgroup_root":"C:\\not\\a\\cgroup",
        "artifact_sha256":"ab".repeat(32), "timeout_secs":30,
        "report":{"sum":6}, "passed":true
    })).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aw-cli"))
        .args(["owner-exec", "--config", path.to_str().unwrap()]).output().unwrap();
    assert!(!output.status.success());
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("unknown field `passed`") || error.contains("unknown field `report`"), "{error}");
    assert!(output.stdout.is_empty(), "unvalidated input emitted an owner event");
}

#[test]
fn fixed_sum_child_requires_gate_and_computes_its_own_report() {
    let mut child = Command::new(env!("CARGO_BIN_EXE_aw-cli"))
        .args(["owner-child", "sum-v1", "session-1"])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"G").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(output.status.success(), "{}", String::from_utf8_lossy(&output.stderr));
    let report: serde_json::Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(report, json!({"schema":"native-sum/1","session":"session-1","inputs":[1,2,3],"sum":6}));
    assert!(String::from_utf8_lossy(&output.stderr).contains("owner-child"));
}

fn worker_value() -> serde_json::Value {
    json!({"schema":"native-worker-config/1", "deployment":"native-test", "worker_id":"B",
        "key_file":"not-read.key", "cgroup_root":"/sys/fs/cgroup/user.slice",
        "artifact_sha256":"ab".repeat(32), "timeout_secs":30})
}

#[test]
fn current_unit_config_is_accepted_and_explicit_remains_default() {
    use ao_reason_workbench::core::native_worker::NativeWorkerConfig;
    let mut value = worker_value();
    let cfg: NativeWorkerConfig = serde_json::from_value(value.clone()).unwrap();
    assert_eq!(serde_json::to_value(cfg).unwrap()["cgroup_root_mode"], "explicit");
    value["cgroup_root_mode"] = json!("current-unit");
    let cfg = serde_json::from_value::<NativeWorkerConfig>(value);
    assert!(cfg.is_ok(), "current-unit is a supported mode: {cfg:?}");
    let cfg=cfg.unwrap();
    assert!(cfg.validate_owner_marker(None).is_err());
    assert!(cfg.validate_owner_marker(Some(std::ffi::OsStr::new("29"))).is_err());
    cfg.validate_owner_marker(Some(std::ffi::OsStr::new("30"))).unwrap();
    let mut unknown=worker_value();
    unknown["cgroup_root_mode"]=json!("auto");
    assert!(serde_json::from_value::<NativeWorkerConfig>(unknown).is_err());
}

#[test]
fn owner_exec_rejects_service_marker_with_explicit_mode_before_key_or_child() {
    let root = tempfile::tempdir().unwrap();
    let path = root.path().join("worker.json");
    let mut value = worker_value();
    value["cgroup_root"] = json!(root.path());
    std::fs::write(&path, serde_json::to_vec(&value).unwrap()).unwrap();
    let output = Command::new(env!("CARGO_BIN_EXE_aw-cli"))
        .args(["owner-exec", "--config", path.to_str().unwrap()])
        .env("AW_EXPECTED_OWNER_TIMEOUT_SECS", "30").output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(String::from_utf8_lossy(&output.stderr).contains("marker"), "{}", String::from_utf8_lossy(&output.stderr));
}

#[test]
fn fixed_sum_child_never_runs_on_closed_gate() {
    let output = Command::new(env!("CARGO_BIN_EXE_aw-cli"))
        .args(["owner-child", "sum-v1", "session-1"]).stdin(Stdio::null()).output().unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
}
