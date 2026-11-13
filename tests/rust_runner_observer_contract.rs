use agentranch::{
    adapters::{AoPort, Observation, WorkRequest},
    config::{Actor, NodeSpec},
    core::{
        observer::{ManagedProcessObserver, ObserverEvent},
        protocol::ExecutionIdentity,
        runner::Runner,
        store::Store,
    },
};
use serde_json::{json, Value};
use std::{
    fs,
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

fn root(case_name: &str) -> PathBuf {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("dist")
        .join(format!("linux-wave2-observer-20261107-runner-{case_name}-{}", uuid::Uuid::new_v4()));
    fs::create_dir_all(&root).unwrap();
    println!("RUNNER_OBSERVER_EVIDENCE={}", root.display());
    root
}

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(),
        address: "runner-observer-B/runner".into(),
        key_hex: "07".repeat(32),
        slots: 1,
        admitted: true,
        healthy: true,
        draining: false,
        templates: vec!["sum-native-v1".into()],
    }
}

struct ScriptedAo {
    session: String,
}

impl AoPort for ScriptedAo {
    fn create(&mut self, request: &WorkRequest) -> anyhow::Result<String> {
        self.session = request.session.clone();
        Ok(request.session.clone())
    }

    fn observe(&mut self, session: &str) -> anyhow::Result<Observation> {
        assert_eq!(session, self.session);
        Ok(Observation::Running)
    }

    fn resolve(&mut self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("approval is not part of this fixture")
    }

    fn kill(&mut self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("cancellation is not part of this fixture")
    }
}

