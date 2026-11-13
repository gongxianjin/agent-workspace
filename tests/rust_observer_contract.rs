use ao_reason_workbench::{
    core::{observer::{ManagedProcessObserver, ObserverEvent, RunnerObserver}, protocol::ExecutionIdentity},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn evidence_root(case_name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("dist")
        .join(format!("linux-wave2-observer-20261107-{case_name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    println!("OBSERVER_EVIDENCE={}", root.display());
    root
}

fn sha256(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn identity(session: &str) -> ExecutionIdentity {
    ExecutionIdentity::new(
        "observer-contract",
        "observer-run",
        1,
        "observer-epoch",
        Some(session),
        None,
        None,
    )
}

fn worker_source(report: &str, delay_ms: u64, exit_code: i32) -> String {
    format!(
        r##"
use std::{{io::{{Read, Write}}, thread, time::Duration}};
fn main() {{
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 5);
    assert_eq!(args[1], "fixture-worker");
    assert!(args[2] == "sum-native-v1");
    let mut gate = [0_u8];
    std::io::stdin().read_exact(&mut gate).unwrap();
    assert_eq!(gate, [b'G']);
    let report = {report:?};
    if !report.is_empty() {{ std::fs::write(&args[3], report).unwrap(); }}
    std::io::stdout().write_all(b"synthetic-worker-stdout\\x00").unwrap();
    std::io::stderr().write_all(b"synthetic-worker-stderr\\xff").unwrap();
    thread::sleep(Duration::from_millis({delay_ms}));
    std::process::exit({exit_code});
}}
"##,
        report = report,
        delay_ms = delay_ms,
        exit_code = exit_code,
    )
}

fn compile_worker(root: &Path, name: &str, report: &str, delay_ms: u64, exit_code: i32) -> PathBuf {
    let source = root.join(format!("{name}.rs"));
    let worker = if cfg!(windows) {
        root.join(format!("{name}.exe"))
    } else {
        root.join(name)
    };
    fs::write(&source, worker_source(report, delay_ms, exit_code)).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let args = vec![
        "rustc".to_owned(),
        "--edition=2024".to_owned(),
        source.display().to_string(),
        "-o".to_owned(),
        worker.display().to_string(),
    ];
    let mut compile = Command::new("rustc");
    compile.current_dir(&cwd).args(&args[1..]).stdout(Stdio::piped()).stderr(Stdio::piped());
    let child = compile.spawn().unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    fs::write(root.join("compile.stdout.bin"), &output.stdout).unwrap();
    fs::write(root.join("compile.stderr.bin"), &output.stderr).unwrap();
    fs::write(
        root.join("compile-trace.json"),
        serde_json::to_vec_pretty(&json!({
            "argv": args,
            "cwd": cwd,
            "pid": pid,
            "wait_completed": true,
            "exit_code": output.status.code(),
            "stdout": {"path":"compile.stdout.bin", "sha256":sha256(&output.stdout), "bytes":output.stdout.len()},
            "stderr": {"path":"compile.stderr.bin", "sha256":sha256(&output.stderr), "bytes":output.stderr.len()},
        })).unwrap(),
    ).unwrap();
    assert!(output.status.success(), "synthetic worker compile failed: {}", String::from_utf8_lossy(&output.stderr));
    worker
}

fn observe_until_terminal(
    observer: &mut impl RunnerObserver,
    expected: &ExecutionIdentity,
    deadline: Instant,
) -> ObserverEvent {
    loop {
        let event = observer.observe(expected).unwrap();
        if matches!(event, ObserverEvent::Exited { .. } | ObserverEvent::Unknown { .. }) {
            return event;
        }
        assert!(Instant::now() < deadline, "observer did not reach terminal event");
        thread::sleep(Duration::from_millis(10));
    }
}

fn save_event(root: &Path, event: &ObserverEvent) {
    fs::write(root.join("observer-event.json"), serde_json::to_vec_pretty(event).unwrap()).unwrap();
}

#[test]
fn synthetic_success_exit0_returns_wait_exit_and_identity_evidence() {
    let root = evidence_root("success");
    let session = "observer-success";
    let expected = identity(session);
    let artifact = root.join("result.json");
    let worker = compile_worker(
        &root,
        "success-worker",
        &format!(r#"{{"schema":"observer-report/1","session":"{session}","sum":6}}"#),
        0,
        0,
    );
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let event = observe_until_terminal(&mut observer, &expected, Instant::now() + Duration::from_secs(5));
    save_event(&root, &event);
    match event {
        ObserverEvent::Exited { identity, report: Some(report), evidence } => {
            assert_eq!(identity, expected);
            assert_eq!(report["sum"], 6);
            assert_eq!(evidence.identity, expected);
            assert_eq!(evidence.wait_completed, true);
            assert_eq!(evidence.exit_code, Some(0));
            assert!(evidence.pid > 0);
            assert_eq!(evidence.stdout.bytes, b"synthetic-worker-stdout\\x00");
            assert_eq!(evidence.stderr.bytes, b"synthetic-worker-stderr\\xff");
            assert_eq!(evidence.stdout.sha256, sha256(&evidence.stdout.bytes));
            assert_eq!(evidence.stderr.sha256, sha256(&evidence.stderr.bytes));
            assert_eq!(evidence.argv[0], worker.display().to_string());
            assert_eq!(evidence.argv[1], "fixture-worker");
            assert_eq!(evidence.argv[2], "sum-native-v1");
            assert_eq!(evidence.argv[3], artifact.display().to_string());
        }
        other => panic!("exit0 must produce terminal evidence, got {other:?}"),
    }
}

#[test]
fn synthetic_nonzero_exit_preserves_exit_without_success() {
    let root = evidence_root("nonzero");
    let session = "observer-nonzero";
    let expected = identity(session);
    let artifact = root.join("result.json");
    let worker = compile_worker(
        &root,
        "nonzero-worker",
        &format!(r#"{{"schema":"observer-report/1","session":"{session}","sum":6}}"#),
        0,
        7,
    );
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let event = observe_until_terminal(&mut observer, &expected, Instant::now() + Duration::from_secs(5));
    save_event(&root, &event);
    match event {
        ObserverEvent::Exited { identity, evidence, .. } => {
            assert_eq!(identity, expected);
            assert_eq!(evidence.exit_code, Some(7));
            assert!(!evidence.success());
            assert!(evidence.wait_completed);
        }
        other => panic!("nonzero exit must retain evidence, got {other:?}"),
    }
}

#[test]
fn report_before_exit_is_pending_until_wait_completes() {
    let root = evidence_root("pending");
    let session = "observer-pending";
    let expected = identity(session);
    let artifact = root.join("result.json");
    let worker = compile_worker(
        &root,
        "pending-worker",
        &format!(r#"{{"schema":"observer-report/1","session":"{session}","sum":6}}"#),
        500,
        0,
    );
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let pending_deadline = Instant::now() + Duration::from_secs(3);
    let pending = loop {
        let event = observer.observe(&expected).unwrap();
        if matches!(event, ObserverEvent::Pending { report: Some(_), .. }) {
            break event;
        }
        assert!(Instant::now() < pending_deadline, "report was not observed before child exit");
        thread::sleep(Duration::from_millis(10));
    };
    save_event(&root, &pending);
    match pending {
        ObserverEvent::Pending { identity, report: Some(report), reason } => {
            assert_eq!(identity, expected);
            assert_eq!(report["session"], session);
            assert!(reason.contains("wait"));
        }
        other => panic!("expected report-pending event, got {other:?}"),
    }
    let final_event = observe_until_terminal(&mut observer, &expected, Instant::now() + Duration::from_secs(5));
    fs::write(root.join("observer-final-event.json"), serde_json::to_vec_pretty(&final_event).unwrap()).unwrap();
    assert!(matches!(final_event, ObserverEvent::Exited { evidence, .. } if evidence.exit_code == Some(0)));
}

#[test]
fn malformed_report_becomes_unknown_without_completion() {
    let root = evidence_root("unknown");
    let session = "observer-unknown";
    let expected = identity(session);
    let artifact = root.join("result.json");
    let worker = compile_worker(&root, "unknown-worker", "{not-json", 0, 0);
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let event = observe_until_terminal(&mut observer, &expected, Instant::now() + Duration::from_secs(5));
    save_event(&root, &event);
    match event {
        ObserverEvent::Unknown { identity, reason } => {
            assert_eq!(identity, Some(expected));
            assert!(reason.contains("report"));
        }
        other => panic!("observer failure must be UNKNOWN, got {other:?}"),
    }
}

#[test]
fn mismatched_identity_returns_unknown_and_never_releases_exit_event() {
    let root = evidence_root("identity-mismatch");
    let expected = identity("observer-identity");
    let wrong = identity("observer-wrong");
    let artifact = root.join("result.json");
    let worker = compile_worker(
        &root,
        "identity-worker",
        r#"{"schema":"observer-report/1","session":"observer-identity","sum":6}"#,
        300,
        0,
    );
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let mismatched = observer.observe(&wrong).unwrap();
    save_event(&root, &mismatched);
    match mismatched {
        ObserverEvent::Unknown { identity, reason } => {
            assert_eq!(identity, Some(expected.clone()));
            assert!(reason.contains("identity"));
        }
        other => panic!("identity mismatch must not release/complete, got {other:?}"),
    }
    let event = observe_until_terminal(&mut observer, &expected, Instant::now() + Duration::from_secs(5));
    assert!(matches!(event, ObserverEvent::Exited { identity, .. } if identity == expected));
}
