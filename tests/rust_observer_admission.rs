use ao_reason_workbench::{
    adapters::{AoPort, Observation, WorkRequest},
    config::{Actor, NodeSpec},
    core::{
        observer::{ObserverEvent, ProcessEvidence, RunnerObserver, StreamEvidence},
        protocol::ExecutionIdentity,
        runner::Runner,
        store::Store,
    },
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::Digest;
use std::path::{Path, PathBuf};

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(),
        address: "observer-admission-B/runner".into(),
        key_hex: "07".repeat(32),
        slots: 1,
        admitted: true,
        healthy: true,
        draining: false,
        templates: vec!["sum-native-v1".into()],
    }
}

struct RunningAo;

impl AoPort for RunningAo {
    fn create(&mut self, request: &WorkRequest) -> anyhow::Result<String> {
        Ok(request.session.clone())
    }

    fn observe(&mut self, _session: &str) -> anyhow::Result<Observation> {
        Ok(Observation::Running)
    }

    fn resolve(&mut self, _: &str, _: &str, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("approval is not part of this fixture")
    }

    fn kill(&mut self, _: &str) -> anyhow::Result<()> {
        anyhow::bail!("cancellation is not part of this fixture")
    }
}

struct DeterministicObserver {
    events: Vec<ObserverEvent>,
    calls: usize,
}

impl DeterministicObserver {
    fn new(event: ObserverEvent) -> Self {
        Self::sequence(vec![event])
    }

    fn sequence(events: Vec<ObserverEvent>) -> Self {
        Self { events, calls: 0 }
    }
}

impl RunnerObserver for DeterministicObserver {
    fn observe(&mut self, _: &ExecutionIdentity) -> anyhow::Result<ObserverEvent> {
        self.calls += 1;
        if self.events.is_empty() {
            anyhow::bail!("deterministic observer called after sequence exhausted")
        }
        Ok(self.events.remove(0))
    }
}

struct Fixture {
    _temp: tempfile::TempDir,
    runner_path: PathBuf,
    store: Store,
    runner: Runner,
    run_id: String,
    expected: ExecutionIdentity,
}

fn setup_accepted() -> Fixture {
    let temp = tempfile::tempdir().unwrap();
    let runner_path = temp.path().join("runner.sqlite");
    let node = node();
    let mut store = Store::open(&temp.path().join("controller.sqlite"), "observer-admission", &[node.clone()]).unwrap();
    let run = store.dispatch(&Actor::Local("observer-admission-owner".into()), "sum-native-v1", 100).unwrap();
    store.assign_next(100).unwrap().unwrap();
    let mut runner = Runner::open(&runner_path, "observer-admission", node).unwrap();
    for (_, message) in store.pending_outbox(8).unwrap() {
        runner.accept(&message, 100).unwrap();
    }
    let expected = run.execution_identity("observer-admission").unwrap();
    Fixture { _temp: temp, runner_path, store, runner, run_id: run.id, expected }
}

fn setup_running() -> Fixture {
    let mut fixture = setup_accepted();
    let mut ao = RunningAo;
    fixture.runner.tick(&mut ao, 100).unwrap();
    let messages = fixture.runner.pending_outbox(8).unwrap();
    let session = messages
        .iter()
        .find(|message| message.kind == "status")
        .and_then(|message| message.payload["session"].as_str())
        .expect("running status must persist a session")
        .to_owned();
    for message in messages {
        fixture.store.receive(&message, 100).unwrap();
        fixture.runner.acknowledge_outbox(&message.message_id).unwrap();
    }
    fixture.expected.session = Some(session);
    fixture
}

