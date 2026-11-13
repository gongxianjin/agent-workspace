//! Injected-time production Store/Runner round trips only; no live AO/Reason/model or worker.
use agentranch::{
    config::{Actor, NodeSpec},
    core::{protocol::Envelope, runner::Runner, store::{Run, Store}},
};
use rusqlite::{Connection, OpenFlags};
use serde_json::{json, Value};
use std::path::Path;

const DEPLOYMENT: &str = "wave7-readiness-roundtrip";
const KEY: [u8; 32] = [7; 32];
type RawMessage = (String, Vec<u8>);

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(), address: "owned-B/runner".into(), key_hex: hex::encode(KEY),
        slots: 1, admitted: true, healthy: false, draining: false,
        templates: vec!["sum-v1".into(), "project-work-v1".into()],
    }
}

fn open_pair(root: &Path) -> (Store, Runner) {
    let spec = node();
    (
        Store::open(&root.join("controller.sqlite"), DEPLOYMENT, std::slice::from_ref(&spec)).unwrap(),
        Runner::open(&root.join("runner.sqlite"), DEPLOYMENT, spec).unwrap(),
    )
}

fn readonly(root: &Path, name: &str) -> Connection {
    Connection::open_with_flags(root.join(name), OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap()
}

fn probe_metadata(root: &Path) -> Value {
    let raw: String = readonly(root, "controller.sqlite").query_row(
        "SELECT value FROM metadata WHERE key='probe:B'", [], |row| row.get(0),
    ).unwrap();
    serde_json::from_str(&raw).unwrap()
}

fn production_probe(store: &mut Store, root: &Path, now: u64) -> Envelope {
    store.probe_nodes(now).unwrap();
    let current = probe_metadata(root);
    let probes: Vec<Envelope> = store.pending_outbox_raw(100).unwrap().into_iter()
        .map(|(id, destination, body)| {
            let message: Envelope = serde_json::from_slice(&body).unwrap();
            assert_eq!(id, message.message_id);
            assert_eq!(destination, node().address);
            message
        })
        .filter(|message| message.kind == "probe" && current["run_id"] == message.run_id
            && current["epoch"] == message.epoch)
        .collect();
    assert_eq!(probes.len(), 1, "identify the actual current probe, not the pending queue head");
    let probe = probes.into_iter().next().unwrap();
    probe.verify(&KEY, DEPLOYMENT, "B", now).unwrap();
    assert_eq!(probe.sent_at, now);
    assert_eq!(probe.attempt, 1);
    probe
}

fn production_ready(runner: &mut Runner, probe: &Envelope, now: u64) -> RawMessage {
    let before = runner.pending_outbox_raw(100).unwrap();
    runner.accept_probe(probe, now, true).unwrap();
    let after = runner.pending_outbox_raw(100).unwrap();
    let added: Vec<_> = after.iter().filter(|(id, _)| !before.iter().any(|(old, _)| old == id)).collect();
    assert_eq!(added.len(), 1, "accept_probe must persist exactly one ready");
    let raw = added[0].clone();
    let ready: Envelope = serde_json::from_slice(&raw.1).unwrap();
    assert_eq!(raw.0, ready.message_id);
    assert_eq!(ready.kind, "ready");
    assert_eq!(ready.from, node().id);
    assert_eq!(ready.to, "A");
    assert_eq!(ready.run_id, probe.run_id);
    assert_eq!(ready.attempt, probe.attempt);
    assert_eq!(ready.epoch, probe.epoch);
    assert_eq!(ready.sent_at, now);
    assert_eq!(ready.payload, json!({"address":node().address, "templates":node().templates, "ao_project_ready":true}));
    ready.verify(&KEY, DEPLOYMENT, "A", now).unwrap();
    let unchanged: Vec<_> = after.into_iter().filter(|(id, _)| id != &raw.0).collect();
    assert_eq!(unchanged, before, "unrelated persisted outbox bodies must not be rewritten");
    raw
}

fn assert_bounds(raw: &RawMessage, probe: &Envelope, expiry: u64, ttl: u64) {
    let ready: Envelope = serde_json::from_slice(&raw.1).unwrap();
    assert_eq!(ready.expires_at, expiry);
    let actual_ttl = ready.expires_at.checked_sub(ready.sent_at).unwrap();
    assert_eq!(actual_ttl, ttl);
    assert!((1..=60).contains(&actual_ttl));
    assert!(ready.expires_at <= probe.sent_at.checked_add(60).unwrap());
    assert!(ready.expires_at <= probe.expires_at);
}

fn receive_raw(store: &mut Store, raw: &RawMessage, now: u64) -> anyhow::Result<bool> {
    store.receive(&serde_json::from_slice::<Envelope>(&raw.1).unwrap(), now)
}

fn assert_healthy(store: &Store, now: u64, expected: bool) {
    let readiness = store.readiness(now).unwrap();
    assert_eq!(readiness.len(), 1);
    assert_eq!(readiness[0].healthy, expected);
    assert_eq!(readiness[0].verified, expected);
}

fn assert_rejected_without_outbox_change(runner: &mut Runner, probe: &Envelope, now: u64) {
    let before = runner.pending_outbox_raw(100).unwrap();
    assert!(runner.accept_probe(probe, now, true).is_err());
    assert_eq!(runner.pending_outbox_raw(100).unwrap(), before);
}

fn resign_probe(probe: &Envelope, sent_at: u64, expires_at: u64) -> Envelope {
    // Only the explicitly labelled input variants are changed; never synthesize a ready.
    let mut variant = Envelope { sent_at, expires_at, ..probe.clone() };
    variant.sign(&KEY).unwrap();
    variant
}

fn assert_no_runner_execution(root: &Path) {
    let db = readonly(root, "runner.sqlite");
    let executions: i64 = db.query_row("SELECT count(*) FROM executions", [], |row| row.get(0)).unwrap();
    let inbox: i64 = db.query_row("SELECT count(*) FROM inbox", [], |row| row.get(0)).unwrap();
    assert_eq!((executions, inbox), (0, 0), "readiness must not create business execution or command records");
    let mut statement = db.prepare("SELECT body FROM outbox").unwrap();
    for raw in statement.query_map([], |row| row.get::<_, String>(0)).unwrap() {
        let message: Envelope = serde_json::from_str(&raw.unwrap()).unwrap();
        assert_eq!(message.kind, "ready");
        assert!(message.payload.get("report").is_none());
        assert!(message.payload.get("exit_proof").is_none());
    }
}

#[test]
fn production_store_runner_round_trip_uses_probe_deadline_for_normal_delays() {
    for (delay, ttl) in [(0, 60), (1, 59), (30, 30), (59, 1)] {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut runner) = open_pair(root.path());
        let probe = production_probe(&mut store, root.path(), 100);
        assert_eq!((probe.sent_at, probe.expires_at), (100, 400));
        assert_eq!(probe_metadata(root.path())["expires_at"], 160);
        let raw = production_ready(&mut runner, &probe, 100 + delay);
        assert_bounds(&raw, &probe, 160, ttl);
        assert!(receive_raw(&mut store, &raw, 100 + delay).unwrap());
        assert_healthy(&store, 100 + delay, true);
        assert_no_runner_execution(root.path());
    }
}