fn worker_source(report: &str, delay_ms: u64, exit_code: i32) -> String {
    format!(
        r##"
use std::{{io::{{Read, Write}}, thread, time::Duration}};
fn main() {{
    let args: Vec<String> = std::env::args().collect();
    assert_eq!(args.len(), 5);
    assert_eq!(args[1], "fixture-worker");
    let mut gate = [0_u8];
    std::io::stdin().read_exact(&mut gate).unwrap();
    assert_eq!(gate, [b'G']);
    let report = {report:?};
    if !report.is_empty() {{ std::fs::write(&args[3], report).unwrap(); }}
    std::io::stdout().write_all(b"runner-observer-stdout").unwrap();
    std::io::stderr().write_all(b"runner-observer-stderr").unwrap();
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
    let worker = if cfg!(windows) { root.join(format!("{name}.exe")) } else { root.join(name) };
    fs::write(&source, worker_source(report, delay_ms, exit_code)).unwrap();
    let cwd = std::env::current_dir().unwrap();
    let mut command = Command::new("rustc");
    command
        .current_dir(&cwd)
        .args(["--edition=2024"])
        .arg(&source)
        .arg("-o")
        .arg(&worker)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let child = command.spawn().unwrap();
    let pid = child.id();
    let output = child.wait_with_output().unwrap();
    fs::write(root.join("compile.stdout.bin"), &output.stdout).unwrap();
    fs::write(root.join("compile.stderr.bin"), &output.stderr).unwrap();
    fs::write(
        root.join("compile-trace.json"),
        serde_json::to_vec_pretty(&json!({
            "argv": ["rustc", "--edition=2024", source, "-o", worker],
            "cwd": cwd,
            "pid": pid,
            "direct_wait": "Child::wait_with_output",
            "wait_completed": true,
            "exit_code": output.status.code(),
        }))
        .unwrap(),
    )
    .unwrap();
    assert!(output.status.success(), "worker compile failed: {}", String::from_utf8_lossy(&output.stderr));
    worker
}

fn setup(case_name: &str, session: &str) -> (PathBuf, Store, Runner, String, String) {
    let root = root(case_name);
    let mut store = Store::open(&root.join("controller.sqlite"), "runner-observer", &[node()]).unwrap();
    let run = store.dispatch(&Actor::Local("runner-observer-owner".into()), "sum-native-v1", 100).unwrap();
    store.assign_next(100).unwrap().unwrap();
    let mut runner = Runner::open(&root.join("runner.sqlite"), "runner-observer", node()).unwrap();
    for (_, message) in store.pending_outbox(8).unwrap() {
        runner.accept(&message, 100).unwrap();
    }
    let mut ao = ScriptedAo { session: session.into() };
    runner.tick(&mut ao, 100).unwrap();
    for message in runner.pending_outbox(100).unwrap() {
        store.receive(&message, 100).unwrap();
        runner.acknowledge_outbox(&message.message_id).unwrap();
    }
    let actual_session = store.run(&run.id).unwrap().session.expect("Runner session must be durable");
    (root, store, runner, run.id, actual_session)
}

fn expected_identity(run_id: &str, epoch: &str, session: &str) -> ExecutionIdentity {
    ExecutionIdentity::new("runner-observer", run_id, 1, epoch, Some(session), None, None)
}

fn save(root: &Path, name: &str, value: &impl serde::Serialize) {
    fs::write(root.join(name), serde_json::to_vec_pretty(value).unwrap()).unwrap();
}

fn observe_until_exited(
    runner: &mut Runner,
    observer: &mut ManagedProcessObserver,
    expected: &ExecutionIdentity,
    deadline: Instant,
) -> ObserverEvent {
    loop {
        let event = runner.observe_owned_process(observer, expected, 100).unwrap();
        if matches!(event, ObserverEvent::Exited { .. } | ObserverEvent::Unknown { .. }) {
            return event;
        }
        assert!(Instant::now() < deadline, "Runner observer did not reach terminal event");
        thread::sleep(Duration::from_millis(10));
    }
}

#[test]
fn runner_keeps_reservation_when_report_arrives_before_direct_wait() {
    let (root, mut store, mut runner, run_id, session) = setup("pending", "runner-pending");
    let epoch = store.run(&run_id).unwrap().epoch;
    let expected = expected_identity(&run_id, &epoch, &session);
    let artifact = root.join("pending-result.json");
    let worker = compile_worker(
        &root,
        "pending-worker",
        &format!(r#"{{"schema":"observer-report/1","session":"{session}","sum":6}}"#),
        500,
        0,
    );
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    let event = loop {
        let event = runner.observe_owned_process(&mut observer, &expected, 100).unwrap();
        if matches!(event, ObserverEvent::Pending { report: Some(_), .. }) {
            break event;
        }
        assert!(Instant::now() < deadline, "Runner did not retain pending report");
        thread::sleep(Duration::from_millis(10));
    };
    save(&root, "runner-pending-event.json", &event);
    save(&root, "runner-pending-outbox.json", &runner.pending_outbox(100).unwrap());
    let report = runner.pending_outbox(100).unwrap().into_iter().find(|message| message.kind == "report").unwrap();
    assert_eq!(report.payload["verification"]["state"], "report_pending_exit");
    for message in runner.pending_outbox(100).unwrap() {
        if message.kind == "report" && message.payload["report"].is_object() {
            let mut accepted = message.clone();
            accepted.payload["verification"] = json!({"passed": null, "state": "report_pending_exit"});
            accepted.sign(&[0x07; 32]).unwrap();
            store.receive(&accepted, 100).unwrap();
        }
        runner.acknowledge_outbox(&message.message_id).unwrap();
    }
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
}

#[test]
fn runner_marks_observer_failure_unknown_without_releasing() {
    let (root, mut store, mut runner, run_id, session) = setup("unknown", "runner-unknown");
    let epoch = store.run(&run_id).unwrap().epoch;
    let expected = expected_identity(&run_id, &epoch, &session);
    let artifact = root.join("unknown-result.json");
    let worker = compile_worker(&root, "unknown-worker", "{not-json", 0, 0);
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let event = observe_until_exited(&mut runner, &mut observer, &expected, Instant::now() + Duration::from_secs(5));
    save(&root, "runner-unknown-event.json", &event);
    save(&root, "runner-unknown-outbox.json", &runner.pending_outbox(100).unwrap());
    assert!(matches!(event, ObserverEvent::Unknown { .. }));
    for message in runner.pending_outbox(100).unwrap() {
        if message.kind == "report" && message.payload["report"].is_object() {
            let mut accepted = message.clone();
            accepted.payload["verification"] = json!({"passed": null, "state": "report_pending_exit"});
            accepted.sign(&[0x07; 32]).unwrap();
            store.receive(&accepted, 100).unwrap();
        }
        runner.acknowledge_outbox(&message.message_id).unwrap();
    }
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
}

#[test]
fn runner_rejects_observer_identity_mismatch_without_terminal_release() {
    let (root, mut store, mut runner, run_id, session) = setup("identity-mismatch", "runner-identity");
    let epoch = store.run(&run_id).unwrap().epoch;
    let expected = expected_identity(&run_id, &epoch, &session);
    let wrong = expected_identity(&run_id, &epoch, "runner-wrong");
    let artifact = root.join("identity-result.json");
    let worker = compile_worker(
        &root,
        "identity-worker",
        &format!(r#"{{"schema":"observer-report/1","session":"{session}","sum":6}}"#),
        300,
        0,
    );
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let event = runner.observe_owned_process(&mut observer, &wrong, 100).unwrap();
    save(&root, "runner-identity-event.json", &event);
    assert!(matches!(event, ObserverEvent::Unknown { .. }));
    assert!(runner.pending_outbox(100).unwrap().iter().all(|message| message.payload.get("exit_proof").is_none_or(Value::is_null)));
    for message in runner.pending_outbox(100).unwrap() {
        store.receive(&message, 100).unwrap();
        runner.acknowledge_outbox(&message.message_id).unwrap();
    }
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
}

#[test]
fn runner_accepts_terminal_evidence_only_after_direct_wait() {
    let (root, mut store, mut runner, run_id, session) = setup("terminal", "runner-terminal");
    let epoch = store.run(&run_id).unwrap().epoch;
    let expected = expected_identity(&run_id, &epoch, &session);
    let artifact = root.join("terminal-result.json");
    let worker = compile_worker(
        &root,
        "terminal-worker",
        &format!(r#"{{"schema":"observer-report/1","session":"{session}","sum":6}}"#),
        0,
        0,
    );
    let mut observer = ManagedProcessObserver::spawn(&worker, "sum-native-v1", &artifact, expected.clone()).unwrap();
    let event = observe_until_exited(&mut runner, &mut observer, &expected, Instant::now() + Duration::from_secs(5));
    save(&root, "runner-terminal-event.json", &event);
    match event {
        ObserverEvent::Exited { evidence, .. } => {
            assert!(evidence.wait_completed);
            assert_eq!(evidence.exit_code, Some(0));
            assert_eq!(evidence.identity, expected);
            assert!(evidence.pid > 0);
        }
        other => panic!("expected direct-wait terminal evidence, got {other:?}"),
    }
    save(&root, "runner-terminal-outbox.json", &runner.pending_outbox(100).unwrap());
    let report = runner
        .pending_outbox(100)
        .unwrap()
        .into_iter()
        .find(|message| message.kind == "report" && message.payload["verification"]["state"] == "owner_evidence_required")
        .expect("terminal report must be retained as non-authoritative evidence");
    assert!(report.payload["exit_proof"].is_null());
    assert_eq!(report.payload["verification"]["state"], "owner_evidence_required");
    for message in runner.pending_outbox(100).unwrap() {
        if message.kind == "report" && message.payload["verification"]["state"] == "owner_evidence_required" {
            store.receive(&message, 100).unwrap();
        }
        runner.acknowledge_outbox(&message.message_id).unwrap();
    }
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
}
