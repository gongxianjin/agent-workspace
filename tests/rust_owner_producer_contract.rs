//! Contract-first scripted tests for the M1 owner-evidence producer.
//! These fixtures establish internal consistency only; they do not authenticate an
//! owner, observe an operating system, persist a binding, or admit/release work.

use ao_reason_workbench::core::{
    owner_producer::{
        CleanupFacts, LaunchFacts, OwnedProcessPort, OwnerLaunchPlan, StreamReadOutcome,
        TerminalFacts, complete_binding, owner_evidence_files, produce_owner_evidence,
    },
    protocol::{ExecutionIdentity, canonical},
    worker_owner::{
        ExpectedOwnerBinding, OwnerProcessIdentity, OwnerRef, ProcessIdentity, validate_bundle,
    },
};
use anyhow::Result;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::cell::RefCell;

const INSTANCE: &str = "11111111-1111-4111-8111-111111111111";
const BOOT: &str = "22222222-2222-4222-8222-222222222222";
const REQUEST: &[u8] = b"M1 opaque request\x00with exact bytes";
const HANDLE_PREIMAGE: &str = concat!(
    "{\"boot_id\":\"22222222-2222-4222-8222-222222222222\",\"execution\":{",
    "\"artifact_sha256\":\"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb\",",
    "\"attempt\":1,\"deployment\":\"owner-m0\",\"epoch\":\"epoch-1\",",
    "\"run_id\":\"run-1\",\"session\":\"session-1\",",
    "\"source_sha256\":\"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\"},",
    "\"instance_id\":\"11111111-1111-4111-8111-111111111111\",",
    "\"node_id\":\"linux-1\",\"schema\":\"linux-worker-owner-evidence/1\"}"
);

fn digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

fn fixed_handle() -> String {
    format!("ow-{}", digest(HANDLE_PREIMAGE.as_bytes()))
}

fn plan() -> OwnerLaunchPlan {
    OwnerLaunchPlan {
        execution: ExecutionIdentity::new(
            "owner-m0", "run-1", 1, "epoch-1", Some("session-1"),
            Some(&"a".repeat(64)), Some(&"b".repeat(64)),
        ),
        node_id: "linux-1".into(),
        instance_id: INSTANCE.into(),
        boot_id: BOOT.into(),
        request_bytes: REQUEST.to_vec(),
    }
}

fn maximal_plan() -> OwnerLaunchPlan {
    let id = "z".repeat(128);
    OwnerLaunchPlan {
        execution: ExecutionIdentity::new(
            &id, &id, u32::MAX, &id, Some(&id),
            Some(&"A".repeat(64)), Some(&"B".repeat(64)),
        ),
        node_id: id,
        instance_id: INSTANCE.into(),
        boot_id: BOOT.into(),
        request_bytes: vec![0x5a; 65_536],
    }
}

fn owner() -> OwnerProcessIdentity {
    OwnerProcessIdentity { pid: 200, start_ticks: 10_000 }
}

fn maximal_owner() -> OwnerProcessIdentity {
    OwnerProcessIdentity { pid: u32::MAX - 1, start_ticks: u64::MAX }
}

fn launch() -> LaunchFacts {
    LaunchFacts { child_pid: 201, child_start_ticks: 10_001, pgid: 201 }
}

fn maximal_launch() -> LaunchFacts {
    LaunchFacts { child_pid: u32::MAX, child_start_ticks: u64::MAX, pgid: u32::MAX }
}

// This expected binding is independently constructed from a literal preimage.
fn literal_binding(for_plan: &OwnerLaunchPlan) -> ExpectedOwnerBinding {
    ExpectedOwnerBinding {
        execution: for_plan.execution.clone(),
        owner: OwnerRef {
            node_id: for_plan.node_id.clone(),
            instance_id: for_plan.instance_id.clone(),
            boot_id: for_plan.boot_id.clone(),
            owner_handle: fixed_handle(),
        },
        request_sha256: digest(&for_plan.request_bytes),
        owner_process: owner(),
        process: ProcessIdentity {
            pid: 201,
            parent_pid: 200,
            start_ticks: 10_001,
            pgid: 201,
            cgroup_path: format!("workers/{INSTANCE}/{}", fixed_handle()),
        },
    }
}

