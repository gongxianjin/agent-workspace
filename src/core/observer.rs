//! Runner-owned local process observer contract.
//!
//! This module is a bounded synthetic/Linux-compatible seam around the existing
//! `ManagedChild`. It proves OS wait and evidence identity without claiming that
//! an AO/Reason session is a local child process.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{fs, path::{Path, PathBuf}};

use crate::{
    adapters::process::{ChildEvidence, ManagedChild},
    core::protocol::ExecutionIdentity,
};

const MAX_REPORT_BYTES: usize = 16 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct StreamEvidence {
    pub bytes: Vec<u8>,
    pub total_bytes: usize,
    pub truncated: bool,
    pub sha256: String,
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
pub struct ProcessEvidence {
    pub identity: ExecutionIdentity,
    pub pid: u32,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub wait_completed: bool,
    pub exit_code: Option<i32>,
    pub stdout: StreamEvidence,
    pub stderr: StreamEvidence,
}

impl ProcessEvidence {
    pub fn success(&self) -> bool {
        self.wait_completed && self.exit_code == Some(0)
            && self.stdout.error.is_none() && self.stderr.error.is_none()
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ObserverEvent {
    Pending {
        identity: ExecutionIdentity,
        report: Option<Value>,
        reason: String,
    },
    Exited {
        identity: ExecutionIdentity,
        report: Option<Value>,
        evidence: ProcessEvidence,
    },
    Unknown {
        identity: Option<ExecutionIdentity>,
        reason: String,
    },
}

pub trait RunnerObserver {
    fn observe(&mut self, expected: &ExecutionIdentity) -> Result<ObserverEvent>;
}

pub struct ManagedProcessObserver {
    child: ManagedChild,
    identity: ExecutionIdentity,
    worker: PathBuf,
    template: String,
    artifact: PathBuf,
    cwd: PathBuf,
    terminal: Option<ObserverEvent>,
}

impl ManagedProcessObserver {
    pub fn spawn(
        worker: &Path,
        template: &str,
        artifact: &Path,
        identity: ExecutionIdentity,
    ) -> Result<Self> {
        identity.validate()?;
        let cwd = std::env::current_dir().context("observer current directory unavailable")?;
        let child = ManagedChild::spawn(
            worker,
            template,
            artifact,
            identity.session.as_deref().context("observer session missing")?,
        )?;
        Ok(Self {
            child,
            identity,
            worker: worker.to_owned(),
            template: template.into(),
            artifact: artifact.to_owned(),
            cwd,
            terminal: None,
        })
    }

    fn command_args(&self) -> Vec<String> {
        vec![
            self.worker.display().to_string(),
            "fixture-worker".into(),
            self.template.clone(),
            self.artifact.display().to_string(),
            self.identity.session.clone().unwrap_or_default(),
        ]
    }

    fn report(&self) -> Result<Option<Value>> {
        if !self.artifact.is_file() {
            return Ok(None);
        }
        let raw = fs::read(&self.artifact)?;
        ensure!(raw.len() <= MAX_REPORT_BYTES, "observer report exceeds bound");
        Ok(Some(serde_json::from_slice(&raw).context("observer report is invalid JSON")?))
    }

    fn validate_report(&self, report: &Value) -> Result<()> {
        ensure!(report["session"].as_str() == self.identity.session.as_deref(), "observer report identity mismatch");
        Ok(())
    }

    fn stream(bytes: Vec<u8>, total_bytes: usize, truncated: bool, error: Option<String>) -> StreamEvidence {
        StreamEvidence {
            sha256: hex::encode(Sha256::digest(&bytes)),
            bytes,
            total_bytes,
            truncated,
            error,
        }
    }

    fn evidence(&self, child: &ChildEvidence) -> ProcessEvidence {
        ProcessEvidence {
            identity: self.identity.clone(),
            pid: self.child.pid(),
            argv: self.command_args(),
            cwd: self.cwd.clone(),
            wait_completed: true,
            exit_code: child.exit_code,
            stdout: Self::stream(child.stdout.clone(), child.stdout_total_bytes, child.stdout_truncated, child.stdout_error.clone()),
            stderr: Self::stream(child.stderr.clone(), child.stderr_total_bytes, child.stderr_truncated, child.stderr_error.clone()),
        }
    }

    fn unknown(&self, reason: impl Into<String>) -> ObserverEvent {
        ObserverEvent::Unknown { identity: Some(self.identity.clone()), reason: reason.into() }
    }
}

fn admit_cached_event<'a>(expected: &ExecutionIdentity, owned: &ExecutionIdentity, terminal: Option<&'a ObserverEvent>) -> Option<ObserverEvent> {
    if expected != owned {
        return Some(ObserverEvent::Unknown { identity: Some(owned.clone()), reason: "observer execution identity mismatch; no exit release".into() });
    }
    terminal.cloned()
}

impl RunnerObserver for ManagedProcessObserver {
    fn observe(&mut self, expected: &ExecutionIdentity) -> Result<ObserverEvent> {
        if let Some(event) = admit_cached_event(expected, &self.identity, self.terminal.as_ref()) {
            return Ok(event);
        }
        let active = self.child.active()?;
        let report = match self.report() {
            Ok(report) => report,
            Err(error) if active > 0 => {
                return Ok(ObserverEvent::Pending {
                    identity: self.identity.clone(),
                    report: None,
                    reason: format!("report pending while process is running: {error}"),
                });
            }
            Err(error) => {
                let event = self.unknown(format!("observer report unavailable after wait: {error}"));
                self.terminal = Some(event.clone());
                return Ok(event);
            }
        };
        if let Some(report) = &report {
            if let Err(error) = self.validate_report(report) {
                let event = self.unknown(error.to_string());
                if active == 0 {
                    self.terminal = Some(event.clone());
                }
                return Ok(event);
            }
        }
        if active != 0 {
            return Ok(ObserverEvent::Pending {
                identity: self.identity.clone(),
                report,
                reason: "report observed before direct OS wait completed".into(),
            });
        }
        let Some(child) = self.child.evidence()? else {
            return Ok(ObserverEvent::Pending {
                identity: self.identity.clone(),
                report,
                reason: "OS wait completed but stdout/stderr evidence is still pending".into(),
            });
        };
        let event = ObserverEvent::Exited {
            identity: self.identity.clone(),
            report,
            evidence: self.evidence(&child),
        };
        self.terminal = Some(event.clone());
        Ok(event)
    }
}

#[cfg(test)]
mod tests {
    use super::{admit_cached_event, ObserverEvent};
    use crate::core::protocol::ExecutionIdentity;

    fn identity(session: &str) -> ExecutionIdentity {
        ExecutionIdentity::new("observer-admission", "cache-run", 1, "cache-epoch", Some(session), None, None)
    }

    #[test]
    fn cached_terminal_event_does_not_bypass_identity_admission() {
        let expected = identity("expected-session");
        let wrong = identity("wrong-session");
        let terminal = ObserverEvent::Exited {
            identity: expected.clone(),
            report: None,
            evidence: super::ProcessEvidence {
                identity: expected.clone(),
                pid: 1,
                argv: Vec::new(),
                cwd: std::path::PathBuf::from("."),
                wait_completed: true,
                exit_code: Some(0),
                stdout: super::StreamEvidence {
                    bytes: Vec::new(),
                    total_bytes: 0,
                    truncated: false,
                    sha256: String::new(),
                    error: None,
                },
                stderr: super::StreamEvidence {
                    bytes: Vec::new(),
                    total_bytes: 0,
                    truncated: false,
                    sha256: String::new(),
                    error: None,
                },
            },
        };

        let event = admit_cached_event(&wrong, &expected, Some(&terminal)).expect("wrong identity must be rejected");

        assert!(matches!(event, ObserverEvent::Unknown { identity: Some(identity), .. } if identity == expected));
    }
}
