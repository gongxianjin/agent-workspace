//! Real Store/Projects/Service paths with owned local signed-ready IPC fixtures.
//! No live Reason/AO/model, native GUI, Linux, process observer or deployment.
#[path = "support/lifecycle_reason.rs"]
mod lifecycle_reason;
use lifecycle_reason::LocalReason;
use ao_reason_workbench::{config::Mode, core::{projects::Projects, service::{AppSnapshot, Readiness, ServiceHandle, ServiceOptions, UiCommand}}};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, sync::Arc, time::{Duration, Instant}};

fn configured(root: &Path, endpoint: &str) -> (Projects, PathBuf) {
    let state = root.join("workflow");
    let mut projects = Projects::open(&state).unwrap();
    let id = projects.command(UiCommand::ProjectCreate { name: "idle lifecycle".into() }).unwrap()["id"].as_str().unwrap().to_owned();
    let text = "# Confirmed\nProbe recovery must not call a model.\n";
    let doc = root.join("scope.md");
    std::fs::write(&doc, text).unwrap();
    projects.command(UiCommand::ProjectImport { project_id: id.clone(), path: doc }).unwrap();
    projects.command(UiCommand::ProjectConfirm { project_id: id, version: 1, sha256: hex::encode(Sha256::digest(text.as_bytes())) }).unwrap();
    std::fs::write(root.join("node.key"), "07".repeat(32)).unwrap();
    let cfg = root.join("controller.json");
    std::fs::write(&cfg, serde_json::to_vec(&json!({
        "schema":"ao-reason-config/1","mode":"real","role":"controller","deployment":"lifecycle-test",
        "state_dir":root.join("host"),"nodes":[{"id":"B","address":"owned-B/runner","key_file":"node.key","slots":1,
            "templates":["sum-v1","project-work-v1"],"admitted":true}],
        "ao":{"base_url":"http://127.0.0.1:1","project_id":"unused","model":"unused"},
        "reason_endpoint":endpoint,"endpoint":"owned-A/controller"
    })).unwrap()).unwrap();
    projects.command(UiCommand::ProjectConfigure { config_path: cfg }).unwrap();
    (projects, state)
}
fn service(state: &Path) -> ServiceHandle {
    ServiceHandle::spawn(ServiceOptions { mode: Mode::Real, state_dir: state.into(), worker_exe: state.join("unused.exe"), gateway_key: None, gateway_model_token: None }, Arc::new(||{})).unwrap()
}
fn wait(handle: &mut ServiceHandle, predicate: impl Fn(&AppSnapshot)->bool) -> Arc<AppSnapshot> {
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = handle.snapshot();
        if predicate(&snapshot) { return snapshot; }
        assert!(Instant::now() < deadline, "lifecycle deadline: {snapshot:?}");
        std::thread::sleep(Duration::from_millis(5));
    }
}
fn stop(handle: &mut ServiceHandle) { handle.stop(); wait(handle, |snapshot|!snapshot.running); }

#[test]
fn local_fixture_supports_repeated_reason_inbox() {
    let root = tempfile::tempdir().unwrap();
    let fixture = LocalReason::new(root.path(), false);
    let reason = ao_reason_workbench::adapters::reason::Reason::new(fixture.endpoint.clone()).unwrap();
    for _ in 0..3 { reason.inbox("owned-A/controller").expect("real adapter local IPC fixture contract"); }
}

#[test]
fn idle_service_sends_probe_and_consumes_signed_ready_without_dispatch() {
    // Break caught: gating p.tick on unfinished prevents configured GUI bootstrap.
    let root = tempfile::tempdir().unwrap();
    let reason = LocalReason::new(root.path(), true);
    let (projects, state) = configured(root.path(), &reason.endpoint);
    assert_eq!(projects.unfinished(), 0);
    drop(projects);
    let mut handle = service(&state);
    let deadline = Instant::now() + Duration::from_secs(2);
    while reason.probes().is_empty() && Instant::now() < deadline { std::thread::sleep(Duration::from_millis(5)); }
    let sent = reason.probes();
    if sent.is_empty() { stop(&mut handle); panic!("idle configured service never sent its signed probe"); }
    let snapshot = wait(&mut handle, |s|s.readiness == Readiness::Ready);
    assert_eq!(snapshot.unfinished, 0);
    assert!(snapshot.nodes[0].healthy);
    assert!(snapshot.projects[0].can_split);
    assert!(snapshot.projects[0].blockers.is_empty());
    assert_eq!(reason.starts(), 0, "readiness must not auto-dispatch project work");
    stop(&mut handle);
}

