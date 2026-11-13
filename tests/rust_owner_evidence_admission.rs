//! Local scripted SQLite admission contract only. No OS owner, child, AO, Reason,
//! model, native GUI, remote host or Linux facts are observed by these tests.
use agentranch::{
    config::{Actor, NodeSpec},
    core::{
        protocol::{Envelope, PROTOCOL, canonical},
        store::{ExitProof, Run, Store},
        worker_owner::{ExpectedOwnerBinding, OwnerEvidenceReference, OwnerVerification},
    },
};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const DEPLOYMENT: &str = "owner-evidence-admission";
const SESSION: &str = "owner-session";

fn node() -> NodeSpec {
    NodeSpec { id: "B".into(), address: "owner-evidence-B/runner".into(),
        key_hex: "07".repeat(32), slots: 1, admitted: true, healthy: true,
        draining: false, templates: vec!["control-native-v1".into()] }
}
fn actor() -> Actor { Actor::Local("owner-evidence-test".into()) }
fn digest(value: &Value) -> String { hex::encode(Sha256::digest(canonical(value).unwrap())) }
fn setup() -> (tempfile::TempDir, Store, Run) {
    let temp = tempfile::tempdir().unwrap();
    let mut store = Store::open(&temp.path().join("controller.sqlite"), DEPLOYMENT, &[node()]).unwrap();
    let run = store.dispatch(&actor(), "control-native-v1", 100).unwrap();
    store.assign_next(100).unwrap().unwrap();
    store.claim(&run.id, 1, &run.epoch).unwrap();
    store.bind(&run.id, 1, &run.epoch, SESSION).unwrap();
    (temp, store, run)
}
fn proof(run: &Run) -> ExitProof {
    ExitProof { run_id: run.id.clone(), attempt: run.attempt, epoch: run.epoch.clone(),
        session: Some(SESSION.into()), empty: true, observer_alive: true,
        not_started: false, owner_evidence: None }
}
fn signed(run: &Run, kind: &str, payload: Value) -> Envelope {
    let mut m = Envelope { schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
        deployment: DEPLOYMENT.into(), from: "B".into(), to: "A".into(),
        run_id: run.id.clone(), attempt: run.attempt, epoch: run.epoch.clone(),
        kind: kind.into(), sent_at: 100, expires_at: 400, payload, mac: String::new() };
    m.sign(&hex::decode(node().key_hex).unwrap()).unwrap(); m
}
fn expected(store: &Store, run: &Run) -> ExpectedOwnerBinding {
    let execution = store.run(&run.id).unwrap().execution_identity(DEPLOYMENT).unwrap();
    let instance = "123e4567-e89b-42d3-a456-426614174000";
    let boot = "123e4567-e89b-12d3-a456-426614174001";
    // Independent literal preimage, not a call to the implementation digest helper.
    let handle = format!("ow-{}", digest(&json!({"schema":"linux-worker-owner-evidence/1",
        "execution":execution,"node_id":"B","instance_id":instance,"boot_id":boot})));
    let start = store.pending_outbox(10).unwrap().into_iter().find(|(_, m)| m.kind == "start").unwrap().1;
    serde_json::from_value(json!({"execution":execution,
        "owner":{"node_id":"B","instance_id":instance,"boot_id":boot,"owner_handle":handle},
        "request_sha256":digest(&start.payload), "owner_process":{"pid":100,"start_ticks":1000},
        "process":{"pid":200,"parent_pid":100,"start_ticks":2000,"pgid":200,
            "cgroup_path":format!("workers/{instance}/{handle}")}})).unwrap()
}
fn bundle(expected: &ExpectedOwnerBinding, raw_status: i32) -> Value {
    let (code, signal, numeric) = if raw_status & 0xff == 0 {
        (Some(raw_status >> 8), None, raw_status >> 8)
    } else { (None, Some(raw_status), -raw_status) };
    let empty_sha = hex::encode(Sha256::digest(b""));
    let stream = json!({"prefix_len":0,"total_read_bytes":0,"truncated":false,
        "eof":true,"error":null,"prefix_sha256":empty_sha});
    let prefix = format!("owners/{}/{}",expected.owner.instance_id,expected.owner.owner_handle);
    let record = json!({"schema":"linux-worker-owner-evidence/1","execution":expected.execution,
        "owner":expected.owner,"request_sha256":expected.request_sha256,
        "owner_process":{"pid":100,"start_ticks":1000},
        "process":{"pid":200,"parent_pid":100,"start_ticks":2000,"pgid":200,"cgroup_path":expected.process.cgroup_path},
        "direct_wait":{"wait_completed":true,"method":"child_wait","raw_wait_status":raw_status,
            "exit_code":code,"termination_signal":signal,"numeric_exit":numeric},
        "stdout":stream,"stderr":stream,
        "cleanup":{"proc_scan_complete":true,"process_group_scan_complete":true,"cgroup_scan_complete":true,
            "pgid":200,"cgroup_path":expected.process.cgroup_path,"process_group_members":0,"cgroup_members":0,"error":null},
        "evidence_refs":{"record":format!("{prefix}/record.json"),"stdout":format!("{prefix}/stdout.bin"),"stderr":format!("{prefix}/stderr.bin")}});
    json!({"record_sha256":digest(&record),"record":record,"stdout_prefix_hex":"","stderr_prefix_hex":""})
}
fn launch(run: &Run, expected: &ExpectedOwnerBinding) -> Envelope {
    signed(run, "report", json!({"schema":"owner-evidence-launch/1","phase":"launch",
        "session":SESSION,"binding":expected}))
}

