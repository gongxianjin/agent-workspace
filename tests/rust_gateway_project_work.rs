use ao_reason_workbench::{config::{Actor, NodeSpec}, core::{project_work::{ConfirmedDocument, ProjectWork, ProjectStage, WorkTask}, ssh_gateway::{GatewayRequest, SshGatewayConfig, TransportOutput, WorkerTransport, run_once}, store::Store}};
use serde_json::json;
use sha2::{Digest, Sha256};

struct NoopTransport;
impl WorkerTransport for NoopTransport {
    fn exchange(&mut self,_request:&[u8],_timeout_secs:u64)->anyhow::Result<TransportOutput>{
        Ok(TransportOutput{stdout:Vec::new(),stderr:Vec::new(),exit_code:Some(0),failure:None})
    }
}

fn node() -> NodeSpec {
    NodeSpec { id:"B".into(), address:"B".into(), key_hex:"07".repeat(32), slots:1,
        admitted:true, healthy:true, draining:false, templates:vec!["sum-v1".into(),"project-work-v1".into()] }
}
fn unprobed_node() -> NodeSpec {
    // healthy=false: the ONLY route to assignment is the direct_pinned
    // artifact path — exactly the seam that gates on template=="sum-v1".
    NodeSpec { healthy:false, ..node() }
}
fn config() -> SshGatewayConfig {
    serde_json::from_value(json!({"schema":"native-ssh-gateway/1","deployment":"dispatch-test",
        "worker_id":"B","host":"b.example","port":22,"user":"worker",
        "known_hosts":"known_hosts","identity_file":"id_ed25519","key_file":"worker.key",
        "controller_config":"controller.json","state_dir":"state","remote_worker":"/opt/aw/aw-cli",
        "remote_config":"/etc/aw/worker.json","artifact_sha256":"ab".repeat(32),"artifact_bytes":123,"timeout_secs":5})).unwrap()
}
fn work() -> ProjectWork {
    let text="# 设计\n成都遛娃静态页（示例数据）。".to_string();
    let document=ConfirmedDocument{version:1,sha256:hex::encode(Sha256::digest(text.as_bytes())),text};
    let tasks=vec![WorkTask{id:"t1".into(),title:"实现静态页".into(),description:"按设计文档生成 index.html/css/style.css/js/data.js/js/app.js".into(),acceptance:"包含设计 §5 稳定选择器；总大小 ≤16KB".into(),depends_on:vec![],review:"approved".into(),reason:None}];
    ProjectWork{schema:"project-work/1".into(),project_id:"cdlw-demo".into(),action_id:"a1".into(),stage:ProjectStage::Develop,document,tasks,target_task:Some("t1".into()),repair_reason:None,repair_round:0,test_spec:None,artifact:None}
}
#[test]
fn gateway_request_supports_project_work_template() {
    let root=tempfile::tempdir().unwrap();
    let mut store=Store::open(&root.path().join("controller.sqlite"),"dispatch-test",&[node()]).unwrap();
    let w=work();
    let expected_doc_sha=w.document.sha256.clone();
    let run=store.dispatch_project(&Actor::Local("owner".into()),&w,100).unwrap();
    store.assign_run_with_worker_artifact(100,&run.id,"B","ab".repeat(32)).unwrap().unwrap();
    let request=GatewayRequest::from_store(&store,&config(),&run.id).unwrap();
    assert_eq!(request.start.payload["template"],"project-work-v1");
    assert_eq!(request.start.payload["project_work"]["project_id"],"cdlw-demo");
    assert_eq!(request.start.payload["project_work"]["document"]["sha256"],json!(expected_doc_sha));
    assert!(serde_json::to_vec(&request.start).unwrap().len()<=60000,"signed start frame must stay under the 64KiB envelope bound");
}
#[test]
fn run_once_accepts_project_work_start_beyond_sum_gate() {
    let root=tempfile::tempdir().unwrap();
    let mut store=Store::open(&root.path().join("controller.sqlite"),"dispatch-test",&[node()]).unwrap();
    let run=store.dispatch_project(&Actor::Local("owner".into()),&work(),100).unwrap();
    store.assign_run_with_worker_artifact(100,&run.id,"B","ab".repeat(32)).unwrap().unwrap();
    let request=GatewayRequest::from_store(&store,&config(),&run.id).unwrap();
    // A transport that returns EOF must NOT fail with "only pinned sum-v1 start
    // supported": the template gate must accept project-work-v1 before transport.
    let result=run_once(&config(),&request,&mut store,&mut NoopTransport,200).unwrap();
    assert_ne!(result.reason.as_deref(),Some("only pinned sum-v1 start supported"));
}
#[test]
fn project_work_start_assigns_to_pinned_worker_with_artifact() {
    // assign_run_with_worker_artifact must route project-work-v1 to the pinned
    // worker exactly like sum-v1 (direct_pinned gate currently keys on sum-v1).
    // healthy=false makes the pinned path the ONLY possible assignment route.
    let root=tempfile::tempdir().unwrap();
    let mut store=Store::open(&root.path().join("controller.sqlite"),"dispatch-test",&[unprobed_node()]).unwrap();
    let run=store.dispatch_project(&Actor::Local("owner".into()),&work(),100).unwrap();
    let assigned=store.assign_run_with_worker_artifact(100,&run.id,"B","ab".repeat(32)).unwrap();
    assert!(assigned.is_some(),"project-work-v1 run must be assignable to the pinned artifact worker");
    let request=GatewayRequest::from_store(&store,&config(),&run.id).unwrap();
    assert_eq!(request.start.payload["worker_artifact_sha256"],"ab".repeat(32));
}
#[test]
fn ssh_manager_open_accepts_project_work_capable_worker() {
    // SshManager::open pins "sum-v1 capability" check; a worker advertising
    // sum-v1 + project-work-v1 must still open (no regression), and a
    // project-work dispatch must reach the transport with the template.
    let root=tempfile::tempdir().unwrap();
    let base=root.path();
    std::fs::write(base.join("known_hosts"),"").unwrap();
    std::fs::write(base.join("worker.key"),"07".repeat(32)).unwrap();
    let text="# 设计\n成都遛娃静态页。".to_string();
    let document=ConfirmedDocument{version:1,sha256:hex::encode(Sha256::digest(text.as_bytes())),text};
    let tasks=vec![WorkTask{id:"t1".into(),title:"实现静态页".into(),description:"生成静态文件".into(),acceptance:"稳定选择器齐全".into(),depends_on:vec![],review:"approved".into(),reason:None}];
    let work=ProjectWork{schema:"project-work/1".into(),project_id:"cdlw-demo".into(),action_id:"a1".into(),stage:ProjectStage::Develop,document,tasks,target_task:Some("t1".into()),repair_reason:None,repair_round:0,test_spec:None,artifact:None};
    serde_json::to_writer_pretty(std::fs::File::create(base.join("controller.json")).unwrap(),&json!({
        "schema":"ao-reason-config/1","mode":"real","deployment":"dispatch-test","role":"controller",
        "state_dir":"state","nodes":[{"id":"B","address":"B","key_file":"worker.key","slots":1,
        "admitted":true,"healthy":true,"templates":["sum-v1","project-work-v1"]}]})).unwrap();
    std::fs::write(base.join("gateway.json"),serde_json::to_string(&json!({
        "schema":"native-ssh-gateway/1","deployment":"dispatch-test","worker_id":"B",
        "host":"b.example","port":22,"user":"worker","known_hosts":"known_hosts",
        "identity_file":"id_ed25519","key_file":"worker.key","controller_config":"controller.json",
        "state_dir":"gateway-state","remote_worker":"/opt/aw/aw-cli","remote_config":"/etc/aw/worker.json",
        "artifact_sha256":"ab".repeat(32),"artifact_bytes":123,"timeout_secs":5})).unwrap()).unwrap();
    let mut manager=ao_reason_workbench::core::ssh_manager::SshManager::open(&base.join("gateway.json")).unwrap();
    let mut capture=CaptureTransport{start_frame:Vec::new()};
    let result=manager.dispatch_project_with_transport(&Actor::Local("owner".into()),&work,&mut capture,100).unwrap();
    assert_eq!(result.status.as_str(),"unknown","EOF transport leaves run unknown, slot retained");
    let parsed:serde_json::Value=serde_json::from_slice(&capture.start_frame).unwrap();
    assert_eq!(parsed["payload"]["template"],"project-work-v1");
    assert_eq!(parsed["payload"]["project_work"]["project_id"],"cdlw-demo");
}
struct CaptureTransport{start_frame:Vec<u8>}
impl WorkerTransport for CaptureTransport {
    fn exchange(&mut self,request:&[u8],_timeout_secs:u64)->anyhow::Result<TransportOutput>{
        self.start_frame=request.to_vec();
        Ok(TransportOutput{stdout:Vec::new(),stderr:Vec::new(),exit_code:Some(0),failure:None})
    }
}
