// Breaks caught: unauthenticated/expired/foreign envelopes and unsafe static nodes.
use ao_reason_workbench::{config::NodeSpec, core::protocol::{Envelope, PROTOCOL}};
use serde_json::json;
fn message() -> Envelope {
    Envelope { schema: PROTOCOL.into(), message_id:"m-1".into(), deployment:"native-test".into(),
        from:"A".into(), to:"B".into(), run_id:"r-1".into(), attempt:1, epoch:"e-1".into(),
        kind:"start".into(), sent_at:100, expires_at:400, payload:json!({"template":"sum-native-v1","label":"中文\n"}),mac:String::new() }
}
#[test]
fn signed_unicode_envelope_roundtrips_and_tampering_is_rejected() {
    let mut m=message(); m.sign(&[7;32]).unwrap();
    assert_eq!(m.preimage().unwrap(),r#"{"attempt":1,"deployment":"native-test","epoch":"e-1","expires_at":400,"from":"A","kind":"start","message_id":"m-1","payload":{"label":"中文\n","template":"sum-native-v1"},"run_id":"r-1","schema":"ao-reason-native/1","sent_at":100,"to":"B"}"#.as_bytes());
    // Independent Python stdlib json/HMAC golden vector; not Rust-generated expectation.
    assert_eq!(m.mac,"7e3bfd73b410c1b27f29c587763ef70718e52ebd9cba2a22a50013f82aaa214a");
    let encoded=serde_json::to_vec(&m).unwrap();
    let decoded:Envelope=serde_json::from_slice(&encoded).unwrap();
    decoded.verify(&[7;32],"native-test","B",110).unwrap();
    m.payload["label"]=json!("other");
    assert!(m.verify(&[7;32],"native-test","B",110).is_err());
}
#[test]
fn wrong_key_deployment_recipient_and_expired_are_rejected() {
    let mut m=message();m.sign(&[7;32]).unwrap();
    assert!(m.verify(&[8;32],"native-test","B",110).is_err());
    assert!(m.verify(&[7;32],"other","B",110).is_err());
    assert!(m.verify(&[7;32],"native-test","C",110).is_err());
    assert!(m.verify(&[7;32],"native-test","B",400).is_err());
}
#[test]
fn future_long_ttl_zero_attempt_legacy_and_extra_fields_are_rejected() {
    for change in 0..4 {
        let mut m=message();match change {0=>m.sent_at=200,1=>m.expires_at=1000,2=>m.attempt=0,_=>m.schema="ao-fleet-lab/1".into()};
        m.sign(&[7;32]).unwrap();assert!(m.verify(&[7;32],"native-test","B",110).is_err());
    }
    let mut value=serde_json::to_value(message()).unwrap();value["owner"]=json!("forged");
    assert!(serde_json::from_value::<Envelope>(value).is_err());
}
#[test]
fn unsafe_node_shapes_are_rejected_but_valid_static_node_is_accepted() {
    let good=NodeSpec{id:"B".into(),address:"lab-b/runner".into(),key_hex:"07".repeat(32),slots:2,
        admitted:true,healthy:true,draining:false,templates:vec!["sum-native-v1".into()]};
    good.validate().unwrap();
    for change in 0..4 {let mut bad=good.clone();match change {0=>bad.slots=0,1=>bad.key_hex="07".into(),2=>bad.address="".into(),_=>bad.id="../B".into()};assert!(bad.validate().is_err());}
}

#[test]
fn execution_identity_is_canonical_and_legacy_project_optional_inputs_survive() {
    use ao_reason_workbench::core::protocol::ExecutionIdentity;
    let source_sha256 = "11".repeat(32);
    let artifact_sha256 = "22".repeat(32);
    let first: ExecutionIdentity = serde_json::from_value(json!({
        "deployment":"native-test", "run_id":"run-1", "attempt":1,
        "epoch":"epoch-1", "session":"session-1",
        "artifact_sha256":artifact_sha256, "source_sha256":source_sha256
    })).unwrap();
    let reordered: ExecutionIdentity = serde_json::from_value(json!({
        "source_sha256":source_sha256, "session":"session-1", "epoch":"epoch-1",
        "attempt":1, "run_id":"run-1", "deployment":"native-test",
        "artifact_sha256":artifact_sha256
    })).unwrap();
    assert_eq!(first, reordered);
    assert_eq!(first.canonical_bytes().unwrap(), reordered.canonical_bytes().unwrap());
    assert_eq!(first.canonical_hash().unwrap(), reordered.canonical_hash().unwrap());

    for change in [
        ("deployment", json!("other-deployment")),
        ("run_id", json!("run-2")),
        ("attempt", json!(2)),
        ("epoch", json!("epoch-2")),
        ("session", json!("session-2")),
        ("source_sha256", json!("33".repeat(32))),
        ("artifact_sha256", json!("44".repeat(32))),
    ] {
        let mut value = serde_json::to_value(&first).unwrap();
        value[change.0] = change.1;
        let changed: ExecutionIdentity = serde_json::from_value(value).unwrap();
        assert_ne!(first, changed, "identity field {} must be significant", change.0);
    }

    let legacy: ExecutionIdentity = serde_json::from_value(json!({
        "deployment":"native-test", "run_id":"run-1", "attempt":1,
        "epoch":"epoch-1", "session":null
    })).unwrap();
    assert_eq!(legacy.source_sha256, None);
    assert_eq!(legacy.artifact_sha256, None);
    assert_eq!(legacy.deployment, first.deployment);
}
