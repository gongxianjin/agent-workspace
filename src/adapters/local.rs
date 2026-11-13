//! Explicit local fixture. Never used as fallback for real AO/Reason.
use anyhow::{Result,Context,ensure};
use std::{path::{Path,PathBuf},collections::BTreeMap,io::Read};
use serde_json::json;
use sha2::{Digest,Sha256};
use crate::core::{relay_runtime::{ControllerRunnerRelay,LocalRelayRuntime},runner::Runner,store::{ExitProof,Store}};
use super::{AoPort,Observation,WorkRequest,process::{ChildEvidence,ManagedChild}};
struct Worker {child:ManagedChild,request:WorkRequest,artifact:PathBuf,decision:Option<String>}
pub struct LocalAo {worker:PathBuf,state:PathBuf,workers:BTreeMap<String,Worker>,creates:usize}
impl LocalAo {
 pub fn new(worker:&Path,state:&Path)->Result<Self>{ensure!(worker.is_file(),"bundled aw-cli missing");std::fs::create_dir_all(state)?;Ok(Self{worker:worker.to_owned(),state:state.to_owned(),workers:BTreeMap::new(),creates:0})}
 pub fn create_count(&self)->usize{self.creates}
 pub fn live_owned_processes(&mut self)->Result<u32>{let mut count=0;for w in self.workers.values_mut(){count+=w.child.active()?;}Ok(count)}
}
fn exit_proof(request:&WorkRequest)->ExitProof {ExitProof{run_id:request.run_id.clone(),attempt:request.attempt,epoch:request.epoch.clone(),session:Some(request.session.clone()),empty:true,observer_alive:true,not_started:false,owner_evidence:None}}
impl AoPort for LocalAo {
 fn create(&mut self,request:&WorkRequest)->Result<String>{
  ensure!(!self.workers.contains_key(&request.session),"duplicate create; must reconcile instead");
  ensure!(crate::config::valid_id(&request.session),"invalid local session");
  let dir=self.state.join(&request.session);std::fs::create_dir(&dir)?;let artifact=dir.join("result.json");
  let child=ManagedChild::spawn(&self.worker,&request.template,&artifact,&request.session)?;
  self.workers.insert(request.session.clone(),Worker{child,request:request.clone(),artifact,decision:None});self.creates+=1;Ok(request.session.clone())
 }
 fn observe(&mut self,session:&str)->Result<Observation>{
  let Some(w)=self.workers.get_mut(session) else{return Ok(Observation::Unknown("fixture observer handle unavailable; no exit proof".into()))};
  let active=w.child.active()?;
  let evidence=if active==0{w.child.evidence()?}else{None};
  let durable=match evidence.as_ref(){
   Some(evidence)=>match retain_evidence(w,evidence){Ok(value)=>Some(value),Err(error)=>return Ok(Observation::Unknown(format!("fixture evidence retention unavailable: {error}")))},
   None=>None,
  };
  if let Some(evidence)=evidence.as_ref(){
   if evidence.stdout_error.is_some() || evidence.stderr_error.is_some() {
    return Ok(Observation::Unknown("fixture output reader error; prefix and exit retained; no completion".into()));
   }
  }
  if active==0&&evidence.is_none(){return Ok(Observation::Unknown("fixture exit/output not yet completely observed; retained reservation".into()))}
  if !w.artifact.exists(){return if let Some(durable)=durable{let evidence=evidence.as_ref().context("fixture exit evidence missing")?;let report=json!({"schema":"native-worker-failure/1","session":session,"reason":"worker exited without report"});let verification=failure_verification(evidence,durable);Ok(Observation::ExitProof{proof:exit_proof(&w.request),report,verification})}else{Ok(Observation::Running)}}
  let mut raw=Vec::new();std::fs::File::open(&w.artifact)?.take(16385).read_to_end(&mut raw)?;ensure!(raw.len()<=16384,"fixture artifact exceeds bound");
  let value:serde_json::Value=match serde_json::from_slice(&raw){Ok(v)=>v,Err(_) if active>0=>return Ok(Observation::Running),Err(e)=>return Err(e.into())};
  ensure!(value["session"].as_str()==Some(session),"artifact session mismatch");
  if w.request.template=="sum-native-v1" {
   if active>0{return Ok(Observation::Running)}
   let evidence=evidence.as_ref().context("fixture exit evidence missing")?;
   let mut verification=failure_verification(evidence,durable.context("durable fixture evidence missing")?);
   // A valid artifact alone never overrides a nonzero or nonnumeric child exit.
   verification["passed"]=json!(evidence.exit_code==Some(0)&&value==json!({"schema":"native-sum/1","session":session,"inputs":[1,2,3],"sum":6}));
   verification["source"]=json!("native-verifier");verification["contract"]=json!("sum-native-v1");
   verification["artifact_sha256"]=json!(hex::encode(Sha256::digest(&raw)));verification["expected_sum"]=json!(6);
   Ok(Observation::ExitProof{proof:exit_proof(&w.request),report:value,verification})
  }else if active==0 {let evidence=evidence.as_ref().context("fixture exit evidence missing")?;let verification=failure_verification(evidence,durable.context("durable fixture evidence missing")?);Ok(Observation::ExitProof{proof:exit_proof(&w.request),report:value,verification})}
  else if w.decision.is_some(){Ok(Observation::Running)}
  else{Ok(Observation::Approval(json!({"request_id":"local-once","generation":1,"scope_hash":hex::encode(Sha256::digest(session.as_bytes())),"offered":["allow_once","reject_once"],"summary":"模拟：只允许本次固定等待任务；无外部模型"})))}
 }
 fn resolve(&mut self,session:&str,request:&str,decision:&str)->Result<()>{let w=self.workers.get_mut(session).context("fixture observer missing")?;ensure!(request=="local-once"&&matches!(decision,"allow_once"|"reject_once")&&w.decision.is_none(),"invalid/repeated local decision");w.decision=Some(decision.into());if decision=="reject_once"{w.child.stop()?;}Ok(())}
 fn kill(&mut self,session:&str)->Result<()>{self.workers.get_mut(session).context("fixture observer missing; no exit proof")?.child.stop()}
}
// Files are written and synced before a completed observation can enter the outbox.
// Existing evidence is immutable and must match exactly on repeated observation.
fn retain_file(path:&Path,raw:&[u8])->Result<()> {
 use std::io::Write;
 match std::fs::OpenOptions::new().write(true).create_new(true).open(path) {
  Ok(mut file)=>{file.write_all(raw)?;#[cfg(test)] test_sync::before_sync(path)?;file.sync_all()?;},
  Err(error) if error.kind()==std::io::ErrorKind::AlreadyExists=>{
   ensure!(std::fs::read(path)?==raw,"fixture evidence changed or incomplete; retained reservation");
   let file=std::fs::OpenOptions::new().write(true).open(path)?;
   #[cfg(test)] test_sync::before_sync(path)?;
   file.sync_all()?;
  },
  Err(error)=>return Err(error.into()),
 }
 Ok(())
}
fn retain_evidence(worker:&Worker,evidence:&ChildEvidence)->Result<serde_json::Value> {
 let dir=worker.artifact.parent().context("fixture evidence directory missing")?;
 let stdout=dir.join("stdout.bin");let stderr=dir.join("stderr.bin");let status=dir.join("status.json");
 retain_file(&stdout,&evidence.stdout)?;retain_file(&stderr,&evidence.stderr)?;
 let metadata=json!({"schema":"native-child-status/1","run_id":worker.request.run_id,"attempt":worker.request.attempt,"epoch":worker.request.epoch,"session":worker.request.session,"wait_completed":true,"exit_code":evidence.exit_code,"stdout_error":evidence.stdout_error,"stderr_error":evidence.stderr_error,
  "stdout":{"path":stdout,"bytes":evidence.stdout.len(),"total_bytes":evidence.stdout_total_bytes,"sha256":hex::encode(Sha256::digest(&evidence.stdout)),"truncated":evidence.stdout_truncated},
  "stderr":{"path":stderr,"bytes":evidence.stderr.len(),"total_bytes":evidence.stderr_total_bytes,"sha256":hex::encode(Sha256::digest(&evidence.stderr)),"truncated":evidence.stderr_truncated}});
 retain_file(&status,&serde_json::to_vec(&metadata)?)?;
 Ok(json!({"status_path":status,"status":metadata}))
}
fn failure_verification(evidence:&ChildEvidence,durable:serde_json::Value)->serde_json::Value {
 json!({"passed":false,"source":"native-process-observer","child_exit_code":evidence.exit_code,"child_stdout":String::from_utf8_lossy(&evidence.stdout),"child_stderr":String::from_utf8_lossy(&evidence.stderr),"child_stdout_bytes":evidence.stdout.len(),"child_stderr_bytes":evidence.stderr.len(),"child_stdout_truncated":evidence.stdout_truncated,"child_stderr_truncated":evidence.stderr_truncated,"child_evidence":durable})
}
/// LocalReason durable-accept/ack transport: the same signed envelopes and separate ledgers.
pub fn pump_local(c:&mut Store,r:&mut Runner,ao:&mut LocalAo,now:u64)->Result<()>{
 let mut relay=LocalRelayRuntime::for_runner(r)?;
 pump_local_with_relay(c,r,ao,&mut relay,now)
}

pub fn pump_local_with_relay(c:&mut Store,r:&mut Runner,ao:&mut LocalAo,relay:&mut impl ControllerRunnerRelay,now:u64)->Result<()>{
 c.expire(now)?;c.assign_next(now)?;
 relay.forward_controller_outbox(c,r,now)?;
 r.tick(ao,now)?;
 for message in r.pending_outbox(32)?{c.receive(&message,now)?;r.acknowledge_outbox(&message.message_id)?;}
 Ok(())
}

#[cfg(test)]
mod test_sync {
 use std::{cell::RefCell,path::Path};
 thread_local! {static FAULT:RefCell<Option<String>>=const{RefCell::new(None)};}
 pub fn fail(name:Option<&str>){FAULT.with(|f|*f.borrow_mut()=name.map(str::to_owned));}
 pub fn before_sync(path:&Path)->std::io::Result<()>{FAULT.with(|f|if f.borrow().as_deref().is_some_and(|name|path.file_name().and_then(|n|n.to_str())==Some(name)){Err(std::io::Error::other("injected post-write sync failure"))}else{Ok(())})}
}
#[cfg(test)]
#[path="worker_fault_cases.rs"]
mod worker_fault_cases;
