//! Pure producer for Linux worker-owner evidence v1.
//!
//! This module records only facts supplied by an `OwnedProcessPort`; it does not
//! launch a real process itself, inspect an OS, persist a binding, authenticate an
//! owner, write files, or grant release authority. A returned normal-zero record is
//! ordinary data, never business success or trusted exit proof.

use anyhow::{Context, Result, ensure};
use sha2::{Digest, Sha256};

use crate::core::{
    protocol::{ExecutionIdentity, canonical},
    worker_owner::{
        CleanupEvidence, DirectWaitEvidence, EvidenceRefs, ExpectedOwnerBinding,
        OwnerEvidenceBundle, OwnerEvidenceRecord, OwnerProcessIdentity, OwnerRef,
        OwnerStreamEvidence, ProcessIdentity, derive_owner_handle, validate_bundle,
    },
};

const SCHEMA: &str = "linux-worker-owner-evidence/1";
const MAX_REQUEST_BYTES: usize = 65_536;
const MAX_PREFIX_BYTES: usize = 4_096;
const MAX_RECORD_BYTES: usize = 16_384;

/// Trusted pre-launch input. Child identity facts are only known after launch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerLaunchPlan {
    pub execution: ExecutionIdentity,
    pub node_id: String,
    pub instance_id: String,
    pub boot_id: String,
    pub request_bytes: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchFacts {
    pub child_pid: u32,
    pub child_start_ticks: u64,
    pub pgid: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StreamReadOutcome {
    pub prefix: Vec<u8>,
    pub total_read_bytes: u64,
    pub eof: bool,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CleanupFacts {
    pub proc_scan_complete: bool,
    pub process_group_scan_complete: bool,
    pub cgroup_scan_complete: bool,
    pub process_group_members: u64,
    pub cgroup_members: u64,
    pub error: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TerminalFacts {
    pub wait_completed: bool,
    pub wait_method: String,
    pub raw_wait_status: i32,
    pub stdout: StreamReadOutcome,
    pub stderr: StreamReadOutcome,
    pub cleanup: CleanupFacts,
}

/// Provider-neutral owner-process boundary. A real Linux implementation is deferred.
pub trait OwnedProcessPort {
    fn owner_identity(&self) -> Result<OwnerProcessIdentity>;
    fn launch(&mut self, plan: &OwnerLaunchPlan) -> Result<LaunchFacts>;
    fn wait_terminal(&mut self) -> Result<TerminalFacts>;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum WaitOutcome {
    Exit(u8),
    Signal(u32),
}

/// Decode only v1 terminal Unix wait-status forms, independent of the host OS.
pub fn decode_raw_wait_status(raw: i32) -> Option<WaitOutcome> {
    if raw < 0 {
        return None;
    }
    if raw <= (u8::MAX as i32) << 8 && raw & 0xff == 0 {
        return Some(WaitOutcome::Exit((raw >> 8) as u8));
    }
    if (1..=64).contains(&raw) {
        return Some(WaitOutcome::Signal(raw as u32));
    }
    if raw & !0xff == 0 && raw & 0x80 != 0 {
        let signal = raw & !0x80;
        if (1..=64).contains(&signal) {
            return Some(WaitOutcome::Signal(signal as u32));
        }
    }
    None
}

/// Complete the post-launch binding from trusted plan input and observed launch facts.
pub fn complete_binding(
    plan: &OwnerLaunchPlan,
    owner_process: &OwnerProcessIdentity,
    launch: &LaunchFacts,
) -> Result<ExpectedOwnerBinding> {
    ensure!(plan.request_bytes.len() <= MAX_REQUEST_BYTES, "request exceeds 65536 bytes");
    ensure!(owner_process.pid > 0 && owner_process.start_ticks > 0, "invalid owner process");
    ensure!(launch.child_pid > 0 && launch.child_start_ticks > 0 && launch.pgid > 0,
        "invalid child process");
    ensure!(launch.child_pid != owner_process.pid, "child process must differ from owner");
    ensure!(launch.pgid == launch.child_pid, "child must own its dedicated process group");

    let owner_handle = derive_owner_handle(
        &plan.execution,
        &plan.node_id,
        &plan.instance_id,
        &plan.boot_id,
    )?;
    let owner = OwnerRef {
        node_id: plan.node_id.clone(),
        instance_id: plan.instance_id.clone(),
        boot_id: plan.boot_id.clone(),
        owner_handle: owner_handle.clone(),
    };
    Ok(ExpectedOwnerBinding {
        execution: plan.execution.clone(),
        owner,
        request_sha256: digest(&plan.request_bytes),
        owner_process: owner_process.clone(),
        process: ProcessIdentity {
            pid: launch.child_pid,
            parent_pid: owner_process.pid,
            start_ticks: launch.child_start_ticks,
            pgid: launch.pgid,
            cgroup_path: format!("workers/{}/{}", plan.instance_id, owner_handle),
        },
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OwnerEvidenceFile {
    pub relative_path: String,
    pub bytes: Vec<u8>,
}

/// Construct the exact in-memory three-file layout for a future durable writer.
pub fn owner_evidence_files(bundle: &OwnerEvidenceBundle) -> Result<Vec<OwnerEvidenceFile>> {
    let stdout = hex::decode(&bundle.stdout_prefix_hex).context("invalid stdout prefix hex")?;
    let stderr = hex::decode(&bundle.stderr_prefix_hex).context("invalid stderr prefix hex")?;
    Ok(vec![
        OwnerEvidenceFile {
            relative_path: bundle.record.evidence_refs.record.clone(),
            bytes: canonical(&serde_json::to_value(&bundle.record)?)?,
        },
        OwnerEvidenceFile {
            relative_path: bundle.record.evidence_refs.stdout.clone(),
            bytes: stdout,
        },
        OwnerEvidenceFile {
            relative_path: bundle.record.evidence_refs.stderr.clone(),
            bytes: stderr,
        },
    ])
}

#[derive(Debug)]
pub struct OwnerProduction {
    pub binding: ExpectedOwnerBinding,
    pub bundle: OwnerEvidenceBundle,
    bundle_bytes: Vec<u8>,
}

impl OwnerProduction {
    pub fn bundle_bytes(&self) -> &[u8] {
        &self.bundle_bytes
    }

    pub fn record_json_bytes(&self) -> Result<Vec<u8>> {
        canonical(&serde_json::to_value(&self.bundle.record)?)
    }
}

/// Complete a bundle after launch has been independently observed and admitted.
/// This split is required so callers can persist the launch binding before waiting
/// for terminal evidence; terminal data cannot bootstrap its own expected binding.
pub fn finish_owner_evidence(
    port: &mut impl OwnedProcessPort,
    plan: &OwnerLaunchPlan,
    owner_process: &OwnerProcessIdentity,
    launch: &LaunchFacts,
) -> Result<OwnerProduction> {
    let binding = complete_binding(plan, owner_process, launch)?;
    let terminal = port.wait_terminal()?;

    ensure!(terminal.wait_completed, "owner wait pending; no bundle");
    ensure!(matches!(terminal.wait_method.as_str(), "child_wait" | "child_try_wait"),
        "invalid owner wait method");
    let outcome = decode_raw_wait_status(terminal.raw_wait_status)
        .context("owner wait status is not terminal evidence")?;
    let stdout = stream_evidence(&terminal.stdout)?;
    let stderr = stream_evidence(&terminal.stderr)?;
    validate_cleanup(&terminal.cleanup)?;

    let record = OwnerEvidenceRecord {
        schema: SCHEMA.into(),
        execution: binding.execution.clone(),
        owner: binding.owner.clone(),
        request_sha256: binding.request_sha256.clone(),
        owner_process: binding.owner_process.clone(),
        process: binding.process.clone(),
        direct_wait: wait_evidence(&terminal, outcome),
        stdout,
        stderr,
        cleanup: CleanupEvidence {
            proc_scan_complete: terminal.cleanup.proc_scan_complete,
            process_group_scan_complete: terminal.cleanup.process_group_scan_complete,
            cgroup_scan_complete: terminal.cleanup.cgroup_scan_complete,
            pgid: binding.process.pgid,
            cgroup_path: binding.process.cgroup_path.clone(),
            process_group_members: terminal.cleanup.process_group_members,
            cgroup_members: terminal.cleanup.cgroup_members,
            error: terminal.cleanup.error.clone(),
        },
        evidence_refs: evidence_refs(&binding.owner),
    };
    let record_bytes = canonical(&serde_json::to_value(&record)?)?;
    ensure!(record_bytes.len() <= MAX_RECORD_BYTES, "record exceeds 16384 canonical bytes");

    let bundle = OwnerEvidenceBundle {
        record,
        record_sha256: digest(&record_bytes),
        stdout_prefix_hex: hex::encode(retained_prefix(&terminal.stdout)),
        stderr_prefix_hex: hex::encode(retained_prefix(&terminal.stderr)),
    };
    let bundle_bytes = serde_json::to_vec(&bundle)?;
    validate_bundle(&bundle_bytes, Some(&binding))?;
    Ok(OwnerProduction { binding, bundle, bundle_bytes })
}

/// Produce a complete owner bundle for contract users that do not need a
/// durable launch-registration boundary. Product paths should use
/// `finish_owner_evidence` after persisting the binding.
pub fn produce_owner_evidence(
    port: &mut impl OwnedProcessPort,
    plan: &OwnerLaunchPlan,
) -> Result<OwnerProduction> {
    let owner_process = port.owner_identity()?;
    let launch = port.launch(plan)?;
    finish_owner_evidence(port, plan, &owner_process, &launch)
}

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn retained_prefix(stream: &StreamReadOutcome) -> &[u8] {
    &stream.prefix[..stream.prefix.len().min(MAX_PREFIX_BYTES)]
}

fn stream_evidence(stream: &StreamReadOutcome) -> Result<OwnerStreamEvidence> {
    ensure!(stream.error.is_none() && stream.eof, "stream reader incomplete or failed");
    ensure!(stream.total_read_bytes >= stream.prefix.len() as u64,
        "stream prefix exceeds reported total read bytes");
    let prefix = retained_prefix(stream);
    let prefix_len = prefix.len() as u32;
    Ok(OwnerStreamEvidence {
        prefix_len,
        total_read_bytes: stream.total_read_bytes,
        truncated: stream.total_read_bytes > u64::from(prefix_len),
        eof: stream.eof,
        error: stream.error.clone(),
        prefix_sha256: digest(prefix),
    })
}

fn validate_cleanup(cleanup: &CleanupFacts) -> Result<()> {
    ensure!(cleanup.proc_scan_complete && cleanup.process_group_scan_complete && cleanup.cgroup_scan_complete,
        "cleanup scan incomplete");
    ensure!(cleanup.process_group_members == 0 && cleanup.cgroup_members == 0,
        "cleanup claims live process occupancy");
    ensure!(cleanup.error.is_none(), "cleanup scan failed");
    Ok(())
}

fn wait_evidence(terminal: &TerminalFacts, outcome: WaitOutcome) -> DirectWaitEvidence {
    let (exit_code, termination_signal, numeric_exit) = match outcome {
        WaitOutcome::Exit(code) => (Some(code), None, i32::from(code)),
        WaitOutcome::Signal(signal) => (None, Some(signal), -(signal as i32)),
    };
    DirectWaitEvidence {
        wait_completed: terminal.wait_completed,
        method: terminal.wait_method.clone(),
        raw_wait_status: terminal.raw_wait_status,
        exit_code,
        termination_signal,
        numeric_exit,
    }
}

fn evidence_refs(owner: &OwnerRef) -> EvidenceRefs {
    let prefix = format!("owners/{}/{}", owner.instance_id, owner.owner_handle);
    EvidenceRefs {
        record: format!("{prefix}/record.json"),
        stdout: format!("{prefix}/stdout.bin"),
        stderr: format!("{prefix}/stderr.bin"),
    }
}
