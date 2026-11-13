use ao_reason_workbench::{
    adapters::{AoPort, Observation, WorkRequest, local::LocalAo, process::ManagedChild},
    config::{Actor, NodeSpec},
    core::{runner::Runner, store::Store},
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

#[test]
fn managed_child_reports_numeric_exit_and_bounded_stdout_stderr() {
    let temp = tempfile::tempdir().unwrap();
    let artifact = temp.path().join("result.json");
    let worker = Path::new(env!("CARGO_BIN_EXE_aw-cli"));
    let mut child = ManagedChild::spawn(worker, "sum-native-v1", &artifact, "capturesession")
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let evidence = loop {
        if let Some(evidence) = child.evidence().unwrap() {
            break evidence;
        }
        assert!(Instant::now() < deadline, "fixture did not exit within bound");
        std::thread::sleep(Duration::from_millis(10));
    };

    assert_eq!(evidence.exit_code, Some(0));
    assert!(!evidence.stdout_truncated);
    assert!(!evidence.stderr_truncated);
    assert!(String::from_utf8_lossy(&evidence.stdout).contains("capturesession"));
    assert!(String::from_utf8_lossy(&evidence.stderr).contains("fixture-worker"));
}

fn request(session: &str) -> WorkRequest {
    WorkRequest {
        run_id: "run-evidence".into(), attempt: 1, epoch: "epoch-evidence".into(),
        template: "sum-native-v1".into(), session: session.into(), project_work: None,
    }
}

fn completed(ao: &mut LocalAo, session: &str) -> (Value, Value) {
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match ao.observe(session).unwrap() {
            Observation::ExitProof { report, verification, .. } => return (report, verification),
            Observation::Running => {},
            other => panic!("expected retained exit evidence, got {other:?}"),
        }
        assert!(Instant::now() < deadline, "fixture evidence timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn local_exit_evidence_is_repeatable_and_raw_files_survive_observer_drop() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("workers");
    let worker = Path::new(env!("CARGO_BIN_EXE_aw-cli"));
    let mut ao = LocalAo::new(worker, &state).unwrap();
    let req = request("repeatable-evidence");
    ao.create(&req).unwrap();
    let first = completed(&mut ao, &req.session);
    assert_eq!(first.1["child_exit_code"], 0);
    let second = completed(&mut ao, &req.session);
    assert_eq!(second, first, "repeated observe must retain the original exit/output");
    drop(ao);

    let dir = state.join(&req.session);
    let status: Value = serde_json::from_slice(&std::fs::read(dir.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["run_id"], req.run_id);
    assert_eq!(status["attempt"], 1);
    assert_eq!(status["epoch"], req.epoch);
    assert_eq!(status["session"], req.session);
    assert_eq!(status["exit_code"], 0);
    assert_eq!(status["wait_completed"], true);
    assert!(std::fs::read(dir.join("stdout.bin")).unwrap().windows(20).any(|b| b == b"repeatable-evidence\""));
    assert!(String::from_utf8_lossy(&std::fs::read(dir.join("stderr.bin")).unwrap()).contains("fixture-worker"));
}

fn node() -> NodeSpec {
    NodeSpec { id: "B".into(), address: "local-B/runner".into(), key_hex: "07".repeat(32),
        slots: 1, admitted: true, healthy: true, draining: false, templates: vec!["sum-native-v1".into()] }
}

#[test]
fn pending_report_retains_raw_evidence_and_exact_id_after_runner_reopen() {
    let temp = tempfile::tempdir().unwrap();
    let controller_path = temp.path().join("controller.db");
    let runner_path = temp.path().join("runner.db");
    let mut controller = Store::open(&controller_path, "evidence-deploy", &[node()]).unwrap();
    let mut runner = Runner::open(&runner_path, "evidence-deploy", node()).unwrap();
    let mut ao = LocalAo::new(Path::new(env!("CARGO_BIN_EXE_aw-cli")), &temp.path().join("workers")).unwrap();
    let run = controller.dispatch(&Actor::Local("owner".into()), "sum-native-v1", 100).unwrap();
    controller.assign_next(100).unwrap();
    for (_, message) in controller.pending_outbox(8).unwrap() { runner.accept(&message, 100).unwrap(); }
    let deadline = Instant::now() + Duration::from_secs(8);
    let report = loop {
        runner.tick(&mut ao, 100).unwrap();
        if let Some(report) = runner.pending_outbox(8).unwrap().into_iter().find(|m| m.kind == "report") { break report; }
        assert!(Instant::now() < deadline, "runner report timed out");
        std::thread::sleep(Duration::from_millis(10));
    };
    assert_eq!(report.payload["verification"]["child_exit_code"], 0);
    let evidence_dir = temp.path().join("workers").join(report.payload["session"].as_str().unwrap());
    assert!(evidence_dir.join("status.json").is_file(), "evidence must be durable before outbox publication");
    assert!(evidence_dir.join("stdout.bin").is_file());
    assert!(evidence_dir.join("stderr.bin").is_file());
    drop(runner);
    drop(ao);
    let runner = Runner::open(&runner_path, "evidence-deploy", node()).unwrap();
    let replay = runner.pending_outbox(8).unwrap().into_iter().find(|m| m.message_id == report.message_id).unwrap();
    assert_eq!(serde_json::to_value(&replay).unwrap(), serde_json::to_value(&report).unwrap());
    assert!(!controller.assignment(&run.id, 1).unwrap().unwrap().released, "unacknowledged report cannot release the controller assignment");
}

// Real, bounded subprocess fixture. It implements the fixed gate/argv contract;
// no LocalAo/Runner fields are patched or bypassed by these tests.
#[cfg(windows)]
fn owned_process_fixture(dir: &Path, emit_report: bool, exit: i32, output_bytes: usize) -> PathBuf {
    let source = dir.join("owned-process.rs");
    let exe = dir.join("owned-process.exe");
    let body = format!(r##"
use std::io::{{Read, Write}};
fn main() {{
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 5);
    assert_eq!(args[1], "fixture-worker");
    let mut gate = [0_u8];
    std::io::stdin().read_exact(&mut gate).unwrap();
    assert_eq!(gate, [b'G']);
    if {emit_report} {{
        let report = [r#"{{"schema":"native-sum/1","session":""#, &args[4], r#"","inputs":[1,2,3],"sum":6}}"#].concat();
        std::fs::write(&args[3], report).unwrap();
    }}
    let raw: Vec<u8> = (0..{output_bytes}).map(|i| (i % 251) as u8).collect();
    std::io::stdout().write_all(&raw).unwrap();
    std::io::stderr().write_all(b"owned-stderr\xff").unwrap();
    std::process::exit({exit});
}}
"##);
    std::fs::write(&source, body).unwrap();
    let output = std::process::Command::new("rustc")
        .arg("--edition=2024").arg(&source).arg("-o").arg(&exe).output().unwrap();
    assert!(output.status.success(), "owned fixture compile failed: {}", String::from_utf8_lossy(&output.stderr));
    exe
}

#[cfg(windows)]
#[test]
fn valid_artifact_with_nonzero_child_exit_never_becomes_success() {
    let temp = tempfile::tempdir().unwrap();
    let worker = owned_process_fixture(temp.path(), true, 7, 1024);
    let mut ao = LocalAo::new(&worker, &temp.path().join("workers")).unwrap();
    ao.create(&request("nonzero-result")).unwrap();
    let (report, verification) = completed(&mut ao, "nonzero-result");
    assert_eq!(report["sum"], 6);
    assert_eq!(verification["child_exit_code"], 7);
    assert_eq!(verification["passed"], false, "artifact correctness cannot mask nonzero exit");
    assert_eq!(ao.create_count(), 1);
    assert!(matches!(ao.observe("missing-observer").unwrap(), Observation::Unknown(_)));
    assert_eq!(ao.create_count(), 1, "unknown observation must never spawn or rebind");
}

#[test]
fn evidence_write_failure_is_unknown_and_never_published_as_completion() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("workers");
    let mut ao = LocalAo::new(Path::new(env!("CARGO_BIN_EXE_aw-cli")), &state).unwrap();
    let req = request("evidence-write-failure");
    ao.create(&req).unwrap();
    std::fs::create_dir(state.join(&req.session).join("status.json")).unwrap();
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        match ao.observe(&req.session).unwrap() {
            Observation::Unknown(reason) => {
                assert!(reason.contains("evidence"));
                break;
            }
            Observation::Running => {}
            other => panic!("write failure must not publish completion: {other:?}"),
        }
        assert!(Instant::now() < deadline, "fixture did not reach evidence write");
        std::thread::sleep(Duration::from_millis(10));
    }
    std::fs::remove_dir(state.join(&req.session).join("status.json")).unwrap();
    let (_, verification) = completed(&mut ao, &req.session);
    assert_eq!(verification["child_exit_code"], 0);
}

#[cfg(windows)]
#[test]
fn nonzero_without_artifact_retains_exit_and_raw_bytes_instead_of_losing_status() {
    let temp = tempfile::tempdir().unwrap();
    let worker = owned_process_fixture(temp.path(), false, 7, 100000);
    let state = temp.path().join("workers");
    let mut ao = LocalAo::new(&worker, &state).unwrap();
    ao.create(&request("nonzero-no-result")).unwrap();
    let (_, verification) = completed(&mut ao, "nonzero-no-result");
    assert_eq!(verification["passed"], false);
    assert_eq!(verification["child_exit_code"], 7);
    let dir = state.join("nonzero-no-result");
    let stdout = std::fs::read(dir.join("stdout.bin")).unwrap();
    assert_eq!(stdout.len(), 4096, "captured output must obey its durable bound");
    assert_eq!(stdout, (0..4096).map(|i| (i % 251) as u8).collect::<Vec<_>>());
    assert_eq!(std::fs::read(dir.join("stderr.bin")).unwrap(), b"owned-stderr\xff");
    let status: Value = serde_json::from_slice(&std::fs::read(dir.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["stdout"]["total_bytes"], 100000);
    assert_eq!(status["stdout"]["truncated"], true);
    assert_eq!(status["exit_code"], 7);
    assert_eq!(ao.create_count(), 1);
}