#[test]
fn actual_a100_b101_a102_round_trip_is_accepted_by_store() {
    // Regression: the old production Runner emits expiry 161, which Store rejects against 160.
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let probe = production_probe(&mut store, root.path(), 100);
    assert_eq!((probe.sent_at, probe.expires_at), (100, 400));
    assert_eq!(probe_metadata(root.path())["expires_at"], 160);
    let raw = production_ready(&mut runner, &probe, 101);
    assert!(receive_raw(&mut store, &raw, 102).expect("actual production ready must match current probe deadline"));
    assert_bounds(&raw, &probe, 160, 59);
    assert_healthy(&store, 102, true);
}

#[test]
fn ready_generated_at159_is_rejected_when_it_arrives_at160() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let probe = production_probe(&mut store, root.path(), 100);
    let raw = production_ready(&mut runner, &probe, 159);
    assert_bounds(&raw, &probe, 160, 1);
    assert!(receive_raw(&mut store, &raw, 160).is_err());
    assert_healthy(&store, 160, false);
}

#[test]
fn expired_readiness_is_rejected_before_outbox_insertion_even_when_envelope_is_valid() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let probe = production_probe(&mut store, root.path(), 100);
    production_ready(&mut runner, &probe, 101);
    assert!(!runner.pending_outbox_raw(100).unwrap().is_empty());
    for now in [160, 161] {
        probe.verify(&KEY, DEPLOYMENT, "B", now).unwrap();
        assert_rejected_without_outbox_change(&mut runner, &probe, now);
    }
    assert_no_runner_execution(root.path());
}

