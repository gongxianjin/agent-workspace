use anyhow::{bail, Result};
use agentranch::{
    config::{Actor, NodeSpec},
    core::{
        protocol::Envelope,
        relay::{RelayBridge, RelayEndpoint, RelayOutcome},
        relay_runtime::LocalRelayRuntime,
        runner::Runner,
        store::Store,
    },
};
use tempfile::TempDir;

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(),
        address: "local-B/runner".into(),
        key_hex: "07".repeat(32),
        slots: 1,
        admitted: true,
        healthy: true,
        draining: false,
        templates: vec!["sum-native-v1".into()],
    }
}

fn setup() -> (TempDir, Store, Runner, String, Vec<u8>) {
    let temp = tempfile::tempdir().unwrap();
    let spec = node();
    let mut store = Store::open(&temp.path().join("controller.sqlite"), "native-test", &[spec.clone()]).unwrap();
    let run = store.dispatch(&Actor::Local("tester".into()), "sum-native-v1", 100).unwrap();
    store.assign_next(101).unwrap().unwrap();
    let frame = store.pending_outbox_raw(1).unwrap().into_iter().next().unwrap().2;
    let runner = Runner::open(&temp.path().join("runner.sqlite"), "native-test", spec).unwrap();
    (temp, store, runner, run.id, frame)
}

struct RunnerRecordingEndpoint<'a> {
    runner: &'a mut Runner,
    now: u64,
    frames: Vec<Vec<u8>>,
}

impl RelayEndpoint for RunnerRecordingEndpoint<'_> {
    fn deliver(&mut self, frame: &[u8]) -> Result<()> {
        self.frames.push(frame.to_vec());
        let message: Envelope = serde_json::from_slice(frame)?;
        self.runner.accept(&message, self.now).map(|_| ())
    }
}

struct FailingEndpoint;

impl RelayEndpoint for FailingEndpoint {
    fn deliver(&mut self, _frame: &[u8]) -> Result<()> {
        bail!("synthetic relay transport failure")
    }
}

fn runtime() -> LocalRelayRuntime {
    LocalRelayRuntime::new(RelayBridge::new("native-test", "B", [7; 32]).unwrap())
}

#[test]
fn controller_outbox_reaches_runner_as_exact_signed_bytes_and_deduplicates() {
    let (_temp, mut store, mut runner, _run_id, frame) = setup();
    let mut relay = runtime();
    let mut endpoint = RunnerRecordingEndpoint { runner: &mut runner, now: 110, frames: Vec::new() };

    assert_eq!(relay.forward_one(&mut store, &mut endpoint, 110).unwrap(), Some(RelayOutcome::Forwarded));
    assert_eq!(endpoint.frames, vec![frame.clone()]);
    assert_eq!(store.outbox_count().unwrap(), 0);
    assert_eq!(relay.forward_frame(&frame, &mut endpoint, 110).unwrap(), RelayOutcome::Duplicate);
    assert_eq!(endpoint.frames, vec![frame]);
}

#[test]
fn relay_rejects_same_message_id_with_different_signed_bytes() {
    let (_temp, _store, mut runner, _run_id, frame) = setup();
    let mut relay = runtime();
    let mut endpoint = RunnerRecordingEndpoint { runner: &mut runner, now: 110, frames: Vec::new() };
    assert_eq!(relay.forward_frame(&frame, &mut endpoint, 110).unwrap(), RelayOutcome::Forwarded);

    let conflicting = [b" ".as_slice(), frame.as_slice(), b"\n".as_slice()].concat();
    assert!(relay.forward_frame(&conflicting, &mut endpoint, 110).is_err());
    assert_eq!(endpoint.frames, vec![frame]);
}

#[test]
fn relay_transport_failure_keeps_outbox_and_reservation_without_ready() {
    let (_temp, mut store, runner, run_id, _frame) = setup();
    let mut relay = runtime();
    let mut endpoint = FailingEndpoint;

    assert!(relay.forward_one(&mut store, &mut endpoint, 110).is_err());
    assert_eq!(store.outbox_count().unwrap(), 1);
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
    assert!(!runner.pending_outbox(10).unwrap().iter().any(|message| message.kind == "ready"));
}