fn terminal(run: &Run, expected: &ExpectedOwnerBinding, mut bundle: Value, passed: bool) -> Envelope {
    bundle["record_sha256"] = json!(digest(&bundle["record"]));
    let report = json!({"schema":"scripted-owner-report/1","value":6});
    let verification = OwnerVerification { passed, state: if passed {"verified"} else {"failed"}.into(), details: json!({"source":"scripted-independent-test-verifier"}) };
    let reference = OwnerEvidenceReference { schema: "owner-evidence-reference/1".into(),
        binding_sha256: digest(&json!({"schema":"owner-binding-reference/1","binding":expected})),
        bundle_sha256: digest(&bundle), record_sha256: bundle["record_sha256"].as_str().unwrap().into(),
        report_sha256: digest(&json!({"schema":"owner-report-binding/1","execution":expected.execution,"report":report,"verification":verification})) };
    let mut exit_proof = proof(run); exit_proof.owner_evidence = Some(reference.clone());
    signed(run, "report", json!({"schema":"owner-evidence-report/1","phase":"terminal",
        "session":SESSION,"report":report,"verification":verification,
        "owner_evidence":{"reference":reference,"bundle_json":serde_json::to_string(&bundle).unwrap()},"exit_proof":exit_proof}))
}

#[test]
fn launch_phase_registers_binding_before_terminal_admission() {
    let (_temp, mut store, run) = setup();
    let expected = expected(&store, &run);
    assert!(store.receive(&launch(&run, &expected), 100).unwrap());
    let message = terminal(&run, &expected, bundle(&expected, 0), true);
    assert!(store.receive(&message, 101).unwrap());
    assert!(store.assignment(&run.id, 1).unwrap().unwrap().released);
}

#[test]
fn terminal_phase_without_launch_cannot_self_register_expected_binding() {
    let (_temp, mut store, run) = setup();
    let expected = expected(&store, &run);
    let message = terminal(&run, &expected, bundle(&expected, 0), true);
    assert!(store.receive(&message, 100).is_err());
    assert!(!store.assignment(&run.id, 1).unwrap().unwrap().released);
}

#[test]
fn matching_minimal_process_exit_proof_without_m0_retains_assignment() {
    let (_temp, mut store, run) = setup();
    store.cancel_at(&actor(), &run.id, 101).unwrap();
    assert!(!store.release(&proof(&run)).unwrap());
    assert!(!store.assignment(&run.id, 1).unwrap().unwrap().released);
}

