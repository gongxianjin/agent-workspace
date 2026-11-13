use ao_reason_workbench::{config::{Actor, NodeSpec}, core::{ssh_gateway::{GatewayRequest, SshGatewayConfig}, store::Store}};
use serde_json::json;

fn node() -> NodeSpec {
    NodeSpec { id:"B".into(), address:"B".into(), key_hex:"07".repeat(32), slots:1,
        admitted:true, healthy:true, draining:false, templates:vec!["sum-v1".into()] }
}
fn config() -> SshGatewayConfig {
    serde_json::from_value(json!({"schema":"native-ssh-gateway/1","deployment":"dispatch-test",
        "worker_id":"B","host":"b.example","port":22,"user":"worker",
        "known_hosts":"known_hosts","identity_file":"id_ed25519","key_file":"worker.key",
        "controller_config":"controller.json","state_dir":"state","remote_worker":"/opt/aw/aw-cli",
        "remote_config":"/etc/aw/worker.json","artifact_sha256":"ab".repeat(32),"artifact_bytes":123,"timeout_secs":5})).unwrap()
}
#[test]
fn gateway_assignment_persists_selected_worker_artifact_identity() {
    let root=tempfile::tempdir().unwrap();
    let mut store=Store::open(&root.path().join("controller.sqlite"),"dispatch-test",&[node()]).unwrap();
    let run=store.dispatch(&Actor::Local("owner".into()),"sum-v1",100).unwrap();
    store.assign_next_with_worker_artifact(100,"B","ab".repeat(32)).unwrap().unwrap();
    let request=GatewayRequest::from_store(&store,&config(),&run.id).unwrap();
    assert_eq!(request.start.payload["worker_id"],"B");
    assert_eq!(request.start.payload["worker_artifact_sha256"],"ab".repeat(32));
    assert_eq!(request.identity.artifact_sha256,Some("ab".repeat(32)));
    assert_eq!(store.run(&run.id).unwrap().execution_identity("dispatch-test").unwrap().artifact_sha256,Some("ab".repeat(32)));
}