#[test]
fn service_transport_failure_publishes_unready_and_recovers_actions() {
    // Break caught: service computes readiness before a failing engine tick.
    let root = tempfile::tempdir().unwrap();
    let reason = LocalReason::new(root.path(), true);
    let (projects, state) = configured(root.path(), &reason.endpoint);
    drop(projects);
    let mut handle = service(&state);
    wait(&mut handle, |s|s.readiness == Readiness::Ready);
    reason.fail_inbox(true);
    let failed = wait(&mut handle, |s|s.errors.iter().any(|e|e.code == "project_execution_pending"));
    let gated = failed.readiness == Readiness::NotReady && !failed.projects[0].can_split && failed.nodes.iter().all(|n|!n.healthy);
    if !gated { stop(&mut handle); panic!("failed transport snapshot must not publish previous Ready/actions: {failed:?}"); }
    reason.fail_inbox(false);
    let recovered = wait(&mut handle, |s|s.readiness == Readiness::Ready && s.projects[0].blockers.is_empty());
    assert!(recovered.projects[0].can_split);
    assert_eq!(reason.starts(), 0);
    stop(&mut handle);
}

#[test]
fn transport_recovery_emits_fresh_probe_and_accepts_fresh_ready_without_reopen() {
    // Break caught: engine only probes in open and never after invalidate.
    let root = tempfile::tempdir().unwrap();
    let reason = LocalReason::new(root.path(), true);
    let (mut projects, _) = configured(root.path(), &reason.endpoint);
    projects.tick().unwrap();
    projects.tick().unwrap();
    let now = reason.probes()[0].sent_at;
    assert!(projects.node_readiness(now).unwrap()[0].healthy);
    let old = reason.probes()[0].clone();
    reason.fail_inbox(true);
    assert!(projects.tick().is_err());
    assert!(!projects.node_readiness(now).unwrap()[0].healthy);
    reason.fail_inbox(false);
    projects.tick().unwrap();
    let probes = reason.probes();
    assert_eq!(probes.len(), 2, "successful recovered inbox must send exactly one fresh probe");
    assert_ne!(old.run_id, probes[1].run_id);
    assert_ne!(old.epoch, probes[1].epoch);
    projects.tick().unwrap();
    assert!(projects.node_readiness(now).unwrap()[0].healthy);
    assert_eq!(reason.probes().len(), 2);
    assert_eq!(reason.starts(), 0);
}

#[test]
fn delayed_signed_old_ready_is_discarded_by_engine_before_fresh_recovery() {
    // Break caught: rejected old ready left in the inbox poisons all later ticks.
    let root = tempfile::tempdir().unwrap();
    let reason = LocalReason::new(root.path(), true);
    let (mut projects, _) = configured(root.path(), &reason.endpoint);
    projects.tick().unwrap(); projects.tick().unwrap();
    let old = reason.probes()[0].clone();
    reason.fail_inbox(true); assert!(projects.tick().is_err());
    reason.replay_ready(&old);
    reason.fail_inbox(false);
    projects.tick().expect("signed obsolete ready must not poison recovered transport");
    assert!(!projects.node_readiness(old.sent_at).unwrap()[0].healthy, "old ready cannot restore readiness");
    assert_eq!(reason.probes().len(), 2);
    projects.tick().unwrap();
    assert!(projects.node_readiness(old.sent_at).unwrap()[0].healthy);
    assert_eq!(reason.starts(), 0);
}

#[test]
fn pending_probe_survives_ticks_until_its_delayed_signed_ready() {
    // Break caught: unconditional re-probe per tick prevents ready correlation.
    let root = tempfile::tempdir().unwrap();
    let reason = LocalReason::new(root.path(), false);
    let (mut projects, _) = configured(root.path(), &reason.endpoint);
    for _ in 0..4 { projects.tick().unwrap(); }
    assert_eq!(reason.probes().len(), 1, "unexpired pending probe must keep its identity");
    let now = reason.probes()[0].sent_at;
    assert!(!projects.node_readiness(now).unwrap()[0].healthy);
    reason.enable_ready();
    projects.tick().unwrap();
    assert!(projects.node_readiness(now).unwrap()[0].healthy);
    assert_eq!(reason.probes().len(), 1);
}

#[test]
fn recovery_retains_report_pending_exit_reservation_and_does_not_redispatch() {
    assert_unresolved_recovery(true);
}

#[test]
fn recovery_retains_unknown_reservation_and_does_not_redispatch() {
    assert_unresolved_recovery(false);
}

