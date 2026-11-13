//! Pure owner-contract fixtures only: no process, filesystem, network, or clock probes.
//! Written before the implementation; NOT_RUN in the resource-gated implementation wave.

use ao_reason_workbench::core::{
    protocol::{ExecutionIdentity, canonical},
    worker_owner::{
        ExpectedOwnerBinding, OwnerEvidenceBundle, OwnerProcessIdentity, OwnerRef,
        ProcessIdentity, derive_owner_handle, validate_bundle,
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const INSTANCE: &str = "11111111-1111-4111-8111-111111111111";
const BOOT: &str = "22222222-2222-4222-8222-222222222222";
const EMPTY_SHA: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ABC_SHA: &str = "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad";
// Literal sorted JSON, independent of the production helper and serde's struct order.
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

// This fixed expected input is never deserialized or copied from a submitted bundle.
fn expected() -> ExpectedOwnerBinding {
    ExpectedOwnerBinding {
        execution: ExecutionIdentity::new(
            "owner-m0", "run-1", 1, "epoch-1", Some("session-1"),
            Some(&"a".repeat(64)), Some(&"b".repeat(64)),
        ),
        owner: OwnerRef {
            node_id: "linux-1".into(), instance_id: INSTANCE.into(), boot_id: BOOT.into(),
            owner_handle: fixed_handle(),
        },
        request_sha256: "c".repeat(64),
        owner_process: OwnerProcessIdentity { pid: 200, start_ticks: 10000 },
        process: ProcessIdentity {
            pid: 201, parent_pid: 200, start_ticks: 10001, pgid: 201,
            cgroup_path: format!("workers/{INSTANCE}/{}", fixed_handle()),
        },
    }
}

// Submitted fixture is independently specified, not built by cloning the expected binding.
fn fixture() -> Value {
    let handle = fixed_handle();
    let cgroup = format!("workers/{INSTANCE}/{handle}");
    let refs = format!("owners/{INSTANCE}/{handle}");
    let mut bundle = json!({
        "record": {
            "schema": "linux-worker-owner-evidence/1",
            "execution": {"deployment":"owner-m0","run_id":"run-1","attempt":1,
                "epoch":"epoch-1","session":"session-1",
                "source_sha256":"a".repeat(64),"artifact_sha256":"b".repeat(64)},
            "owner": {"node_id":"linux-1","instance_id":INSTANCE,"boot_id":BOOT,"owner_handle":handle},
            "request_sha256":"c".repeat(64),
            "owner_process":{"pid":200,"start_ticks":10000},
            "process":{"pid":201,"parent_pid":200,"start_ticks":10001,"pgid":201,"cgroup_path":cgroup},
            "direct_wait":{"wait_completed":true,"method":"child_wait","raw_wait_status":0,
                "exit_code":0,"termination_signal":null,"numeric_exit":0},
            "stdout":{"prefix_len":3,"total_read_bytes":3,"truncated":false,"eof":true,
                "error":null,"prefix_sha256":ABC_SHA},
            "stderr":{"prefix_len":0,"total_read_bytes":0,"truncated":false,"eof":true,
                "error":null,"prefix_sha256":EMPTY_SHA},
            "cleanup":{"proc_scan_complete":true,"process_group_scan_complete":true,
                "cgroup_scan_complete":true,"pgid":201,"cgroup_path":cgroup,
                "process_group_members":0,"cgroup_members":0,"error":null},
            "evidence_refs":{"record":format!("{refs}/record.json"),
                "stdout":format!("{refs}/stdout.bin"),"stderr":format!("{refs}/stderr.bin")}
        },
        "record_sha256":"", "stdout_prefix_hex":"616263", "stderr_prefix_hex":""
    });
    rehash(&mut bundle);
    bundle
}

fn rehash(bundle: &mut Value) {
    bundle["record_sha256"] = json!(digest(&canonical(&bundle["record"]).unwrap()));
}

fn bytes(bundle: &Value) -> Vec<u8> {
    serde_json::to_vec(bundle).unwrap()
}

fn accepted(bundle: &Value, binding: &ExpectedOwnerBinding) -> bool {
    validate_bundle(&bytes(bundle), Some(binding)).is_ok()
}

fn rejected_mutation(path: &str, value: Value) {
    let mut bundle = fixture();
    *bundle.pointer_mut(path).unwrap() = value;
    rehash(&mut bundle);
    assert!(!accepted(&bundle, &expected()), "accepted mutation at {path}");
}

// Coherent substitutions use an independent test-side preimage, not production handle derivation.
fn rebind_record(bundle: &mut Value) {
    let record = &mut bundle["record"];
    let preimage = json!({
        "schema":"linux-worker-owner-evidence/1", "execution":record["execution"],
        "node_id":record["owner"]["node_id"], "instance_id":record["owner"]["instance_id"],
        "boot_id":record["owner"]["boot_id"]
    });
    let handle = format!("ow-{}", digest(&canonical(&preimage).unwrap()));
    record["owner"]["owner_handle"] = json!(handle);
    let instance = record["owner"]["instance_id"].as_str().unwrap().to_owned();
    let cgroup = format!("workers/{instance}/{handle}");
    record["process"]["cgroup_path"] = json!(cgroup);
    record["cleanup"]["cgroup_path"] = json!(cgroup);
    let prefix = format!("owners/{instance}/{handle}");
    record["evidence_refs"] = json!({"record":format!("{prefix}/record.json"),
        "stdout":format!("{prefix}/stdout.bin"),"stderr":format!("{prefix}/stderr.bin")});
    rehash(bundle);
}

// For positive compatibility variants only: derive expected references from caller fixture
// inputs, never by copying any submitted bundle field or using the production helper.
fn refresh_expected_handle(binding: &mut ExpectedOwnerBinding) {
    let preimage = json!({"schema":"linux-worker-owner-evidence/1", "execution":binding.execution,
        "node_id":binding.owner.node_id, "instance_id":binding.owner.instance_id,
        "boot_id":binding.owner.boot_id});
    binding.owner.owner_handle = format!("ow-{}", digest(&canonical(&preimage).unwrap()));
    binding.process.cgroup_path = format!("workers/{}/{}", binding.owner.instance_id, binding.owner.owner_handle);
}

fn set_stream(bundle: &mut Value, stream: &str, prefix: &[u8], total: u64) {
    bundle[format!("{stream}_prefix_hex").as_str()] = json!(hex::encode(prefix));
    bundle["record"][stream] = json!({"prefix_len":prefix.len(),"total_read_bytes":total,
        "truncated":total > prefix.len() as u64,"eof":true,"error":null,"prefix_sha256":digest(prefix)});
    rehash(bundle);
}

// Detects accidental default success, empty output, or broken fixed-fixture admission.
#[test]
fn fixed_bundle_returns_ordinary_normal_zero_record() {
    let record = validate_bundle(&bytes(&fixture()), Some(&expected())).unwrap();
    assert_eq!(record.execution, expected().execution);
    assert_eq!(record.direct_wait.numeric_exit, 0);
    assert_eq!(record.stdout.prefix_len, 3);
    assert_eq!(record.stderr.total_read_bytes, 0);
}

// Detects handle preimage omissions, field renames, or noncanonical hashing.
#[test]
fn handle_matches_literal_canonical_preimage() {
    let binding = expected();
    assert_eq!(derive_owner_handle(&binding.execution, "linux-1", INSTANCE, BOOT).unwrap(), fixed_handle());
    let mut changed_identities = Vec::new();
    let mut e = binding.execution.clone(); e.deployment = "owner-other".into(); changed_identities.push(e);
    let mut e = binding.execution.clone(); e.run_id = "run-2".into(); changed_identities.push(e);
    let mut e = binding.execution.clone(); e.attempt = 2; changed_identities.push(e);
    let mut e = binding.execution.clone(); e.epoch = "epoch-2".into(); changed_identities.push(e);
    let mut e = binding.execution.clone(); e.session = Some("session-2".into()); changed_identities.push(e);
    let mut e = binding.execution.clone(); e.source_sha256 = Some("d".repeat(64)); changed_identities.push(e);
    let mut e = binding.execution.clone(); e.artifact_sha256 = Some("e".repeat(64)); changed_identities.push(e);
    for changed in changed_identities {
        assert_ne!(derive_owner_handle(&changed, "linux-1", INSTANCE, BOOT).unwrap(), fixed_handle());
    }
    for (node, instance, boot) in [
        ("linux-2", INSTANCE, BOOT),
        ("linux-1", "33333333-3333-4333-8333-333333333333", BOOT),
        ("linux-1", INSTANCE, "44444444-4444-1444-8444-444444444444"),
    ] {
        assert_ne!(derive_owner_handle(&binding.execution, node, instance, boot).unwrap(), fixed_handle());
    }
}

// Detects partial identity equality or trusting a coherently substituted submitted identity.
#[test]
fn full_identity_mismatch_rejects_coherent_substitutions() {
    for (field, value) in [
        ("deployment", json!("owner-other")), ("run_id", json!("run-2")),
        ("attempt", json!(2)), ("epoch", json!("epoch-2")), ("session", json!("session-2")),
        ("source_sha256", json!("d".repeat(64))), ("artifact_sha256", json!("e".repeat(64))),
    ] {
        let mut bundle = fixture();
        bundle["record"]["execution"][field] = value;
        rebind_record(&mut bundle);
        assert!(!accepted(&bundle, &expected()), "accepted mismatched {field}");
    }
}

// Detects absent expected inputs, auto-binding, or embedded bundle assertions of trust.
#[test]
fn expected_binding_required_and_never_taken_from_bundle() {
    assert!(validate_bundle(&bytes(&fixture()), None).is_err());
    assert!(validate_bundle(b"not-json", None).is_err());
    let mut bundle = fixture();
    bundle["expected"] = bundle["record"].clone();
    assert!(!accepted(&bundle, &expected()));
    assert!(validate_bundle(&bytes(&bundle), None).is_err());
}

// Detects trusting structurally malformed expected caller inputs independently of evidence.
#[test]
fn expected_binding_is_validated_independently() {
    let fixed = expected();
    let mut invalid = Vec::new();
    let mut b = fixed.clone(); b.execution.session = None; invalid.push(b);
    let mut b = fixed.clone(); b.owner.owner_handle = "ow-invalid".into(); invalid.push(b);
    let mut b = fixed.clone(); b.request_sha256 = "C".repeat(64); invalid.push(b);
    let mut b = fixed.clone(); b.owner_process.pid = 0; invalid.push(b);
    let mut b = fixed.clone(); b.owner_process.start_ticks = 0; invalid.push(b);
    let mut b = fixed.clone(); b.process.start_ticks = 0; invalid.push(b);
    let mut b = fixed.clone(); b.process.pid = 0; invalid.push(b);
    let mut b = fixed.clone(); b.process.parent_pid = 0; invalid.push(b);
    let mut b = fixed.clone(); b.process.pgid = 0; invalid.push(b);
    let mut b = fixed.clone(); b.process.parent_pid = 202; invalid.push(b);
    let mut b = fixed.clone(); b.process.pid = 200; b.process.pgid = 200; invalid.push(b);
    let mut b = fixed.clone(); b.process.pgid = 202; invalid.push(b);
    let mut b = fixed.clone(); b.process.cgroup_path = "/absolute".into(); invalid.push(b);
    for binding in invalid {
        let mut bundle = fixture();
        bundle["record"]["execution"] = serde_json::to_value(&binding.execution).unwrap();
        bundle["record"]["owner"] = serde_json::to_value(&binding.owner).unwrap();
        bundle["record"]["request_sha256"] = json!(binding.request_sha256);
        bundle["record"]["owner_process"] = serde_json::to_value(&binding.owner_process).unwrap();
        bundle["record"]["process"] = serde_json::to_value(&binding.process).unwrap();
        rehash(&mut bundle);
        let error = validate_bundle(&bytes(&bundle), Some(&binding)).unwrap_err();
        assert!(error.to_string().contains("invalid expected owner binding"));
    }
}

// Detects owner/request substitutions even if submitted hashes, handles, and paths agree.
#[test]
fn owner_and_request_mismatch_reject_coherent_substitutions() {
    for (field, value) in [
        ("node_id", "linux-2"), ("instance_id", "33333333-3333-4333-8333-333333333333"),
        ("boot_id", "44444444-4444-1444-8444-444444444444"),
    ] {
        let mut bundle = fixture();
        bundle["record"]["owner"][field] = json!(value);
        rebind_record(&mut bundle);
        assert!(!accepted(&bundle, &expected()));
    }
    rejected_mutation("/record/owner/owner_handle", json!(format!("ow-{}", "f".repeat(64))));
    rejected_mutation("/record/request_sha256", json!("d".repeat(64)));
}

// Detects incomplete owner/process comparison, including parent, ticks, PG, and cgroup.
#[test]
fn owner_process_and_all_child_fields_must_match_expected() {
    for (path, value) in [
        ("/record/owner_process/pid", json!(202)),
        ("/record/owner_process/start_ticks", json!(10002)),
        ("/record/process/pid", json!(202)), ("/record/process/parent_pid", json!(202)),
        ("/record/process/start_ticks", json!(10002)), ("/record/process/pgid", json!(202)),
        ("/record/process/cgroup_path", json!("workers/other/handle")),
    ] {
        rejected_mutation(path, value);
    }
    let mut bundle = fixture();
    bundle["record"]["owner_process"]["pid"] = json!(202);
    bundle["record"]["process"]["parent_pid"] = json!(202);
    rehash(&mut bundle);
    assert!(!accepted(&bundle, &expected()));
    let mut bundle = fixture();
    for object in ["process", "cleanup"] { bundle["record"][object]["pgid"] = json!(202); }
    bundle["record"]["process"]["pid"] = json!(202);
    rehash(&mut bundle);
    assert!(!accepted(&bundle, &expected()));
}

// Detects no-check zero identities and path normalization/traversal acceptance.
#[test]
fn numeric_identity_zeros_and_nonexact_paths_are_rejected() {
    for path in ["/record/owner_process/pid", "/record/owner_process/start_ticks",
        "/record/process/pid", "/record/process/parent_pid", "/record/process/start_ticks", "/record/process/pgid"] {
        rejected_mutation(path, json!(0));
    }
    for path in ["/absolute", "../escape", "workers/../escape", "workers//x", "workers/x/",
        "workers\\x", "C:/workers/x", "workers/%2e%2e/x"] {
        rejected_mutation("/record/process/cgroup_path", json!(path));
        rejected_mutation("/record/cleanup/cgroup_path", json!(path));
        for field in ["record", "stdout", "stderr"] {
            rejected_mutation(&format!("/record/evidence_refs/{field}"), json!(path));
        }
    }
}

// Detects narrowing existing ASCII-hex identity rules or normalizing case before equality.
#[test]
fn identity_digest_case_is_preserved_and_compared_exactly() {
    let mut bundle = fixture();
    let mut binding = expected();
    for field in ["source_sha256", "artifact_sha256"] {
        let upper = bundle["record"]["execution"][field].as_str().unwrap().to_uppercase();
        bundle["record"]["execution"][field] = json!(upper);
    }
    rebind_record(&mut bundle);
    assert!(!accepted(&bundle, &binding));
    binding.execution.source_sha256 = Some("A".repeat(64));
    binding.execution.artifact_sha256 = Some("B".repeat(64));
    refresh_expected_handle(&mut binding);
    assert!(accepted(&bundle, &binding));
}

// Detects requiring legacy optional digests or accepting missing/empty mandatory session.
#[test]
fn optional_digests_omit_but_session_is_required() {
    let mut bundle = fixture();
    let mut binding = expected();
    for field in ["source_sha256", "artifact_sha256"] {
        bundle["record"]["execution"].as_object_mut().unwrap().remove(field);
    }
    rebind_record(&mut bundle);
    assert!(!accepted(&bundle, &binding));
    binding.execution.source_sha256 = None;
    binding.execution.artifact_sha256 = None;
    refresh_expected_handle(&mut binding);
    let record = validate_bundle(&bytes(&bundle), Some(&binding)).unwrap();
    let serialized = serde_json::to_value(record).unwrap();
    assert!(serialized["execution"].get("source_sha256").is_none());
    assert!(serialized["execution"].get("artifact_sha256").is_none());
    for value in [json!(null), json!("")] { rejected_mutation("/record/execution/session", value); }
    bundle["record"]["execution"].as_object_mut().unwrap().remove("session");
    rehash(&mut bundle);
    assert!(!accepted(&bundle, &binding));
}

// Detects hash drift caused by accepted explicit-null optional identity digests.
#[test]
fn null_optional_identity_digests_canonicalize_as_omitted() {
    let mut binding = expected();
    binding.execution.source_sha256 = None;
    binding.execution.artifact_sha256 = None;
    refresh_expected_handle(&mut binding);
    let mut bundle = fixture();
    bundle["record"]["execution"].as_object_mut().unwrap().remove("source_sha256");
    bundle["record"]["execution"].as_object_mut().unwrap().remove("artifact_sha256");
    rebind_record(&mut bundle);
    // Keep the omitted-form hash while adding permitted legacy nulls on the wire.
    bundle["record"]["execution"]["source_sha256"] = Value::Null;
    bundle["record"]["execution"]["artifact_sha256"] = Value::Null;
    assert!(accepted(&bundle, &binding));
}

// Detects UUID parser leniency leaking into canonical wire identity rules.
#[test]
fn uuid_node_and_new_digest_encodings_are_strict() {
    let binding = expected();
    for instance in ["aaaaaaaa-aaaa-4aaa-8aaa-aaaaaaaaaaaA", "11111111111141118111111111111111",
        "11111111-1111-1111-8111-111111111111", "11111111-1111-4111-c111-111111111111"] {
        assert!(derive_owner_handle(&binding.execution, "linux-1", instance, BOOT).is_err());
    }
    for boot in ["AAAAAAAA-AAAA-1AAA-8AAA-AAAAAAAAAAAA", "22222222222242228222222222222222", "not-uuid"] {
        assert!(derive_owner_handle(&binding.execution, "linux-1", INSTANCE, boot).is_err());
    }
    assert!(derive_owner_handle(&binding.execution, "bad/node", INSTANCE, BOOT).is_err());
    assert!(derive_owner_handle(&binding.execution, "linux-1", INSTANCE,
        "44444444-4444-1444-8444-444444444444").is_ok());
    for path in ["/record/request_sha256", "/record/stdout/prefix_sha256", "/record/stderr/prefix_sha256"] {
        for invalid in ["D".repeat(64), "g".repeat(64), "a".repeat(63)] {
            rejected_mutation(path, json!(invalid));
        }
    }
}

// Detects null->zero, signal->128+signal, or host-specific ExitStatus decoding.
#[test]
fn normal0_exit7_signal9_and_normal137_have_exact_numeric_semantics() {
    for method in ["child_wait", "child_try_wait"] {
        for (raw, code, signal, numeric) in [
            (0, Some(0_u8), None, 0), (1792, Some(7), None, 7),
            (9, None, Some(9_u32), -9), (137, None, Some(9), -9),
            (35072, Some(137), None, 137), (65280, Some(255), None, 255),
            (1, None, Some(1), -1), (64, None, Some(64), -64), (192, None, Some(64), -64),
        ] {
            let mut bundle = fixture();
            bundle["record"]["direct_wait"] = json!({"wait_completed":true,"method":method,
                "raw_wait_status":raw,"exit_code":code,"termination_signal":signal,"numeric_exit":numeric});
            rehash(&mut bundle);
            let record = validate_bundle(&bytes(&bundle), Some(&expected())).unwrap();
            assert_eq!(record.direct_wait.numeric_exit, numeric);
            assert_eq!(record.direct_wait.exit_code, code);
            assert_eq!(record.direct_wait.termination_signal, signal);
        }
    }
}

// Detects acceptance of pending/stopped/continued waits or contradictory terminal metadata.
#[test]
fn pending_and_inconsistent_wait_statuses_are_rejected() {
    for (path, value) in [
        ("wait_completed", json!(false)), ("method", json!("report")),
        ("raw_wait_status", json!(-1)), ("raw_wait_status", json!(127)),
        ("raw_wait_status", json!(65535)), ("raw_wait_status", json!(65536)),
        ("exit_code", json!(null)), ("termination_signal", json!(9)), ("numeric_exit", json!(137)),
    ] { rejected_mutation(&format!("/record/direct_wait/{path}"), value); }
    for (raw, signal, numeric) in [(0, 0, 0), (65, 65, -65), (9, 9, 0), (9, 9, 137), (265, 9, -9)] {
        let mut bundle = fixture();
        bundle["record"]["direct_wait"]["exit_code"] = Value::Null;
        bundle["record"]["direct_wait"]["raw_wait_status"] = json!(raw);
        bundle["record"]["direct_wait"]["termination_signal"] = json!(signal);
        bundle["record"]["direct_wait"]["numeric_exit"] = json!(numeric);
        rehash(&mut bundle);
        assert!(!accepted(&bundle, &expected()));
    }
}

// Detects skipped content hashing, record hashing, or one-stream-only integrity checks.
#[test]
fn raw_prefix_and_hash_mutations_are_rejected_for_both_streams() {
    for stream in ["stdout", "stderr"] {
        let mut bundle = fixture();
        set_stream(&mut bundle, stream, b"abc", 3);
        bundle[format!("{stream}_prefix_hex").as_str()] = json!("616264");
        assert!(!accepted(&bundle, &expected()));
        bundle[format!("{stream}_prefix_hex").as_str()] = json!("616263");
        bundle["record"][stream]["prefix_sha256"] = json!("0".repeat(64));
        rehash(&mut bundle);
        assert!(!accepted(&bundle, &expected()));
    }
    let mut bundle = fixture(); bundle["record_sha256"] = json!("0".repeat(64));
    assert!(!accepted(&bundle, &expected()));
    for invalid in ["A".repeat(64), "0".repeat(63), "z".repeat(64)] {
        let mut bundle = fixture(); bundle["record_sha256"] = json!(invalid);
        assert!(!accepted(&bundle, &expected()));
    }
}

// Detects character-count confusion, incomplete count checks, or permissive hex decoding.
#[test]
fn prefix_count_truncation_and_hex_mutations_are_rejected() {
    for stream in ["stdout", "stderr"] {
        for (field, value) in [("prefix_len", json!(2)), ("total_read_bytes", json!(2)),
            ("total_read_bytes", json!(4)), ("truncated", json!(true))] {
            let mut bundle = fixture(); set_stream(&mut bundle, stream, b"abc", 3);
            bundle["record"][stream][field] = value;
            rehash(&mut bundle);
            assert!(!accepted(&bundle, &expected()));
        }
        for invalid in ["61626", "61626A", "zz", " 616263", "0x616263"] {
            let mut bundle = fixture(); set_stream(&mut bundle, stream, b"abc", 3);
            bundle[format!("{stream}_prefix_hex").as_str()] = json!(invalid);
            assert!(!accepted(&bundle, &expected()));
        }
    }
}

// Detects loss of binary bytes, per-stream cap errors, or hashing a decoded text conversion.
#[test]
fn binary_stream_boundaries_and_claim_limit_are_explicit() {
    for total in [0_u64, 4095, 4096, 4097, 4098, u64::MAX] {
        let prefix: Vec<u8> = (0..total.min(4096)).map(|i| (i % 256) as u8).collect();
        let mut bundle = fixture();
        for stream in ["stdout", "stderr"] { set_stream(&mut bundle, stream, &prefix, total); }
        let record = validate_bundle(&bytes(&bundle), Some(&expected())).unwrap();
        assert_eq!(record.stdout.prefix_len as usize, prefix.len());
        assert_eq!(record.stderr.total_read_bytes, total);
        assert!(canonical(&bundle["record"]).unwrap().len() <= 16384);
    }
    for stream in ["stdout", "stderr"] {
        let mut bundle = fixture(); set_stream(&mut bundle, stream, &vec![0xff; 4097], 4097);
        assert!(!accepted(&bundle, &expected()));
    }
    // Coherent 4097 -> 4098 changes are intentionally accepted: no external count oracle exists.
}

// Detects accepting incomplete readers or live/failed cleanup from rehashed metadata.
#[test]
fn reader_errors_pending_eof_and_incomplete_cleanup_are_rejected() {
    for stream in ["stdout", "stderr"] {
        rejected_mutation(&format!("/record/{stream}/eof"), json!(false));
        rejected_mutation(&format!("/record/{stream}/error"), json!("read-failed"));
    }
    for flag in ["proc_scan_complete", "process_group_scan_complete", "cgroup_scan_complete"] {
        rejected_mutation(&format!("/record/cleanup/{flag}"), json!(false));
    }
    for field in ["process_group_members", "cgroup_members"] {
        rejected_mutation(&format!("/record/cleanup/{field}"), json!(1));
    }
    rejected_mutation("/record/cleanup/error", json!("scan-failed"));
    rejected_mutation("/record/cleanup/pgid", json!(202));
    rejected_mutation("/record/cleanup/cgroup_path", json!("workers/not-bound/handle"));
}

fn strict_wire_objects() -> Vec<&'static str> {
    vec!["", "/record", "/record/owner", "/record/owner_process", "/record/process",
        "/record/direct_wait", "/record/stdout", "/record/stderr", "/record/cleanup", "/record/evidence_refs"]
}

// Detects deny_unknown_fields omissions and accidentally optional required-nullable keys.
#[test]
fn extra_and_every_missing_new_wire_field_fail_typed_decoding() {
    for path in strict_wire_objects() {
        let original = fixture();
        let keys: Vec<String> = original.pointer(path).unwrap().as_object().unwrap().keys().cloned().collect();
        for key in keys {
            let mut bundle = original.clone();
            bundle.pointer_mut(path).unwrap().as_object_mut().unwrap().remove(&key);
            let raw = bytes(&bundle);
            assert!(serde_json::from_slice::<OwnerEvidenceBundle>(&raw).is_err(), "missing {path}/{key}");
            assert!(validate_bundle(&raw, Some(&expected())).is_err());
        }
        let mut bundle = original;
        bundle.pointer_mut(path).unwrap().as_object_mut().unwrap().insert("unexpected".into(), json!(true));
        let raw = bytes(&bundle);
        assert!(serde_json::from_slice::<OwnerEvidenceBundle>(&raw).is_err(), "extra at {path}");
        assert!(validate_bundle(&raw, Some(&expected())).is_err());
    }
}

// Detects a permissive Value preparse that silently collapses duplicate keys, including null.
#[test]
fn duplicate_keys_at_every_wire_object_are_rejected() {
    let bundle = fixture();
    for path in strict_wire_objects().into_iter().chain(["/record/execution"]) {
        let object = bundle.pointer(path).unwrap().as_object().unwrap();
        for (key, value) in object {
            let original = serde_json::to_string(bundle.pointer(path).unwrap()).unwrap();
            let mut replacement = original.clone(); replacement.pop();
            replacement.push_str(&format!(",{}:{}}}", serde_json::to_string(key).unwrap(), value));
            let raw = serde_json::to_string(&bundle).unwrap().replacen(&original, &replacement, 1);
            assert!(serde_json::from_str::<OwnerEvidenceBundle>(&raw).is_err(), "duplicate {path}/{key}");
            assert!(validate_bundle(raw.as_bytes(), Some(&expected())).is_err());
        }
    }
}

// Detects schema fallback, serde integer coercions, range overflows, or lossy JSON parsing.
#[test]
fn wrong_types_ranges_schema_and_malformed_json_are_rejected() {
    rejected_mutation("/record/schema", json!("linux-worker-owner-evidence/2"));
    for (path, value) in [
        ("/record/process/pid", json!(-1)), ("/record/process/pid", json!(4294967296_u64)),
        ("/record/process/start_ticks", json!(1.0)), ("/record/process/pgid", json!("201")),
        ("/record/stdout/prefix_len", json!(null)), ("/record/stderr/total_read_bytes", json!(-1)),
        ("/record/direct_wait/exit_code", json!(256)),
        ("/record/direct_wait/termination_signal", json!(-1)),
        ("/record/direct_wait/raw_wait_status", json!(2147483648_i64)),
        ("/record/stdout/eof", json!(1)), ("/record/cleanup/error", json!(false)),
    ] {
        let mut bundle = fixture();
        *bundle.pointer_mut(path).unwrap() = value;
        // No canonicalization: floats intentionally violate the canonical input contract.
        let raw = bytes(&bundle);
        assert!(serde_json::from_slice::<OwnerEvidenceBundle>(&raw).is_err(), "wrong type {path}");
        assert!(validate_bundle(&raw, Some(&expected())).is_err());
    }
    for raw in [b"{".as_slice(), b"null".as_slice(), b"[]".as_slice(), b"{} trailing".as_slice()] {
        assert!(validate_bundle(raw, Some(&expected())).is_err());
    }
}

// Detects applying the 16KiB cap to hex prefixes instead of the canonical record.
#[test]
fn canonical_record_size_is_bounded_separately_from_prefixes() {
    let mut bundle = fixture();
    for stream in ["stdout", "stderr"] { set_stream(&mut bundle, stream, &vec![0; 4096], 4096); }
    assert!(bytes(&bundle).len() > 16384);
    assert!(accepted(&bundle, &expected()));
    bundle["record"]["schema"] = json!("x".repeat(16385));
    rehash(&mut bundle);
    assert!(canonical(&bundle["record"]).unwrap().len() > 16384);
    let error = validate_bundle(&bytes(&bundle), Some(&expected())).unwrap_err();
    assert!(error.to_string().contains("record exceeds 16384 canonical bytes"));
}

// Detects hashing incoming whitespace/key order instead of typed canonical record data.
#[test]
fn reordered_whitespace_input_uses_the_same_canonical_record_hash() {
    let bundle = fixture();
    let raw = format!("{{\n \"stderr_prefix_hex\":{},\"stdout_prefix_hex\":{},\"record_sha256\":{},\"record\":{}\n}}",
        bundle["stderr_prefix_hex"], bundle["stdout_prefix_hex"], bundle["record_sha256"],
        serde_json::to_string_pretty(&bundle["record"]).unwrap());
    assert!(validate_bundle(raw.as_bytes(), Some(&expected())).is_ok());
}
