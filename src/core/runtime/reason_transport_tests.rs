use anyhow::{ensure, Result};
use crate::adapters::reason::Reason;
use crate::config::{Actor, Config, NodeSpec};
use crate::core::{
    protocol::{Envelope, PROTOCOL},
    relay::RelayBridge,
    runtime::{controller, forward_controller_outbox_entry, forward_reason_frame, runner as run_runtime},
    runner::Runner,
    store::Store,
};
use serde_json::{json, Value};
use std::{
    io::{Read, Write},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
    time::{Duration, Instant},
};

#[derive(Clone, Copy)]
enum Reply {
    Queued,
    Accepted,
    Delivered,
    Refused,
    Eof,
    WrongRecipient,
    MultipleRecipients,
    InvalidState,
    EmptyIds,
    EmptyId,
    NonStringId,
    OuterIdMismatch,
}

struct PipeFixture {
    endpoint: String,
    requests: Arc<Mutex<Vec<Value>>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<Result<()>>>,
}

impl PipeFixture {
    fn new(replies: Vec<Reply>) -> Self {
        let endpoint = format!(r"\\.\pipe\wave6-reason-{}", uuid::Uuid::new_v4());
        let requests = Arc::new(Mutex::new(Vec::new()));
        let stop = Arc::new(AtomicBool::new(false));
        let first = make_pipe(&endpoint).expect("create named-pipe fixture");
        let state = requests.clone();
        let cancelled = stop.clone();
        let address = endpoint.clone();
        let thread = std::thread::spawn(move || {
            let keep_listening = replies.is_empty();
            let mut listener = Some(first);
            let mut index = 0usize;
            while !cancelled.load(Ordering::Acquire) && (keep_listening || index < replies.len()) {
                let mut stream = listener.take().expect("fixture listener");
                if !connect(&stream)? {
                    break;
                }
                let Some(request) = read_request(&mut stream, &cancelled)? else {
                    drop(stream);
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    listener = Some(make_pipe(&address)?);
                    continue;
                };
                state.lock().expect("fixture state lock").push(request.clone());
                match request["op"].as_str() {
                    Some("inbox") => write_result(&mut stream, &request, json!({"messages": []}))?,
                    Some("delivered") => write_result(&mut stream, &request, json!({}))?,
                    Some("send") => {
                        let reply = replies.get(index).copied().unwrap_or(Reply::Accepted);
                        index += 1;
                        write_fixture_reply(&mut stream, &request, reply)?;
                    }
                    other => anyhow::bail!("unexpected fixture operation: {other:?}"),
                }
                drop(stream);
                if !keep_listening && index >= replies.len() {
                    break;
                }
                listener = Some(make_pipe(&address)?);
            }
            Ok(())
        });
        Self {
            endpoint,
            requests,
            stop,
            thread: Some(thread),
        }
    }

    fn requests(&self) -> Vec<Value> {
        self.requests.lock().expect("fixture state lock").clone()
    }
}

impl Drop for PipeFixture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        let _ = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .open(&self.endpoint);
        if let Some(thread) = self.thread.take() {
            thread.join().expect("named-pipe fixture thread").expect("named-pipe fixture");
        }
    }
}

fn signed_frame(message_id: &str) -> Vec<u8> {
    signed_frame_for(message_id, "wave6-test", "A", "B", [7; 32])
}

fn signed_frame_for(message_id: &str, deployment: &str, from: &str, to: &str, key: [u8; 32]) -> Vec<u8> {
    signed_frame_at(message_id, deployment, from, to, key, 100, 400)
}

fn signed_frame_at(message_id: &str, deployment: &str, from: &str, to: &str, key: [u8; 32], sent_at: u64, expires_at: u64) -> Vec<u8> {
    let mut envelope = Envelope {
        schema: PROTOCOL.into(),
        message_id: message_id.into(),
        deployment: deployment.into(),
        from: from.into(),
        to: to.into(),

        run_id: uuid::Uuid::new_v4().to_string(),
        attempt: 1,
        epoch: uuid::Uuid::new_v4().to_string(),
        kind: "start".into(),
        sent_at,
        expires_at,
        payload: json!({"text": "前后合法空白与非 ASCII", "values": [1, 2]}),
        mac: String::new(),
    };
    envelope.sign(&key).expect("sign synthetic frame");
    let compact = serde_json::to_vec_pretty(&envelope).expect("serialize synthetic frame");
    [b" \n\t".as_slice(), compact.as_slice(), b"\n \t".as_slice()].concat()
}

fn reason(endpoint: String) -> Reason {
    Reason::new(endpoint).expect("valid named-pipe Reason endpoint")
}

fn bridge() -> RelayBridge {
    RelayBridge::new("wave6-test", "B", [7; 32]).expect("valid relay bridge")
}

#[test]
fn frame_preserves_raw_utf8_and_fixed_from() {
    let fixture = PipeFixture::new(vec![Reply::Accepted]);
    let message_id = uuid::Uuid::new_v4().to_string();
    let frame = signed_frame(&message_id);
    let mut relay = bridge();

    forward_reason_frame(
        &reason(fixture.endpoint.clone()),
        "owned-A/controller",
        "owned-B/runner",
        &message_id,
        &frame,
        &mut relay,
        110,
    )
    .expect("valid Reason receipt");

    let requests = fixture.requests();
    assert_eq!(requests.len(), 1);
    let request = &requests[0];
    assert_eq!(request["op"], "send");
    assert_eq!(request["from"], "owned-A/controller");
    assert_eq!(request["to"], "owned-B/runner");
    assert_eq!(request["idempotency_key"], message_id);
    assert_eq!(request["wake"], false);
    assert_eq!(request["body"].as_str().expect("raw body"), std::str::from_utf8(&frame).unwrap());
    assert_eq!(request["body"].as_str().unwrap().as_bytes(), frame.as_slice());
}

