//! Production manager config/Store with an offline transport seam; no SSH or GUI process.
use agentranch::{config::Actor, core::{ssh_gateway::{TransportOutput,WorkerTransport}, ssh_manager::SshManager}};
use serde_json::json;

struct NoRemote { calls:usize }
impl WorkerTransport for NoRemote {
    fn exchange(&mut self,request:&[u8],_:u64)->anyhow::Result<TransportOutput> {
        self.calls+=1;
        let value:serde_json::Value=serde_json::from_slice(request)?;
        assert_eq!(value["payload"]["worker_id"],"B");
        assert_eq!(value["payload"]["worker_artifact_sha256"],"ab".repeat(32));
        Ok(TransportOutput{stdout:vec![],stderr:b"offline-disconnect".to_vec(),exit_code:Some(255),failure:Some("offline disconnect".into())})
    }
}
fn fixture()->(tempfile::TempDir,std::path::PathBuf) {
    let root=tempfile::tempdir().unwrap();
    std::fs::write(root.path().join("test.key"),"07".repeat(32)).unwrap();
    let controller=json!({"schema":"ao-reason-config/1","mode":"real","role":"controller","deployment":"manager-test","state_dir":"state",
        "nodes":[{"id":"B","address":"B","key_file":"test.key","slots":1,"templates":["sum-v1"],"admitted":true}]});
    std::fs::write(root.path().join("controller.json"),serde_json::to_vec(&controller).unwrap()).unwrap();
    let config=json!({"schema":"native-ssh-gateway/1","deployment":"manager-test","worker_id":"B","host":"b.example","port":22,"user":"worker",
        "known_hosts":"known_hosts","identity_file":"id_ed25519","key_file":"test.key","controller_config":"controller.json","state_dir":"state",
        "remote_worker":"/home/worker/aw-cli","remote_config":"/home/worker/worker.json","artifact_sha256":"ab".repeat(32),"artifact_bytes":123,"timeout_secs":1});
    let path=root.path().join("gateway.json");
    std::fs::write(&path,serde_json::to_vec(&config).unwrap()).unwrap();
    (root,path)
}
#[test]
fn config_to_gateway_uses_real_ledger_and_retains_unknown_reservation() {
    let(root,path)=fixture();
    let mut manager=SshManager::open(&path).unwrap();
    let mut wire=NoRemote{calls:0};
    let result=manager.dispatch_with_transport(&Actor::Local("manager".into()),&mut wire,100).unwrap();
    assert_eq!(wire.calls,1);
    assert_eq!(result.status,"unknown");
    assert!(result.slot_reserved);
    let runs=manager.runs().unwrap();
    assert_eq!(runs.len(),1);
    assert_eq!(runs[0].status,"unknown");
    assert_eq!(runs[0].execution_identity("manager-test").unwrap().artifact_sha256,Some("ab".repeat(32)));
    assert!(root.path().join("state/controller.sqlite").is_file());
    assert!(SshManager::open(&path).is_err(),"second controller must not open same ledger");
}
#[test]
fn existing_queued_run_is_not_assigned_in_place_of_current_dispatch() {
    use agentranch::{config::{Config,NodeSpec},core::store::Store};
    let(root,path)=fixture();
    let controller:Config=serde_json::from_slice(&std::fs::read(root.path().join("controller.json")).unwrap()).unwrap();
    let state=controller.prepare_state(root.path()).unwrap();
    let nodes:Vec<NodeSpec>=controller.load_nodes(root.path()).unwrap();
    let mut store=Store::open(&state.join("controller.sqlite"),"manager-test",&nodes).unwrap();
    let old=store.dispatch(&Actor::Local("old-owner".into()),"sum-v1",90).unwrap();
    drop(store);
    let mut manager=SshManager::open(&path).unwrap();
    let mut wire=NoRemote{calls:0};
    let result=manager.dispatch_with_transport(&Actor::Local("new-owner".into()),&mut wire,100);
    assert!(result.is_ok(),"current dispatch must not assign the old queue: {result:?}");
    assert_eq!(wire.calls,1);
    assert_eq!(manager.runs().unwrap().into_iter().find(|run|run.id==old.id).unwrap().status,"queued");
}
#[test]
fn background_service_loads_pinned_worker_without_starting_transport() {
    use agentranch::{config::Mode,core::service::{ServiceHandle,ServiceOptions,UiCommand}};
    use std::{sync::Arc,time::{Duration,Instant}};
    let(root,path)=fixture();
    let mut service=ServiceHandle::spawn(ServiceOptions{mode:Mode::Real,state_dir:root.path().join("ui"),worker_exe:"unused.exe".into(),gateway_key:None,gateway_model_token:None},Arc::new(||{})).unwrap();
    let request=service.try_submit(UiCommand::GatewayConfigure{config_path:path}).unwrap();
    let until=Instant::now()+Duration::from_secs(3);
    loop {
        if let Some(reply)=service.drain_results().into_iter().find(|reply|reply.request_id==request) {
            let result=reply.result.unwrap();
            assert_eq!(result["worker_id"],"B");
            assert_eq!(result["remote_verified"],false);
            break;
        }
        assert!(Instant::now()<until,"configure did not return");
        std::thread::sleep(Duration::from_millis(10));
    }
    let snapshot=service.snapshot();
    let gateway=snapshot.gateway.as_ref().expect("configured worker must reach UI snapshot");
    assert_eq!(gateway.worker_id,"B");
    assert!(!gateway.remote_verified);
    assert!(snapshot.runs.is_empty(),"configuration alone must not dispatch");
    service.stop();
}