fn assert_unresolved_recovery(report_only: bool) {
    // Compatibility control: no exit proof, release, retry or automatic reassignment.
    let root = tempfile::tempdir().unwrap();
    let reason = LocalReason::new(root.path(), true);
    let (mut projects, _) = configured(root.path(), &reason.endpoint);
    projects.tick().unwrap(); projects.tick().unwrap();
    let id = projects.snapshots()[0].id.clone();
    projects.command(UiCommand::ProjectSplit { project_id: id.clone() }).unwrap();
    projects.tick().unwrap();
    let db = rusqlite::Connection::open(root.path().join("host/controller.sqlite")).unwrap();
    let raw: String = db.query_row("SELECT body FROM runs", [], |r|r.get(0)).unwrap();
    let run: ao_reason_workbench::core::store::Run = serde_json::from_str(&raw).unwrap();
    reason.queue_run_event(&run, "status", json!({"state":if report_only {"running"}else{"unknown"},"spawn_intent":true,"session":"owned-session"}));
    if report_only {
        let work = &run.detail["project_work"];
        reason.queue_run_event(&run, "report", json!({"session":"owned-session","verification":{"passed":true},"report":{"project_result":{
            "schema":"project-result/1","project_id":id,"action_id":work["action_id"],"stage":"split",
            "document_version":work["document"]["version"],"document_sha256":work["document"]["sha256"],
            "tasks":[{"id":"inventory","title":"Inventory","description":"Deduct atomically","acceptance":"Nonnegative","depends_on":[]}]
        }}}));
    }
    projects.tick().unwrap();
    let expected = if report_only {"report_pending_exit"}else{"unknown"};
    assert_eq!(projects.snapshots()[0].actions[0].state, expected);
    reason.fail_inbox(true); assert!(projects.tick().is_err());
    reason.fail_inbox(false); projects.tick().unwrap(); projects.tick().unwrap();
    let recovered = &projects.snapshots()[0];
    assert_eq!(recovered.actions[0].state, expected);
    assert_eq!(projects.unfinished(), 1);
    assert!(!recovered.can_split && !recovered.can_start);
    if report_only { assert!(recovered.blockers.iter().any(|b|b.contains("退出")), "independent exit blocker must remain"); }
    assert!(recovered.blockers.iter().all(|b|!b.contains("Reason/transport")));
    let reserved: i64 = db.query_row("SELECT count(*) FROM assignments WHERE released=0", [], |r|r.get(0)).unwrap();
    assert_eq!(reserved, 1); assert_eq!(reason.starts(), 1, "recovery must not dispatch again");
    assert_eq!(reason.probes().len(), 2);
}

#[test]
fn recovery_retains_independent_business_blocker() {
    assert_business_blocker_survives("business: independent review required");
}

#[test]
fn recovery_retains_business_blocker_even_when_it_mentions_transport_readiness() {
    assert_business_blocker_survives("business: transport readiness review required");
}

fn assert_business_blocker_survives(business: &str) {
    // Break caught: replacing blockers on failure or substring cleanup on success.
    let root = tempfile::tempdir().unwrap();
    let reason = LocalReason::new(root.path(), true);
    let (mut projects, state) = configured(root.path(), &reason.endpoint);
    let db = rusqlite::Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    let raw: String = db.query_row("SELECT body FROM projects", [], |r|r.get(0)).unwrap();
    let mut row: serde_json::Value = serde_json::from_str(&raw).unwrap();
    row["blockers"] = json!([business]);
    db.execute("UPDATE projects SET body=?1", [row.to_string()]).unwrap();
    drop(projects);
    projects = Projects::open(&state).unwrap();
    reason.fail_inbox(true);
    assert!(projects.tick().is_err());
    let during_failure = projects.snapshots()[0].blockers.clone();
    reason.fail_inbox(false);
    projects.tick().unwrap();
    projects.tick().unwrap();
    let recovered = &projects.snapshots()[0];
    assert_eq!(recovered.blockers, [business], "recovery must remove only its derived blocker; failure state was {during_failure:?}");
    assert!(recovered.can_split, "restored readiness must not leave stale derived action gating");
    assert!(during_failure.iter().any(|blocker| blocker == business), "failure must preserve business blockers");
    assert_eq!(during_failure.len(), 2, "failure must append its own derived blocker even when business text contains transport/readiness");
}

#[test]
fn service_gates_actions_with_existing_readiness_worded_business_blocker() {
    // Break caught: action gating is conditional on appending a new blocker.
    let root = tempfile::tempdir().unwrap();
    let reason = LocalReason::new(root.path(), false);
    let (projects, state) = configured(root.path(), &reason.endpoint);
    let db = rusqlite::Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    let raw: String = db.query_row("SELECT body FROM projects", [], |r|r.get(0)).unwrap();
    let mut row: serde_json::Value = serde_json::from_str(&raw).unwrap();
    let business = "business: transport readiness review required";
    row["blockers"] = json!([business]);
    db.execute("UPDATE projects SET body=?1", [row.to_string()]).unwrap();
    drop(projects);
    let mut handle = service(&state);
    let snapshot = wait(&mut handle, |s| !s.projects.is_empty() && !s.nodes.is_empty());
    let disabled = !snapshot.projects[0].can_split && !snapshot.projects[0].can_start;
    if !disabled { stop(&mut handle); panic!("unready service must gate actions even with existing blocker: {snapshot:?}"); }
    reason.enable_ready();
    let recovered = wait(&mut handle, |s| s.readiness == Readiness::Ready);
    assert_eq!(recovered.projects[0].blockers, [business]);
    assert!(recovered.projects[0].can_split);
    assert!(!recovered.projects[0].can_start, "readiness must not bypass task-review workflow gates");
    assert_eq!(reason.starts(), 0);
    stop(&mut handle);
}