#[test]
fn reason_receipts_accept_only_single_valid_recipient_state_and_ids() {
    for reply in [Reply::Queued, Reply::Accepted, Reply::Delivered] {
        let fixture = PipeFixture::new(vec![reply]);
        let message_id = uuid::Uuid::new_v4().to_string();
        let frame = signed_frame(&message_id);
        forward_reason_frame(
            &reason(fixture.endpoint.clone()),
            "owned-A/controller",
            "owned-B/runner",
            &message_id,
            &frame,
            &mut bridge(),
            110,
        ).expect("queued, accepted, and delivered receipts are valid");
    }
    for reply in [
        Reply::WrongRecipient,
        Reply::MultipleRecipients,
        Reply::InvalidState,
        Reply::EmptyIds,
        Reply::EmptyId,
        Reply::NonStringId,
        Reply::OuterIdMismatch,
        Reply::Refused,
    ] {
        let fixture = PipeFixture::new(vec![reply]);
        let message_id = uuid::Uuid::new_v4().to_string();
        let frame = signed_frame(&message_id);
        assert!(forward_reason_frame(
            &reason(fixture.endpoint.clone()),
            "owned-A/controller",
            "owned-B/runner",
            &message_id,
            &frame,
            &mut bridge(),
            110,
        ).is_err(), "invalid receipt must retain the sender's failure");
    }
}

#[test]
fn frame_rejects_bad_id_utf8_size_or_mac_before_ipc() {
    let fixture = PipeFixture::new(Vec::new());
    let message_id = uuid::Uuid::new_v4().to_string();
    let frame = signed_frame(&message_id);
    let mut bad_mac_value: Value = serde_json::from_slice(&frame).expect("synthetic frame JSON");
    bad_mac_value["mac"] = json!("00".repeat(32));
    let bad_mac = serde_json::to_vec(&bad_mac_value).expect("bad MAC frame");

    assert!(forward_reason_frame(
        &reason(fixture.endpoint.clone()),
        "owned-A/controller",
        "owned-B/runner",
        "different-message-id",
        &frame,
        &mut bridge(),
        110,
    )
    .is_err());
    assert!(forward_reason_frame(
        &reason(fixture.endpoint.clone()),
        "owned-A/controller",
        "owned-B/runner",
        &message_id,
        &[0xff, 0xfe],
        &mut bridge(),
        110,
    )
    .is_err());
    assert!(forward_reason_frame(
        &reason(fixture.endpoint.clone()),
        "owned-A/controller",
        "owned-B/runner",
        &message_id,
        &vec![b'x'; 64 * 1024 + 1],
        &mut bridge(),
        110,
    )
    .is_err());
    assert!(forward_reason_frame(
        &reason(fixture.endpoint.clone()),
        "owned-A/controller",
        "owned-B/runner",
        &message_id,
        &bad_mac,
        &mut bridge(),
        110,
    )
    .is_err());
    assert!(fixture.requests().is_empty(), "invalid frames must not reach IPC");
}

#[test]
fn controller_helper_routes_distinct_nodes() {
    let fixture = PipeFixture::new(Vec::new());
    let temp = retained_test_dir();
    let nodes = vec![node("B", "owned-B/runner", "07"), node("C", "owned-C/runner", "08")];
    let mut store = Store::open(&temp.as_path().join("controller.sqlite"), "wave6-controller", &nodes).unwrap();
    store.dispatch(&Actor::Local("controller-test".into()), "sum-v1", 100).unwrap();
    store.assign_next(101).unwrap().unwrap();
    store.dispatch(&Actor::Local("controller-test".into()), "sum-v1", 100).unwrap();
    store.assign_next(101).unwrap().unwrap();
    let pending = store.pending_outbox_raw(32).unwrap();
    assert_eq!(pending.len(), 2);
    let reason = reason(fixture.endpoint.clone());
    for (id, destination, frame) in pending {
        forward_controller_outbox_entry(
            &mut store,
            &reason,
            "owned-A/controller",
            &id,
            &destination,
            &frame,
            110,
        )
        .expect("configured destination is routable");
    }
    let requests = fixture.requests();
    let sends: Vec<_> = requests.iter().filter(|request| request["op"] == "send").collect();
    assert_eq!(sends.len(), 2);
    assert_eq!(sends[0]["to"], "owned-B/runner");
    assert_eq!(sends[1]["to"], "owned-C/runner");
    assert_eq!(sends[0]["from"], "owned-A/controller");
    assert_eq!(sends[1]["from"], "owned-A/controller");
    assert_eq!(store.outbox_count().unwrap(), 0);
}

