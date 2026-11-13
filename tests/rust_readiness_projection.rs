use ao_reason_workbench::{
    config::{Actor, Mode, NodeSpec},
    core::{
        projects::Projects,
        protocol::{Envelope, PROTOCOL},
        service::{ServiceHandle, ServiceOptions, UiCommand},
        store::Store,
    },
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, sync::Arc, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(), address: "owned-B/runner".into(), key_hex: "07".repeat(32),
        slots: 1, admitted: true, healthy: false, draining: false,
        templates: vec!["sum-v1".into(), "project-work-v1".into()],
    }
}

fn signed_ready(probe: &Envelope, payload: Value, sent_at: u64, expires_at: u64) -> Envelope {
    let mut ready = Envelope {
        schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
        deployment: probe.deployment.clone(), from: "B".into(), to: "A".into(),
        run_id: probe.run_id.clone(), attempt: probe.attempt, epoch: probe.epoch.clone(),
        kind: "ready".into(), sent_at, expires_at, payload, mac: String::new(),
    };
    ready.sign(&hex::decode("07".repeat(32)).unwrap()).unwrap();
    ready
}

fn ready_payload() -> Value {
    json!({"address":"owned-B/runner", "templates":["sum-v1", "project-work-v1"], "ao_project_ready":true})
}

fn config_fixture(root: &Path) -> PathBuf {
    std::fs::write(root.join("node.key"), "07".repeat(32)).unwrap();
    let endpoint = if cfg!(windows) {
        format!(r"\\.\pipe\missing-readiness-{}", uuid::Uuid::new_v4())
    } else {
        root.join("missing-reason.sock").display().to_string()
    };
    let config = json!({
        "schema":"ao-reason-config/1", "mode":"real", "role":"controller",
        "deployment":"readiness-projection-fixture", "state_dir":root.join("host"),
        "nodes":[{"id":"B","address":"owned-B/runner","key_file":"node.key","slots":1,
            "templates":["sum-v1","project-work-v1"],"admitted":true,"healthy":true}],
        "ao":{"base_url":"http://127.0.0.1:1","project_id":"fixture","model":"fixture-only"},
        "reason_endpoint":endpoint, "endpoint":"owned-A/controller"
    });
    let path = root.join("controller.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    path
}

#[test]
fn configured_real_node_is_not_assignable_before_current_verified_ready() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(&temp.path().join("controller.sqlite"), "readiness-test", &[node()]).unwrap();
    let run = store.dispatch(&Actor::Local("owner".into()), "sum-v1", now()).unwrap();
    assert!(store.assign_next(now()).unwrap().is_none(), "valid configuration must not imply readiness");

    let probe_at = now();
    store.probe_nodes(probe_at).unwrap();
    let (_, probe) = store.pending_outbox(10).unwrap().into_iter()
        .find(|(_, message)| message.kind == "probe").expect("current signed probe");
    store.receive(&signed_ready(&probe, ready_payload(), probe_at + 1, probe_at + 60), probe_at + 1).unwrap();
    let assignment = store.assign_next(probe_at + 1).unwrap().expect("current ready enables assignment");
    assert_eq!(assignment.run_id, run.id);
}

