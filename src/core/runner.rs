//! Separate node-local durable execution ledger. No Controller DB shared state shortcut.
use anyhow::{Result,Context,ensure};
use rusqlite::{Connection,params,OptionalExtension,TransactionBehavior};
use serde::{Serialize,Deserialize};
use serde_json::{Value,json};
use std::path::Path;
use sha2::{Digest,Sha256};
use crate::{config::NodeSpec,adapters::{AoPort,Observation,WorkRequest}};
use super::{observer::{ObserverEvent,RunnerObserver},protocol::{Envelope,ExecutionIdentity,PROTOCOL},relay::RelayBridge,store::ExitProof};
#[derive(Clone,Debug,Serialize,Deserialize)]
struct Execution {
 run_id:String,attempt:u32,epoch:String,template:String,deadline:u64,
 #[serde(default)] identity:Option<ExecutionIdentity>,
 #[serde(default)] project_work:Option<super::project_work::ProjectWork>,
 state:String,claimed:bool,session:Option<String>,reason:Option<String>,
 approval:Option<Value>,decision:Option<Value>,resolve_claimed:bool,finished:bool,
}
pub struct Runner {db:Connection,deployment:String,node:NodeSpec}
fn read(db:&Connection,id:&str,attempt:u32)->Result<Execution>{let raw:String=db.query_row("SELECT body FROM executions WHERE run=?1 AND attempt=?2",params![id,attempt],|r|r.get(0))?;Ok(serde_json::from_str(&raw)?)}
fn save(db:&Connection,e:&Execution)->Result<()>{db.execute("UPDATE executions SET body=?1 WHERE run=?2 AND attempt=?3",params![serde_json::to_string(e)?,e.run_id,e.attempt])?;Ok(())}
fn emit(db:&Connection,deployment:&str,node:&NodeSpec,e:&Execution,kind:&str,payload:Value,now:u64)->Result<()>{
 let mut m=Envelope{schema:PROTOCOL.into(),message_id:uuid::Uuid::new_v4().to_string(),deployment:deployment.into(),from:node.id.clone(),to:"A".into(),run_id:e.run_id.clone(),attempt:e.attempt,epoch:e.epoch.clone(),kind:kind.into(),sent_at:now,expires_at:now.saturating_add(300),payload,mac:String::new()};m.sign(&hex::decode(&node.key_hex)?)?;db.execute("INSERT INTO outbox(id,body) VALUES(?1,?2)",params![m.message_id,serde_json::to_string(&m)?])?;Ok(())
}
fn matches_exit_proof(e:&Execution,session:&str,proof:&ExitProof)->bool {
 proof.run_id==e.run_id && proof.attempt==e.attempt && proof.epoch==e.epoch
  && proof.session.as_deref()==Some(session) && proof.empty && proof.observer_alive && !proof.not_started
}
impl Runner {
 pub fn open(path:&Path,deployment:&str,node:NodeSpec)->Result<Self>{
  node.validate()?;ensure!(crate::config::valid_id(deployment),"invalid deployment");
  if let Some(parent)=path.parent(){std::fs::create_dir_all(parent)?;}
  let mut db=Connection::open(path)?;db.busy_timeout(std::time::Duration::from_secs(3))?;
  let version:i64=db.pragma_query_value(None,"user_version",|r|r.get(0))?;
  ensure!(version<=1,"future Runner schema");
  if version==0 {let tables:i64=db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0))?;ensure!(tables==0,"legacy/non-Runner database");}
  db.pragma_update(None,"journal_mode","WAL")?;db.pragma_update(None,"synchronous","FULL")?;
  let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
  tx.execute_batch("CREATE TABLE IF NOT EXISTS identity(pin TEXT NOT NULL);
   CREATE TABLE IF NOT EXISTS executions(run TEXT NOT NULL,attempt INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(run,attempt));
   CREATE TABLE IF NOT EXISTS inbox(id TEXT PRIMARY KEY,body TEXT NOT NULL);
   CREATE TABLE IF NOT EXISTS outbox(id TEXT PRIMARY KEY,body TEXT NOT NULL,sent INTEGER NOT NULL DEFAULT 0);")?;
  let pin=json!({"schema":"native-runner/1","deployment":deployment,"node":node.id,"address":node.address,"key_fingerprint":hex::encode(Sha256::digest(hex::decode(&node.key_hex)?))}).to_string();
  let old:Option<String>=tx.query_row("SELECT pin FROM identity",[],|r|r.get(0)).optional()?;
  if let Some(old)=old{ensure!(old==pin,"Runner identity changed");}else{ensure!(version==0,"missing Runner identity");tx.execute("INSERT INTO identity VALUES(?1)",[pin])?;}
  tx.pragma_update(None,"user_version",1)?;tx.commit()?;Ok(Self{db,deployment:deployment.into(),node})
 }
 pub fn address(&self)->&str{&self.node.address}
 pub fn claim_native(&mut self,m:&Envelope,now:u64)->Result<Option<ExecutionIdentity>>{
  let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
  let mut e=read(&tx,&m.run_id,m.attempt)?;
  if e.finished || e.claimed { return Ok(None); }
  ensure!(e.epoch==m.epoch && matches!(e.template.as_str(),"sum-v1"|"project-work-v1"), "native execution mismatch");
  e.claimed=true;e.session=m.payload["session"].as_str().map(str::to_owned).or_else(||Some(format!("native-{}",m.message_id)));
  let artifact=m.payload["worker_artifact_sha256"].as_str().context("native worker artifact SHA missing")?;
  ensure!(artifact.len()==64&&artifact.bytes().all(|b|b.is_ascii_hexdigit()),"native worker artifact SHA invalid");
  // For project-work-v1 the controller admits the owner launch only when the
  // worker's identity equals its from_detail reconstruction, which binds
  // source_sha256 = project_work.document.sha256. Claim must carry it.
  let source=e.project_work.as_ref().map(|w|w.document.sha256.clone());
  e.state="running".into();e.identity=Some(ExecutionIdentity::new(&self.deployment,&e.run_id,e.attempt,&e.epoch,e.session.as_deref(),source.as_deref(),Some(artifact)));
  let identity=e.identity.clone().context("native execution identity missing")?;save(&tx,&e)?;tx.commit()?;let _=now;Ok(Some(identity))
 }
 pub fn project_work(&self,identity:&ExecutionIdentity)->Result<super::project_work::ProjectWork> {
  let e=read(&self.db,&identity.run_id,identity.attempt)?;
  ensure!(e.identity.as_ref()==Some(identity),"project work identity mismatch");
  e.project_work.context("project work missing for execution")
 }
 pub fn native_launch(&mut self,binding:&crate::core::worker_owner::ExpectedOwnerBinding,now:u64)->Result<()> {
  let e=read(&self.db,&binding.execution.run_id,binding.execution.attempt)?;
  ensure!(e.identity.as_ref()==Some(&binding.execution),"native launch identity mismatch");
  let mut m=Envelope{schema:PROTOCOL.into(),message_id:uuid::Uuid::new_v4().to_string(),deployment:self.deployment.clone(),from:self.node.id.clone(),to:"A".into(),run_id:e.run_id.clone(),attempt:e.attempt,epoch:e.epoch.clone(),kind:"report".into(),sent_at:now,expires_at:now.saturating_add(300),payload:serde_json::json!({"schema":"owner-evidence-launch/1","phase":"launch","session":binding.execution.session,"binding":binding}),mac:String::new()};m.sign(&hex::decode(&self.node.key_hex)?)?;self.db.execute("INSERT INTO outbox(id,body) VALUES(?1,?2)",params![m.message_id,serde_json::to_string(&m)?])?;Ok(())
 }
 pub fn native_terminal(&mut self,production:&crate::core::owner_producer::OwnerProduction,now:u64)->Result<()> {
  self.native_terminal_for(production,None,now)
 }
 /// Terminal admission with an optional managed run workspace: for
 /// project-work-v1 the child's stdout manifest is cross-checked against the
 /// actual files on disk and the artifact package rides inside the report.
 pub fn native_terminal_for(&mut self,production:&crate::core::owner_producer::OwnerProduction,workspace:Option<&std::path::Path>,now:u64)->Result<()> {
  let execution=&production.binding.execution;
  let raw=hex::decode(&production.bundle.stdout_prefix_hex)?;
  // A non-JSON stdout means the child died before printing its report; the
  // retained stderr prefix is the only diagnostic (linux221 run 1152214d:
  // NotValidForName was only recoverable via remote forensics). Ride it
  // along so native_unknown persists the real cause.
  let report:Value=serde_json::from_slice(&raw).map_err(|parse| {
    let stderr=hex::decode(&production.bundle.stderr_prefix_hex).unwrap_or_default();
    let prefix=String::from_utf8_lossy(&stderr);
    let trimmed=prefix.trim_end();
    anyhow::Error::new(parse).context(if trimmed.is_empty() {
      "owner stdout is not a JSON report (child stderr empty)".to_owned()
    } else {
      format!("owner stdout is not a JSON report (child stderr: {trimmed})")
    })
  })?;
  let template=read(&self.db,&execution.run_id,execution.attempt)?.template;
  let (report,verification)=match template.as_str() {
   "sum-v1"=>{
    ensure!(report["schema"]=="native-sum/1"&&report["session"]==execution.session.clone().unwrap_or_default(),"owner business report identity mismatch");
    ensure!(report["inputs"]==json!([1,2,3])&&report["sum"]==6,"owner sum-v1 report failed independent verification");
    let artifact=execution.artifact_sha256.clone().context("worker artifact SHA missing from execution identity")?;
    (report.clone(),serde_json::json!({"passed":true,"state":"verified","details":{"source":"native-sum-verifier","artifact_sha256":artifact}}))
   }
   "project-work-v1"=>{
    let workspace=workspace.context("project terminal admission requires the managed run workspace")?;
    let work=self.project_work(execution)?;
    ensure!(report["schema"]=="native-project/1"&&report["session"]==execution.session.clone().unwrap_or_default(),"owner project report identity mismatch");
    ensure!(report["project_id"]==work.project_id&&report["document_sha256"]==work.document.sha256,"owner project report document binding mismatch");
    let manifest=&report["files"];
    let package=super::owner_project::package_workspace_files(manifest,workspace,&work)?;
    let files_verified=package["files"].as_array().map(|f|f.len()).unwrap_or(0);
    let mut enriched=report.clone();
    enriched["artifact"]=package;
    (enriched,serde_json::json!({"passed":true,"state":"verified","details":{"source":"native-project-verifier","document_sha256":work.document.sha256,"files_verified":files_verified}}))
   }
   other=>anyhow::bail!("unsupported native template {other}"),
  };
  ensure!(production.bundle.record.direct_wait.exit_code==Some(0)&&production.bundle.record.direct_wait.termination_signal.is_none(),"owner terminal exit is not successful");
  let typed_verification:crate::core::worker_owner::OwnerVerification=serde_json::from_value(verification.clone())?;
  let reference=crate::core::worker_owner::OwnerEvidenceReference{schema:"owner-evidence-reference/1".into(),binding_sha256:crate::core::worker_owner::owner_binding_sha256(&production.binding)?,bundle_sha256:crate::core::worker_owner::owner_bundle_sha256(&production.bundle)?,record_sha256:production.bundle.record_sha256.clone(),report_sha256:crate::core::worker_owner::owner_report_sha256(execution,&report,&typed_verification)?};
  let proof=crate::core::store::ExitProof{run_id:execution.run_id.clone(),attempt:execution.attempt,epoch:execution.epoch.clone(),session:execution.session.clone(),empty:true,observer_alive:true,not_started:false,owner_evidence:Some(reference.clone())};
  let payload=serde_json::json!({"schema":"owner-evidence-report/1","phase":"terminal","session":execution.session,"report":report,"verification":verification,"owner_evidence":{"reference":reference,"bundle_json":String::from_utf8(production.bundle_bytes().to_vec())?},"exit_proof":proof});
  let mut m=Envelope{schema:PROTOCOL.into(),message_id:uuid::Uuid::new_v4().to_string(),deployment:self.deployment.clone(),from:self.node.id.clone(),to:"A".into(),run_id:execution.run_id.clone(),attempt:execution.attempt,epoch:execution.epoch.clone(),kind:"report".into(),sent_at:now,expires_at:now.saturating_add(300),payload,mac:String::new()};m.sign(&hex::decode(&self.node.key_hex)?)?;
  let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut e=read(&tx,&execution.run_id,execution.attempt)?;ensure!(e.identity.as_ref()==Some(execution)&&!e.finished,"native terminal execution fence mismatch");e.finished=true;e.state="done".into();save(&tx,&e)?;tx.execute("INSERT INTO outbox(id,body) VALUES(?1,?2)",params![m.message_id,serde_json::to_string(&m)?])?;tx.commit()?;Ok(())
 }
 pub fn native_unknown(&mut self,identity:&ExecutionIdentity,reason:&str,now:u64)->Result<()> {let mut e=read(&self.db,&identity.run_id,identity.attempt)?;ensure!(e.identity.as_ref()==Some(identity),"native unknown identity mismatch");if e.finished{return Ok(())}e.state="unknown".into();e.reason=Some(reason.into());self.status(&e,None,now)}
 pub fn relay_bridge(&self)->Result<RelayBridge>{RelayBridge::new(&self.deployment,&self.node.id,hex::decode(&self.node.key_hex)?.try_into().map_err(|_|anyhow::anyhow!("node key must be 32 bytes"))?)}
 pub fn accept(&mut self,m:&Envelope,now:u64)->Result<bool>{
  m.verify(&hex::decode(&self.node.key_hex)?,&self.deployment,&self.node.id,now)?;
  ensure!(m.from=="A"&&matches!(m.kind.as_str(),"start"|"cancel"|"decision"),"foreign Runner command");
  let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
  let body=serde_json::to_string(m)?;
  let old:Option<String>=tx.query_row("SELECT body FROM inbox WHERE id=?1",[&m.message_id],|r|r.get(0)).optional()?;
  if let Some(old)=old{ensure!(old==body,"message id collision");return Ok(false)}
  let prior:Option<String>=tx.query_row("SELECT body FROM executions WHERE run=?1 AND attempt=?2",params![m.run_id,m.attempt],|r|r.get(0)).optional()?;
  let mut e=if let Some(raw)=prior{let e:Execution=serde_json::from_str(&raw)?;ensure!(e.epoch==m.epoch,"execution epoch mismatch");e}
  else {
   let previous:Option<(u32,String)>=tx.query_row("SELECT attempt,body FROM executions WHERE run=?1 ORDER BY attempt DESC LIMIT 1",[&m.run_id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
   if let Some((attempt,raw))=previous {let previous:Execution=serde_json::from_str(&raw)?;ensure!(m.attempt==attempt.checked_add(1).context("attempt overflow")?&&previous.finished,"previous execution not proven ended or unsupported attempt gap");}
   else{ensure!(m.attempt==1,"missing earlier Runner attempt/fence");}
   let e=Execution{run_id:m.run_id.clone(),attempt:m.attempt,epoch:m.epoch.clone(),template:String::new(),deadline:now.saturating_add(900),identity:None,project_work:None,state:"accepted".into(),claimed:false,session:None,reason:None,approval:None,decision:None,resolve_claimed:false,finished:false};
   tx.execute("INSERT INTO executions VALUES(?1,?2,?3)",params![e.run_id,e.attempt,serde_json::to_string(&e)?])?;e
  };
  match m.kind.as_str(){
   "start"=>{let template=m.payload["template"].as_str().context("missing fixed template")?;ensure!(self.node.templates.iter().any(|t|t==template),"unsupported node template");let deadline=m.payload["deadline"].as_u64().context("missing deadline")?;ensure!(e.template.is_empty()||(e.template==template&&e.deadline==deadline),"start execution changed");let work=if template=="project-work-v1"{let work:super::project_work::ProjectWork=serde_json::from_value(m.payload.get("project_work").context("document work missing")?.clone())?;work.validate()?;Some(work)}else{ensure!(m.payload.get("project_work").is_none(),"document work requires project template");None};let detail=m.payload.get("project_work").cloned().map(|value|json!({"project_work":value})).unwrap_or_else(||json!({}));let identity=ExecutionIdentity::from_detail(&self.deployment,&m.run_id,m.attempt,&m.epoch,None,&detail)?;ensure!(e.identity.as_ref().is_none_or(|old|old==&identity),"start execution identity changed");e.identity=Some(identity);e.template=template.into();e.deadline=deadline;e.project_work=work;},
   "cancel"=>{if !e.finished{e.state="cancel_pending".into();e.reason=Some(m.payload.get("reason").and_then(Value::as_str).unwrap_or("cancel requested").into());}},
   "decision"=>{
    ensure!(!e.finished&&e.state=="waiting_approval"&&m.payload["session"].as_str()==e.session.as_deref(),"decision execution/session mismatch");
    let approval=e.approval.as_ref().context("approval missing")?;
    ensure!(["request_id","generation","scope_hash"].iter().all(|k|approval[*k]==m.payload[*k]),"decision scope mismatch");
    ensure!(now<e.deadline&&approval["offered"].as_array().is_some_and(|a|a.contains(&m.payload["decision_id"])),"unoffered/expired decision");
    ensure!(e.decision.is_none(),"decision already persisted");e.decision=Some(m.payload.clone());
   },_=>unreachable!(),
  }
  save(&tx,&e)?;tx.execute("INSERT INTO inbox VALUES(?1,?2)",params![m.message_id,body])?;tx.commit()?;Ok(true)
 }
 pub fn accept_probe(&mut self,m:&Envelope,now:u64,ao_project_ready:bool)->Result<()>{m.verify(&hex::decode(&self.node.key_hex)?,&self.deployment,&self.node.id,now)?;ensure!(m.from=="A"&&m.kind=="probe"&&ao_project_ready,"probe readiness unavailable");let requested=m.payload["templates"].as_array().context("probe templates missing")?;ensure!(requested.iter().all(|t|t.as_str().is_some_and(|t|self.node.templates.iter().any(|v|v==t))),"probe capability mismatch");let probe_deadline=m.sent_at.checked_add(60).context("probe readiness deadline overflow")?;let cap=probe_deadline.min(m.expires_at);ensure!(now<cap,"probe readiness deadline expired");let remaining=cap.checked_sub(now).context("probe readiness remaining time underflow")?;let ttl=remaining.min(60);let expires_at=now.checked_add(ttl).context("ready expiry overflow")?;let mut reply=Envelope{schema:PROTOCOL.into(),message_id:uuid::Uuid::new_v4().to_string(),deployment:self.deployment.clone(),from:self.node.id.clone(),to:"A".into(),run_id:m.run_id.clone(),attempt:m.attempt,epoch:m.epoch.clone(),kind:"ready".into(),sent_at:now,expires_at,payload:json!({"address":self.node.address,"templates":self.node.templates,"ao_project_ready":true}),mac:String::new()};reply.sign(&hex::decode(&self.node.key_hex)?)?;self.db.execute("INSERT INTO outbox(id,body) VALUES(?1,?2)",params![reply.message_id,serde_json::to_string(&reply)?])?;Ok(())}
 pub fn pending_outbox_raw(&self,limit:usize)->Result<Vec<(String,Vec<u8>)>>{let mut stmt=self.db.prepare("SELECT id,body FROM outbox WHERE sent=0 ORDER BY rowid LIMIT ?1")?;let rows=stmt.query_map([limit.min(100) as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?.into_bytes())))?;rows.collect::<std::result::Result<Vec<_>,_>>().map_err(Into::into)}
 pub fn pending_outbox(&self,limit:usize)->Result<Vec<Envelope>>{self.pending_outbox_raw(limit)?.into_iter().map(|(_id,body)|Ok(serde_json::from_slice(&body)?)).collect()}
 pub fn acknowledge_outbox(&mut self,id:&str)->Result<()>{ensure!(self.db.execute("UPDATE outbox SET sent=1 WHERE id=?1",[id])?==1,"Runner outbox id missing");Ok(())}
 fn update_event(&mut self,e:&Execution,kind:&str,payload:Value,now:u64)->Result<()>{let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;save(&tx,e)?;emit(&tx,&self.deployment,&self.node,e,kind,payload,now)?;tx.commit()?;Ok(())}
 fn status(&mut self,e:&Execution,proof:Option<ExitProof>,now:u64)->Result<()>{self.update_event(e,"status",json!({"state":e.state,"session":e.session,"spawn_intent":e.claimed,"reason":e.reason,"exit_proof":proof}),now)}
 pub fn observe_owned_process(&mut self,observer:&mut impl RunnerObserver,expected:&ExecutionIdentity,now:u64)->Result<ObserverEvent>{
  let mut execution=read(&self.db,&expected.run_id,expected.attempt)?;
  ensure!(execution.epoch==expected.epoch,"observer execution epoch mismatch");
  if execution.identity.as_ref()!=Some(expected)||execution.session.is_none()||expected.session.is_none()||execution.session!=expected.session{
   return Ok(ObserverEvent::Unknown{identity:execution.identity.clone(),reason:"observer execution identity/session mismatch; retained reservation".into()});
  }
  if !execution.claimed||execution.finished||!matches!(execution.state.as_str(),"running"|"unknown"){
   return Ok(ObserverEvent::Unknown{identity:Some(expected.clone()),reason:format!("observer execution is not claimed and observable; state={} claimed={} finished={}; retained reservation",execution.state,execution.claimed,execution.finished)});
  }
  let event=observer.observe(expected)?;
  match &event {
   ObserverEvent::Pending{identity,report,reason}=>{ensure!(identity==expected,"observer pending identity mismatch");execution.state="unknown".into();execution.reason=Some(reason.clone());self.update_event(&execution,"report",json!({"session":expected.session,"report":report,"verification":{"passed":Value::Null,"state":"report_pending_exit","reason":reason}}),now)?;}
   ObserverEvent::Unknown{identity,reason}=>{let reason=if identity.as_ref().is_some_and(|value|value!=expected){"observer execution identity mismatch; retained reservation".to_owned()}else{reason.clone()};execution.state="unknown".into();execution.reason=Some(reason);self.status(&execution,None,now)?;}
   ObserverEvent::Exited{identity,report,evidence}=>{
    ensure!(identity==expected&&evidence.identity==*expected,"observer terminal identity mismatch");
    let streams_clean=evidence.stdout.error.is_none()&&evidence.stderr.error.is_none();
    let terminal=evidence.wait_completed&&streams_clean;
    let verification=json!({"passed":if terminal {Some(evidence.success()&&report.is_some())}else{None::<bool>},"wait_completed":evidence.wait_completed,"child_exit_code":evidence.exit_code,"pid":evidence.pid,"argv":evidence.argv,"cwd":evidence.cwd,"stdout":evidence.stdout,"stderr":evidence.stderr});
    if terminal {
     execution.state="unknown".into();
     execution.finished=false;
     execution.reason=Some("ordinary observer exit proof lacks admitted M0 owner evidence; retained reservation".into());
     let mut verification=verification;
     verification["passed"]=Value::Null;
     verification["state"]=json!("owner_evidence_required");
     self.update_event(&execution,"report",json!({"session":expected.session,"report":report,"verification":verification}),now)?;
    } else {
     execution.state="unknown".into();execution.reason=Some("observer exit evidence incomplete; retained reservation".into());
     self.update_event(&execution,"report",json!({"session":expected.session,"report":report,"verification":verification}),now)?;
    }
   }
  }
  Ok(event)
 }
 pub fn tick(&mut self,ao:&mut impl AoPort,now:u64)->Result<()>{
  let mut active:Vec<Execution>={let mut stmt=self.db.prepare("SELECT body FROM executions ORDER BY rowid")?;let rows=stmt.query_map([],|r|r.get::<_,String>(0))?;rows.map(|r|Ok(serde_json::from_str(&r?)?)).collect::<Result<_>>()?};
  active.sort_by_key(|e|e.state!="cancel_pending");
  for mut e in active {
   if e.finished{continue}
   if now>=e.deadline&&e.state!="cancel_pending"{e.state="cancel_pending".into();e.reason=Some("deadline expired".into());save(&self.db,&e)?;}
   if e.state=="cancel_pending" {
    if !e.claimed&&e.session.is_none(){e.finished=true;let proof=ExitProof{run_id:e.run_id.clone(),attempt:e.attempt,epoch:e.epoch.clone(),session:None,empty:true,observer_alive:true,not_started:true,owner_evidence:None};self.status(&e,Some(proof),now)?;continue}
    if let Some(session)=e.session.clone(){let _=ao.kill(&session);let observation=ao.observe(&session)?;
     match observation {
      Observation::ExitProof{..}=>{e.state="unknown".into();e.reason=Some("ordinary observer exit proof lacks admitted M0 owner evidence; retained reservation".into());self.status(&e,None,now)?;}
      Observation::Unknown(reason)=>{if e.reason.as_ref()!=Some(&reason){e.reason=Some(reason);self.status(&e,None,now)?;}}
      Observation::Completed{..}|Observation::Exited=>{e.state="unknown".into();e.reason=Some("observer exit proof required; retained reservation".into());self.status(&e,None,now)?;}
      Observation::Running|Observation::Approval(_)|Observation::Reported{..}=>{e.state="unknown".into();e.reason=Some("observer exit proof required; retained reservation".into());self.status(&e,None,now)?;}
     }
    } else if e.reason.as_deref()!=Some("create outcome unknown; retained reservation") {e.reason=Some("create outcome unknown; retained reservation".into());self.status(&e,None,now)?;}
    continue
   }
   if !e.claimed&&!e.template.is_empty(){
    // Re-read and claim latest row under a write transaction before external create.
    let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut latest=read(&tx,&e.run_id,e.attempt)?;
    if latest.claimed||latest.state!="accepted"{continue}
    latest.claimed=true;save(&tx,&latest)?;tx.commit()?;e=latest;
    let seed=format!("{}:{}:{}:{}",self.deployment,e.run_id,e.attempt,e.epoch);let session=format!("local-{}",hex::encode(Sha256::digest(seed.as_bytes())));
    let request=WorkRequest{run_id:e.run_id.clone(),attempt:e.attempt,epoch:e.epoch.clone(),template:e.template.clone(),session,project_work:e.project_work.clone()};
    match ao.create(&request){Ok(session)=>{let mut latest=read(&self.db,&e.run_id,e.attempt)?;ensure!(latest.epoch==e.epoch&&latest.claimed,"create response stale");latest.session=Some(session.clone());if let Some(identity)=latest.identity.as_mut(){identity.session=Some(session.clone());identity.validate()?;}if latest.state=="accepted"{latest.state="running".into();}self.status(&latest,None,now)?;e=latest;},Err(error)=>{e.state="unknown".into();e.reason=Some(format!("create outcome unknown: {error}"));self.status(&e,None,now)?;continue}}
   }
   let Some(session)=e.session.clone() else{if e.claimed&&e.state!="unknown"{e.state="unknown".into();e.reason=Some("create outcome unknown; retained reservation".into());self.status(&e,None,now)?;}continue};
   if let Some(identity)=e.identity.as_ref(){ensure!(identity.session.as_deref()==Some(&session),"execution identity/session mismatch");}
   ao.bind(&session,&WorkRequest{run_id:e.run_id.clone(),attempt:e.attempt,epoch:e.epoch.clone(),template:e.template.clone(),session:session.clone(),project_work:e.project_work.clone()})?;
   if let Some(decision)=e.decision.clone(){if !e.resolve_claimed{
    let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut latest=read(&tx,&e.run_id,e.attempt)?;if latest.resolve_claimed||latest.state=="cancel_pending"{continue}latest.resolve_claimed=true;save(&tx,&latest)?;tx.commit()?;e=latest;
    if let Err(error)=ao.resolve(&session,decision["request_id"].as_str().context("invalid persisted request")?,decision["decision_id"].as_str().context("invalid persisted decision")?){e.state="unknown".into();e.reason=Some(format!("resolve outcome unknown; never repeat: {error}"));self.status(&e,None,now)?;continue}
   }}
   match ao.observe(&session)? {
    Observation::Unknown(reason)=>{if e.state!="unknown"||e.reason.as_ref()!=Some(&reason){e.state="unknown".into();e.reason=Some(reason);self.status(&e,None,now)?;}},
    Observation::Running=>{if e.state!="running"{e.state="running".into();self.status(&e,None,now)?;}},
    Observation::Approval(mut p)=>{if e.approval.is_none(){p["session"]=json!(session);p["expires_at"]=json!(e.deadline);e.approval=Some(p.clone());e.state="waiting_approval".into();self.update_event(&e,"approval",p,now)?;}},
    Observation::Reported{report,verification}=>{e.state="unknown".into();e.reason=Some("actual AO report available; independent verification and process exit not established".into());self.update_event(&e,"report",json!({"session":session,"report":report,"verification":verification}),now)?;},
    Observation::ExitProof{proof,report,verification}=>{
     if matches_exit_proof(&e,&session,&proof) {
      e.state="unknown".into();e.finished=false;
      e.reason=Some("ordinary AoPort exit proof lacks admitted M0 owner evidence; retained reservation".into());
      let mut verification=verification;
      if verification["passed"] != false { verification["passed"] = Value::Null; }
      verification["state"]=json!("owner_evidence_required");
      self.update_event(&e,"report",json!({"session":session,"report":report,"verification":verification}),now)?;
     } else {
      e.state="unknown".into();e.reason=Some("observer exit proof identity mismatch; retained reservation".into());
      let mut rejected=verification;
      rejected["passed"]=Value::Null;
      rejected["state"]=json!("exit_proof_rejected");
      self.update_event(&e,"report",json!({"session":session,"report":report,"verification":rejected}),now)?;
     }
    },
    Observation::Completed{report,verification}=>{e.state="unknown".into();e.reason=Some("provider completion observed; explicit exit proof required".into());let mut verification=verification;verification["passed"]=Value::Null;verification["state"]=json!("exit_proof_required");self.update_event(&e,"report",json!({"session":session,"report":report,"verification":verification}),now)?;},
    Observation::Exited=>{e.state="unknown".into();e.reason=Some("worker exit observed; explicit exit proof required".into());self.status(&e,None,now)?;},
   }
  }Ok(())
 }
}
