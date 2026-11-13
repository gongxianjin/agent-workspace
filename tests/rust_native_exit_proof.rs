use agentranch::{
    config::{Actor, NodeSpec},
    core::{protocol::{Envelope, PROTOCOL}, store::Store},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::Path, time::{SystemTime, UNIX_EPOCH}};

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(),
        address: "native-B/runner".into(),
        key_hex: "07".repeat(32),
        slots: 1,
        admitted: true,
        healthy: true,
        draining: false,
        templates: vec!["sum-native-v1".into()],
    }
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs()
}

fn signed_status(run_id: &str, epoch: &str, payload: Value, timestamp: u64) -> Envelope {
    let mut message = Envelope {
        schema: PROTOCOL.into(),
        message_id: uuid::Uuid::new_v4().to_string(),
        deployment: "native-exit-proof-contract".into(),
        from: "B".into(),
        to: "A".into(),
        run_id: run_id.into(),
        attempt: 1,
        epoch: epoch.into(),
        kind: "status".into(),
        sent_at: timestamp,
        expires_at: timestamp + 300,
        payload,
        mac: String::new(),
    };
    message.sign(&[0x07; 32]).unwrap();
    message
}

#[test]
fn unverified_native_exit_fields_do_not_release_or_become_durable_evidence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("dist/native-exit-proof-contract");
    std::fs::create_dir_all(&root).unwrap();
    let evidence = root.join(format!("case-rejection-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&evidence).unwrap();
    let synthetic_bytes = b"synthetic only: this file was not written by an AO observer\n";
    std::fs::write(evidence.join("status.json"), synthetic_bytes).unwrap();
    let synthetic_sha256 = hex::encode(Sha256::digest(synthetic_bytes));
    let timestamp = now();

    let mut store = Store::open(&evidence.join("controller.sqlite"), "native-exit-proof-contract", &[node()]).unwrap();
    let run = store.dispatch(&Actor::Local("contract-test".into()), "sum-native-v1", timestamp).unwrap();
    store.assign_next(timestamp).unwrap().unwrap();
    store.receive(&signed_status(&run.id, &run.epoch, json!({
        "state": "running",
        "spawn_intent": true,
        "session": "owned-session-1"
    }), timestamp), timestamp).unwrap();
    store.cancel_at(&Actor::Local("contract-test".into()), &run.id, timestamp).unwrap();

    // These values intentionally look like a proof but have no independent OS
    // observation: the PID is this test process, and the file/hash are synthetic.
    // A normal signed status is not a provider ExitProof ingress.
    let unverified = json!({
        "run_id": run.id,
        "attempt": 1,
        "epoch": run.epoch,
        "session": "owned-session-1",
        "not_started": false,
        "exit_code": 0,
        "pid": std::process::id(),
        "process_identity": format!("pid:{}:synthetic-test-process", std::process::id()),
        "job_empty": true,
        "observer_healthy": true,
        "observed_at": timestamp,
        "evidence_sha256": synthetic_sha256,
        "evidence_path": evidence.join("status.json"),
    });
    store.receive(&signed_status(&run.id, &run.epoch, json!({
        "state": "failed",
        "session": "owned-session-1",
        "native_exit_proof": unverified,
    }), timestamp), timestamp).unwrap();

    let assignment = store.assignment(&run.id, 1).unwrap().unwrap();
    assert!(!assignment.released,
        "unverified status fields must not release the exact assignment");
    let result = store.run(&run.id).unwrap();
    assert!(result.detail.get("native_exit_proof").is_none(),
        "unverified fields must not become durable native evidence");
    eprintln!(
        "NATIVE_EXIT_PROOF_REJECTED evidence={} pid={} wait_completed=false numeric_exit=UNVERIFIED source_sha256={} released=false",
        evidence.display(),
        std::process::id(),
        synthetic_sha256
    );
}
