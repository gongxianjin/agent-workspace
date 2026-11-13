use super::*;
use crate::config::{Actor, NodeSpec};
use std::{path::{Path, PathBuf}, time::{Duration, Instant}};

fn node() -> NodeSpec {
    NodeSpec { id: "B".into(), address: "local-B/runner".into(), key_hex: "07".repeat(32),
        slots: 1, admitted: true, healthy: true, draining: false, templates: vec!["sum-native-v1".into()] }
}

fn case_root() -> PathBuf {
    let manifest = Path::new(env!("CARGO_MANIFEST_DIR"));
    if manifest.file_name().and_then(|name| name.to_str()) == Some("source") {
        manifest.parent().unwrap().join("evidence/cases")
    } else {
        manifest.join("dist/worker-fault-cases")
    }
}

fn case_dir(name: &str) -> PathBuf {
    let root = case_root();
    std::fs::create_dir_all(&root).unwrap();
    loop {
        let dir = root.join(format!("{name}-{}", uuid::Uuid::new_v4()));
        match std::fs::create_dir(&dir) {
            Ok(()) => {
                println!("CASE_DIR={}", dir.display());
                return dir;
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => panic!("create isolated case directory: {error}"),
        }
    }
}

fn fixture(dir: &Path) -> PathBuf {
    let source = dir.join("owned-worker.rs");
    let exe = dir.join("owned-worker.exe");
    std::fs::write(&source, r##"
use std::io::{Read, Write};
fn main() {
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 5);
    assert_eq!(args[1], "fixture-worker");
    let mut gate = [0];
    std::io::stdin().read_exact(&mut gate).unwrap();
    assert_eq!(gate, [b'G']);
    let report = [r#"{"schema":"native-sum/1","session":""#, &args[4], r#"","inputs":[1,2,3],"sum":6}"#].concat();
    std::fs::write(&args[3], report).unwrap();
    std::io::stdout().write_all(b"known-stdout-prefix\xff").unwrap();
    std::io::stderr().write_all(b"known-stderr-prefix\xfe").unwrap();
    std::process::exit(7);
}
"##).unwrap();
    let child = std::process::Command::new("rustc").args(["--edition=2024"])
        .arg(&source).arg("-o").arg(&exe).stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped()).spawn().unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    std::fs::write(dir.join("fixture-rustc.stdout.bin"), &output.stdout).unwrap();
    std::fs::write(dir.join("fixture-rustc.stderr.bin"), &output.stderr).unwrap();
    println!("FIXTURE_COMPILE pid={pid} wait_completed=true numeric_exit={:?}", output.status.code());
    assert!(output.status.success(), "fixture compile: {}", String::from_utf8_lossy(&output.stderr));
    exe
}

fn setup(dir: &Path) -> (Store, Runner, LocalAo, String) {
    let mut controller = Store::open(&dir.join("controller.db"), "worker-faults", &[node()]).unwrap();
    let mut runner = Runner::open(&dir.join("runner.db"), "worker-faults", node()).unwrap();
    let run = controller.dispatch(&Actor::Local("owner".into()), "sum-native-v1", 100).unwrap();
    controller.assign_next(100).unwrap();
    for (_, message) in controller.pending_outbox(8).unwrap() { runner.accept(&message, 100).unwrap(); }
    let ao = LocalAo::new(&fixture(dir), &dir.join("workers")).unwrap();
    (controller, runner, ao, run.id)
}

fn wait_for_child(ao: &mut LocalAo) -> String {
    let session = ao.workers.keys().next().unwrap().clone();
    let deadline = Instant::now() + Duration::from_secs(8);
    while ao.workers.get_mut(&session).unwrap().child.active().unwrap() != 0 {
        assert!(Instant::now() < deadline, "real child did not exit");
        std::thread::sleep(Duration::from_millis(10));
    }
    session
}

fn no_completion(runner: &Runner) -> bool {
    runner.pending_outbox(100).unwrap().iter().all(|message|
        message.kind != "report" && message.payload.get("exit_proof").is_none_or(|proof| proof.is_null()))
}

struct ResetSync;
impl Drop for ResetSync { fn drop(&mut self) { super::test_sync::fail(None); crate::adapters::process::test_capture::fail(None); } }

fn sync_gate(name: &str) {
    let _reset = ResetSync;
    let dir = case_dir(&format!("f1-{}", name.replace('.', "-")));
    let (controller, mut runner, mut ao, run) = setup(&dir);
    super::test_sync::fail(Some(name));
    runner.tick(&mut ao, 100).unwrap();
    let session = wait_for_child(&mut ao);
    let deadline = Instant::now() + Duration::from_secs(8);
    while !dir.join("workers").join(&session).join(name).is_file() {
        runner.tick(&mut ao, 100).unwrap();
        assert!(Instant::now() < deadline, "post-write fault was never reached");
        std::thread::sleep(Duration::from_millis(10));
    }
    let evidence = ao.workers.get_mut(&session).unwrap().child.evidence().unwrap().unwrap();
    assert_eq!(evidence.exit_code, Some(7));
    assert_eq!(evidence.stdout, b"known-stdout-prefix\xff");
    assert_eq!(evidence.stderr, b"known-stderr-prefix\xfe");
    assert!(no_completion(&runner), "first failed sync must block completion");
    runner.tick(&mut ao, 100).unwrap();
    let forbidden = !no_completion(&runner);
    let messages = runner.pending_outbox(100).unwrap();
    std::fs::write(dir.join("outbox-after-repeat.json"), serde_json::to_vec_pretty(&messages).unwrap()).unwrap();
    println!("F1 fault={name} wait_completed=true numeric_exit={:?} equal_existing_file=true forbidden_completion={forbidden}", evidence.exit_code);
    assert!(!controller.assignment(&run, 1).unwrap().unwrap().released);
    assert_eq!(ao.create_count(), 1);
    assert!(!forbidden, "F1: equal bytes bypassed failed sync and Runner published report/ExitProof");
    super::test_sync::fail(None);
    runner.tick(&mut ao, 100).unwrap();
    let report = runner.pending_outbox(100).unwrap().into_iter().find(|message| message.kind == "report").unwrap();
    assert_eq!(report.payload["verification"]["child_exit_code"], 7);
    assert_eq!(
        report.payload["verification"]["passed"].as_bool(),
        Some(false),
        "completed non-zero child verification must remain false while owner admission is pending"
    );
    assert_eq!(report.payload["verification"]["state"], "owner_evidence_required");
    assert!(
        report
            .payload
            .get("exit_proof")
            .is_none_or(serde_json::Value::is_null),
        "ordinary observer exit proof must not enter the trusted envelope"
    );
    for file in ["stdout.bin", "stderr.bin", "status.json"] {
        assert!(dir.join("workers").join(&session).join(file).is_file());
    }
}

#[test]
fn case_dir_uuid_tagged_name_is_atomic_and_isolated() {
    let name = format!("isolation-regression-{}", uuid::Uuid::new_v4());
    let first = case_dir(&name);
    let sentinel = first.join("sentinel.txt");
    std::fs::write(&sentinel, b"first-case").unwrap();

    let second = case_dir(&name);

    assert_ne!(first, second, "same UUID-tagged case name must allocate a fresh directory");
    assert_eq!(std::fs::read(&sentinel).unwrap(), b"first-case");
    assert!(std::fs::read_dir(second).unwrap().next().is_none());
}

#[test]
fn f1_status_sync_failure_blocks_runner_report_until_resync() { sync_gate("status.json"); }

#[test]
fn f2_stdout_reader_error_retains_prefix_stderr_and_exit_as_unknown() {
    let _reset = ResetSync;
    crate::adapters::process::test_capture::fail(Some("stdout"));
    let dir = case_dir("f2-reader-error");
    let (_controller, mut runner, mut ao, _run) = setup(&dir);
    runner.tick(&mut ao, 100).unwrap();
    let session = wait_for_child(&mut ao);
    let deadline = Instant::now() + Duration::from_secs(8);
    loop {
        if ao.workers.get_mut(&session).unwrap().child.evidence().unwrap().is_some() {
            break;
        }
        assert!(Instant::now() < deadline, "reader-error child evidence timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
    let result = ao.observe(&session).unwrap();
    assert!(matches!(result, Observation::Unknown(_)), "reader error must be UNKNOWN, got {result:?}");
    runner.tick(&mut ao, 100).unwrap();
    let messages = runner.pending_outbox(100).unwrap();
    std::fs::write(dir.join("outbox-after-reader-error.json"), serde_json::to_vec_pretty(&messages).unwrap()).unwrap();
    let evidence_dir = dir.join("workers").join(&session);
    assert_eq!(std::fs::read(evidence_dir.join("stdout.bin")).unwrap(), b"known-stdout-prefix\xff");
    assert_eq!(std::fs::read(evidence_dir.join("stderr.bin")).unwrap(), b"known-stderr-prefix\xfe");
    let status: serde_json::Value = serde_json::from_slice(&std::fs::read(evidence_dir.join("status.json")).unwrap()).unwrap();
    assert_eq!(status["exit_code"], 7);
    assert!(status["stdout_error"].as_str().is_some_and(|error| error.contains("injected reader error")));
    assert!(status["stderr_error"].is_null());
    println!("F2 reader=stdout wait_completed=true numeric_exit=7 stdout_prefix=true stderr_prefix=true stdout_error=true");
    assert!(no_completion(&runner), "reader error must block report/ExitProof");
    assert_eq!(ao.create_count(), 1, "reader error must not retry or rebind");
}