#[test]
fn earlier_signed_probe_envelope_caps_ready_without_modifying_store_metadata() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let probe = production_probe(&mut store, root.path(), 100);
    let capped = resign_probe(&probe, 100, 130);
    let raw = production_ready(&mut runner, &capped, 101);
    assert_bounds(&raw, &capped, 130, 29);
    assert_eq!(probe_metadata(root.path())["expires_at"], 160);
    assert!(receive_raw(&mut store, &raw, 101).unwrap());
    assert_healthy(&store, 101, true);
    assert_rejected_without_outbox_change(&mut runner, &capped, 130);
}

#[test]
fn existing_clock_tolerance_does_not_extend_readiness_or_ready_ttl() {
    for (runner_at, expiry, ttl) in [(55, 115, 60), (145, 160, 15)] {
        let root = tempfile::tempdir().unwrap();
        let (mut store, mut runner) = open_pair(root.path());
        let probe = production_probe(&mut store, root.path(), 100);
        let raw = production_ready(&mut runner, &probe, runner_at);
        assert_bounds(&raw, &probe, expiry, ttl);
        assert!(receive_raw(&mut store, &raw, 100).unwrap());
        assert_healthy(&store, 100, true);
    }
}

#[test]
fn probe_more_than45_seconds_in_future_is_rejected_without_outbox_change() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let probe = production_probe(&mut store, root.path(), 100);
    production_ready(&mut runner, &probe, 101);
    assert_rejected_without_outbox_change(&mut runner, &probe, 54);
}

#[test]
fn checked_probe_deadline_overflow_fails_closed_without_outbox_write() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let normal = production_probe(&mut store, root.path(), 100);
    production_ready(&mut runner, &normal, 101);
    // Derived signed input only: Store cannot generate S+60 when it is unrepresentable.
    let overflow_probe = resign_probe(&normal, u64::MAX - 59, u64::MAX);
    overflow_probe.verify(&KEY, DEPLOYMENT, "B", u64::MAX - 59).unwrap();
    assert_rejected_without_outbox_change(&mut runner, &overflow_probe, u64::MAX - 59);
    assert_no_runner_execution(root.path());
}

#[test]
fn high_u64_production_probe_uses_representable_capped_remaining_time() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let probe = production_probe(&mut store, root.path(), u64::MAX - 100);
    assert_eq!(probe.expires_at, u64::MAX);
    assert_eq!(probe_metadata(root.path())["expires_at"].as_u64(), Some(u64::MAX - 40));
    assert!((u64::MAX - 59).checked_add(60).is_none());
    let raw = production_ready(&mut runner, &probe, u64::MAX - 59);
    assert_bounds(&raw, &probe, u64::MAX - 40, 19);
    assert!(receive_raw(&mut store, &raw, u64::MAX - 59).unwrap());
    assert_healthy(&store, u64::MAX - 59, true);
    assert_no_runner_execution(root.path());
}

#[test]
fn current_real_probe_epoch_rejects_old_ready_but_accepts_fresh_production_ready() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let old = production_probe(&mut store, root.path(), 100);
    let fresh = production_probe(&mut store, root.path(), 102);
    assert_ne!(old.run_id, fresh.run_id);
    assert_ne!(old.epoch, fresh.epoch);
    let obsolete_raw = production_ready(&mut runner, &old, 103);
    assert_bounds(&obsolete_raw, &old, 160, 57);
    assert!(receive_raw(&mut store, &obsolete_raw, 103).is_err());
    assert_healthy(&store, 103, false);
    let current_raw = production_ready(&mut runner, &fresh, 103);
    assert_bounds(&current_raw, &fresh, 162, 59);
    assert!(receive_raw(&mut store, &current_raw, 103).unwrap());
    assert_healthy(&store, 103, true);
}

#[test]
fn invalidation_rejects_unexpired_production_ready_until_a_new_real_probe() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let old = production_probe(&mut store, root.path(), 100);
    let first = production_ready(&mut runner, &old, 101);
    assert!(receive_raw(&mut store, &first, 101).unwrap());
    let delayed = production_ready(&mut runner, &old, 102);
    assert_bounds(&delayed, &old, 160, 58);
    store.invalidate_readiness().unwrap();
    assert!(receive_raw(&mut store, &delayed, 102).is_err());
    assert_healthy(&store, 102, false);
    let fresh = production_probe(&mut store, root.path(), 103);
    let current = production_ready(&mut runner, &fresh, 104);
    assert_bounds(&current, &fresh, 163, 59);
    assert!(receive_raw(&mut store, &current, 104).unwrap());
    assert_healthy(&store, 104, true);
}