#[test]
fn stale_or_expired_ready_is_rejected_and_expiry_preserves_reservation() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(&temp.path().join("controller.sqlite"), "readiness-test", &[node()]).unwrap();
    store.probe_nodes(100).unwrap();
    let (_, probe) = store.pending_outbox(10).unwrap().into_iter()
        .find(|(_, message)| message.kind == "probe").unwrap();

    let mut stale = signed_ready(&probe, ready_payload(), 101, 160);
    stale.epoch = "stale-epoch".into();
    stale.sign(&hex::decode("07".repeat(32)).unwrap()).unwrap();
    assert!(store.receive(&stale, 101).is_err());

    let expired = signed_ready(&probe, ready_payload(), 101, 102);
    assert!(store.receive(&expired, 103).is_err());

    store.receive(&signed_ready(&probe, ready_payload(), 101, 160), 101).unwrap();
    let run = store.dispatch(&Actor::Local("owner".into()), "sum-v1", 101).unwrap();
    let assignment = store.assign_next(101).unwrap().unwrap();
    assert_eq!(assignment.run_id, run.id);
    assert!(!assignment.released);

    store.expire(161).unwrap();
    assert!(!store.assignment(&run.id, run.attempt).unwrap().unwrap().released,
        "readiness invalidation must not release an existing reservation");
    let queued = store.dispatch(&Actor::Local("owner-2".into()), "sum-v1", 161).unwrap();
    assert!(store.assign_next(161).unwrap().is_none(), "expired readiness must block new assignment");
    assert_eq!(store.run(&queued.id).unwrap().status, "queued");
}

#[test]
fn real_service_projects_configured_node_and_readiness_blocker_without_fixture_fallback() {
    let temp = tempfile::tempdir().unwrap();
    let workflow = temp.path().join("workflow");
    let mut projects = Projects::open(&workflow).unwrap();
    let project = projects.command(UiCommand::ProjectCreate { name: "readiness projection".into() }).unwrap();
    let id = project["id"].as_str().unwrap().to_owned();
    projects.command(UiCommand::ProjectConfigure { config_path: config_fixture(temp.path()) }).unwrap();
    drop(projects);

    let mut service = ServiceHandle::spawn(ServiceOptions {
        mode: Mode::Real, state_dir: workflow, worker_exe: temp.path().join("unused.exe"), gateway_key: None, gateway_model_token: None,
    }, Arc::new(|| {})).unwrap();
    let deadline = Instant::now() + Duration::from_secs(3);
    loop {
        let snapshot = service.snapshot();
        if snapshot.projects.iter().any(|project| project.id == id && !project.blockers.is_empty()) {
            assert_eq!(snapshot.nodes.len(), 1, "real configured node must be projected");
            assert_eq!(snapshot.nodes[0].id, "B");
            assert!(!snapshot.nodes[0].healthy, "configured real node must remain unready until signed ready");
            assert!(snapshot.nodes[0].source.contains("readiness"));
            assert_eq!(snapshot.readiness, ao_reason_workbench::core::service::Readiness::NotReady);
            break;
        }
        assert!(Instant::now() < deadline, "real node/blocker projection deadline: {:?}", snapshot);
        std::thread::sleep(Duration::from_millis(15));
    }
    service.stop();
}

#[test]
fn transport_failure_invalidates_readiness_without_releasing_existing_or_retrying_unknown() {
    let temp = tempfile::tempdir().unwrap();
    let mut projects = Projects::open(&temp.path().join("workflow")).unwrap();
    let id = projects.command(UiCommand::ProjectCreate { name: "transport readiness".into() }).unwrap()["id"].as_str().unwrap().to_owned();
    let document = "# Confirmed\nTransport failure must retain reservations.\n";
    let path = temp.path().join("design.md");
    std::fs::write(&path, document).unwrap();
    projects.command(UiCommand::ProjectImport { project_id: id.clone(), path }).unwrap();
    projects.command(UiCommand::ProjectConfirm {
        project_id: id.clone(), version: 1, sha256: hex::encode(Sha256::digest(document.as_bytes())),
    }).unwrap();
    let config = config_fixture(temp.path());
    projects.command(UiCommand::ProjectConfigure { config_path: config }).unwrap();
    // This fixture keeps the host queue limited to the existing reservation;
    // ProjectSplit is covered by the separate lifecycle regression.
    let mut host = Store::open(&temp.path().join("host/controller.sqlite"), "readiness-projection-fixture", &[node()]).unwrap();
    host.probe_nodes(now()).unwrap();
    let (_, probe) = host.pending_outbox(20).unwrap().into_iter()
        .filter(|(_, message)| message.kind == "probe")
        .max_by_key(|(_, message)| message.sent_at)
        .unwrap();
    host.receive(&signed_ready(&probe, ready_payload(), 201, 260), 201).unwrap();
    let existing = host.dispatch(&Actor::Local("existing-owner".into()), "sum-v1", 201).unwrap();
    let assignment = host.assign_next(201).unwrap().expect("verified readiness permits existing reservation");
    let existing_id = assignment.run_id.clone();
    assert_eq!(existing_id, existing.id);

    // A fresh probe supersedes the prior signed ready. No new ready arrives,
    // which models transport failure/expiry without touching the existing run.
    host.probe_nodes(202).unwrap();
    assert!(!host.assignment(&existing.id, existing.attempt).unwrap().unwrap().released,
        "readiness invalidation must not release an existing reservation");
    let queued = host.dispatch(&Actor::Local("queued-owner".into()), "sum-v1", 202).unwrap();
    assert!(host.assign_next(202).unwrap().is_none(),
        "missing current ready must block new assignment");
    assert_eq!(host.run(&queued.id).unwrap().status, "queued");

    let error = projects.tick().expect_err("missing Reason transport must remain observable");
    assert!(error.to_string().contains("project transport unavailable"));
    assert!(projects.snapshots()[0].blockers.iter().any(|blocker| blocker.contains("readiness") || blocker.contains("transport")));
}