#[test]
fn controller_helper_rejects_non_controller_sender_before_ipc() {
    let fixture = PipeFixture::new(Vec::new());
    let temp = retained_test_dir();
    let nodes = vec![node("B", "owned-B/runner", "07")];
    let mut store = Store::open(&temp.as_path().join("controller.sqlite"), "wave6-controller", &nodes).unwrap();
    let message_id = uuid::Uuid::new_v4().to_string();
    let current = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let frame = signed_frame_at(
        &message_id,
        "wave6-controller",
        "C",
        "B",
        [7; 32],
        current,
        current + 300,
    );
    let raw = std::str::from_utf8(&frame).unwrap();
    let db = rusqlite::Connection::open(temp.as_path().join("controller.sqlite")).unwrap();
    db.execute(
        "INSERT INTO outbox(id,destination,body,sent) VALUES(?1,?2,?3,0)",
        [&message_id, "owned-B/runner", raw],
    )
    .unwrap();
    drop(db);

    assert!(forward_controller_outbox_entry(
        &mut store,
        &reason(fixture.endpoint.clone()),
        "owned-A/controller",
        &message_id,
        "owned-B/runner",
        &frame,
        current,
    )
    .is_err(), "foreign controller sender must be rejected");
    assert_eq!(store.pending_outbox_raw(1).unwrap()[0].2, frame);
    assert!(fixture.requests().is_empty(), "foreign controller sender must not reach IPC");
}

#[test]
fn controller_once_retains_business_outbox_on_refusal_eof_then_acks_same_id() {
    let fixture = PipeFixture::new(vec![Reply::Refused, Reply::Eof, Reply::Accepted]);
    let temp = retained_test_dir();
    let config_path = write_controller_config(temp.as_path(), &fixture.endpoint, &[node_config("B", "owned-B/runner", "B.key", "sum-v1")]);
    let config: Config = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let nodes = config.load_nodes(temp.as_path()).unwrap();
    let db_path = temp.as_path().join("state/controller.sqlite");
    let mut store = Store::open(&db_path, &config.deployment, &nodes).unwrap();
    let now = current_time();
    ready_nodes(&mut store, now);
    let run = store.dispatch(&Actor::Local("raw-retry-owner".into()), "sum-v1", now).unwrap();
    store.assign_next(now).unwrap().unwrap();
    let (message_id, _, frame) = padded_business_entry(&mut store, &db_path);
    drop(store);

    for _ in 0..2 {
        assert!(controller(&config_path, &config, true).is_err());
        let reopened = Store::open(&db_path, &config.deployment, &nodes).unwrap();
        assert_eq!(reopened.pending_outbox_raw(1).unwrap()[0].0, message_id);
        assert_eq!(reopened.pending_outbox_raw(1).unwrap()[0].2, frame);
        assert_run_reserved(&reopened, &run.id, &run.epoch);
    }
    controller(&config_path, &config, true).expect("valid receipt acknowledges the original outbox");
    let done = Store::open(&db_path, &config.deployment, &nodes).unwrap();
    assert!(done.pending_outbox_raw(1).unwrap().is_empty());
    assert_run_reserved(&done, &run.id, &run.epoch);
    let sends: Vec<_> = fixture.requests().into_iter().filter(|request| request["op"] == "send").collect();
    assert_eq!(sends.len(), 3);
    for request in sends {
        assert_eq!(request["from"], config.endpoint.as_deref().unwrap());
        assert_eq!(request["to"], "owned-B/runner");
        assert_eq!(request["idempotency_key"], message_id);
        assert_eq!(request["body"].as_str().unwrap().as_bytes(), frame.as_slice());
    }
}

#[test]
fn project_tick_continues_after_refusal_without_completing_actions_or_release() {
    use crate::core::service::UiCommand;
    let fixture = PipeFixture::new(vec![Reply::Refused, Reply::Accepted]);
    let root = retained_test_dir();
    let config_path = write_controller_config(&root, &fixture.endpoint, &[node_config("B", "owned-B/runner", "B.key", "project-work-v1"), node_config("C", "owned-C/runner", "C.key", "project-work-v1")]);
    let config: Config = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let mut projects = crate::core::projects::Projects::open(&root.join("projects")).unwrap();
    projects.command(UiCommand::ProjectConfigure { config_path }).unwrap();
    for name in ["first", "second"] {
        create_split_action(&mut projects, &root, name);
    }
    let db_path = root.join("state/controller.sqlite");
    let mut store = Store::open(&db_path, &config.deployment, &config.load_nodes(&root).unwrap()).unwrap();
    ready_existing_probes(&mut store, current_time());
    for _ in 0..2 { store.assign_next(current_time()).unwrap().unwrap(); }
    let first = padded_business_entry(&mut store, &db_path);
    let entries = store.pending_outbox_raw(32).unwrap();
    assert_eq!(entries.len(), 2);
    let runs = store.list_runs(8).unwrap();
    drop(store);

    assert!(projects.tick().is_err(), "first refusal must remain observable");
    let after = Store::open(&db_path, &config.deployment, &config.load_nodes(&root).unwrap()).unwrap();
    assert_eq!(after.pending_outbox_raw(32).unwrap(), vec![first.clone()]);
    for run in runs { assert_run_reserved(&after, &run.id, &run.epoch); }
    assert!(after.readiness(current_time()).unwrap().iter().all(|node| !node.healthy && !node.verified));
    assert!(projects.snapshots().iter().all(|project| project.phase == "splitting" && !project.can_start && project.actions.iter().all(|action| action.state != "succeeded")));
    let sends: Vec<_> = fixture.requests().into_iter().filter(|request| request["op"] == "send").collect();
    assert_eq!(sends.len(), 2);
    for (request, entry) in sends.iter().zip(&entries) {
        assert_eq!(request["from"], "owned-A/controller");
        assert_eq!(request["to"], entry.1);
        assert_eq!(request["idempotency_key"], entry.0);
        assert_eq!(request["body"].as_str().unwrap().as_bytes(), entry.2);
    }
    assert_eq!(sends[0]["to"], "owned-B/runner");
    assert_eq!(sends[1]["to"], "owned-C/runner");
}

