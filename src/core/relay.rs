//! Pure-local signed Envelope relay contract.
//!
//! This module validates and forwards an existing signed wire frame without
//! changing its JSON bytes. It is deliberately not connected to Reason or the
//! controller/runner runtime; a transport can implement `RelayEndpoint` in a
//! local fixture or a separately tested loopback adapter.

use anyhow::{ensure, Context, Result};
use std::collections::BTreeMap;

use super::protocol::Envelope;

pub const MAX_RELAY_FRAME: usize = 64 * 1024;

pub trait RelayEndpoint {
    fn deliver(&mut self, frame: &[u8]) -> Result<()>;
}

pub trait RelayTransport {
    fn send(&mut self, destination: &str, frame: &[u8]) -> Result<()>;
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RelayOutcome {
    Forwarded,
    Duplicate,
}

pub struct RelayBridge {
    deployment: String,
    recipient: String,
    key: [u8; 32],
    delivered: BTreeMap<String, Vec<u8>>,
}

impl RelayBridge {
    pub fn new(deployment: &str, recipient: &str, key: [u8; 32]) -> Result<Self> {
        ensure!(crate::config::valid_id(deployment), "invalid relay deployment");
        ensure!(
            !recipient.is_empty()
                && recipient.len() <= 256
                && recipient.is_ascii()
                && !recipient.chars().any(char::is_whitespace),
            "invalid relay recipient"
        );
        Ok(Self {
            deployment: deployment.into(),
            recipient: recipient.into(),
            key,
            delivered: BTreeMap::new(),
        })
    }

    pub fn forward(
        &mut self,
        frame: &[u8],
        recipient_endpoint: &mut impl RelayEndpoint,
        now: u64,
    ) -> Result<RelayOutcome> {
        ensure!(frame.len() <= MAX_RELAY_FRAME, "relay frame exceeds 64KiB");
        let message: Envelope = serde_json::from_slice(frame).context("invalid relay envelope")?;
        message.verify(&self.key, &self.deployment, &self.recipient, now)?;
        if let Some(previous) = self.delivered.get(&message.message_id) {
            ensure!(previous == frame, "message id collision");
            return Ok(RelayOutcome::Duplicate);
        }
        recipient_endpoint.deliver(frame)?;
        self.delivered.insert(message.message_id, frame.to_vec());
        Ok(RelayOutcome::Forwarded)
    }
}