fn maximal_handle(for_plan: &OwnerLaunchPlan) -> String {
    let preimage = json!({
        "schema": "linux-worker-owner-evidence/1",
        "execution": &for_plan.execution,
        "node_id": &for_plan.node_id,
        "instance_id": &for_plan.instance_id,
        "boot_id": &for_plan.boot_id,
    });
    format!("ow-{}", digest(&canonical(&preimage).unwrap()))
}

fn maximal_literal_binding(for_plan: &OwnerLaunchPlan) -> ExpectedOwnerBinding {
    let handle = maximal_handle(for_plan);
    let owner_process = maximal_owner();
    let launch = maximal_launch();
    ExpectedOwnerBinding {
        execution: for_plan.execution.clone(),
        owner: OwnerRef {
            node_id: for_plan.node_id.clone(),
            instance_id: for_plan.instance_id.clone(),
            boot_id: for_plan.boot_id.clone(),
            owner_handle: handle.clone(),
        },
        request_sha256: digest(&for_plan.request_bytes),
        owner_process: owner_process.clone(),
        process: ProcessIdentity {
            pid: launch.child_pid,
            parent_pid: owner_process.pid,
            start_ticks: launch.child_start_ticks,
            pgid: launch.pgid,
            cgroup_path: format!("workers/{INSTANCE}/{handle}"),
        },
    }
}

fn clean_stream(prefix: &[u8], total: u64) -> StreamReadOutcome {
    StreamReadOutcome {
        prefix: prefix.to_vec(),
        total_read_bytes: total,
        eof: true,
        error: None,
    }
}

fn clean_cleanup() -> CleanupFacts {
    CleanupFacts {
        proc_scan_complete: true,
        process_group_scan_complete: true,
        cgroup_scan_complete: true,
        process_group_members: 0,
        cgroup_members: 0,
        error: None,
    }
}

fn terminal() -> TerminalFacts {
    TerminalFacts {
        wait_completed: true,
        wait_method: "child_wait".into(),
        raw_wait_status: 0,
        stdout: clean_stream(b"abc", 3),
        stderr: clean_stream(b"", 0),
        cleanup: clean_cleanup(),
    }
}

#[derive(Clone)]
struct ScriptedOwnedPort {
    owner_result: std::result::Result<OwnerProcessIdentity, String>,
    launch_result: std::result::Result<LaunchFacts, String>,
    terminal_result: std::result::Result<TerminalFacts, String>,
    captured_plan: Option<OwnerLaunchPlan>,
    events: RefCell<Vec<&'static str>>,
}

impl ScriptedOwnedPort {
    fn valid() -> Self {
        Self {
            owner_result: Ok(owner()),
            launch_result: Ok(launch()),
            terminal_result: Ok(terminal()),
            captured_plan: None,
            events: RefCell::new(Vec::new()),
        }
    }
}

impl OwnedProcessPort for ScriptedOwnedPort {
    fn owner_identity(&self) -> Result<OwnerProcessIdentity> {
        self.events.borrow_mut().push("owner_identity");
        self.owner_result.clone().map_err(anyhow::Error::msg)
    }

    fn launch(&mut self, input: &OwnerLaunchPlan) -> Result<LaunchFacts> {
        self.events.get_mut().push("launch");
        self.captured_plan = Some(input.clone());
        self.launch_result.clone().map_err(anyhow::Error::msg)
    }

    fn wait_terminal(&mut self) -> Result<TerminalFacts> {
        self.events.get_mut().push("wait_terminal");
        self.terminal_result.clone().map_err(anyhow::Error::msg)
    }
}

#[test]
fn normal_zero_production_validates_against_literal_expected_binding() {
    let launch_plan = plan();
    let expected = literal_binding(&launch_plan);
    let production = produce_owner_evidence(&mut ScriptedOwnedPort::valid(), &launch_plan).unwrap();
    let record = validate_bundle(production.bundle_bytes(), Some(&expected)).unwrap();

    assert_eq!(production.binding, expected);
    assert_eq!(record.direct_wait.numeric_exit, 0);
    assert_eq!(record.stdout.prefix_len, 3);
    assert_eq!(record.stderr.total_read_bytes, 0);
    let wire: Value = serde_json::from_slice(production.bundle_bytes()).unwrap();
    assert_eq!(wire.as_object().unwrap().len(), 4);
}

