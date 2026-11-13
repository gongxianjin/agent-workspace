//! Service/Projects/Store boundary tests. All approvals and signed ingress are
//! synthetic local fixtures, NOT Linux, live AO/Reason/model, or native ownership
//! evidence. The configured IPC endpoint is deliberately absent; no worker runs.
use ao_reason_workbench::{
    config::{Actor, Mode, NodeSpec},
    core::{
        projects::Projects,
        protocol::{Envelope, PROTOCOL},
        service::{AppSnapshot, DecisionKind, ServiceHandle, ServiceOptions, UiCommand},
        store::{Run, Store},
    },
};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, sync::Arc, time::{Duration, Instant}};

fn now() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn node() -> NodeSpec {
    NodeSpec { id: "B".into(), address: "owned-B/runner".into(), key_hex: "07".repeat(32),
        slots: 1, admitted: true, healthy: true, draining: false,
        templates: vec!["project-work-v1".into()] }
}

fn signed(run: &Run, kind: &str, payload: Value) -> Envelope {
    let time = now();
    let mut envelope = Envelope { schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
        deployment: "project-approval-fixture".into(), from: "B".into(), to: "A".into(),
        run_id: run.id.clone(), attempt: run.attempt, epoch: run.epoch.clone(),
        kind: kind.into(), sent_at: time, expires_at: time + 300, payload, mac: String::new() };
    envelope.sign(&[7; 32]).unwrap();
    envelope
}

struct Fixture {
    _root: tempfile::TempDir,
    state: PathBuf,
    host: PathBuf,
    project_id: String,
    action_id: String,
    run: Run,
    expires: u64,
}

impl Fixture {
    fn new() -> Self {
        let root = tempfile::tempdir().unwrap();
        let state = root.path().join("workflow");
        let host = root.path().join("host/controller.sqlite");
        let mut projects = Projects::open(&state).unwrap();
        let project_id = projects.command(UiCommand::ProjectCreate { name: "Approval routing".into() })
            .unwrap()["id"].as_str().unwrap().to_owned();
        let text = "# Confirmed scope\nKeep inventory nonnegative.\n";
        let document = root.path().join("requirements.md");
        std::fs::write(&document, text).unwrap();
        projects.command(UiCommand::ProjectImport { project_id: project_id.clone(), path: document }).unwrap();
        projects.command(UiCommand::ProjectConfirm { project_id: project_id.clone(), version: 1,
            sha256: hex::encode(Sha256::digest(text.as_bytes())) }).unwrap();
        #[cfg(windows)]
        let endpoint = format!(r"\\.\pipe\absent-project-approval-{}", uuid::Uuid::new_v4());
        #[cfg(not(windows))]
        let endpoint = root.path().join("absent-reason.sock").to_string_lossy().into_owned();
        std::fs::write(root.path().join("node.key"), node().key_hex).unwrap();
        let config_path = root.path().join("controller.json");
        std::fs::write(&config_path, serde_json::to_vec(&json!({
            "schema": "ao-reason-config/1", "mode": "real", "role": "controller",
            "deployment": "project-approval-fixture", "state_dir": root.path().join("host"),
            "nodes": [{"id": "B", "address": "owned-B/runner", "key_file": "node.key", "slots": 1,
                "templates": ["project-work-v1"], "admitted": true}],
            "ao": {"base_url": "http://127.0.0.1:1", "project_id": "fixture-only", "model": "fixture-only"},
            "reason_endpoint": endpoint, "endpoint": "owned-A/controller"
        })).unwrap()).unwrap();
        projects.command(UiCommand::ProjectConfigure { config_path }).unwrap();
        projects.command(UiCommand::ProjectSplit { project_id: project_id.clone() }).unwrap();
        let mut store = Store::open(&host, "project-approval-fixture", &[node()]).unwrap();
        let run = store.list_runs(10).unwrap().remove(0);
        let action_id = run.detail["project_work"]["action_id"].as_str().unwrap().to_owned();
        assert_eq!(store.assign_next(now()).unwrap().unwrap().run_id, run.id);
        store.receive(&signed(&run, "status", json!({"state": "running", "spawn_intent": true,
            "session": "project-session"})), now()).unwrap();
        let expires = now() + 300;
        store.receive(&signed(&run, "approval", json!({
            "session": "project-session", "request_id": "project-request", "generation": 7,
            "scope_hash": "project-scope-v7", "expires_at": expires, "offered": ["allow_once"],
            "summary": "Approve only the scoped project operation"
        })), now()).unwrap();
        drop(store);
        drop(projects); // Release the real project Controller lock before service reopening.
        Self { _root: root, state, host, project_id, action_id, run, expires }
    }