#[test]
fn minimal_process_exit_report_and_status_ingress_retain_assignment() {
    for kind in ["report", "status"] {
        let (_temp, mut store, run) = setup();
        let payload = if kind == "report" { json!({"session":SESSION,"report":{},"verification":{"passed":true},"exit_proof":proof(&run)}) }
            else { json!({"state":"failed","session":SESSION,"spawn_intent":true,"exit_proof":proof(&run)}) };
        store.receive(&signed(&run, kind, payload), 100).unwrap();
        assert!(!store.assignment(&run.id, 1).unwrap().unwrap().released);
        assert_ne!(store.run(&run.id).unwrap().detail["retry_safe"], true);
        assert_ne!(store.run(&run.id).unwrap().status, "done");
    }
}

#[test]
fn complete_zero_nonzero_and_signal_m0_admission_releases_only_current_assignment() {
    for (raw, passed, state) in [(0,true,"done"),(7<<8,false,"failed"),(9,false,"failed"),(0,false,"failed")] {
        let (temp, mut store, run) = setup();
        let expected = expected(&store, &run);
        store.register_local_owner_binding(&run.id, 1, &expected, 100).unwrap();
        let message = terminal(&run,&expected,bundle(&expected,raw),passed);
        assert!(store.receive(&message,100).unwrap());
        assert!(store.assignment(&run.id,1).unwrap().unwrap().released);
        assert_eq!(store.run(&run.id).unwrap().status,state);
        let exit_proof: ExitProof = serde_json::from_value(message.payload["exit_proof"].clone()).unwrap();
        assert!(!store.release(&exit_proof).unwrap());
        assert!(!store.receive(&message,100).unwrap());
        let db = rusqlite::Connection::open(temp.path().join("controller.sqlite")).unwrap();
        assert_eq!(db.query_row("SELECT count(*) FROM owner_evidence",[],|r|r.get::<_,i64>(0)).unwrap(),1);
    }
}

#[test]
fn absent_expected_fake_reference_and_wrong_digest_fail_closed() {
    let (_temp, mut store, run) = setup();
    let expected = expected(&store,&run);
    let message = terminal(&run,&expected,bundle(&expected,0),true);
    assert!(store.receive(&message,100).is_err());
    let fake: ExitProof = serde_json::from_value(message.payload["exit_proof"].clone()).unwrap();
    assert!(!store.release(&fake).unwrap());
    store.register_local_owner_binding(&run.id,1,&expected,100).unwrap();
    assert!(!store.release(&fake).unwrap());
    let mut altered = message.clone();
    altered.payload["owner_evidence"]["reference"]["bundle_sha256"] = json!("00".repeat(32));
    altered.sign(&hex::decode(node().key_hex).unwrap()).unwrap();
    assert!(store.receive(&altered,100).is_err());
    assert!(!store.assignment(&run.id,1).unwrap().unwrap().released);
}

#[test]
fn duplicate_bundle_keys_and_incomplete_m0_are_rejected() {
    for mutation in 0..8 {
        let (_temp,mut store,run)=setup(); let expected=expected(&store,&run);
        store.register_local_owner_binding(&run.id,1,&expected,100).unwrap();
        let mut b=bundle(&expected,0);
        match mutation {
            0=>b["record"]["direct_wait"]["wait_completed"]=json!(false),
            1=>b["record"]["stdout"]["eof"]=json!(false),
            2=>b["record"]["stderr"]["error"]=json!("reader failed"),
            3=>b["record"]["cleanup"]["proc_scan_complete"]=json!(false),
            4=>b["record"]["cleanup"]["process_group_members"]=json!(1),
            5=>b["record"]["cleanup"]["cgroup_members"]=json!(1),
            6=>b["record"]["execution"]["session"]=json!("wrong-session"),
            _=>{},
        }
        let mut m=terminal(&run,&expected,b,true);
        if mutation==7 { let raw=m.payload["owner_evidence"]["bundle_json"].as_str().unwrap();
            m.payload["owner_evidence"]["bundle_json"]=json!(raw.replacen("\"wait_completed\":true","\"wait_completed\":true,\"wait_completed\":true",1)); }
        m.sign(&hex::decode(node().key_hex).unwrap()).unwrap();
        assert!(store.receive(&m,100).is_err(),"mutation {mutation}");
        assert!(!store.assignment(&run.id,1).unwrap().unwrap().released);
    }
}