#[test]
fn complete_binding_is_exact_and_dedicated_group_only() {
    let launch_plan = plan();
    assert_eq!(complete_binding(&launch_plan, &owner(), &launch()).unwrap(), literal_binding(&launch_plan));

    let mut invalid_plans = Vec::new();
    let mut changed = launch_plan.clone(); changed.execution.session = None; invalid_plans.push(changed);
    let mut changed = launch_plan.clone(); changed.node_id = "bad/node".into(); invalid_plans.push(changed);
    let mut changed = launch_plan.clone(); changed.instance_id = "not-a-uuid".into(); invalid_plans.push(changed);
    let mut changed = launch_plan.clone(); changed.boot_id = "not-a-uuid".into(); invalid_plans.push(changed);
    let mut changed = launch_plan.clone(); changed.request_bytes = vec![0; 65_537]; invalid_plans.push(changed);
    for invalid in invalid_plans {
        assert!(complete_binding(&invalid, &owner(), &launch()).is_err());
    }
    for invalid_owner in [
        OwnerProcessIdentity { pid: 0, start_ticks: 10_000 },
        OwnerProcessIdentity { pid: 200, start_ticks: 0 },
    ] {
        assert!(complete_binding(&launch_plan, &invalid_owner, &launch()).is_err());
    }
    for invalid_launch in [
        LaunchFacts { child_pid: 0, ..launch() },
        LaunchFacts { child_start_ticks: 0, ..launch() },
        LaunchFacts { pgid: 0, ..launch() },
        LaunchFacts { pgid: 202, ..launch() },
        LaunchFacts { child_pid: 200, pgid: 200, ..launch() },
    ] {
        assert!(complete_binding(&launch_plan, &owner(), &invalid_launch).is_err());
    }
}

#[test]
fn producer_calls_port_in_order_and_stops_at_first_failure() {
    let launch_plan = plan();
    let mut port = ScriptedOwnedPort::valid();
    let production = produce_owner_evidence(&mut port, &launch_plan).unwrap();
    assert_eq!(&*port.events.borrow(), &["owner_identity", "launch", "wait_terminal"]);
    assert_eq!(production.binding, literal_binding(&launch_plan));

    let mut port = ScriptedOwnedPort::valid();
    port.owner_result = Err("owner-failed".into());
    let error = produce_owner_evidence(&mut port, &launch_plan).unwrap_err();
    assert!(error.to_string().contains("owner-failed"));
    assert_eq!(&*port.events.borrow(), &["owner_identity"]);

    let mut port = ScriptedOwnedPort::valid();
    port.launch_result = Err("launch-failed".into());
    let error = produce_owner_evidence(&mut port, &launch_plan).unwrap_err();
    assert!(error.to_string().contains("launch-failed"));
    assert_eq!(&*port.events.borrow(), &["owner_identity", "launch"]);

    let mut port = ScriptedOwnedPort::valid();
    port.launch_result = Ok(LaunchFacts { pgid: 202, ..launch() });
    let error = produce_owner_evidence(&mut port, &launch_plan).unwrap_err();
    assert_eq!(error.to_string(), "child must own its dedicated process group");
    assert_eq!(&*port.events.borrow(), &["owner_identity", "launch"]);

    let mut invalid_plan = launch_plan.clone();
    invalid_plan.execution.session = None;
    let mut port = ScriptedOwnedPort::valid();
    let error = produce_owner_evidence(&mut port, &invalid_plan).unwrap_err();
    assert_eq!(error.to_string(), "owner execution requires nonempty session");
    assert_eq!(&*port.events.borrow(), &["owner_identity", "launch"]);

    let mut port = ScriptedOwnedPort::valid();
    port.terminal_result = Err("wait-failed".into());
    let error = produce_owner_evidence(&mut port, &launch_plan).unwrap_err();
    assert!(error.to_string().contains("wait-failed"));
    assert_eq!(&*port.events.borrow(), &["owner_identity", "launch", "wait_terminal"]);
}

#[test]
fn pending_wait_never_produces_a_bundle() {
    let mut port = ScriptedOwnedPort::valid();
    port.terminal_result.as_mut().unwrap().wait_completed = false;
    let error = produce_owner_evidence(&mut port, &plan()).unwrap_err();
    assert!(error.to_string().contains("pending"));
}

#[test]
fn invalid_raw_wait_status_is_rejected_at_production() {
    for raw in [-1, 0x7f, 0xffff, 65] {
        let mut port = ScriptedOwnedPort::valid();
        port.terminal_result.as_mut().unwrap().raw_wait_status = raw;
        assert!(produce_owner_evidence(&mut port, &plan()).is_err(), "raw status {raw} was accepted");
    }
}