    fn start(&self) -> ServiceHandle {
        ServiceHandle::spawn(ServiceOptions { mode: Mode::Real, state_dir: self.state.clone(),
            worker_exe: self.state.join("absent-local-fixture.exe"), gateway_key: None, gateway_model_token: None }, Arc::new(|| {})).unwrap()
    }

    fn decision(&self) -> UiCommand {
        UiCommand::Decide { run_id: self.run.id.clone(), attempt: 1, session: "project-session".into(),
            request_id: "project-request".into(), generation: 7, scope_hash: "project-scope-v7".into(),
            decision_id: "allow_once".into() }
    }

    fn update_run(&self, edit: impl FnOnce(&mut Run)) {
        let db = Connection::open(&self.host).unwrap();
        let body: String = db.query_row("SELECT body FROM runs WHERE id=?1", [&self.run.id], |r| r.get(0)).unwrap();
        let mut run: Run = serde_json::from_str(&body).unwrap();
        edit(&mut run);
        db.execute("UPDATE runs SET body=?1,status=?2,attempt=?3 WHERE id=?4",
            params![serde_json::to_string(&run).unwrap(), run.status, run.attempt, run.id]).unwrap();
    }
}

fn wait_snapshot(handle: &mut ServiceHandle, predicate: impl Fn(&AppSnapshot) -> bool) -> Arc<AppSnapshot> {
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        let snapshot = handle.snapshot();
        if predicate(&snapshot) { return snapshot; }
        assert!(Instant::now() < end, "project approval projection not observed: {snapshot:?}");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn reply(handle: &mut ServiceHandle, command: UiCommand) -> Result<Value, ao_reason_workbench::core::service::ServiceError> {
    let id = handle.try_submit(command).unwrap();
    let end = Instant::now() + Duration::from_secs(5);
    loop {
        for result in handle.drain_results() {
            assert_eq!(result.request_id, id);
            return result.result;
        }
        assert!(Instant::now() < end, "project decision result not returned");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn logical_decisions(path: &Path) -> Value {
    let db = Connection::open(path).unwrap();
    let rows = |sql: &str| -> Vec<String> {
        db.prepare(sql).unwrap().query_map([], |r| r.get(0)).unwrap()
            .collect::<Result<Vec<String>, _>>().unwrap()
    };
    json!({
        "approvals": rows("SELECT json_array(run,attempt,session,request,body,decided) FROM approvals ORDER BY run,attempt,session,request"),
        "intents": rows("SELECT json_array(run,attempt,session,request,decision) FROM resolve_intents ORDER BY run,attempt,session,request"),
        "decisions": rows("SELECT body FROM outbox WHERE json_extract(body,'$.kind')='decision' ORDER BY id")
    })
}

#[test]
fn project_pending_approval_is_projected_to_app_snapshot() {
    // A fixture-only projection, missing project projection, or stale identity must fail this.
    let fixture = Fixture::new();
    let mut handle = fixture.start();
    let snapshot = wait_snapshot(&mut handle, |s| !s.approvals.is_empty());
    assert_eq!(snapshot.approvals.len(), 1);
    let approval = &snapshot.approvals[0];
    assert_eq!(approval.run_id, fixture.run.id);
    assert_eq!(approval.attempt, 1);
    assert_eq!(approval.session, "project-session");
    assert_eq!(approval.request_id, "project-request");
    assert_eq!(approval.generation, 7);
    assert_eq!(approval.scope_hash, "project-scope-v7");
    assert_eq!(approval.expires_at, fixture.expires);
    assert_eq!(approval.summary, "Approve only the scoped project operation");
    assert!(!approval.simulated);
    assert!(approval.can_decide);
    assert_eq!(approval.offered.len(), 1);
    assert_eq!(approval.offered[0].id, "allow_once");
    assert_eq!(approval.offered[0].kind, DecisionKind::AllowOnce);
    assert!(!fixture.state.join("local-host.json").exists());
    assert!(!fixture.state.join("runner.sqlite").exists());
    // A refresh replaces, rather than appends or retains, durable approval state.
    let db = Connection::open(&fixture.host).unwrap();
    db.execute("UPDATE approvals SET decided=1", []).unwrap();
    wait_snapshot(&mut handle, |s| s.approvals.is_empty());
    handle.stop();
}

#[test]
fn valid_project_decide_routes_to_project_controller() {
    // Swallowing Decide or writing to the fixture ledger must fail this test.
    let fixture = Fixture::new();
    let local_node = NodeSpec { templates: vec!["control-native-v1".into()], ..node() };
    let mut local = Store::open(&fixture.state.join("controller.sqlite"), "untouched-local-fixture", &[local_node]).unwrap();
    local.dispatch(&Actor::Local("fixture-owner".into()), "control-native-v1", now()).unwrap();
    drop(local);
    let local_before = logical_decisions(&fixture.state.join("controller.sqlite"));
    let mut handle = fixture.start();
    let result = reply(&mut handle, fixture.decision()).expect("project decision must reach the host controller without LocalRuntime");
    assert_eq!(result, json!({"state": "queued", "meaning": "persisted decision intent; not proof of effect"}));
    let db = Connection::open(&fixture.host).unwrap();
    let intent: (String, u32, String, String, String) = db.query_row(
        "SELECT run,attempt,session,request,decision FROM resolve_intents", [],
        |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
    assert_eq!(intent, (fixture.run.id.clone(), 1, "project-session".into(), "project-request".into(), "allow_once".into()));
    assert_eq!(db.query_row("SELECT decided FROM approvals", [], |r| r.get::<_, i64>(0)).unwrap(), 1);
    let decision: String = db.query_row("SELECT body FROM outbox WHERE json_extract(body,'$.kind')='decision'", [], |r| r.get(0)).unwrap();
    let decision: Value = serde_json::from_str(&decision).unwrap();
    assert_eq!(decision["run_id"], fixture.run.id);
    assert_eq!(decision["payload"], json!({"session":"project-session", "request_id":"project-request",
        "generation":7, "scope_hash":"project-scope-v7", "decision_id":"allow_once"}));
    assert_eq!(logical_decisions(&fixture.state.join("controller.sqlite")), local_before);
    wait_snapshot(&mut handle, |s| s.approvals.is_empty());
    assert!(!fixture.state.join("local-host.json").exists());
    handle.stop();
}

#[test]
fn wrong_owner_run_is_rejected_without_mutation() {
    // Merely matching a run id/action link is not enough to authorize a project.
    let fixture = Fixture::new();
    fixture.update_run(|r| r.owner = Actor::Local("another-project".into()));
    let before = logical_decisions(&fixture.host);
    let mut handle = fixture.start();
    let error = reply(&mut handle, fixture.decision()).expect_err("wrong project owner must not decide");
    assert_eq!(error.code, "project_decision_rejected", "must reject at the project route, not generic not_started");
    assert_eq!(logical_decisions(&fixture.host), before);
    assert!(handle.snapshot().approvals.is_empty());
    handle.stop();
}

#[test]
fn stale_attempt_or_session_cannot_decide_project_approval() {
    // Omitting the GUI execution binding permits a stale button to authorize a new execution.
    let fixture = Fixture::new();
    let mut handle = fixture.start();
    wait_snapshot(&mut handle, |s| s.approvals.len() == 1);
    let before = logical_decisions(&fixture.host);
    for stale in [0, 1] {
        let mut command = fixture.decision();
        if let UiCommand::Decide { attempt, session, .. } = &mut command {
            if stale == 0 { *attempt = 2; } else { *session = "old-project-session".into(); }
        }
        assert!(reply(&mut handle, command).is_err());
        assert_eq!(logical_decisions(&fixture.host), before);
    }
    handle.stop();
}

#[test]
fn stale_approval_scope_and_unoffered_decision_are_rejected() {
    // Weaker parsing or decision validation must not bypass current Store approval scope.
    let fixture = Fixture::new();
    let mut handle = fixture.start();
    wait_snapshot(&mut handle, |s| s.approvals.len() == 1);
    let before = logical_decisions(&fixture.host);
    for stale in 0..4 {
        let mut command = fixture.decision();
        if let UiCommand::Decide { request_id, generation, scope_hash, decision_id, .. } = &mut command {
            match stale { 0 => *request_id = "old-request".into(), 1 => *generation = 6,
                2 => *scope_hash = "old-scope".into(), _ => *decision_id = "reject_once".into() }
        }
        assert!(reply(&mut handle, command).is_err());
        assert_eq!(logical_decisions(&fixture.host), before);
    }
    let db = Connection::open(&fixture.host).unwrap();
    let raw: String = db.query_row("SELECT body FROM approvals", [], |r| r.get(0)).unwrap();
    let mut approval: Value = serde_json::from_str(&raw).unwrap();
    approval["expires_at"] = json!(now().saturating_sub(1));
    db.execute("UPDATE approvals SET body=?1", [approval.to_string()]).unwrap();
    let expired_before = logical_decisions(&fixture.host);
    assert!(reply(&mut handle, fixture.decision()).is_err());
    assert_eq!(logical_decisions(&fixture.host), expired_before);
    wait_snapshot(&mut handle, |s| s.approvals.is_empty());
    handle.stop();
}

#[test]
fn real_decide_never_falls_back_to_local_fixture() {
    // An unavailable project host is a deterministic error, not a local start/fallback.
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("must-not-be-created");
    let mut handle = ServiceHandle::spawn(ServiceOptions { mode: Mode::Real, state_dir: state.clone(),
        worker_exe: root.path().join("absent-worker.exe"), gateway_key: None, gateway_model_token: None }, Arc::new(|| {})).unwrap();
    let result = reply(&mut handle, UiCommand::Decide { run_id: "fixture-run".into(), attempt: 1,
        session: "fixture-session".into(), request_id: "fixture-request".into(), generation: 1,
        scope_hash: "fixture-scope".into(), decision_id: "allow_once".into() });
    let error = result.expect_err("no real project host is configured");
    assert_eq!(error.code, "project_host_unavailable");
    assert!(!state.exists(), "decide must not initialize a local fixture or workflow");
    handle.stop();
}

#[test]
fn broken_or_ambiguous_project_action_binding_is_not_projected_or_decided() {
    // A forged work action id, missing link, or duplicate snapshot link fails closed.
    for corrupt in 0..3 {
        let fixture = Fixture::new();
        if corrupt == 0 {
            fixture.update_run(|r| r.detail["project_work"]["action_id"] = json!("foreign-action"));
        } else {
            let db = Connection::open(fixture.state.join("project-workflow/controller.sqlite")).unwrap();
            if corrupt == 1 {
                db.execute("DELETE FROM project_settings WHERE key=?1", [format!("action:{}", fixture.action_id)]).unwrap();
            } else {
                let raw: String = db.query_row("SELECT body FROM projects WHERE id=?1", [&fixture.project_id], |r| r.get(0)).unwrap();
                let mut snapshot: Value = serde_json::from_str(&raw).unwrap();
                let duplicate = snapshot["actions"][0].clone();
                snapshot["actions"].as_array_mut().unwrap().push(duplicate);
                db.execute("UPDATE projects SET body=?1 WHERE id=?2", params![snapshot.to_string(), fixture.project_id]).unwrap();
            }
        }
        let before = logical_decisions(&fixture.host);
        let mut handle = fixture.start();
        let error = reply(&mut handle, fixture.decision()).expect_err("broken binding must not decide");
        assert_eq!(error.code, "project_decision_rejected");
        assert_eq!(logical_decisions(&fixture.host), before);
        assert!(handle.snapshot().approvals.is_empty());
        handle.stop();
    }
}