fn execution_body(path: &Path, run_id: &str) -> Value {
    let db = Connection::open(path).unwrap();
    let raw: String = db
        .query_row("SELECT body FROM executions WHERE run=?1 AND attempt=1", [run_id], |row| row.get(0))
        .unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn ledger_snapshot(path: &Path, run_id: &str) -> (String, Vec<(String, String, i64)>) {
    let db = Connection::open(path).unwrap();
    let execution: String = db
        .query_row("SELECT body FROM executions WHERE run=?1 AND attempt=1", [run_id], |row| row.get(0))
        .unwrap();
    let mut statement = db.prepare("SELECT id, body, sent FROM outbox ORDER BY rowid").unwrap();
    let outbox = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap();
    (execution, outbox)
}

fn mutate_execution(path: &Path, run_id: &str, mutate: impl FnOnce(&mut Value)) {
    let db = Connection::open(path).unwrap();
    let raw: String = db
        .query_row("SELECT body FROM executions WHERE run=?1 AND attempt=1", [run_id], |row| row.get(0))
        .unwrap();
    let mut body: Value = serde_json::from_str(&raw).unwrap();
    mutate(&mut body);
    db.execute(
        "UPDATE executions SET body=?1 WHERE run=?2 AND attempt=1",
        params![body.to_string(), run_id],
    )
    .unwrap();
}

fn ingest_outbox(fixture: &mut Fixture) {
    for message in fixture.runner.pending_outbox(8).unwrap() {
        if message.kind != "report" || message.payload["report"].is_object() {
            fixture.store.receive(&message, 100).unwrap();
        }
        fixture.runner.acknowledge_outbox(&message.message_id).unwrap();
    }
}

fn assert_owner_evidence_required(path: &Path, run_id: &str) {
    let body = execution_body(path, run_id);
    assert_eq!(body["state"], "unknown");
    assert_eq!(body["finished"], false);
    let (_, outbox) = ledger_snapshot(path, run_id);
    let report: Value = outbox
        .iter()
        .map(|(_, body, _)| serde_json::from_str(body).unwrap())
        .find(|message: &Value| {
            message["kind"] == "report"
                && message["payload"]["verification"]["state"] == "owner_evidence_required"
        })
        .expect("complete observer exit must wait for owner evidence");
    assert!(report["payload"].get("exit_proof").is_none_or(Value::is_null));
    assert_eq!(report["payload"]["verification"]["passed"], Value::Null);
}

fn stream(error: Option<&str>) -> StreamEvidence {
    StreamEvidence {
        bytes: Vec::new(),
        total_bytes: 0,
        truncated: false,
        sha256: hex::encode(sha2::Sha256::digest([])),
        error: error.map(str::to_owned),
    }
}

fn exited(expected: &ExecutionIdentity, wait_completed: bool, stream_error: Option<&str>) -> ObserverEvent {
    ObserverEvent::Exited {
        identity: expected.clone(),
        report: Some(json!({"schema": "observer-report/1", "sum": 6})),
        evidence: ProcessEvidence {
            identity: expected.clone(),
            pid: 17,
            argv: vec!["synthetic-observer".into()],
            cwd: PathBuf::from("."),
            wait_completed,
            exit_code: Some(0),
            stdout: stream(stream_error),
            stderr: stream(stream_error),
        },
    }
}

#[test]
fn persisted_identity_mismatch_is_rejected_before_observer_and_durable_state_is_unchanged() {
    let mut fixture = setup_running();
    let before = ledger_snapshot(&fixture.runner_path, &fixture.run_id);
    mutate_execution(&fixture.runner_path, &fixture.run_id, |body| {
        body["identity"]["session"] = json!("persisted-other-session");
    });
    let before_observation = ledger_snapshot(&fixture.runner_path, &fixture.run_id);
    let mut observer = DeterministicObserver::new(exited(&fixture.expected, true, None));

    let result = fixture.runner.observe_owned_process(&mut observer, &fixture.expected, 100).unwrap();

    assert!(matches!(result, ObserverEvent::Unknown { .. }), "persisted identity mismatch must be rejected");
    assert_eq!(observer.calls, 0, "observer must not be called after admission rejection");
    assert_ne!(before, before_observation, "fixture must actually persist the mismatch");
    assert_eq!(ledger_snapshot(&fixture.runner_path, &fixture.run_id), before_observation);
}

#[test]
fn unclaimed_finished_and_cancel_pending_states_retain_their_durable_reservation() {
    let cases = ["unclaimed", "finished", "cancel_pending"];

    for state in cases {
        let mut fixture = if state == "unclaimed" { setup_accepted() } else { setup_running() };
        mutate_execution(&fixture.runner_path, &fixture.run_id, |body| match state {
            "unclaimed" => {
                body["state"] = json!("accepted");
                body["claimed"] = json!(false);
                body["finished"] = json!(false);
            }
            "finished" => {
                body["state"] = json!("done");
                body["claimed"] = json!(true);
                body["finished"] = json!(true);
            }
            "cancel_pending" => {
                body["state"] = json!("cancel_pending");
                body["claimed"] = json!(true);
                body["finished"] = json!(false);
            }
            _ => unreachable!(),
        });
        let before = ledger_snapshot(&fixture.runner_path, &fixture.run_id);
        let mut observer = DeterministicObserver::new(exited(&fixture.expected, true, None));

        let result = fixture.runner.observe_owned_process(&mut observer, &fixture.expected, 100).unwrap();

        assert!(matches!(result, ObserverEvent::Unknown { .. }), "{state} execution must be rejected");
        assert_eq!(observer.calls, 0, "{state} execution must not call observer");
        assert_eq!(ledger_snapshot(&fixture.runner_path, &fixture.run_id), before);
        let body = execution_body(&fixture.runner_path, &fixture.run_id);
        let expected_state = match state {
            "unclaimed" => "accepted",
            "finished" => "done",
            other => other,
        };
        assert_eq!(body["state"], expected_state);
        assert_eq!(body["finished"], state == "finished");
    }
}

#[test]
fn pending_report_then_exit_retains_unknown_until_owner_evidence() {
    let mut fixture = setup_running();
    let expected = fixture.expected.clone();
    let report = json!({"schema": "observer-report/1", "sum": 6});
    let mut observer = DeterministicObserver::sequence(vec![
        ObserverEvent::Pending {
            identity: expected.clone(),
            report: None,
            reason: "report pending while process is running".into(),
        },
        ObserverEvent::Pending {
            identity: expected.clone(),
            report: Some(report.clone()),
            reason: "report observed before direct OS wait completed".into(),
        },
        exited(&expected, true, None),
    ]);

    let first = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();
    assert!(matches!(first, ObserverEvent::Pending { report: None, .. }));
    let second = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();
    assert!(matches!(second, ObserverEvent::Pending { report: Some(_), .. }));
    let third = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();
    assert!(matches!(third, ObserverEvent::Exited { .. }));
    assert_eq!(observer.calls, 3);

    let body = execution_body(&fixture.runner_path, &fixture.run_id);
    assert_eq!(body["session"].as_str(), expected.session.as_deref());
    assert_owner_evidence_required(&fixture.runner_path, &fixture.run_id);
    ingest_outbox(&mut fixture);
    assert!(!fixture.store.assignment(&fixture.run_id, 1).unwrap().unwrap().released);
}

#[test]
fn os_wait_pending_then_exit_retains_assignment_until_owner_evidence() {
    let mut fixture = setup_running();
    let expected = fixture.expected.clone();
    let mut observer = DeterministicObserver::sequence(vec![
        ObserverEvent::Pending {
            identity: expected.clone(),
            report: Some(json!({"schema": "observer-report/1", "sum": 6})),
            reason: "OS wait completed but stdout/stderr evidence is still pending".into(),
        },
        exited(&expected, true, None),
    ]);

    let pending = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();
    assert!(matches!(pending, ObserverEvent::Pending { .. }));
    let exited = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();
    assert!(matches!(exited, ObserverEvent::Exited { .. }));
    assert_eq!(observer.calls, 2);
    assert_owner_evidence_required(&fixture.runner_path, &fixture.run_id);
    ingest_outbox(&mut fixture);
    assert!(!fixture.store.assignment(&fixture.run_id, 1).unwrap().unwrap().released);
}

#[test]
fn incomplete_then_complete_exit_evidence_still_requires_owner_admission() {
    let mut fixture = setup_running();
    let expected = fixture.expected.clone();
    let mut observer = DeterministicObserver::sequence(vec![
        exited(&expected, false, None),
        exited(&expected, true, None),
    ]);

    let incomplete = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();
    assert!(matches!(incomplete, ObserverEvent::Exited { .. }));
    let body = execution_body(&fixture.runner_path, &fixture.run_id);
    assert_eq!(body["state"], "unknown");
    assert_eq!(body["finished"], false);
    let complete = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();
    assert!(matches!(complete, ObserverEvent::Exited { .. }));
    assert_eq!(observer.calls, 2);
    let body = execution_body(&fixture.runner_path, &fixture.run_id);
    assert_eq!(body["session"].as_str(), expected.session.as_deref());
    assert_owner_evidence_required(&fixture.runner_path, &fixture.run_id);
}

#[test]
fn persisted_session_must_match_expected_and_remain_nonempty_after_identity_admission() {
    for persisted_session in [Some("persisted-other-session"), None] {
        let mut fixture = setup_running();
        let expected = fixture.expected.clone();
        mutate_execution(&fixture.runner_path, &fixture.run_id, |body| {
            body["session"] = persisted_session.map_or(Value::Null, |value| json!(value));
        });
        let before = ledger_snapshot(&fixture.runner_path, &fixture.run_id);
        let mut observer = DeterministicObserver::new(exited(&expected, true, None));

        let result = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();

        assert!(matches!(result, ObserverEvent::Unknown { .. }));
        assert_eq!(observer.calls, 0);
        assert_eq!(ledger_snapshot(&fixture.runner_path, &fixture.run_id), before);
    }

    let mut fixture = setup_running();
    let mut expected = fixture.expected.clone();
    expected.session = None;
    mutate_execution(&fixture.runner_path, &fixture.run_id, |body| {
        body["session"] = Value::Null;
        body["identity"]["session"] = Value::Null;
    });
    let before = ledger_snapshot(&fixture.runner_path, &fixture.run_id);
    let mut observer = DeterministicObserver::new(exited(&expected, true, None));

    let result = fixture.runner.observe_owned_process(&mut observer, &expected, 100).unwrap();

    assert!(matches!(result, ObserverEvent::Unknown { .. }));
    assert_eq!(observer.calls, 0);
    assert_eq!(ledger_snapshot(&fixture.runner_path, &fixture.run_id), before);
}

#[test]
fn incomplete_exit_evidence_retains_unknown_reservation_without_exit_proof() {
    for (wait_completed, stream_error) in [(false, None), (true, Some("stdout read failed")), (true, Some("stderr read failed"))] {
        let mut fixture = setup_running();
        let mut observer = DeterministicObserver::new(exited(&fixture.expected, wait_completed, stream_error));

        let event = fixture.runner.observe_owned_process(&mut observer, &fixture.expected, 100).unwrap();

        assert!(matches!(event, ObserverEvent::Exited { .. }));
        assert_eq!(observer.calls, 1);
        let body = execution_body(&fixture.runner_path, &fixture.run_id);
        assert_eq!(body["state"], "unknown");
        assert_eq!(body["finished"], false);
        let (_, outbox) = ledger_snapshot(&fixture.runner_path, &fixture.run_id);
        let report: Value = outbox
            .iter()
            .map(|(_, body, _)| serde_json::from_str(body).unwrap())
            .find(|message: &Value| message["kind"] == "report")
            .expect("incomplete evidence must emit a non-terminal report");
        assert!(report["payload"].get("exit_proof").is_none_or(Value::is_null));
        assert_eq!(report["payload"]["verification"]["passed"], Value::Null);
    }
}

#[test]
fn exact_running_identity_and_clean_waited_exit_retains_unknown_without_owner_evidence() {
    let mut fixture = setup_running();
    let mut observer = DeterministicObserver::new(exited(&fixture.expected, true, None));

    let event = fixture.runner.observe_owned_process(&mut observer, &fixture.expected, 100).unwrap();

    assert!(matches!(event, ObserverEvent::Exited { .. }));
    assert_owner_evidence_required(&fixture.runner_path, &fixture.run_id);
    ingest_outbox(&mut fixture);
    assert!(!fixture.store.assignment(&fixture.run_id, 1).unwrap().unwrap().released);
}

