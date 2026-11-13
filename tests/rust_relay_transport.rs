use anyhow::{bail, Result};
use agentranch::{
    config::{Actor, NodeSpec},
    core::{
        relay::{RelayBridge, RelayOutcome},
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

fn setup() -> (TempDir, Store, Runner, String, String, Vec<u8>) {
    let temp = tempfile::tempdir().unwrap();
    let spec = node();
    let mut store = Store::open(
        &temp.path().join("controller.sqlite"),
        "native-test",
        &[spec.clone()],
    )
    .unwrap();
    let run = store
        .dispatch(&Actor::Local("tester".into()), "sum-native-v1", 100)
        .unwrap();
    store.assign_next(101).unwrap().unwrap();
    let (_message_id, destination, frame) = store
        .pending_outbox_raw(1)
        .unwrap()
        .into_iter()
        .next()
        .unwrap();
    let runner = Runner::open(&temp.path().join("runner.sqlite"), "native-test", spec).unwrap();
    (temp, store, runner, run.id, destination, frame)
}

struct RecordingTransport {
    destinations: Vec<String>,
    frames: Vec<Vec<u8>>,
    failure: Option<String>,
}

impl RecordingTransport {
    fn successful() -> Self {
        Self {
            destinations: Vec::new(),
            frames: Vec::new(),
            failure: None,
        }
    }

    fn failing(message: &str) -> Self {
        Self {
            destinations: Vec::new(),
            frames: Vec::new(),
            failure: Some(message.into()),
        }
    }
}

impl agentranch::core::relay::RelayTransport for RecordingTransport {
    fn send(&mut self, destination: &str, frame: &[u8]) -> Result<()> {
        if let Some(message) = &self.failure {
            bail!("{message}");
        }
        self.destinations.push(destination.into());
        self.frames.push(frame.to_vec());
        Ok(())
    }
}

fn runtime() -> LocalRelayRuntime {
    LocalRelayRuntime::new(RelayBridge::new("native-test", "B", [7; 32]).unwrap())
}

#[test]
fn transport_forward_preserves_destination_and_raw_frame_before_ack() {
    let (_temp, mut store, _runner, _message_id, destination, frame) = setup();
    let expected_destination = destination.clone();
    let expected_frame = frame.clone();
    let mut relay = runtime();
    let mut transport = RecordingTransport::successful();

    assert_eq!(
        relay
            .forward_one_via_transport(&mut store, &mut transport, 110)
            .unwrap(),
        Some(RelayOutcome::Forwarded)
    );
    assert_eq!(transport.destinations, vec![expected_destination]);
    assert_eq!(transport.frames, vec![expected_frame]);
    assert_eq!(store.outbox_count().unwrap(), 0);
}

#[test]
fn transport_failure_keeps_outbox_and_assignment_without_ready() {
    let (_temp, mut store, runner, run_id, _destination, _frame) = setup();
    let mut relay = runtime();
    let mut transport = RecordingTransport::failing("synthetic transport failure");

    assert!(relay
        .forward_one_via_transport(&mut store, &mut transport, 110)
        .is_err());
    assert_eq!(store.outbox_count().unwrap(), 1);
    assert!(!store.assignment(&run_id, 1).unwrap().unwrap().released);
    assert!(!runner
        .pending_outbox(10)
        .unwrap()
        .iter()
        .any(|message| message.kind == "ready"));
    assert!(transport.destinations.is_empty());
    assert!(transport.frames.is_empty());
}