#[test]
fn schema1_migration_preserves_records_without_backfilling_owner_authority() {
    let (temp, store, run) = setup();
    drop(store);
    let path = temp.path().join("controller.sqlite");
    let db = rusqlite::Connection::open(&path).unwrap();
    let outbox: i64 = db.query_row("SELECT count(*) FROM outbox", [], |r| r.get(0)).unwrap();
    db.execute_batch("DROP TABLE owner_evidence; DROP TABLE owner_bindings; ALTER TABLE inbox DROP COLUMN received_at; PRAGMA user_version=1;").unwrap();
    drop(db);
    let mut reopened = Store::open(&path, DEPLOYMENT, &[node()]).unwrap();
    assert_eq!(reopened.run(&run.id).unwrap().session.as_deref(), Some(SESSION));
    reopened.cancel_at(&actor(), &run.id, 101).unwrap();
    assert!(!reopened.release(&proof(&run)).unwrap());
    let db = rusqlite::Connection::open(&path).unwrap();
    assert_eq!(db.query_row("PRAGMA user_version", [], |r| r.get::<_,i64>(0)).unwrap(), 2);
    assert_eq!(db.query_row("SELECT count(*) FROM owner_bindings", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
    assert_eq!(db.query_row("SELECT count(*) FROM owner_evidence", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
    assert!(db.query_row("SELECT count(*) FROM outbox", [], |r| r.get::<_,i64>(0)).unwrap() >= outbox);
    assert!(!reopened.assignment(&run.id, 1).unwrap().unwrap().released);
}

#[test]
fn inbox_insert_failure_rolls_back_admission_report_and_release() {
    let (temp, mut store, run) = setup();
    let expected = expected(&store, &run);
    store.register_local_owner_binding(&run.id, 1, &expected, 100).unwrap();
    let message = terminal(&run, &expected, bundle(&expected, 0), true);
    let db = rusqlite::Connection::open(temp.path().join("controller.sqlite")).unwrap();
    db.execute_batch("CREATE TRIGGER reject_owner_inbox BEFORE INSERT ON inbox BEGIN SELECT RAISE(ABORT,'injected inbox failure'); END;").unwrap();
    assert!(store.receive(&message, 100).is_err());
    assert!(!store.assignment(&run.id, 1).unwrap().unwrap().released);
    assert_eq!(store.run(&run.id).unwrap().status, "running");
    assert!(store.run(&run.id).unwrap().detail.get("report").is_none());
    assert_eq!(db.query_row("SELECT count(*) FROM owner_evidence", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
    assert_eq!(db.query_row("SELECT count(*) FROM inbox", [], |r| r.get::<_,i64>(0)).unwrap(), 0);
    db.execute_batch("DROP TRIGGER reject_owner_inbox;").unwrap();
    assert!(store.receive(&message, 100).unwrap());
    assert!(store.assignment(&run.id, 1).unwrap().unwrap().released);
}

#[test]
fn registered_expected_is_immutable_and_must_match_store_execution() {
    let (_temp,mut store,run)=setup(); let expected=expected(&store,&run);
    store.register_local_owner_binding(&run.id,1,&expected,100).unwrap();
    let mut changed=expected.clone(); changed.process.start_ticks+=1;
    assert!(store.register_local_owner_binding(&run.id,1,&changed,100).is_err());
    for field in 0..4 { let mut changed=expected.clone(); match field {
        0=>changed.execution.attempt+=1,1=>changed.execution.session=Some("other".into()),
        2=>changed.execution.deployment="other".into(),_=>changed.execution.source_sha256=Some("aa".repeat(32)),
    } assert!(store.register_local_owner_binding(&run.id,1,&changed,100).is_err()); }
    assert!(!store.assignment(&run.id,1).unwrap().unwrap().released);
}