#[test]
fn exit7_signal9_and_core_137_are_faithful_but_not_success() {
    for (raw, code, signal, numeric) in [
        (1792, Some(7_u8), None, 7),
        (9, None, Some(9_u32), -9),
        (137, None, Some(9_u32), -9),
    ] {
        let launch_plan = plan();
        let mut port = ScriptedOwnedPort::valid();
        port.terminal_result.as_mut().unwrap().raw_wait_status = raw;
        let production = produce_owner_evidence(&mut port, &launch_plan).unwrap();
        let record = validate_bundle(production.bundle_bytes(), Some(&literal_binding(&launch_plan))).unwrap();
        assert_eq!(record.direct_wait.exit_code, code);
        assert_eq!(record.direct_wait.termination_signal, signal);
        assert_eq!(record.direct_wait.numeric_exit, numeric);
        assert_ne!(record.direct_wait.numeric_exit, 0);
    }
}

#[test]
fn reader_error_or_missing_eof_is_not_admissible() {
    for stream in ["stdout", "stderr"] {
        let mut port = ScriptedOwnedPort::valid();
        let target = if stream == "stdout" {
            &mut port.terminal_result.as_mut().unwrap().stdout
        } else {
            &mut port.terminal_result.as_mut().unwrap().stderr
        };
        target.error = Some("read-failed".into());
        assert!(produce_owner_evidence(&mut port, &plan()).is_err());

        let mut port = ScriptedOwnedPort::valid();
        let target = if stream == "stdout" {
            &mut port.terminal_result.as_mut().unwrap().stdout
        } else {
            &mut port.terminal_result.as_mut().unwrap().stderr
        };
        target.eof = false;
        assert!(produce_owner_evidence(&mut port, &plan()).is_err());
    }
}

#[test]
fn incomplete_or_occupied_cleanup_is_not_admissible() {
    for change in 0..6 {
        let mut port = ScriptedOwnedPort::valid();
        let cleanup = &mut port.terminal_result.as_mut().unwrap().cleanup;
        match change {
            0 => cleanup.proc_scan_complete = false,
            1 => cleanup.process_group_scan_complete = false,
            2 => cleanup.cgroup_scan_complete = false,
            3 => cleanup.process_group_members = 1,
            4 => cleanup.cgroup_members = 1,
            _ => cleanup.error = Some("scan-failed".into()),
        }
        assert!(produce_owner_evidence(&mut port, &plan()).is_err(), "cleanup mutation {change} was accepted");
    }
}

#[test]
fn prefix_is_capped_at_4096_and_truncation_matches_total() {
    let launch_plan = plan();
    let mut port = ScriptedOwnedPort::valid();
    port.terminal_result.as_mut().unwrap().stdout = clean_stream(&vec![0x5a; 5_000], 5_000);
    let production = produce_owner_evidence(&mut port, &launch_plan).unwrap();
    assert_eq!(production.bundle.record.stdout.prefix_len, 4096);
    assert!(production.bundle.record.stdout.truncated);
    assert!(validate_bundle(production.bundle_bytes(), Some(&literal_binding(&launch_plan))).is_ok());

    let mut port = ScriptedOwnedPort::valid();
    port.terminal_result.as_mut().unwrap().stdout = clean_stream(&vec![0x5a; 4096], 4097);
    let production = produce_owner_evidence(&mut port, &plan()).unwrap();
    assert!(production.bundle.record.stdout.truncated);

    let mut port = ScriptedOwnedPort::valid();
    port.terminal_result.as_mut().unwrap().stdout = clean_stream(&vec![0x5a; 10], 3);
    assert!(produce_owner_evidence(&mut port, &plan()).is_err());
}

#[test]
fn request_bytes_are_bound_by_exact_digest() {
    let old_plan = plan();
    let mut new_plan = plan();
    new_plan.request_bytes[0] ^= 1;
    let old = produce_owner_evidence(&mut ScriptedOwnedPort::valid(), &old_plan).unwrap();
    let new = produce_owner_evidence(&mut ScriptedOwnedPort::valid(), &new_plan).unwrap();

    assert_ne!(old.binding.request_sha256, new.binding.request_sha256);
    assert!(validate_bundle(old.bundle_bytes(), Some(&new.binding)).is_err());
    assert!(validate_bundle(new.bundle_bytes(), Some(&old.binding)).is_err());
}

