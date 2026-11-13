//! Scripted transport only: production Gateway and Store are not replaced.
//! RED first (linux221 session -30, run 50252bf3): the real Linux worker
//! finished inside the 300s exchange window — launch sent_at=1794489910,
//! terminal sent_at=1794490018, both well inside expires_at — but the
//! controller consumed events with the frozen dispatch-time `now`
//! (1794489901), so protocol.rs envelope verification rejected the
//! terminal with "invalid envelope time" (sent_at > now+45 by 72s) and
//! the finished run was marked gateway-unknown with the slot held.
//! The exchange deadline governs how long we wait; envelope freshness
//! must be checked at consumption time (arrival), not at dispatch time.
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

const DEPLOYMENT: &str = "gateway-late-terminal-test";
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
        "artifact_sha256":"ab".repeat(32),"artifact_bytes":123,"timeout_secs":300})).unwrap()
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
fn event_at(request:&GatewayRequest,kind:&str,payload:serde_json::Value,sent_at:u64)->Envelope {
    let start=&request.start;
    let mut event=Envelope{schema:PROTOCOL.into(),message_id:uuid::Uuid::new_v4().to_string(),deployment:DEPLOYMENT.into(),
        from:"B".into(),to:"A".into(),run_id:start.run_id.clone(),attempt:start.attempt,epoch:start.epoch.clone(),
        kind:kind.into(),sent_at,expires_at:sent_at+300,payload,mac:String::new()};
    event.sign(&[7;32]).unwrap();event
}
fn frames_at(request:&GatewayRequest,launch_sent_at:u64,terminal_sent_at:u64)->(Envelope,Envelope){
    let plan=OwnerLaunchPlan{execution:request.identity.clone(),node_id:"B".into(),
        instance_id:"123e4567-e89b-42d3-a456-426614174000".into(),boot_id:"123e4567-e89b-12d3-a456-426614174001".into(),request_bytes:canonical(&request.start.payload).unwrap()};
    let production=produce_owner_evidence(&mut Port,&plan).unwrap();
    let launch=event_at(request,"report",json!({"schema":"owner-evidence-launch/1","phase":"launch","session":request.identity.session,"binding":production.binding}),launch_sent_at);
    let report=json!({"schema":"scripted-test/1","sum":6});
    let verification=OwnerVerification{passed:true,state:"verified".into(),details:json!({"source":"scripted"})};
    let reference=OwnerEvidenceReference{schema:"owner-evidence-reference/1".into(),binding_sha256:owner_binding_sha256(&production.binding).unwrap(),
        bundle_sha256:owner_bundle_sha256(&production.bundle).unwrap(),record_sha256:production.bundle.record_sha256.clone(),report_sha256:owner_report_sha256(&request.identity,&report,&verification).unwrap()};
    let proof=ExitProof{run_id:request.identity.run_id.clone(),attempt:request.identity.attempt,epoch:request.identity.epoch.clone(),
        session:request.identity.session.clone(),empty:true,observer_alive:true,not_started:false,owner_evidence:Some(reference.clone())};
    let terminal=event_at(request,"report",json!({"schema":"owner-evidence-report/1","phase":"terminal","session":request.identity.session,
        "report":report,"verification":verification,"owner_evidence":{"reference":reference,"bundle_json":String::from_utf8(production.bundle_bytes().to_vec()).unwrap()},"exit_proof":proof}),terminal_sent_at);
    (launch,terminal)
}
fn transport(events:&[Envelope],failure:Option<&str>)->Scripted {
    let mut stdout=Vec::new();for event in events{stdout.extend(serde_json::to_vec(event).unwrap());stdout.push(b'\n');}
    Scripted{calls:0,output:Some(TransportOutput{stdout,stderr:vec![],exit_code:Some(0),failure:failure.map(str::to_owned)})}
}

#[test]
fn terminal_arriving_after_dispatch_now_plus_45s_still_admits() {
    let(_root,mut store,request)=setup();
    // Real-world shape from run 50252bf3: dispatch now=100, launch arrives
    // +9s, the model call takes ~117s, terminal is signed at t=217
    // (dispatch_now+117 > dispatch_now+45) and expires at t=517 — all
    // inside the 300s exchange deadline. Consumption-time freshness must
    // accept it; the frozen dispatch-time `now` must not reject it.
    let(l,t)=frames_at(&request,109,217);
    let mut wire=transport(&[l,t],None);
    let result=run_once(&config(),&request,&mut store,&mut wire,100).unwrap();
    assert_eq!(result.status,"done","terminal within the exchange window must admit, got reason: {:?}",
        result.reason);
    assert!(!result.slot_reserved,"admitted terminal must release the slot");
}

#[test]
fn launch_and_terminal_beyond_expiry_window_are_still_rejected() {
    let(_root,mut store,request)=setup();
    // Expiry stays strict: a terminal signed so early that it already
    // expired at consumption time (expires_at <= now) must still be
    // rejected — freshness-at-arrival does not mean accepting stale
    // envelopes.
    let(l,mut t)=frames_at(&request,109,1000);
    t.sign(&[7;32]).unwrap();
    let mut wire=transport(&[l,t],None);
    let result=run_once(&config(),&request,&mut store,&mut wire,100).unwrap();
    assert_eq!(result.status,"unknown");
    assert!(result.slot_reserved,"expired terminal must retain the slot");
    assert!(result.reason.unwrap().contains("invalid envelope time"),"must reject expired envelope");
}
