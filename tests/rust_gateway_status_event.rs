//! Scripted transport only: production Gateway and Store are not replaced.
//! These tests provide no real SSH, Linux, cgroup or GUI evidence.
//! RED first: the worker's signed kind="status" event (the only channel the
//! real Linux worker has to report a NOT_READY reason before dying) is
//! currently rejected by consume_events as "worker session/kind mismatch",
//! destroying the real failure cause in the controller ledger. It must be
//! accepted and persisted so the true reason reaches the controller Store.
use anyhow::Result;
use ao_reason_workbench::{
    config::{Actor, NodeSpec},
    core::{
        owner_producer::{CleanupFacts, LaunchFacts, OwnedProcessPort, OwnerLaunchPlan, StreamReadOutcome, TerminalFacts, produce_owner_evidence},
        protocol::{Envelope, PROTOCOL, canonical},
        ssh_gateway::{GatewayRequest, SshGatewayConfig, TransportOutput, WorkerTransport, run_once},
        store::{ExitProof, Store},
        worker_owner::{OwnerEvidenceReference, OwnerProcessIdentity, OwnerVerification, owner_binding_sha256, owner_bundle_sha256, owner_report_sha256},
    },
};
use serde_json::json;

const DEPLOYMENT: &str = "gateway-status-test";
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
fn event(request:&GatewayRequest,kind:&str,payload:serde_json::Value)->Envelope {
    let start=&request.start;
    let mut event=Envelope{schema:PROTOCOL.into(),message_id:uuid::Uuid::new_v4().to_string(),deployment:DEPLOYMENT.into(),
        from:"B".into(),to:"A".into(),run_id:start.run_id.clone(),attempt:start.attempt,epoch:start.epoch.clone(),
        kind:kind.into(),sent_at:100,expires_at:400,payload,mac:String::new()};
    event.sign(&[7;32]).unwrap();event
}
fn frames(request:&GatewayRequest)->(Envelope,Envelope){
    let plan=OwnerLaunchPlan{execution:request.identity.clone(),node_id:"B".into(),
        instance_id:"123e4567-e89b-42d3-a456-426614174000".into(),boot_id:"123e4567-e89b-12d3-a456-426614174001".into(),request_bytes:canonical(&request.start.payload).unwrap()};
    let production=produce_owner_evidence(&mut Port,&plan).unwrap();
    let launch=event(request,"report",json!({"schema":"owner-evidence-launch/1","phase":"launch","session":request.identity.session,"binding":production.binding}));
    let report=json!({"schema":"scripted-test/1","sum":6});
    let verification=OwnerVerification{passed:true,state:"verified".into(),details:json!({"source":"scripted"})};
    let reference=OwnerEvidenceReference{schema:"owner-evidence-reference/1".into(),binding_sha256:owner_binding_sha256(&production.binding).unwrap(),
        bundle_sha256:owner_bundle_sha256(&production.bundle).unwrap(),record_sha256:production.bundle.record_sha256.clone(),report_sha256:owner_report_sha256(&request.identity,&report,&verification).unwrap()};
    let proof=ExitProof{run_id:request.identity.run_id.clone(),attempt:request.identity.attempt,epoch:request.identity.epoch.clone(),
        session:request.identity.session.clone(),empty:true,observer_alive:true,not_started:false,owner_evidence:Some(reference.clone())};
    let terminal=event(request,"report",json!({"schema":"owner-evidence-report/1","phase":"terminal","session":request.identity.session,
        "report":report,"verification":verification,"owner_evidence":{"reference":reference,"bundle_json":String::from_utf8(production.bundle_bytes().to_vec()).unwrap()},"exit_proof":proof}));
    (launch,terminal)
}
fn transport(events:&[Envelope],failure:Option<&str>)->Scripted {
    let mut stdout=Vec::new();for event in events{stdout.extend(serde_json::to_vec(event).unwrap());stdout.push(b'\n');}
    Scripted{calls:0,output:Some(TransportOutput{stdout,stderr:vec![],exit_code:Some(2),failure:failure.map(str::to_owned)})}
}
fn signed_status(request:&GatewayRequest,reason:&str)->Envelope {
    // Matches the real Linux worker's native_unknown status payload:
    // state="unknown" plus the failure reason string.
    event(request,"status",json!({"schema":"worker-status/1","phase":"status","session":request.identity.session,"state":"unknown","reason":reason}))
}

#[test]
fn signed_worker_status_event_is_accepted_and_persists_the_reason() {
    let(_root,mut store,request)=setup();
    let status=signed_status(&request,"native owner UNKNOWN: NOT_READY: owner/child birth cgroup mismatch");
    let mut wire=transport(&[status],Some("transport exited 2"));
    let result=run_once(&config(),&request,&mut store,&mut wire,101).unwrap();
    assert_eq!(result.status,"unknown","status alone must not admit");
    assert!(result.slot_reserved,"no terminal evidence: slot stays reserved");
    let reason=result.reason.expect("reason must be present");
    assert!(reason.contains("birth cgroup mismatch"),"the real worker reason must surface in the gateway result, got: {reason}");
}

#[test]
fn status_then_launch_then_terminal_still_admits() {
    let(_root,mut store,request)=setup();
    let status=signed_status(&request,"worker progress note");
    let(l,t)=frames(&request);
    let mut wire=transport(&[status,l,t],None);
    let result=run_once(&config(),&request,&mut store,&mut wire,101).unwrap();
    assert_eq!(result.status,"done");
    assert!(!result.slot_reserved);
}

#[test]
fn unsigned_or_foreign_session_status_is_still_rejected() {
    let(_root,mut store,request)=setup();
    let mut foreign=signed_status(&request,"x");foreign.payload["session"]=json!("other-session");foreign.sign(&[7;32]).unwrap();
    let mut wire=transport(&[foreign],None);
    let result=run_once(&config(),&request,&mut store,&mut wire,101).unwrap();
    assert_eq!(result.status,"unknown");
    let reason=result.reason.unwrap();
    assert!(reason.contains("mismatch")||reason.contains("rejected"),"foreign session status must be rejected, got: {reason}");
}