#[test]
fn runner_raw_accessor_keeps_existing_api() {
    let temp = retained_test_dir();
    let node = node("B", "owned-B/runner", "07");
    let db_path = temp.as_path().join("runner.sqlite");
    let runner_ledger = Runner::open(&db_path, "wave6-runner", node).unwrap();
    let message_id = uuid::Uuid::new_v4().to_string();
    let frame = signed_frame_for(&message_id, "wave6-runner", "B", "A", [7; 32]);
    let raw = format!(" \n{}\n ", std::str::from_utf8(&frame).unwrap());
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute("INSERT INTO outbox(id,body,sent) VALUES(?1,?2,0)", [&message_id, &raw]).unwrap();
    drop(db);

    let raw_entry = runner_ledger.pending_outbox_raw(1).expect("raw accessor");
    assert_eq!(raw_entry, vec![(message_id.clone(), raw.as_bytes().to_vec())]);
    let old_entry = runner_ledger.pending_outbox(1).expect("existing accessor");
    assert_eq!(old_entry.len(), 1);
    assert_eq!(old_entry[0].to, "A");
    assert_eq!(old_entry[0].message_id, message_id);
}

#[test]
#[allow(non_snake_case)]
fn runner_once_checks_A_own_key_and_uncertain_retry() {
    let fixture = PipeFixture::new(vec![Reply::Refused, Reply::Eof, Reply::Accepted]);
    let temp = retained_test_dir();
    let config_path = write_runner_config(temp.as_path(), &fixture.endpoint);
    let config: Config = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let node = config.load_nodes(temp.as_path()).unwrap().pop().unwrap();
    let db_path = temp.as_path().join("state/runner.sqlite");
    let runner_ledger = Runner::open(&db_path, &config.deployment, node).unwrap();
    let message_id = uuid::Uuid::new_v4().to_string();
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
    let frame = signed_frame_at(&message_id, &config.deployment, "B", "A", [7; 32], now, now + 300);
    let mut status: Envelope = serde_json::from_slice(&frame).unwrap();
    status.kind = "status".into();
    status.sign(&[7; 32]).unwrap();
    let raw = format!(" \n{}\n ", serde_json::to_string_pretty(&status).unwrap());
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute("INSERT INTO outbox(id,body,sent) VALUES(?1,?2,0)", [&message_id, &raw]).unwrap();
    drop(db);
    drop(runner_ledger);

    assert!(run_runtime(&config_path, &config, true).is_err());
    let after_refusal = Runner::open(&db_path, "wave6-runner", config.load_nodes(temp.as_path()).unwrap().pop().unwrap()).unwrap();
    assert_eq!(after_refusal.pending_outbox_raw(1).unwrap()[0].0, message_id);
    drop(after_refusal);
    assert!(run_runtime(&config_path, &config, true).is_err());
    let after_eof = Runner::open(&db_path, "wave6-runner", config.load_nodes(temp.as_path()).unwrap().pop().unwrap()).unwrap();
    assert_eq!(after_eof.pending_outbox_raw(1).unwrap()[0].1, raw.as_bytes());
    drop(after_eof);
    run_runtime(&config_path, &config, true).expect("same key succeeds on retry");
    let done = Runner::open(&db_path, "wave6-runner", config.load_nodes(temp.as_path()).unwrap().pop().unwrap()).unwrap();
    assert!(done.pending_outbox_raw(1).unwrap().is_empty());
    let db = rusqlite::Connection::open(&db_path).unwrap();
    assert_eq!(db.query_row("SELECT count(*) FROM executions", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
    let requests = fixture.requests();
    let sends: Vec<_> = requests.iter().filter(|request| request["op"] == "send").collect();
    assert_eq!(sends.len(), 3);
    for request in sends {
        assert_eq!(request["from"], "owned-B/runner");
        assert_eq!(request["to"], "owned-A/controller");
        assert_eq!(request["idempotency_key"], message_id);
        assert_eq!(request["body"].as_str().unwrap().as_bytes(), raw.as_bytes());
    }
}

#[test]
fn runner_rejects_non_runner_sender_before_ipc() {
    let fixture = PipeFixture::new(Vec::new());
    let temp = retained_test_dir();
    let config_path = write_runner_config(temp.as_path(), &fixture.endpoint);
    let config: Config = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
    let node = config.load_nodes(temp.as_path()).unwrap().pop().unwrap();
    let db_path = temp.as_path().join("state/runner.sqlite");
    let runner_ledger = Runner::open(&db_path, &config.deployment, node).unwrap();
    let message_id = uuid::Uuid::new_v4().to_string();
    let current = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let frame = signed_frame_at(
        &message_id,
        &config.deployment,
        "C",
        "A",
        [7; 32],
        current,
        current + 300,
    );
    let raw = format!(" \n{}\n ", std::str::from_utf8(&frame).unwrap());
    let db = rusqlite::Connection::open(&db_path).unwrap();
    db.execute(
        "INSERT INTO outbox(id,body,sent) VALUES(?1,?2,0)",
        [&message_id, &raw],
    )
    .unwrap();
    drop(db);
    drop(runner_ledger);

    assert!(run_runtime(&config_path, &config, true).is_err());
    let reopened = Runner::open(
        &db_path,
        &config.deployment,
        config.load_nodes(temp.as_path()).unwrap().pop().unwrap(),
    )
    .unwrap();
    assert_eq!(reopened.pending_outbox_raw(1).unwrap()[0].1, raw.as_bytes());
    assert!(fixture.requests().iter().all(|request| request["op"] != "send"));
}

#[test]
fn frame_refusal_and_eof_retry_same_key() {
    let fixture = PipeFixture::new(vec![Reply::Refused, Reply::Eof, Reply::Accepted]);
    let message_id = uuid::Uuid::new_v4().to_string();
    let frame = signed_frame(&message_id);
    let mut relay = bridge();
    let endpoint = reason(fixture.endpoint.clone());

    assert!(forward_reason_frame(
        &endpoint,
        "owned-A/controller",
        "owned-B/runner",
        &message_id,
        &frame,
        &mut relay,
        110,
    )
    .is_err());
    assert!(forward_reason_frame(
        &endpoint,
        "owned-A/controller",
        "owned-B/runner",
        &message_id,
        &frame,
        &mut relay,
        110,
    )
    .is_err());
    forward_reason_frame(
        &endpoint,
        "owned-A/controller",
        "owned-B/runner",
        &message_id,
        &frame,
        &mut relay,
        110,
    )
    .expect("retry receives a valid Reason receipt");

    let requests = fixture.requests();
    assert_eq!(requests.len(), 3);
    for request in &requests {
        assert_eq!(request["idempotency_key"], message_id);
        assert_eq!(request["body"].as_str().unwrap().as_bytes(), frame.as_slice());
    }
}

#[test]
fn controller_receipt_failures_keep_original_business_outbox_and_assignment() {
    for reply in [Reply::WrongRecipient, Reply::MultipleRecipients, Reply::InvalidState, Reply::EmptyIds, Reply::EmptyId, Reply::NonStringId, Reply::OuterIdMismatch, Reply::Refused] {
        let fixture = PipeFixture::new(vec![reply]);
        let root = retained_test_dir();
        let config_path = write_controller_config(&root, &fixture.endpoint, &[node_config("B", "owned-B/runner", "B.key", "sum-v1")]);
        let config: Config = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        let nodes = config.load_nodes(&root).unwrap();
        let db_path = root.join("state/controller.sqlite");
        let mut store = Store::open(&db_path, &config.deployment, &nodes).unwrap();
        ready_nodes(&mut store, current_time());
        let run = store.dispatch(&Actor::Local("receipt-owner".into()), "sum-v1", current_time()).unwrap();
        store.assign_next(current_time()).unwrap().unwrap();
        let entry = padded_business_entry(&mut store, &db_path);
        drop(store);

        assert!(controller(&config_path, &config, true).is_err());
        let after = Store::open(&db_path, &config.deployment, &nodes).unwrap();
        assert_eq!(after.pending_outbox_raw(32).unwrap(), vec![entry]);
        assert_run_reserved(&after, &run.id, &run.epoch);
        assert_eq!(fixture.requests().iter().filter(|request| request["op"] == "send").count(), 1);
    }
}

#[test]
fn public_controller_rejects_corrupt_persisted_frame_before_send() {
    for mismatch in [false, true] {
        let fixture = PipeFixture::new(Vec::new());
        let root = retained_test_dir();
        let config_path = write_controller_config(&root, &fixture.endpoint, &[node_config("B", "owned-B/runner", "B.key", "sum-v1")]);
        let config: Config = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        let nodes = config.load_nodes(&root).unwrap();
        let db_path = root.join("state/controller.sqlite");
        let mut store = Store::open(&db_path, &config.deployment, &nodes).unwrap();
        ready_nodes(&mut store, current_time());
        let run = store.dispatch(&Actor::Local("corrupt-owner".into()), "sum-v1", current_time()).unwrap();
        store.assign_next(current_time()).unwrap().unwrap();
        let entry = corrupt_entry(&mut store, &db_path, mismatch);
        drop(store);

        assert!(controller(&config_path, &config, true).is_err());
        let after = Store::open(&db_path, &config.deployment, &nodes).unwrap();
        assert_eq!(after.pending_outbox_raw(32).unwrap(), vec![entry]);
        assert_run_reserved(&after, &run.id, &run.epoch);
        assert!(fixture.requests().iter().all(|request| request["op"] != "send"));
    }
}

#[test]
fn public_project_rejects_corrupt_business_frame_without_completing_action() {
    use crate::core::service::UiCommand;
    for mismatch in [false, true] {
        let fixture = PipeFixture::new(Vec::new());
        let root = retained_test_dir();
        let config_path = write_controller_config(&root, &fixture.endpoint, &[node_config("B", "owned-B/runner", "B.key", "project-work-v1")]);
        let config: Config = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        let mut projects = crate::core::projects::Projects::open(&root.join("projects")).unwrap();
        projects.command(UiCommand::ProjectConfigure { config_path }).unwrap();
        create_split_action(&mut projects, &root, "corrupt-project");
        let db_path = root.join("state/controller.sqlite");
        let mut store = Store::open(&db_path, &config.deployment, &config.load_nodes(&root).unwrap()).unwrap();
        ready_existing_probes(&mut store, current_time());
        store.assign_next(current_time()).unwrap().unwrap();
        let run = store.list_runs(1).unwrap().pop().unwrap();
        let entry = corrupt_entry(&mut store, &db_path, mismatch);
        drop(store);

        assert!(projects.tick().is_err());
        let after = Store::open(&db_path, &config.deployment, &config.load_nodes(&root).unwrap()).unwrap();
        assert_eq!(after.pending_outbox_raw(32).unwrap(), vec![entry]);
        assert_run_reserved(&after, &run.id, &run.epoch);
        assert!(projects.snapshots()[0].actions.iter().all(|action| action.state != "succeeded"));
        assert!(fixture.requests().iter().all(|request| request["op"] != "send"));
    }
}

#[test]
fn public_runner_rejects_corrupt_persisted_frame_before_send_or_execution() {
    for mismatch in [false, true] {
        let fixture = PipeFixture::new(Vec::new());
        let root = retained_test_dir();
        let config_path = write_runner_config(&root, &fixture.endpoint);
        let config: Config = serde_json::from_slice(&std::fs::read(&config_path).unwrap()).unwrap();
        let node = config.load_nodes(&root).unwrap().pop().unwrap();
        let db_path = root.join("state/runner.sqlite");
        let ledger = Runner::open(&db_path, &config.deployment, node.clone()).unwrap();
        let id = uuid::Uuid::new_v4().to_string();
        let frame = signed_frame_at(&id, &config.deployment, "B", "A", [7;32], current_time(), current_time()+300);
        let mut message: Envelope = serde_json::from_slice(&frame).unwrap();
        if mismatch { message.message_id = uuid::Uuid::new_v4().to_string(); message.sign(&[7;32]).unwrap(); }
        else { message.mac = "00".repeat(32); }
        let body = format!(" \n{}\n ", serde_json::to_string(&message).unwrap());
        let db = rusqlite::Connection::open(&db_path).unwrap();
        db.execute("INSERT INTO outbox(id,body,sent) VALUES(?1,?2,0)", [&id, &body]).unwrap();
        drop(db);
        drop(ledger);

        assert!(run_runtime(&config_path, &config, true).is_err());
        let after = Runner::open(&db_path, &config.deployment, node).unwrap();
        assert_eq!(after.pending_outbox_raw(32).unwrap(), vec![(id, body.into_bytes())]);
        let db = rusqlite::Connection::open(&db_path).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM executions", [], |row| row.get::<_,i64>(0)).unwrap(), 0);
        assert!(fixture.requests().iter().all(|request| request["op"] != "send"));
    }
}

fn corrupt_entry(store: &mut Store, db_path: &std::path::Path, mismatch: bool) -> (String, String, Vec<u8>) {
    let (id, destination, frame) = store.pending_outbox_raw(1).unwrap().pop().unwrap();
    let mut message: Envelope = serde_json::from_slice(&frame).unwrap();
    if mismatch {
        message.message_id = uuid::Uuid::new_v4().to_string();
        let node = store.nodes.iter().find(|node| node.address == destination).unwrap();
        message.sign(&hex::decode(&node.key_hex).unwrap()).unwrap();
    } else { message.mac = "00".repeat(32); }
    let body = format!(" \n{}\n ", serde_json::to_string(&message).unwrap());
    let db = rusqlite::Connection::open(db_path).unwrap();
    db.execute("UPDATE outbox SET body=?1 WHERE id=?2", [&body, &id]).unwrap();
    (id, destination, body.into_bytes())
}

#[test]
fn controller_router_rejects_unknown_destination_wrong_recipient_key_and_id() {
    let fixture = PipeFixture::new(Vec::new());
    let root = retained_test_dir();
    let nodes = vec![node("B", "owned-B/runner", "07"), node("C", "owned-C/runner", "08")];
    let mut store = Store::open(&root.join("controller.sqlite"), "wave6-test", &nodes).unwrap();
    let reason = reason(fixture.endpoint.clone());
    for (destination, recipient, key, mismatch) in [
        ("unknown/runner", "B", [7;32], false),
        ("owned-B/runner", "C", [7;32], false),
        ("owned-B/runner", "B", [8;32], false),
        ("owned-B/runner", "B", [7;32], true),
    ] {
        let id = uuid::Uuid::new_v4().to_string();
        let body_id = if mismatch { uuid::Uuid::new_v4().to_string() } else { id.clone() };
        let frame = signed_frame_for(&body_id, "wave6-test", "A", recipient, key);
        assert!(forward_controller_outbox_entry(&mut store, &reason, "owned-A/controller", &id, destination, &frame, 110).is_err());
    }
    assert!(fixture.requests().is_empty());
}

fn retained_test_dir() -> std::path::PathBuf {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/wave6-reason-cases");
    std::fs::create_dir_all(&root).unwrap();
    let path = root.join(uuid::Uuid::new_v4().to_string());
    std::fs::create_dir(&path).expect("exclusively create retained Wave6 case");
    path
}

fn current_time() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn ready_nodes(store: &mut Store, now: u64) {
    store.probe_nodes(now).unwrap();
    ready_existing_probes(store, now);
}

fn ready_existing_probes(store: &mut Store, now: u64) {
    for (destination, probe) in store.pending_outbox(32).unwrap() {
        let node = store.nodes.iter().find(|node| node.address == destination).unwrap();
        let metadata_raw: String = store.db.query_row("SELECT value FROM metadata WHERE key=?1", [format!("probe:{}", node.id)], |row| row.get(0)).unwrap();
        let metadata: Value = serde_json::from_str(&metadata_raw).unwrap();
        let receipt_now = now;
        let mut ready = Envelope {
            schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
            deployment: probe.deployment.clone(), from: node.id.clone(), to: "A".into(),
            run_id: probe.run_id.clone(), attempt: 1, epoch: probe.epoch.clone(),
            kind: "ready".into(), sent_at: receipt_now, expires_at: metadata["expires_at"].as_u64().unwrap(),
            payload: json!({"address": node.address, "templates": node.templates, "ao_project_ready": true}),
            mac: String::new(),
        };
        ready.sign(&hex::decode(&node.key_hex).unwrap()).unwrap();
        store.receive(&ready, receipt_now).unwrap_or_else(|error| panic!("ready mismatch: error={error}; sent_at={} expires_at={} receipt_now={} attempt={} run={} epoch={}", probe.sent_at, probe.expires_at, receipt_now, probe.attempt, probe.run_id, probe.epoch));
        store.acknowledge_outbox(&probe.message_id).unwrap();
    }
}

fn create_split_action(projects: &mut crate::core::projects::Projects, root: &std::path::Path, name: &str) {
    use crate::core::service::UiCommand;
    let id = projects.command(UiCommand::ProjectCreate { name: name.into() }).unwrap()["id"].as_str().unwrap().to_owned();
    let path = root.join(format!("{name}.md"));
    std::fs::write(&path, "# 已确认设计\n原始非 ASCII 业务文档。\n").unwrap();
    projects.command(UiCommand::ProjectImport { project_id: id.clone(), path }).unwrap();
    let document = projects.snapshots().into_iter().find(|project| project.id == id).unwrap().document.unwrap();
    projects.command(UiCommand::ProjectConfirm { project_id: id.clone(), version: document.version, sha256: document.sha256 }).unwrap();
    projects.command(UiCommand::ProjectSplit { project_id: id }).unwrap();
}

fn padded_business_entry(store: &mut Store, db_path: &std::path::Path) -> (String, String, Vec<u8>) {
    let (id, destination, frame) = store.pending_outbox_raw(1).unwrap().pop().unwrap();
    let mut message: Envelope = serde_json::from_slice(&frame).unwrap();
    message.payload["transport_text"] = json!("原始空白和非 ASCII 业务消息");
    let node = store.nodes.iter().find(|node| node.address == destination).unwrap();
    message.sign(&hex::decode(&node.key_hex).unwrap()).unwrap();
    let raw = format!(" \n\t{}\n \t", serde_json::to_string_pretty(&message).unwrap());
    let db = rusqlite::Connection::open(db_path).unwrap();
    db.execute("UPDATE outbox SET body=?1 WHERE id=?2", [&raw, &id]).unwrap();
    (id, destination, raw.into_bytes())
}

fn assert_run_reserved(store: &Store, id: &str, epoch: &str) {
    let run = store.run(id).unwrap();
    assert_eq!(run.attempt, 1);
    assert_eq!(run.epoch, epoch);
    assert_eq!(run.status, "accepted");
    assert!(run.session.is_none());
    assert!(!run.spawn_intent);
    assert!(run.detail.get("report").is_none());
    assert!(!store.assignment(id, 1).unwrap().unwrap().released);
}

fn node(id: &str, address: &str, key_byte: &str) -> NodeSpec {
    NodeSpec {
        id: id.into(),
        address: address.into(),
        key_hex: key_byte.repeat(64 / key_byte.len()),
        slots: 1,
        admitted: true,
        healthy: true,
        draining: false,
        templates: vec!["sum-v1".into(), "project-work-v1".into()],
    }
}

fn node_config(id: &str, address: &str, key_file: &str, template: &str) -> Value {
    json!({
        "id": id,
        "address": address,
        "key_file": key_file,
        "slots": 1,
        "templates": [template],
        "admitted": true,
        "healthy": true,
    })
}

fn write_runner_config(root: &std::path::Path, endpoint: &str) -> std::path::PathBuf {
    let path = root.join("runner.json");
    std::fs::write(root.join("B.key"), "07".repeat(32)).unwrap();
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema": "ao-reason-config/1",
            "mode": "real",
            "role": "runner",
            "deployment": "wave6-runner",
            "state_dir": "state",
            "nodes": [node_config("B", "owned-B/runner", "B.key", "sum-v1")],
            "runner_node": "B",
            "controller_address": "owned-A/controller",
            "reason_endpoint": endpoint,
            "ao": {"base_url": "http://127.0.0.1:1", "project_id": "wave6-project", "model": "no-call"},
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn write_controller_config(root: &std::path::Path, endpoint: &str, nodes: &[Value]) -> std::path::PathBuf {
    for (index, node) in nodes.iter().enumerate() {
        let file = root.join(node["key_file"].as_str().unwrap());
        std::fs::write(file, format!("{:02x}", index + 7).repeat(32)).unwrap();
    }
    let path = root.join("controller.json");
    std::fs::write(
        &path,
        serde_json::to_vec(&json!({
            "schema": "ao-reason-config/1",
            "mode": "real",
            "role": "controller",
            "deployment": "wave6-controller",
            "state_dir": "state",
            "nodes": nodes,
            "reason_endpoint": endpoint,
            "endpoint": "owned-A/controller",
            "ao": {"base_url": "http://127.0.0.1:1", "project_id": "wave6-project", "model": "no-call"},
        }))
        .unwrap(),
    )
    .unwrap();
    path
}

fn make_pipe(endpoint: &str) -> Result<std::fs::File> {
    use std::os::windows::io::{FromRawHandle, RawHandle};
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::PIPE_ACCESS_DUPLEX,
        System::Pipes::{CreateNamedPipeW, PIPE_WAIT, PIPE_TYPE_BYTE},
    };
    let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
    let handle = unsafe {
        CreateNamedPipeW(
            wide.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_WAIT,
            1,
            65536,
            65536,
            3000,
            std::ptr::null(),
        )
    };
    ensure!(handle != INVALID_HANDLE_VALUE, "named-pipe fixture creation");
    Ok(unsafe { std::fs::File::from_raw_handle(handle as RawHandle) })
}

fn connect(file: &std::fs::File) -> Result<bool> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Foundation::{GetLastError, ERROR_PIPE_CONNECTED},
        System::Pipes::ConnectNamedPipe,
    };
    let connected = unsafe { ConnectNamedPipe(file.as_raw_handle() as _, std::ptr::null_mut()) };
    if connected != 0 {
        return Ok(true);
    }
    let error = unsafe { GetLastError() };
    ensure!(error == ERROR_PIPE_CONNECTED, "named-pipe fixture connect error {error}");
    Ok(true)
}

