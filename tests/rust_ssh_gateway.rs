//! Scripted transport only: production Gateway and Store are not replaced.
//! These tests provide no real SSH, Linux, cgroup or GUI evidence.
use anyhow::Result;
use ao_reason_workbench::{
    config::{Actor, NodeSpec},
    core::{
        owner_producer::{CleanupFacts, LaunchFacts, OwnedProcessPort, OwnerLaunchPlan, StreamReadOutcome, TerminalFacts, produce_owner_evidence},
        protocol::{Envelope, PROTOCOL, canonical},
        ssh_gateway::{GatewayLaunchMode, GatewayRequest, SshGatewayConfig, TransportOutput, WorkerTransport, run_once},
        store::{ExitProof, Store},
        worker_owner::{OwnerEvidenceReference, OwnerProcessIdentity, OwnerVerification, owner_binding_sha256, owner_bundle_sha256, owner_report_sha256},
    },
};
use serde_json::json;

const DEPLOYMENT: &str = "gateway-test";
struct Scripted { output: Option<TransportOutput>, calls: usize }
impl WorkerTransport for Scripted {
    fn exchange(&mut self, _: &[u8], _: u64) -> Result<TransportOutput> {
        self.calls += 1;
        Ok(self.output.take().unwrap())
    }
}
fn node() -> NodeSpec {
    NodeSpec { id:"B".into(), address:"B".into(), key_hex:"07".repeat(32), slots:1,
        admitted:true, healthy:true, draining:false, templates:vec!["sum-v1".into()] }
}
fn config() -> SshGatewayConfig {
    serde_json::from_value(json!({"schema":"native-ssh-gateway/1","deployment":DEPLOYMENT,
        "worker_id":"B","host":"b.example","port":22,"user":"worker",
        "known_hosts":"known_hosts","identity_file":"id_ed25519","key_file":"worker.key",
        "controller_config":"controller.json","state_dir":"state","remote_worker":"/opt/aw/aw-cli","remote_config":"/etc/aw/worker.json",
        "artifact_sha256":"ab".repeat(32),"artifact_bytes":123,"timeout_secs":5})).unwrap()
}
fn setup() -> (tempfile::TempDir, Store, GatewayRequest) {
    let root = tempfile::tempdir().unwrap();
    let mut store = Store::open(&root.path().join("controller.sqlite"),DEPLOYMENT,&[node()]).unwrap();
    let run = store.dispatch(&Actor::Local("scripted".into()),"sum-v1",100).unwrap();
    store.assign_run_with_worker_artifact(100,&run.id,"B","ab".repeat(32)).unwrap().unwrap();
    let request = GatewayRequest::from_store(&store,&config(),&run.id).unwrap();
    (root,store,request)
}
struct Port;
impl OwnedProcessPort for Port {
    fn owner_identity(&self) -> Result<OwnerProcessIdentity> { Ok(OwnerProcessIdentity{pid:100,start_ticks:1000}) }
    fn launch(&mut self,_:&OwnerLaunchPlan)->Result<LaunchFacts>{Ok(LaunchFacts{child_pid:200,child_start_ticks:2000,pgid:200})}
    fn wait_terminal(&mut self)->Result<TerminalFacts>{
        let stream=StreamReadOutcome{prefix:vec![],total_read_bytes:0,eof:true,error:None};
        Ok(TerminalFacts{wait_completed:true,wait_method:"child_wait".into(),raw_wait_status:0,stdout:stream.clone(),stderr:stream,
            cleanup:CleanupFacts{proc_scan_complete:true,process_group_scan_complete:true,cgroup_scan_complete:true,process_group_members:0,cgroup_members:0,error:None}})
    }
}
fn event(request:&GatewayRequest,payload:serde_json::Value)->Envelope {
    let start=&request.start;
    let mut event=Envelope{schema:PROTOCOL.into(),message_id:uuid::Uuid::new_v4().to_string(),deployment:DEPLOYMENT.into(),
        from:"B".into(),to:"A".into(),run_id:start.run_id.clone(),attempt:start.attempt,epoch:start.epoch.clone(),
        kind:"report".into(),sent_at:100,expires_at:400,payload,mac:String::new()};
    event.sign(&[7;32]).unwrap();event
}
fn frames(request:&GatewayRequest)->(Envelope,Envelope){
    let plan=OwnerLaunchPlan{execution:request.identity.clone(),node_id:"B".into(),
        instance_id:"123e4567-e89b-42d3-a456-426614174000".into(),boot_id:"123e4567-e89b-12d3-a456-426614174001".into(),request_bytes:canonical(&request.start.payload).unwrap()};
    let production=produce_owner_evidence(&mut Port,&plan).unwrap();
    let launch=event(request,json!({"schema":"owner-evidence-launch/1","phase":"launch","session":request.identity.session,"binding":production.binding}));
    let report=json!({"schema":"scripted-test/1","sum":6});
    let verification=OwnerVerification{passed:true,state:"verified".into(),details:json!({"source":"scripted"})};
    let reference=OwnerEvidenceReference{schema:"owner-evidence-reference/1".into(),binding_sha256:owner_binding_sha256(&production.binding).unwrap(),
        bundle_sha256:owner_bundle_sha256(&production.bundle).unwrap(),record_sha256:production.bundle.record_sha256.clone(),report_sha256:owner_report_sha256(&request.identity,&report,&verification).unwrap()};
    let proof=ExitProof{run_id:request.identity.run_id.clone(),attempt:request.identity.attempt,epoch:request.identity.epoch.clone(),
        session:request.identity.session.clone(),empty:true,observer_alive:true,not_started:false,owner_evidence:Some(reference.clone())};
    let terminal=event(request,json!({"schema":"owner-evidence-report/1","phase":"terminal","session":request.identity.session,
        "report":report,"verification":verification,"owner_evidence":{"reference":reference,"bundle_json":String::from_utf8(production.bundle_bytes().to_vec()).unwrap()},"exit_proof":proof}));
    (launch,terminal)
}
fn transport(events:&[Envelope],failure:Option<&str>)->Scripted {
    let mut stdout=Vec::new();for event in events{stdout.extend(serde_json::to_vec(event).unwrap());stdout.push(b'\n');}
    Scripted{calls:0,output:Some(TransportOutput{stdout,stderr:vec![],exit_code:Some(0),failure:failure.map(str::to_owned)})}
}
#[test]
fn only_signed_m0_terminal_admission_releases_slot(){
    for mode in [GatewayLaunchMode::Direct, GatewayLaunchMode::DelegatedUserService] {
        for path in [None,Some("/home/worker/lib")] {
            let mut config=config();
            config.launch_mode=mode;
            if mode==GatewayLaunchMode::DelegatedUserService { config.expected_worker_timeout_secs=Some(5); config.timeout_secs=35; }
            config.remote_library_path=path.map(str::to_owned);
            if path.is_some() {
                config.remote_library_sha256=Some("cd".repeat(32));
                config.remote_library_bytes=Some(1557000);
            }
            let(_root,mut store,request)=setup();let(l,t)=frames(&request);
            let mut wire=transport(&[l.clone(),t.clone()],None);
            let result=run_once(&config,&request,&mut store,&mut wire,101).unwrap();
            assert_eq!(result.status,"done");assert!(!result.slot_reserved);
            assert_eq!(result.launch.unwrap(),l.payload);assert_eq!(result.terminal.unwrap(),t.payload);
            assert_eq!(store.run(&request.identity.run_id).unwrap().detail["owner_launch"],l.payload,"durable UI facts must be the admitted signed launch");
            assert!(store.assignment(&request.identity.run_id,1).unwrap().unwrap().released);
        }
    }
}
#[test]
fn eof_timeout_and_disconnect_before_terminal_keep_unknown_slot(){
    for mode in [GatewayLaunchMode::Direct, GatewayLaunchMode::DelegatedUserService] {
        for path in [None,Some("/home/worker/lib")] {
            let mut config=config();config.launch_mode=mode;
            if mode==GatewayLaunchMode::DelegatedUserService { config.expected_worker_timeout_secs=Some(5); config.timeout_secs=35; }config.remote_library_path=path.map(str::to_owned);
            if path.is_some() {
                config.remote_library_sha256=Some("cd".repeat(32));
                config.remote_library_bytes=Some(1557000);
            }
            for failure in [None,Some("timeout"),Some("SSH disconnected"),Some("libz.so.1: cannot open shared object file")]{
                let(_root,mut store,request)=setup();let(l,_)=frames(&request);let mut wire=transport(&[l],failure);
                let result=run_once(&config,&request,&mut store,&mut wire,101).unwrap();
                assert_eq!(result.status,"unknown");assert!(result.slot_reserved);
                assert_eq!(store.run(&request.identity.run_id).unwrap().status,"unknown");
                assert!(!store.assignment(&request.identity.run_id,1).unwrap().unwrap().released);
            }
        }
    }
}
#[test]
fn loader_failure_before_any_launch_keeps_unknown_and_reserved(){
    let mut config=config();config.remote_library_path=Some("/home/worker/lib".into());
    config.remote_library_sha256=Some("cd".repeat(32));
    config.remote_library_bytes=Some(1557000);
    let(_root,mut store,request)=setup();
    let mut wire=transport(&[],Some("libz.so.1: cannot open shared object file"));
    let result=run_once(&config,&request,&mut store,&mut wire,101).unwrap();
    assert_eq!(result.status,"unknown");assert!(result.slot_reserved);
    assert!(result.launch.is_none()&&result.terminal.is_none());
    assert!(result.reason.unwrap().contains("libz.so.1"));
    assert!(!store.assignment(&request.identity.run_id,1).unwrap().unwrap().released);
}
#[test]
fn identical_duplicate_is_idempotent_and_post_terminal_close_preserves_done(){
    let(_root,mut store,request)=setup();let(l,t)=frames(&request);
    let mut wire=transport(&[l.clone(),l,t.clone(),t],Some("SSH closed after terminal"));
    let result=run_once(&config(),&request,&mut store,&mut wire,101).unwrap();
    assert_eq!(result.status,"done");assert!(!result.slot_reserved);
    let mut no_second_spawn=transport(&[],Some("must not be called"));
    assert_eq!(run_once(&config(),&request,&mut store,&mut no_second_spawn,102).unwrap().status,"done");
    assert_eq!(no_second_spawn.calls,0);
}
#[test]
fn conflicting_launch_retains_first_binding_and_reservation(){
    let(_root,mut store,request)=setup();let(l,t)=frames(&request);let mut conflict=l.clone();
    conflict.message_id=uuid::Uuid::new_v4().to_string();conflict.payload["binding"]["process"]["start_ticks"]=json!(9999);conflict.sign(&[7;32]).unwrap();
    let mut wire=transport(&[l,conflict,t],None);
    let result=run_once(&config(),&request,&mut store,&mut wire,101).unwrap();
    assert_eq!(result.status,"unknown");assert!(result.slot_reserved);
    assert!(result.reason.unwrap().contains("conflict"));
}
#[test]
fn untrusted_json_success_never_releases_slot(){
    let(_root,mut store,request)=setup();let(l,mut t)=frames(&request);t.payload["verification"]["passed"]=json!(true);t.mac="00".repeat(32);
    let mut wire=transport(&[l,t],None);
    let result=run_once(&config(),&request,&mut store,&mut wire,101).unwrap();
    assert_eq!(result.status,"unknown");assert!(result.slot_reserved);
}
#[test]
fn admitted_execution_rejects_a_different_session_without_fast_path_success(){
    let(_root,mut store,request)=setup();let(l,t)=frames(&request);
    let mut wire=transport(&[l,t],None);
    assert_eq!(run_once(&config(),&request,&mut store,&mut wire,101).unwrap().status,"done");
    let mut changed=request.clone();changed.identity.session=Some("different-session".into());
    let mut wire=transport(&[],None);
    assert!(run_once(&config(),&changed,&mut store,&mut wire,102).is_err(),"admitted fast path must fence complete identity");
}
#[test]
fn worker_artifact_mismatch_is_rejected_before_transport(){
    let root=tempfile::tempdir().unwrap();
    let mut store=Store::open(&root.path().join("controller.sqlite"),DEPLOYMENT,&[node()]).unwrap();
    let run=store.dispatch(&Actor::Local("scripted".into()),"sum-v1",100).unwrap();
    store.assign_run_with_worker_artifact(100,&run.id,"B","cd".repeat(32)).unwrap().unwrap();
    let request=GatewayRequest::from_store(&store,&config(),&run.id).unwrap();
    let mut wire=transport(&[],None);
    assert!(run_once(&config(),&request,&mut store,&mut wire,101).is_err(),"gateway must fence a request for a different worker artifact");
    assert_eq!(wire.calls,0,"artifact identity must be checked before transport");
}

#[test]
fn foreign_deployment_is_rejected_before_transport(){
    let(_root,mut store,mut request)=setup();request.identity.deployment="foreign".into();let mut wire=transport(&[],None);
    assert!(run_once(&config(),&request,&mut store,&mut wire,101).is_err());assert_eq!(wire.calls,0);
}
