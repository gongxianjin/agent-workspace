//! Injectable local controller-to-runner relay boundary.
//!
//! This seam is intentionally in-process. It preserves the persisted signed
//! frame and does not claim Reason, gateway, or cross-host transport support.

use anyhow::{ensure, Context, Result};
use serde_json::from_slice;

use super::{
    protocol::Envelope,
    relay::{RelayBridge, RelayEndpoint, RelayOutcome, RelayTransport},
    runner::Runner,
    store::Store,
};

pub trait ControllerRunnerRelay {
    fn forward_controller_outbox(
        &mut self,
        store: &mut Store,
        runner: &mut Runner,
        now: u64,
    ) -> Result<()>;
}

struct RunnerEndpoint<'a> {
    runner: &'a mut Runner,
    now: u64,
}

impl RelayEndpoint for RunnerEndpoint<'_> {
    fn deliver(&mut self, frame: &[u8]) -> Result<()> {
        let message: Envelope = from_slice(frame).context("invalid Runner relay frame")?;
        self.runner.accept(&message, self.now).map(|_| ())
    }
}

struct TransportEndpoint<'a, T> {
    transport: &'a mut T,
    destination: &'a str,
}

impl<T: RelayTransport> RelayEndpoint for TransportEndpoint<'_, T> {
    fn deliver(&mut self, frame: &[u8]) -> Result<()> {
        self.transport.send(self.destination, frame)
    }
}

pub struct LocalRelayRuntime {
    bridge: RelayBridge,
}

impl LocalRelayRuntime {
    pub fn new(bridge: RelayBridge) -> Self {
        Self { bridge }
    }

    pub fn for_runner(runner: &Runner) -> Result<Self> {
        Ok(Self::new(runner.relay_bridge()?))
    }

    pub fn forward_one(
        &mut self,
        store: &mut Store,
        endpoint: &mut impl RelayEndpoint,
        now: u64,
    ) -> Result<Option<RelayOutcome>> {
        let Some((message_id, _destination, frame)) = store.pending_outbox_raw(1)?.into_iter().next() else {
            return Ok(None);
        };
        let outcome = self.forward_frame(&frame, endpoint, now)?;
        store.acknowledge_outbox(&message_id)?;
        Ok(Some(outcome))
    }

    pub fn forward_frame(
        &mut self,
        frame: &[u8],
        endpoint: &mut impl RelayEndpoint,
        now: u64,
    ) -> Result<RelayOutcome> {
        self.bridge.forward(frame, endpoint, now)
    }

    pub fn forward_one_via_transport(
        &mut self,
        store: &mut Store,
        transport: &mut impl RelayTransport,
        now: u64,
    ) -> Result<Option<RelayOutcome>> {
        let Some((message_id, destination, frame)) = store.pending_outbox_raw(1)?.into_iter().next() else {
            return Ok(None);
        };
        let mut endpoint = TransportEndpoint {
            transport,
            destination: &destination,
        };
        let outcome = self.bridge.forward(&frame, &mut endpoint, now)?;
        store.acknowledge_outbox(&message_id)?;
        Ok(Some(outcome))
    }
}

impl ControllerRunnerRelay for LocalRelayRuntime {
    fn forward_controller_outbox(
        &mut self,
        store: &mut Store,
        runner: &mut Runner,
        now: u64,
    ) -> Result<()> {
        for (message_id, destination, frame) in store.pending_outbox_raw(32)? {
            ensure!(destination == runner.address(), "local destination mismatch");
            let mut endpoint = RunnerEndpoint { runner, now };
            self.bridge.forward(&frame, &mut endpoint, now)?;
            store.acknowledge_outbox(&message_id)?;
        }
        Ok(())
    }
}