fn read_request(stream: &mut std::fs::File, stop: &AtomicBool) -> Result<Option<Value>> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut raw = Vec::new();
    loop {
        ensure!(Instant::now() < deadline, "fixture read deadline");
        if stop.load(Ordering::Acquire) {
            return Ok(None);
        }
        let mut byte = [0u8; 1];
        match stream.read(&mut byte) {
            Ok(0) => return Ok(None),
            Ok(_) if byte[0] == b'\n' => return Ok(Some(serde_json::from_slice(&raw)?)),
            Ok(_) => raw.push(byte[0]),
            Err(error) if error.raw_os_error() == Some(232) => {
                if stop.load(Ordering::Acquire) {
                    return Ok(None);
                }
                std::thread::sleep(Duration::from_millis(2));
            }
            Err(error) => return Err(error.into()),
        }
        ensure!(raw.len() <= 65536, "fixture request bound");
    }
}

fn write_result(stream: &mut std::fs::File, request: &Value, result: Value) -> Result<()> {
    write_reply_with_result(stream, request, result)
}

fn write_reply_with_result(stream: &mut std::fs::File, request: &Value, result: Value) -> Result<()> {
    let response = json!({"id": request["id"], "ok": true, "result": result});
    writeln!(stream, "{response}")?;
    Ok(())
}