#[test]
fn evidence_files_match_record_hash_and_refs() {
    let production = produce_owner_evidence(&mut ScriptedOwnedPort::valid(), &plan()).unwrap();
    let files = owner_evidence_files(&production.bundle).unwrap();
    assert_eq!(files.len(), 3);
    assert_eq!(files[0].relative_path, production.bundle.record.evidence_refs.record);
    assert_eq!(files[1].relative_path, production.bundle.record.evidence_refs.stdout);
    assert_eq!(files[2].relative_path, production.bundle.record.evidence_refs.stderr);
    assert_eq!(digest(&files[0].bytes), production.bundle.record_sha256);
    assert_eq!(files[1].bytes, b"abc");
    assert!(files[2].bytes.is_empty());
}

#[test]
fn wait_terminal_failure_propagates_without_fabrication() {
    let launch_plan = plan();
    let mut port = ScriptedOwnedPort::valid();
    port.terminal_result = Err("wait-failed".into());
    assert!(produce_owner_evidence(&mut port, &launch_plan).unwrap_err().to_string().contains("wait-failed"));
    assert_eq!(port.captured_plan, Some(launch_plan));

    let launch_plan = plan();
    let mut port = ScriptedOwnedPort::valid();
    port.launch_result = Ok(LaunchFacts { child_pid: 202, child_start_ticks: 10_002, pgid: 202 });
    let production = produce_owner_evidence(&mut port, &launch_plan).unwrap();
    assert_eq!(port.captured_plan, Some(launch_plan));
    assert_eq!(production.binding.process.pid, 202);
    assert_eq!(production.bundle.record.process.start_ticks, 10_002);
}

#[test]
fn maximal_bounded_inputs_stay_under_e11_and_revalidate_with_m0() {
    // Conservative bound derived from actual seam limits: five 128-byte IDs,
    // five 64-byte digests, 67-byte hash handles, fixed UUID/path/reference
    // components, decimal u32/u64 widths (10/20), and fixed JSON overhead.
    const MAX_BOUNDED_CANONICAL_RECORD_BYTES: usize = 4_096;
    let launch_plan = maximal_plan();
    let expected = maximal_literal_binding(&launch_plan);
    let mut port = ScriptedOwnedPort::valid();
    port.owner_result = Ok(maximal_owner());
    port.launch_result = Ok(maximal_launch());
    let terminal = port.terminal_result.as_mut().unwrap();
    terminal.wait_method = "child_try_wait".into();
    terminal.raw_wait_status = 255 << 8;
    terminal.stdout = clean_stream(&vec![0x5a; 4_096], u64::MAX);
    terminal.stderr = clean_stream(&vec![0xa5; 4_096], u64::MAX);

    let production = produce_owner_evidence(&mut port, &launch_plan).unwrap();
    let measured = production.record_json_bytes().unwrap().len();
    println!("maximal bounded canonical record bytes: {measured}");
    assert!(measured <= MAX_BOUNDED_CANONICAL_RECORD_BYTES);
    assert!(measured < 16_384);
    validate_bundle(production.bundle_bytes(), Some(&expected)).unwrap();
}

#[test]
fn schema_and_stream_metadata_follow_m0_rules() {
    let mut port = ScriptedOwnedPort::valid();
    port.terminal_result.as_mut().unwrap().wait_method = "not-a-child-wait".into();
    assert!(produce_owner_evidence(&mut port, &plan()).is_err());

    // This is an M0-oracle-only size test. The schema remains valid, but an
    // otherwise oversized typed field makes the canonical record too large;
    // it does not claim to exercise producer E11 through the accepted seam.
    let production = produce_owner_evidence(&mut ScriptedOwnedPort::valid(), &plan()).unwrap();
    let mut oversized: Value = serde_json::from_slice(production.bundle_bytes()).unwrap();
    oversized["record"]["owner"]["node_id"] = json!("x".repeat(16_385));
    let measured = canonical(&oversized["record"]).unwrap().len();
    assert!(measured > 16_384);
    oversized["record_sha256"] = json!(digest(&canonical(&oversized["record"]).unwrap()));
    let error = validate_bundle(
        &serde_json::to_vec(&oversized).unwrap(),
        Some(&production.binding),
    ).unwrap_err();
    assert_eq!(error.to_string(), "record exceeds 16384 canonical bytes");
}
