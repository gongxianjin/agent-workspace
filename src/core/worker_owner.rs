//! Pure consistency validation for the proposed Linux worker-owner evidence v1.
//!
//! Callers must supply an independently trusted, persisted expected binding. This
//! module does not create that binding or establish its trust. Hashes and matching
//! fields do not prove authenticity, actual direct wait, actual bytes read/EOF,
//! process emptiness, referenced-file existence, filesystem safety, or durability.
//! No OS, filesystem, network, clock, process, or release operations occur here.
//! A returned record is ordinary data: normal zero exit is only a data condition,
//! never business success, trusted exit proof, or release/reservation authority.

use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Deserializer, Serialize};
use serde_json::json;
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::core::protocol::{ExecutionIdentity, canonical};

const SCHEMA: &str = "linux-worker-owner-evidence/1";
const MAX_PREFIX_BYTES: usize = 4096;
const MAX_RECORD_BYTES: usize = 16384;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerRef {
    pub node_id: String,
    pub instance_id: String,
    pub boot_id: String,
    pub owner_handle: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerProcessIdentity {
    pub pid: u32,
    pub start_ticks: u64,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    pub pid: u32,
    pub parent_pid: u32,
    pub start_ticks: u64,
    pub pgid: u32,
    pub cgroup_path: String,
}

/// Separate caller input, deliberately not decoded from an evidence bundle.
/// Future callers must obtain every value from a trusted persisted binding.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExpectedOwnerBinding {
    pub execution: ExecutionIdentity,
    pub owner: OwnerRef,
    pub request_sha256: String,
    pub owner_process: OwnerProcessIdentity,
    pub process: ProcessIdentity,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerEvidenceReference {
    pub schema: String,
    pub binding_sha256: String,
    pub bundle_sha256: String,
    pub record_sha256: String,
    pub report_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OwnerVerification {
    pub passed: bool,
    pub state: String,
    pub details: serde_json::Value,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct OwnerTerminalSubmission {
    pub reference: OwnerEvidenceReference,
    pub bundle_json: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(tag = "phase", rename_all = "snake_case", deny_unknown_fields)]
pub enum OwnerReportPayload {
    Launch {
        schema: String,
        session: String,
        binding: ExpectedOwnerBinding,
    },
    Terminal {
        schema: String,
        session: String,
        report: serde_json::Value,
        verification: OwnerVerification,
        owner_evidence: OwnerTerminalSubmission,
        exit_proof: crate::core::store::ExitProof,
    },
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct DirectWaitEvidence {
    pub wait_completed: bool,
    pub method: String,
    pub raw_wait_status: i32,
    #[serde(deserialize_with = "required_nullable")]
    pub exit_code: Option<u8>,
    #[serde(deserialize_with = "required_nullable")]
    pub termination_signal: Option<u32>,
    pub numeric_exit: i32,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerStreamEvidence {
    pub prefix_len: u32,
    pub total_read_bytes: u64,
    pub truncated: bool,
    pub eof: bool,
    #[serde(deserialize_with = "required_nullable")]
    pub error: Option<String>,
    pub prefix_sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct CleanupEvidence {
    pub proc_scan_complete: bool,
    pub process_group_scan_complete: bool,
    pub cgroup_scan_complete: bool,
    pub pgid: u32,
    pub cgroup_path: String,
    pub process_group_members: u64,
    pub cgroup_members: u64,
    #[serde(deserialize_with = "required_nullable")]
    pub error: Option<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct EvidenceRefs {
    pub record: String,
    pub stdout: String,
    pub stderr: String,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerEvidenceRecord {
    pub schema: String,
    pub execution: ExecutionIdentity,
    pub owner: OwnerRef,
    pub request_sha256: String,
    pub owner_process: OwnerProcessIdentity,
    pub process: ProcessIdentity,
    pub direct_wait: DirectWaitEvidence,
    pub stdout: OwnerStreamEvidence,
    pub stderr: OwnerStreamEvidence,
    pub cleanup: CleanupEvidence,
    pub evidence_refs: EvidenceRefs,
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct OwnerEvidenceBundle {
    pub record: OwnerEvidenceRecord,
    pub record_sha256: String,
    pub stdout_prefix_hex: String,
    pub stderr_prefix_hex: String,
}

// Unlike an unannotated Option<T>, deserialize_with without default requires the
// key to exist. Explicit null still deserializes to None. Derived struct decoding
// rejects duplicate keys (including null-valued duplicates) and unknown fields.
fn required_nullable<'de, D, T>(deserializer: D) -> std::result::Result<Option<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer)
}

fn lower_hex(value: &str) -> bool {
    value.bytes().all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_digest(value: &str) -> Result<()> {
    ensure!(value.len() == 64 && lower_hex(value), "invalid lowercase SHA256");
    Ok(())
}

fn validate_execution(execution: &ExecutionIdentity) -> Result<()> {
    execution.validate()?;
    ensure!(execution.session.as_deref().is_some_and(|value| !value.is_empty()),
        "owner execution requires nonempty session");
    Ok(())
}

fn validate_uuid(value: &str, require_v4: bool) -> Result<()> {
    let uuid = Uuid::parse_str(value).context("invalid owner UUID")?;
    ensure!(uuid.hyphenated().to_string() == value, "owner UUID is not canonical lowercase");
    if require_v4 {
        let bytes = uuid.as_bytes();
        ensure!(bytes[6] >> 4 == 4 && bytes[8] & 0xc0 == 0x80,
            "owner instance must be RFC-variant UUID v4");
    }
    Ok(())
}

/// Deterministic data reference, not a signature, secret, or authorization token.
pub fn derive_owner_handle(
    execution: &ExecutionIdentity,
    node_id: &str,
    instance_id: &str,
    boot_id: &str,
) -> Result<String> {
    validate_execution(execution)?;
    ensure!(crate::config::valid_id(node_id), "invalid owner node");
    validate_uuid(instance_id, true)?;
    validate_uuid(boot_id, false)?;
    let preimage = json!({
        "schema": SCHEMA,
        "execution": execution,
        "node_id": node_id,
        "instance_id": instance_id,
        "boot_id": boot_id,
    });
    Ok(format!("ow-{}", hex::encode(Sha256::digest(canonical(&preimage)?))))
}

fn cgroup_reference(owner: &OwnerRef) -> String {
    format!("workers/{}/{}", owner.instance_id, owner.owner_handle)
}

fn evidence_references(owner: &OwnerRef) -> EvidenceRefs {
    let prefix = format!("owners/{}/{}", owner.instance_id, owner.owner_handle);
    EvidenceRefs {
        record: format!("{prefix}/record.json"),
        stdout: format!("{prefix}/stdout.bin"),
        stderr: format!("{prefix}/stderr.bin"),
    }
}

fn validate_expected(expected: &ExpectedOwnerBinding) -> Result<()> {
    let owner = &expected.owner;
    let handle = derive_owner_handle(&expected.execution, &owner.node_id,
        &owner.instance_id, &owner.boot_id)?;
    ensure!(owner.owner_handle == handle, "expected owner handle mismatch");
    validate_digest(&expected.request_sha256)?;
    let parent = &expected.owner_process;
    let child = &expected.process;
    ensure!(parent.pid > 0 && parent.start_ticks > 0, "invalid expected owner process");
    ensure!(child.pid > 0 && child.parent_pid > 0 && child.start_ticks > 0 && child.pgid > 0,
        "invalid expected child process");
    ensure!(child.pid != parent.pid && child.parent_pid == parent.pid,
        "expected direct-parent binding mismatch");
    ensure!(child.pgid == child.pid, "expected dedicated process-group binding mismatch");
    ensure!(child.cgroup_path == cgroup_reference(owner), "expected cgroup reference mismatch");
    Ok(())
}

pub fn validate_expected_binding(expected: &ExpectedOwnerBinding) -> Result<()> {
    validate_expected(expected)
}

pub fn validate_owner_evidence_reference(reference: &OwnerEvidenceReference) -> Result<()> {
    ensure!(reference.schema == "owner-evidence-reference/1", "unsupported owner evidence reference schema");
    for digest in [
        &reference.binding_sha256,
        &reference.bundle_sha256,
        &reference.record_sha256,
        &reference.report_sha256,
    ] {
        validate_digest(digest)?;
    }
    Ok(())
}

pub fn validate_owner_verification(verification: &OwnerVerification) -> Result<()> {
    ensure!(verification.state == if verification.passed { "verified" } else { "failed" },
        "owner verification state does not match passed flag");
    ensure!(verification.details.is_object(), "owner verification details must be an object");
    ensure!(canonical(&verification.details)?.len() <= 4096, "owner verification details exceed 4096 canonical bytes");
    Ok(())
}

pub fn owner_binding_sha256(expected: &ExpectedOwnerBinding) -> Result<String> {
    validate_expected_binding(expected)?;
    let value = json!({ "schema": "owner-binding-reference/1", "binding": expected });
    Ok(hex::encode(Sha256::digest(canonical(&value)?)))
}

pub fn owner_bundle_sha256(bundle: &OwnerEvidenceBundle) -> Result<String> {
    let encoded = canonical(&serde_json::to_value(bundle)?)?;
    ensure!(encoded.len() <= MAX_RECORD_BYTES + (MAX_PREFIX_BYTES * 4) + 512,
        "owner evidence bundle exceeds local canonical bound");
    Ok(hex::encode(Sha256::digest(encoded)))
}

pub fn owner_report_sha256(
    execution: &ExecutionIdentity,
    report: &serde_json::Value,
    verification: &OwnerVerification,
) -> Result<String> {
    validate_execution(execution)?;
    ensure!(report.is_object(), "owner report must be an object");
    // Raised with ARTIFACT_TOTAL_BUDGET (run bc1cf37d): the project artifact
    // package rides inside the enriched report as content_hex.
    ensure!(canonical(report)?.len() <= 73_728, "owner report exceeds 73728 canonical bytes");
    validate_owner_verification(verification)?;
    let value = json!({
        "schema": "owner-report-binding/1",
        "execution": execution,
        "report": report,
        "verification": verification,
    });
    Ok(hex::encode(Sha256::digest(canonical(&value)?)))
}

fn validate_record_binding(record: &OwnerEvidenceRecord, expected: &ExpectedOwnerBinding) -> Result<()> {
    ensure!(record.schema == SCHEMA, "unsupported owner evidence schema");
    validate_execution(&record.execution)?;
    ensure!(record.execution == expected.execution, "owner execution mismatch");
    ensure!(record.owner == expected.owner, "owner reference mismatch");
    ensure!(record.request_sha256 == expected.request_sha256, "owner request digest mismatch");
    ensure!(record.owner_process == expected.owner_process, "owner process mismatch");
    ensure!(record.process == expected.process, "owned child process mismatch");
    ensure!(record.evidence_refs == evidence_references(&expected.owner), "evidence reference mismatch");
    Ok(())
}

fn validate_record_hash(bundle: &OwnerEvidenceBundle) -> Result<()> {
    let encoded = canonical(&serde_json::to_value(&bundle.record)?)?;
    ensure!(encoded.len() <= MAX_RECORD_BYTES, "record exceeds 16384 canonical bytes");
    validate_digest(&bundle.record_sha256)?;
    ensure!(bundle.record_sha256 == hex::encode(Sha256::digest(&encoded)), "owner record hash mismatch");
    Ok(())
}

fn validate_wait(wait: &DirectWaitEvidence) -> Result<()> {
    ensure!(wait.wait_completed, "direct wait is pending");
    ensure!(matches!(wait.method.as_str(), "child_wait" | "child_try_wait"), "invalid direct-wait method");
    match (wait.exit_code, wait.termination_signal) {
        (Some(code), None) => {
            let numeric = i32::from(code);
            ensure!(wait.raw_wait_status == (numeric << 8) && wait.numeric_exit == numeric,
                "inconsistent normal Unix wait status");
        }
        (None, Some(signal)) => {
            ensure!((1..=64).contains(&signal), "invalid Unix termination signal");
            let numeric = signal as i32;
            ensure!(wait.raw_wait_status == numeric || wait.raw_wait_status == (numeric | 0x80),
                "inconsistent signal Unix wait status");
            ensure!(wait.numeric_exit == -numeric, "signal numeric exit must be negative signal");
        }
        _ => anyhow::bail!("direct wait requires exactly one terminal outcome"),
    }
    Ok(())
}

fn validate_stream(stream: &OwnerStreamEvidence, prefix_hex: &str) -> Result<()> {
    ensure!(prefix_hex.len() <= MAX_PREFIX_BYTES * 2 && prefix_hex.len() % 2 == 0 && lower_hex(prefix_hex),
        "invalid bounded lowercase prefix hex");
    let prefix = hex::decode(prefix_hex).context("invalid raw stream prefix")?;
    ensure!(stream.prefix_len as usize == prefix.len(), "stream prefix length mismatch");
    ensure!(u64::from(stream.prefix_len) == stream.total_read_bytes.min(MAX_PREFIX_BYTES as u64),
        "stream read count/prefix mismatch");
    ensure!(stream.truncated == (stream.total_read_bytes > u64::from(stream.prefix_len)),
        "stream truncation mismatch");
    ensure!(stream.eof && stream.error.is_none(), "stream reader incomplete or failed");
    validate_digest(&stream.prefix_sha256)?;
    ensure!(stream.prefix_sha256 == hex::encode(Sha256::digest(&prefix)), "stream prefix hash mismatch");
    Ok(())
}

fn validate_cleanup(cleanup: &CleanupEvidence, expected: &ExpectedOwnerBinding) -> Result<()> {
    ensure!(cleanup.proc_scan_complete && cleanup.process_group_scan_complete && cleanup.cgroup_scan_complete,
        "cleanup scan incomplete");
    ensure!(cleanup.process_group_members == 0 && cleanup.cgroup_members == 0,
        "cleanup claims live process occupancy");
    ensure!(cleanup.error.is_none(), "cleanup scan failed");
    ensure!(cleanup.pgid == expected.process.pgid && cleanup.cgroup_path == expected.process.cgroup_path,
        "cleanup process-group/cgroup binding mismatch");
    Ok(())
}

/// Validate strict typed wire data against a separate trusted expected binding.
/// Normal, nonzero, and signal exits may all return ordinary consistent records.
/// No method on the returned data confers business success or proof authority.
pub fn validate_bundle(raw: &[u8], expected: Option<&ExpectedOwnerBinding>) -> Result<OwnerEvidenceRecord> {
    let expected = expected.context("trusted expected owner binding required; no auto-binding")?;
    validate_expected(expected).context("invalid expected owner binding")?;
    // Direct typed decoding is essential: Value pre-parsing would erase duplicates.
    let bundle: OwnerEvidenceBundle = serde_json::from_slice(raw).context("invalid owner evidence bundle")?;
    validate_record_hash(&bundle)?;
    validate_record_binding(&bundle.record, expected)?;
    validate_wait(&bundle.record.direct_wait)?;
    validate_stream(&bundle.record.stdout, &bundle.stdout_prefix_hex).context("invalid stdout evidence")?;
    validate_stream(&bundle.record.stderr, &bundle.stderr_prefix_hex).context("invalid stderr evidence")?;
    validate_cleanup(&bundle.record.cleanup, expected)?;
    Ok(bundle.record)
}