fn assert_reservation_retained(store: &mut Store, expected: &Run, queued: &Run, now: u64) {
    let current = store.run(&expected.id).unwrap();
    assert_eq!(serde_json::to_value(&current).unwrap(), serde_json::to_value(expected).unwrap());
    assert_eq!(current.status, "accepted");
    assert!(!current.spawn_intent && current.session.is_none());
    for field in ["exit_proof", "report", "retry_safe", "verification"] {
        assert!(current.detail.get(field).is_none(), "readiness must not create {field}");
    }
    let assignment = store.assignment(&expected.id, expected.attempt).unwrap().unwrap();
    assert_eq!(assignment.run_id, expected.id);
    assert_eq!(assignment.attempt, expected.attempt);
    assert_eq!(assignment.node_id, node().id);
    assert_eq!(assignment.address, node().address);
    assert!(!assignment.released);
    let readiness = store.readiness(now).unwrap();
    assert_eq!((readiness[0].used, readiness[0].slots), (1, 1));
    assert!(store.assign_next(now).unwrap().is_none(), "occupied slot must block the second run");
    assert_eq!(serde_json::to_value(store.run(&queued.id).unwrap()).unwrap(), serde_json::to_value(queued).unwrap());
    assert!(store.assignment(&queued.id, queued.attempt).unwrap().is_none());
}

#[test]
fn reprobe_invalidation_and_ready_never_release_or_complete_a_business_reservation() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let first = production_probe(&mut store, root.path(), 100);
    let raw = production_ready(&mut runner, &first, 101);
    assert_bounds(&raw, &first, 160, 59);
    assert!(receive_raw(&mut store, &raw, 101).unwrap());
    let dispatched = store.dispatch(&Actor::Local("owner".into()), "sum-v1", 101).unwrap();
    let assigned = store.assign_next(101).unwrap().unwrap();
    assert_eq!(assigned.run_id, dispatched.id);
    let expected = store.run(&dispatched.id).unwrap();
    let queued = store.dispatch(&Actor::Local("queued-owner".into()), "sum-v1", 101).unwrap();
    assert_reservation_retained(&mut store, &expected, &queued, 101);
    let next = production_probe(&mut store, root.path(), 102);
    assert_reservation_retained(&mut store, &expected, &queued, 102);
    let old_raw = production_ready(&mut runner, &first, 103);
    assert_bounds(&old_raw, &first, 160, 57);
    assert!(receive_raw(&mut store, &old_raw, 103).is_err());
    assert_reservation_retained(&mut store, &expected, &queued, 103);
    let invalidated_raw = production_ready(&mut runner, &next, 104);
    assert_bounds(&invalidated_raw, &next, 162, 58);
    store.invalidate_readiness().unwrap();
    assert!(receive_raw(&mut store, &invalidated_raw, 104).is_err());
    assert_reservation_retained(&mut store, &expected, &queued, 104);
    let fresh = production_probe(&mut store, root.path(), 105);
    let current = production_ready(&mut runner, &fresh, 106);
    assert_bounds(&current, &fresh, 165, 59);
    assert!(receive_raw(&mut store, &current, 106).unwrap());
    assert_healthy(&store, 106, true);
    assert_reservation_retained(&mut store, &expected, &queued, 106);
    assert_eq!(store.list_runs(100).unwrap().len(), 2);
    assert_no_runner_execution(root.path());
}

#[test]
fn ready_raw_id_mac_and_deadline_survive_reopen_without_recomputation() {
    let root = tempfile::tempdir().unwrap();
    let (mut store, mut runner) = open_pair(root.path());
    let probe = production_probe(&mut store, root.path(), 100);
    let saved = production_ready(&mut runner, &probe, 101);
    assert_bounds(&saved, &probe, 160, 59);
    assert_no_runner_execution(root.path());
    drop(runner);
    let reopened = Runner::open(&root.path().join("runner.sqlite"), DEPLOYMENT, node()).unwrap();
    assert_eq!(reopened.pending_outbox_raw(100).unwrap(), vec![saved.clone()]);
    let original: Envelope = serde_json::from_slice(&saved.1).unwrap();
    assert_eq!(reopened.pending_outbox(100).unwrap(), vec![original]);
    let persisted: (String, String) = readonly(root.path(), "runner.sqlite").query_row(
        "SELECT id,body FROM outbox WHERE sent=0", [], |row| Ok((row.get(0)?, row.get(1)?)),
    ).unwrap();
    assert_eq!((persisted.0, persisted.1.into_bytes()), saved);
    assert!(receive_raw(&mut store, &saved, 102).unwrap());
    assert_healthy(&store, 102, true);
    assert!(receive_raw(&mut store, &saved, 160).is_err());
    assert_eq!(reopened.pending_outbox_raw(100).unwrap(), vec![saved]);
    assert_no_runner_execution(root.path());
}