fn write_refusal(stream: &mut std::fs::File, request: &Value, result: Value) -> Result<()> {
    let response = json!({"id": request["id"], "ok": false, "result": result});
    writeln!(stream, "{response}")?;
    Ok(())
}

fn write_fixture_reply(stream: &mut std::fs::File, request: &Value, reply: Reply) -> Result<()> {
    match reply {
        Reply::Queued => write_reply(stream, request, "queued"),
        Reply::Accepted => write_reply(stream, request, "accepted"),
        Reply::Delivered => write_reply(stream, request, "delivered"),
        Reply::Refused => write_refusal(stream, request, json!({})),
        Reply::Eof => Ok(()),
        Reply::WrongRecipient => write_reply_with_result(stream, request, json!({"recipients":[{"address":"wrong-destination","state":"accepted"}],"ids":[request["idempotency_key"]]})),
        Reply::MultipleRecipients => write_reply_with_result(stream, request, json!({"recipients":[{"address":request["to"],"state":"accepted"},{"address":request["to"],"state":"accepted"}],"ids":[request["idempotency_key"]]})),
        Reply::InvalidState => write_reply_with_result(stream, request, json!({"recipients":[{"address":request["to"],"state":"processing"}],"ids":[request["idempotency_key"]]})),
        Reply::EmptyIds => write_reply_with_result(stream, request, json!({"recipients":[{"address":request["to"],"state":"accepted"}],"ids":[]})),
        Reply::EmptyId => write_reply_with_result(stream, request, json!({"recipients":[{"address":request["to"],"state":"accepted"}],"ids":[""]})),
        Reply::NonStringId => write_reply_with_result(stream, request, json!({"recipients":[{"address":request["to"],"state":"accepted"}],"ids":[7]})),
        Reply::OuterIdMismatch => {
            let response = json!({"id": uuid::Uuid::new_v4(), "ok": true, "result": {"recipients":[{"address":request["to"],"state":"accepted"}],"ids":[request["idempotency_key"]]}});
            writeln!(stream, "{response}").map_err(Into::into)
        }
    }
}

fn write_reply(stream: &mut std::fs::File, request: &Value, state: &str) -> Result<()> {
    write_reply_with_result(
        stream,
        request,
        json!({
            "recipients": [{"address": request["to"], "state": state}],
            "ids": [request["idempotency_key"]],
        }),
    )
}
