use anyhow::{bail, Result};
use agentranch::{
    adapters::local::{pump_local_with_relay, LocalAo},
    config::{Actor, NodeSpec},
    core::{
        relay_runtime::{ControllerRunnerRelay, LocalRelayRuntime},
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

fn setup() -> (TempDir, Store, Runner, LocalAo) {
    let temp = tempfile::tempdir().unwrap();
    let spec = node();
    let store = Store::open(&temp.path().join("controller.sqlite"), "native", &[spec.clone()]).unwrap();
    let runner = Runner::open(&temp.path().join("runner.sqlite"), "native", spec).unwrap();
    let ao = LocalAo::new(
        std::path::Path::new(env!("CARGO_BIN_EXE_aw-cli")),
        &temp.path().join("workers"),
    )
    .unwrap();
    (temp, store, runner, ao)
}

struct CountingRelay {
    inner: LocalRelayRuntime,
    calls: usize,
}

impl ControllerRunnerRelay for CountingRelay {
    fn forward_controller_outbox(
        &mut self,
        store: &mut Store,
        runner: &mut Runner,
        now: u64,
    ) -> Result<()> {
        self.calls += usize::try_from(store.outbox_count()?).unwrap();
        self.inner.forward_controller_outbox(store, runner, now)
    }
}

struct FailingRelay;

impl ControllerRunnerRelay for FailingRelay {
    fn forward_controller_outbox(
        &mut self,
        _store: &mut Store,
        _runner: &mut Runner,
        _now: u64,
    ) -> Result<()> {
        bail!("synthetic production relay transport failure")
    }
}

#[test]
fn production_local_pump_uses_relay_and_acknowledges_persisted_outbox_once() {
    let (_temp, mut store, mut runner, mut ao) = setup();
    let run = store
        .dispatch(&Actor::Local("tester".into()), "sum-native-v1", 100)
        .unwrap();
    let mut relay = CountingRelay {
        inner: LocalRelayRuntime::for_runner(&runner).unwrap(),
        calls: 0,
    };

    pump_local_with_relay(&mut store, &mut runner, &mut ao, &mut relay, 100).unwrap();
    assert_eq!(relay.calls, 1);
    assert_eq!(store.outbox_count().unwrap(), 0);

    pump_local_with_relay(&mut store, &mut runner, &mut ao, &mut relay, 100).unwrap();
    assert_eq!(relay.calls, 1, "an acknowledged outbox must not be delivered again");
    assert_eq!(store.outbox_count().unwrap(), 0);
    assert_eq!(store.run(&run.id).unwrap().status, "running");
}

#[test]
fn production_local_pump_failure_retains_outbox_reservation_and_ready_state() {
    let (_temp, mut store, mut runner, mut ao) = setup();
    let run = store
        .dispatch(&Actor::Local("tester".into()), "sum-native-v1", 100)
        .unwrap();
    let mut relay = FailingRelay;

    assert!(pump_local_with_relay(&mut store, &mut runner, &mut ao, &mut relay, 100).is_err());
    assert_eq!(store.outbox_count().unwrap(), 1);
    assert!(!store.assignment(&run.id, 1).unwrap().unwrap().released);
    assert!(!runner
        .pending_outbox(10)
        .unwrap()
        .iter()
        .any(|message| message.kind == "ready"));
}