#[test]
fn invalidated_readiness_rejects_delayed_old_ready_until_fresh_probe() {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(&temp.path().join("controller.sqlite"), "readiness-lifecycle", &[node()]).unwrap();
    store.probe_nodes(100).unwrap();
    let (_, old_probe) = store.pending_outbox(10).unwrap().into_iter()
        .find(|(_, message)| message.kind == "probe").unwrap();
    store.receive(&signed_ready(&old_probe, ready_payload(), 101, 160), 101).unwrap();

    store.invalidate_readiness().unwrap();
    let delayed = signed_ready(&old_probe, ready_payload(), 102, 160);
    assert!(store.receive(&delayed, 102).is_err(),
        "a delayed ready from before invalidation must not restore readiness");
    assert!(!store.readiness(102).unwrap()[0].healthy);

    store.probe_nodes(103).unwrap();
    let (_, fresh_probe) = store.pending_outbox(10).unwrap().into_iter()
        .filter(|(_, message)| message.kind == "probe")
        .max_by_key(|(_, message)| message.sent_at)
        .unwrap();
    assert_ne!(fresh_probe.run_id, old_probe.run_id);
    assert_ne!(fresh_probe.epoch, old_probe.epoch);
    assert!(store.receive(&signed_ready(&fresh_probe, ready_payload(), 104, 160), 104).is_ok());
    assert!(store.readiness(104).unwrap()[0].healthy);
}

#[test]
fn configured_idle_project_reprobes_after_expiry_without_unfinished_work() {
    let temp = tempfile::tempdir().unwrap();
    let mut projects = Projects::open(&temp.path().join("workflow")).unwrap();
    projects.command(UiCommand::ProjectCreate { name: "idle probe".into() }).unwrap();
    projects.command(UiCommand::ProjectConfigure { config_path: config_fixture(temp.path()) }).unwrap();
    assert_eq!(projects.unfinished(), 0);

    let first_at = now();
    let first = projects.node_readiness(first_at).unwrap();
    assert!(!first[0].healthy);
    let db = rusqlite::Connection::open(temp.path().join("host/controller.sqlite")).unwrap();
    let first_probe: String = db.query_row(
        "SELECT json_extract(value, '$.run_id') FROM metadata WHERE key='probe:B'", [], |row| row.get(0)).unwrap();

    let second = projects.node_readiness(first_at + 61).unwrap();
    assert!(!second[0].healthy);
    let second_probe: String = db.query_row(
        "SELECT json_extract(value, '$.run_id') FROM metadata WHERE key='probe:B'", [], |row| row.get(0)).unwrap();
    assert_ne!(first_probe, second_probe, "expiry must schedule a fresh probe while idle");
}
