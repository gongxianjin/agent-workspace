//! Persistent document intake and user-controlled stage gates, backed by the native Store.
use anyhow::{Result,Context,ensure};
use crate::config::Actor;
use serde::{Serialize,Deserialize};
use serde_json::{Value,json};
use std::{path::{Path,PathBuf},io::{Read,Write}};
use sha2::{Digest,Sha256};
use rusqlite::{params,OptionalExtension,TransactionBehavior};
use super::{store::{Run,Store},service::{ApprovalView,DecisionKind,OfferedDecision,UiCommand},controller::{Decision,decide_with_binding}};
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct DocumentSnapshot{pub version:u32,pub filename:String,pub sha256:String,pub text:String,pub confirmed:bool}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct TaskSnapshot{pub id:String,pub title:String,pub description:String,pub acceptance:String,pub review:String,pub reason:Option<String>}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ProjectActionSnapshot{pub id:String,pub kind:String,pub state:String,pub session:Option<String>,pub detail:String}
#[derive(Clone,Debug,Serialize,Deserialize)]
pub struct ProjectSnapshot{pub id:String,pub name:String,pub phase:String,#[serde(default)] pub project_path:String,#[serde(default)] pub product_path:String,pub document:Option<DocumentSnapshot>,pub tasks:Vec<TaskSnapshot>,pub actions:Vec<ProjectActionSnapshot>,pub blockers:Vec<String>,pub can_confirm:bool,pub can_split:bool,pub can_start:bool,pub can_test:bool,pub can_repair:bool,pub can_accept:bool}
struct ProjectEngine{store:Store,reason:crate::adapters::reason::Reason,endpoint:String,_lock:crate::config::ServiceLock}
impl ProjectEngine{
 fn open(path:&Path)->Result<Self>{let mut raw=Vec::new();std::fs::File::open(path)?.take(65537).read_to_end(&mut raw)?;ensure!(raw.len()<=65536,"configuration bound");let cfg:crate::config::Config=serde_json::from_slice(&raw)?;cfg.validate()?;ensure!(cfg.mode==crate::config::Mode::Real&&cfg.role=="controller","real Controller required");let base=path.parent().unwrap_or(Path::new("."));let nodes=cfg.load_nodes(base)?;let state=cfg.prepare_state(base)?;let lock=crate::config::ServiceLock::acquire(&state,"controller")?;let mut store=Store::open(&state.join("controller.sqlite"),&cfg.deployment,&nodes)?;store.probe_nodes(timestamp())?;let reason=crate::adapters::reason::Reason::new(cfg.reason_endpoint.context("Reason endpoint required")?)?;Ok(Self{store,reason,endpoint:cfg.endpoint.context("controller endpoint required")?,_lock:lock})}
 fn tick(&mut self)->Result<()>{let now=timestamp();self.store.expire(now)?;let mut failed=false;match self.reason.inbox(&self.endpoint){Ok(messages)=>for(id,body)in messages{let accepted=(||->Result<()>{let m:super::protocol::Envelope=serde_json::from_str(&body)?;match self.store.receive(&m,now){Ok(_)=>{},Err(error)if error.is::<super::store::ObsoleteReady>()=>{},Err(error)=>return Err(error)}self.reason.delivered(&self.endpoint,&id)?;Ok(())})();if accepted.is_err(){failed=true;}},Err(_)=>failed=true};if failed{self.store.invalidate_readiness()?;anyhow::bail!("project transport unavailable; durable state retained");}self.store.refresh_probes(now)?;self.store.assign_next(now)?;for(id,destination,frame)in self.store.pending_outbox_raw(32)?{if super::runtime::forward_controller_outbox_entry(&mut self.store,&self.reason,&self.endpoint,&id,&destination,&frame,now).is_err(){failed=true;}}if failed{self.store.invalidate_readiness()?;}ensure!(!failed,"project transport unavailable; durable state retained");Ok(())}
}
fn timestamp()->u64{std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0,|v|v.as_secs())}
pub struct Projects{store:Store,items:Vec<ProjectSnapshot>,config:Option<PathBuf>,state:PathBuf,engine:Option<ProjectEngine>}
const TRANSPORT_BLOCKER:&str="真实项目 Reason/transport 不可用；readiness 已失效，保留既有名额且不自动重派 UNKNOWN";
fn busy(p:&ProjectSnapshot)->bool{p.actions.iter().any(|a|matches!(a.state.as_str(),"requested"|"running"|"unknown"|"report_pending_exit"))}
fn artifact_parts(relative:&str)->Result<Vec<&str>>{ensure!(!relative.is_empty()&&relative.len()<=120&&relative.is_ascii()&&!relative.contains(['\\',':'])&&!relative.starts_with('/'),"unsafe artifact path");let parts:Vec<_>=relative.split('/').collect();ensure!(parts.len()<=8&&parts.iter().all(|part|!part.is_empty()&&*part!="."&&*part!=".."&&!part.ends_with(['.',' '])&&part.bytes().all(|b|b.is_ascii_alphanumeric()||b"._-".contains(&b))),"unsafe artifact path");Ok(parts)}
fn owned_directory(path:&Path,root:&Path)->Result<PathBuf>{match std::fs::symlink_metadata(path){Ok(meta)=>{ensure!(meta.is_dir()&&!meta.file_type().is_symlink(),"managed directory must be regular");#[cfg(windows)]{use std::os::windows::fs::MetadataExt;ensure!(meta.file_attributes()&0x400==0,"managed directory reparse point forbidden");}},Err(error)if error.kind()==std::io::ErrorKind::NotFound=>{std::fs::create_dir(path)?},Err(error)=>return Err(error.into())}let canonical=path.canonicalize()?;ensure!(canonical.starts_with(root)&&canonical!=root,"managed directory escaped root");Ok(canonical)}
fn regular_artifact_file(path:&Path,root:&Path)->Result<PathBuf>{let meta=std::fs::symlink_metadata(path)?;ensure!(meta.is_file()&&!meta.file_type().is_symlink(),"managed artifact must be regular");#[cfg(windows)]{use std::os::windows::fs::MetadataExt;ensure!(meta.file_attributes()&0x400==0,"managed artifact reparse point forbidden");}let canonical=path.canonicalize()?;ensure!(canonical.starts_with(root),"managed artifact escaped root");Ok(canonical)}
fn collect_artifact_files(path:&Path,root:&Path,files:&mut Vec<(String,Vec<u8>)>)->Result<()>{let meta=std::fs::symlink_metadata(path)?;ensure!(!meta.file_type().is_symlink(),"managed artifact link forbidden");if meta.is_dir(){for entry in std::fs::read_dir(path)?{collect_artifact_files(&entry?.path(),root,files)?;}return Ok(())}let canonical=regular_artifact_file(path,root)?;let relative=canonical.strip_prefix(root)?.to_string_lossy().replace('\\',"/");artifact_parts(&relative)?;let bytes=std::fs::read(&canonical)?;ensure!(bytes.len()<=32768,"managed artifact file too large");files.push((relative,bytes));Ok(())}
fn frozen_artifact(project:&ProjectSnapshot,state:&Path)->Result<super::project_work::FrozenInput>{let root=Path::new(&project.product_path).canonicalize()?;let state=state.canonicalize()?;ensure!(root.starts_with(&state),"managed product escaped state");let mut files=Vec::new();collect_artifact_files(&root,&root,&mut files)?;files.sort_by(|a,b|a.0.cmp(&b.0));ensure!(!files.is_empty(),"managed product has no artifact files");let total:usize=files.iter().map(|(_,bytes)|bytes.len()).sum();ensure!(total<=131072,"managed artifact manifest too large");let manifest=json!({"schema":"project-artifact-manifest/1","files":files.iter().map(|(path,bytes)|json!({"path":path,"size":bytes.len(),"sha256":hex::encode(Sha256::digest(bytes))})).collect::<Vec<_>>()});let text=serde_json::to_string(&manifest)?;Ok(super::project_work::FrozenInput{sha256:hex::encode(Sha256::digest(text.as_bytes())),text})}
fn frozen_spec(project:&ProjectSnapshot)->Result<super::project_work::FrozenInput>{let mut text=String::from("# Project checks\n");for task in &project.tasks{ensure!(!task.acceptance.trim().is_empty(),"task acceptance required for test specification");text.push_str("- ");text.push_str(&task.id);text.push_str(": ");text.push_str(&task.acceptance);text.push('\n');}ensure!(text.len()<=65536&&text.lines().count()>1,"nonempty project test specification required");Ok(super::project_work::FrozenInput{sha256:hex::encode(Sha256::digest(text.as_bytes())),text})}
fn verified_artifact_files(package:&Value,project:&ProjectSnapshot,action:&ProjectActionSnapshot)->Result<Vec<(String,Vec<u8>)>>{ensure!(package["schema"]=="project-artifact-package/1"&&package["project_id"]==project.id&&package["action_id"]==action.id,"artifact package identity mismatch");let document=project.document.as_ref().context("document absent")?;ensure!(package["document_version"]==document.version&&package["document_sha256"]==document.sha256,"artifact document identity mismatch");let files=package["files"].as_array().context("artifact package files missing")?;ensure!(!files.is_empty()&&files.len()<=32,"artifact package file count");let mut seen=std::collections::BTreeSet::new();let mut total=0usize;let mut verified=Vec::with_capacity(files.len());for file in files{let path=file["path"].as_str().context("artifact path missing")?;artifact_parts(path)?;ensure!(seen.insert(path.to_ascii_lowercase()),"duplicate artifact path");let size=file["size"].as_u64().context("artifact size missing")?;ensure!(size<=32768,"artifact file too large");total=total.checked_add(size as usize).context("artifact size overflow")?;ensure!(total<=32768,"artifact package too large");let encoded=file["content_hex"].as_str().context("artifact bytes missing")?;ensure!(encoded.len()<=65536&&encoded.len()%2==0,"artifact bytes bound");let bytes=hex::decode(encoded)?;ensure!(bytes.len() as u64==size,"artifact size mismatch");let digest=hex::encode(Sha256::digest(&bytes));ensure!(file["sha256"]==digest,"artifact hash mismatch");verified.push((path.into(),bytes));}Ok(verified)}
fn materialize_artifact(state:&Path,project:&ProjectSnapshot,action:&ProjectActionSnapshot,package:&Value)->Result<()>{let files=verified_artifact_files(package,project,action)?;let state=state.canonicalize()?;let product=owned_directory(Path::new(&project.product_path),&state)?;let action_root=owned_directory(&product.join(&action.id),&product)?;for(relative,bytes)in files{let parts=artifact_parts(&relative)?;let mut parent=action_root.clone();for part in &parts[..parts.len()-1]{parent=owned_directory(&parent.join(part),&action_root)?;}let target=parent.join(parts[parts.len()-1]);match std::fs::symlink_metadata(&target){Ok(meta)=>{ensure!(meta.is_file()&&!meta.file_type().is_symlink(),"artifact target must be regular");ensure!(std::fs::read(&target)?==bytes,"immutable artifact changed")},Err(error)if error.kind()==std::io::ErrorKind::NotFound=>{let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(&target)?;if let Err(error)=file.write_all(&bytes).and_then(|_|file.sync_all()){let _=std::fs::remove_file(&target);return Err(error.into())}},Err(error)=>return Err(error.into())}}Ok(())}
fn project(p:&mut ProjectSnapshot){let confirmed=p.document.as_ref().is_some_and(|d|d.confirmed);let idle=!busy(p);p.can_confirm=idle&&p.document.as_ref().is_some_and(|d|!d.confirmed);p.can_split=idle&&confirmed;p.can_start=idle&&confirmed&&!p.tasks.is_empty()&&p.tasks.iter().all(|t|t.review=="approved")&&p.phase=="ready";p.can_test=idle&&p.phase=="developed";p.can_repair=idle&&p.phase=="test_failed";p.can_accept=idle&&p.phase=="awaiting_acceptance";}
impl Projects{
 pub fn open(state:&Path)->Result<Self> {let store=Store::open(&state.join("project-workflow/controller.sqlite"),"native-project-workflow",&[])?;store.db.execute_batch("CREATE TABLE IF NOT EXISTS projects(id TEXT PRIMARY KEY,body TEXT NOT NULL);CREATE TABLE IF NOT EXISTS project_documents(project TEXT NOT NULL,version INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(project,version));CREATE TABLE IF NOT EXISTS project_settings(key TEXT PRIMARY KEY,value TEXT NOT NULL);")?;let mut items={let mut q=store.db.prepare("SELECT body FROM projects ORDER BY rowid")?;q.query_map([],|r|r.get::<_,String>(0))?.map(|r|Ok(serde_json::from_str::<ProjectSnapshot>(&r?)?)).collect::<Result<Vec<_>>>()?};for p in &mut items{project(p);}let config=store.db.query_row("SELECT value FROM project_settings WHERE key='config'",[],|r|r.get::<_,String>(0)).ok().map(PathBuf::from);Ok(Self{store,items,config,state:state.canonicalize()?,engine:None})}
 fn managed_paths(&self,id:&str)->Result<(String,String)>{ensure!(uuid::Uuid::parse_str(id).is_ok(),"managed project UUID required");let projects=self.state.join("projects");let root=projects.join(id);let product=root.join("product");for path in [&projects,&root,&product]{match std::fs::symlink_metadata(path){Ok(m)=>ensure!(m.is_dir()&&!m.file_type().is_symlink(),"managed directory must not be a link or file"),Err(e)if e.kind()==std::io::ErrorKind::NotFound=>std::fs::create_dir(path)?,Err(e)=>return Err(e.into())}ensure!(path.canonicalize()?.starts_with(&self.state),"managed path escaped state root");}Ok((root.canonicalize()?.to_string_lossy().into(),product.canonicalize()?.to_string_lossy().into()))}
 pub fn snapshots(&self)->Vec<ProjectSnapshot>{self.items.clone()}
 fn project_run_actor(&self,run:&Run)->Result<Actor>{
  ensure!(run.template=="project-work-v1","project template required");
  let work:super::project_work::ProjectWork=serde_json::from_value(run.detail.get("project_work").context("project work missing")?.clone())?;
  work.validate()?;
  let projects:Vec<_>=self.items.iter().filter(|project|project.id==work.project_id).collect();
  ensure!(projects.len()==1,"project binding is ambiguous");
  let project=projects[0];
  ensure!(self.items.iter().flat_map(|project|&project.actions).filter(|action|action.id==work.action_id).count()==1,"project action binding is ambiguous");
  let action=project.actions.iter().find(|action|action.id==work.action_id).context("project action missing")?;
  ensure!(serde_json::to_value(&work.stage)?==action.kind,"project action stage mismatch");
  let document=project.document.as_ref().context("project document missing")?;
  ensure!(document.confirmed&&document.version==work.document.version&&document.sha256==work.document.sha256,"project document mismatch");
  let linked:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[format!("action:{}",work.action_id)],|row|row.get(0)).context("project action link missing")?;
  let aliases:i64=self.store.db.query_row("SELECT count(*) FROM project_settings WHERE key LIKE 'action:%' AND value=?1",[&run.id],|row|row.get(0))?;
  ensure!(linked==run.id&&aliases==1,"project action link mismatch or ambiguous");
  let actor=Actor::Local(project.id.clone());
  ensure!(run.owner==actor,"project run owner mismatch");
  ensure!(run.session.is_some(),"project approval session missing");
  Ok(actor)
 }
 fn open_engine(&mut self)->Result<()>{
  if self.engine.is_none(){
   let path=self.config.clone().context("project host unavailable")?;
   self.engine=Some(ProjectEngine::open(&path).context("project host unavailable")?);
  }
  Ok(())
 }
 pub fn host_configured(&self)->bool{self.config.is_some()}
 pub fn node_readiness(&mut self,now:u64)->Result<Vec<super::store::NodeReadiness>>{self.open_engine()?;let engine=self.engine.as_mut().context("project host unavailable")?;engine.store.refresh_probes(now)?;engine.store.readiness(now)}
 pub fn has_run_link(&self,run_id:&str)->Result<bool>{
  Ok(self.store.db.query_row("SELECT EXISTS(SELECT 1 FROM project_settings WHERE key LIKE 'action:%' AND value=?1)",[run_id],|row|row.get(0))?)
 }
 fn approval_view(run:&Run,raw:&Value,now:u64)->Result<ApprovalView>{
  let offered=raw["offered"].as_array().context("approval decisions missing")?.iter().map(|value|{
   let id=value.as_str().context("invalid offered decision")?;
   let (kind,label)=match id {"allow_once"=>(DecisionKind::AllowOnce,"仅允许本次"),"reject_once"=>(DecisionKind::RejectOnce,"仅拒绝本次"),_=>anyhow::bail!("unsupported decision")};
   Ok(OfferedDecision{id:id.into(),kind,label:label.into()})
  }).collect::<Result<Vec<_>>>()?;
  Ok(ApprovalView{run_id:run.id.clone(),attempt:run.attempt,session:run.session.clone().context("approval session missing")?,request_id:raw["request_id"].as_str().context("request missing")?.into(),generation:raw["generation"].as_u64().context("generation missing")?,scope_hash:raw["scope_hash"].as_str().context("scope missing")?.into(),expires_at:raw["expires_at"].as_u64().context("expiry missing")?,offered,summary:raw["summary"].as_str().unwrap_or("项目审批").chars().take(1024).collect(),simulated:false,can_decide:now<run.deadline})
 }
 pub fn approvals(&mut self,now:u64)->Result<Vec<ApprovalView>>{
  if !self.host_configured(){return Ok(Vec::new())}
  self.open_engine()?;
  let engine=self.engine.as_ref().context("project host unavailable")?;
  let mut result=Vec::new();let mut seen=std::collections::BTreeSet::new();
  for project in &self.items {
   for action in &project.actions {
    let Some(run_id)=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[format!("action:{}",action.id)],|row|row.get::<_,String>(0)).optional()? else {continue};
    let run=engine.store.run(&run_id)?;
    let Ok(actor)=self.project_run_actor(&run) else {continue};
    if !seen.insert(run.id.clone()){continue}
    for approval in engine.store.approvals(&actor,&run.id,now)? {result.push(Self::approval_view(&run,&approval,now)?);}
   }
  }
  Ok(result)
 }
 // Caller serializes project commands with the controller ServiceLock. The
 // separate workflow/host ledgers are not an atomic external-writer boundary.
 pub fn decide(&mut self,decision:&Decision,attempt:u32,session:&str,now:u64)->Result<Value>{
  self.open_engine()?;
  let run=self.engine.as_ref().context("project host unavailable")?.store.run(&decision.run_id)?;
  let actor=self.project_run_actor(&run)?;
  decide_with_binding(&mut self.engine.as_mut().context("project host unavailable")?.store,&actor,decision,Some((attempt,session)),now)
 }
 fn save(&mut self,mut p:ProjectSnapshot)->Result<Value>{project(&mut p);let body=serde_json::to_string(&p)?;self.store.db.execute("INSERT INTO projects(id,body) VALUES(?1,?2) ON CONFLICT(id) DO UPDATE SET body=excluded.body",params![p.id,body])?;let value=serde_json::to_value(&p)?;if let Some(old)=self.items.iter_mut().find(|i|i.id==p.id){*old=p}else{self.items.push(p)}Ok(value)}
 pub fn command(&mut self,command:UiCommand)->Result<Value>{use UiCommand::*;
  if let ProjectCreate{name}=command{ensure!(!name.trim().is_empty()&&name.len()<=256&&self.items.len()<100,"invalid project name or project limit");let id=uuid::Uuid::new_v4().to_string();let(project_path,product_path)=self.managed_paths(&id)?;return self.save(ProjectSnapshot{id,name:name.trim().into(),phase:"needs_document".into(),project_path,product_path,document:None,tasks:vec![],actions:vec![],blockers:vec![],can_confirm:false,can_split:false,can_start:false,can_test:false,can_repair:false,can_accept:false})}
  if let ProjectConfigure{config_path}=command{ensure!(self.unfinished()==0,"cannot replace host config while project execution is unresolved");let mut raw=Vec::new();std::fs::File::open(&config_path)?.take(65537).read_to_end(&mut raw)?;ensure!(raw.len()<=65536,"configuration bound");let cfg:crate::config::Config=serde_json::from_slice(&raw)?;cfg.validate()?;ensure!(cfg.mode==crate::config::Mode::Real&&cfg.role=="controller","real controller host configuration required");ensure!(cfg.ao.is_some()&&cfg.reason_endpoint.is_some()&&cfg.endpoint.is_some(),"AO, Reason and controller endpoint configuration required");cfg.load_nodes(config_path.parent().unwrap_or(Path::new(".")))?;self.store.db.execute("INSERT INTO project_settings VALUES('config',?1) ON CONFLICT(key) DO UPDATE SET value=excluded.value",[config_path.to_string_lossy().as_ref()])?;self.config=Some(config_path);self.engine=None;return Ok(json!({"configured":true,"external_readiness":"not_probed","model_called":false}))}
  let id=match &command{ProjectImport{project_id,..}|ProjectConfirm{project_id,..}|ProjectSplit{project_id}|TaskReview{project_id,..}|TaskRedo{project_id,..}|ProjectStart{project_id}|ProjectTest{project_id}|ProjectRepair{project_id,..}|ProjectAccept{project_id}|ProjectRefresh{project_id}=>project_id,_=>anyhow::bail!("not a project command")};let mut p=self.items.iter().find(|p|p.id==*id).context("unknown project")?.clone();project(&mut p);
  match command {
   ProjectImport{path,..}=>{ensure!(!busy(&p),"execution unresolved; cannot replace document");ensure!(path.extension().and_then(|s|s.to_str()).is_some_and(|s|s.eq_ignore_ascii_case("md")||s.eq_ignore_ascii_case("txt")),"only UTF8 md/txt allowed");let mut raw=Vec::new();std::fs::File::open(&path)?.take(256*1024+1).read_to_end(&mut raw)?;ensure!(raw.len()<=256*1024&&!raw.is_empty(),"document must contain 1..256KiB bytes");let sha256=hex::encode(Sha256::digest(&raw));let text=String::from_utf8(raw).context("strict UTF8 required")?;ensure!(!text.trim().is_empty(),"empty document");let version=p.document.as_ref().map_or(Ok(1),|d|d.version.checked_add(1).context("version overflow"))?;let d=DocumentSnapshot{version,filename:path.file_name().context("filename missing")?.to_string_lossy().into(),sha256,text,confirmed:false};self.store.db.execute("INSERT INTO project_documents VALUES(?1,?2,?3)",params![p.id,version,serde_json::to_string(&d)?])?;p.document=Some(d);p.tasks.clear();p.phase="document_review".into();p.blockers.clear();},
   ProjectConfirm{version,sha256,..}=>{ensure!(p.can_confirm,"document cannot be confirmed now");let d=p.document.as_mut().context("document required")?;ensure!(d.version==version&&d.sha256==sha256,"stale document version/hash");d.confirmed=true;p.phase="confirmed".into();self.store.db.execute("UPDATE project_documents SET body=?1 WHERE project=?2 AND version=?3",params![serde_json::to_string(d)?,p.id,version])?;},
   TaskReview{task_id,approve,reason,..}=>{ensure!(matches!(p.phase.as_str(),"task_review"|"ready")&&p.actions.iter().filter(|a|matches!(a.state.as_str(),"requested"|"running"|"unknown"|"report_pending_exit")).all(|a|a.kind=="split"&&a.state=="report_pending_exit"),"task review not available");let t=p.tasks.iter_mut().find(|t|t.id==task_id).context("unknown task")?;ensure!(approve||!reason.trim().is_empty(),"rejection reason required");ensure!(reason.len()<=4096,"reason bound");t.review=if approve{"approved"}else{"rejected"}.into();t.reason=if approve{None}else{Some(reason)};p.phase=if p.tasks.iter().all(|t|t.review=="approved"){"ready"}else{"task_review"}.into();},
   ProjectAccept{..}=>{ensure!(p.can_accept,"independent tests must pass before manual acceptance");ensure!(!p.actions.iter().any(|a|a.kind=="accept"),"project acceptance already recorded");let test=p.actions.iter().rev().find(|a|a.kind=="test"&&a.state=="succeeded").context("successful test action required")?;let d=p.document.as_ref().context("document absent")?;let spec_key=format!("test-spec:{}",p.id);let artifact_key=format!("test-artifact:{}",p.id);let spec_raw:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[&spec_key],|r|r.get(0)).context("frozen test specification missing")?;let artifact_raw:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[&artifact_key],|r|r.get(0)).context("frozen test artifact missing")?;let spec:Value=serde_json::from_str(&spec_raw)?;let artifact:Value=serde_json::from_str(&artifact_raw)?;let evidence=json!({"schema":"project-acceptance/1","project_id":p.id,"document_version":d.version,"document_sha256":d.sha256,"test_action_id":test.id,"test_spec_sha256":spec["input"]["sha256"],"artifact_sha256":artifact["input"]["sha256"],"accepted":true});self.store.db.execute("INSERT INTO project_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO NOTHING",params![format!("acceptance:{}",p.id),evidence.to_string()])?;p.actions.push(ProjectActionSnapshot{id:uuid::Uuid::new_v4().to_string(),kind:"accept".into(),state:"succeeded".into(),session:None,detail:"人工验收证据已持久化；项目已接受".into()});p.phase="accepted".into();},
   ProjectRefresh{..}=>{let(root,product)=self.managed_paths(&p.id)?;p.project_path=root;p.product_path=product;},
   action=>{let repair_reason=match &action{ProjectRepair{reason,..}=>Some(reason.trim().to_owned()),_=>None};let redo_target=match &action{TaskRedo{task_id,..}=>Some(task_id.clone()),_=>None};let kind=match action{ProjectSplit{..}=>{ensure!(p.can_split,"confirm uploaded document before split");"split"},TaskRedo{ref task_id,..}=>{ensure!(!busy(&p)&&p.tasks.iter().any(|t|t.id==*task_id&&t.review=="rejected"&&t.reason.is_some()),"rejected task and reason required");"redo"},ProjectStart{..}=>{ensure!(p.can_start,"all real split tasks require approval and manual start");"develop"},ProjectTest{..}=>{ensure!(p.can_test,"development has not completed");"test"},ProjectRepair{ref reason,..}=>{ensure!(p.can_repair&&!reason.trim().is_empty(),"explicit repair reason after failed test required");"repair"},_=>anyhow::bail!("unsupported project action")};if kind=="split"&&self.config.is_some(){let path=self.config.clone().context("host config missing")?;if self.engine.is_none(){self.engine=Some(ProjectEngine::open(&path)?);}let action_id=uuid::Uuid::new_v4().to_string();let d=p.document.as_ref().context("confirmed document required")?;let work=super::project_work::ProjectWork{schema:"project-work/1".into(),project_id:p.id.clone(),action_id:action_id.clone(),stage:super::project_work::ProjectStage::Split,document:super::project_work::ConfirmedDocument{version:d.version,sha256:d.sha256.clone(),text:d.text.clone()},tasks:vec![],target_task:None,repair_reason:None,repair_round:0,test_spec:None,artifact:None};let run=self.engine.as_mut().context("engine unavailable")?.store.dispatch_project(&crate::config::Actor::Local(p.id.clone()),&work,timestamp())?;self.store.db.execute("INSERT INTO project_settings(key,value) VALUES(?1,?2)",params![format!("action:{}",action_id),run.id])?;p.actions.push(ProjectActionSnapshot{id:action_id,kind:kind.into(),state:"requested".into(),session:None,detail:"已持久派工；等待签名Runner/AO项目就绪，不代表模型已执行".into()});p.phase="splitting".into();p.blockers.clear();return self.save(p)}if kind=="redo"&&self.config.is_some(){let path=self.config.clone().context("host config missing")?;if self.engine.is_none(){self.engine=Some(ProjectEngine::open(&path)?);}let action_id=uuid::Uuid::new_v4().to_string();let d=p.document.as_ref().context("confirmed document required")?;let target=redo_target.context("redo target required")?;ensure!(matches!(p.phase.as_str(),"task_review"|"ready")&&!p.tasks.is_empty(),"redo requires reviewed tasks");let rejected=p.tasks.iter().find(|task|task.id==target).context("unknown redo task")?;ensure!(rejected.review=="rejected"&&rejected.reason.as_ref().is_some_and(|reason|!reason.trim().is_empty()),"rejected task and reason required");let raw:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[format!("tasks:{}",p.id)],|r|r.get(0)).context("split task input missing")?;let inputs:Value=serde_json::from_str(&raw)?;let source_for=|id:&str|->Result<&Value>{inputs.as_array().and_then(|items|items.iter().find(|item|item["id"]==id)).context("split task identity missing")};let mut closure=std::collections::BTreeSet::from([target.clone()]);loop{let mut changed=false;for task in &p.tasks{let source=source_for(&task.id)?;let depends=source["depends_on"].as_array().cloned().unwrap_or_default();if depends.iter().any(|dependency|dependency.as_str().is_some_and(|id|closure.contains(id)))&&closure.insert(task.id.clone()){changed=true;}}if !changed{break;}}let tasks=p.tasks.iter().filter(|task|closure.contains(&task.id)).map(|task|{let source=source_for(&task.id)?;let review=if task.id==target{"rejected".into()}else{"pending".into()};Ok(super::project_work::WorkTask{id:task.id.clone(),title:task.title.clone(),description:task.description.clone(),acceptance:task.acceptance.clone(),depends_on:serde_json::from_value(source["depends_on"].clone()).unwrap_or_default(),review,reason:if task.id==target{task.reason.clone()}else{None}})}).collect::<Result<Vec<_>>>()?;let work=super::project_work::ProjectWork{schema:"project-work/1".into(),project_id:p.id.clone(),action_id:action_id.clone(),stage:super::project_work::ProjectStage::Redo,document:super::project_work::ConfirmedDocument{version:d.version,sha256:d.sha256.clone(),text:d.text.clone()},tasks,target_task:Some(target.clone()),repair_reason:None,repair_round:0,test_spec:None,artifact:None};let run=self.engine.as_mut().context("engine unavailable")?.store.dispatch_project(&crate::config::Actor::Local(p.id.clone()),&work,timestamp())?;self.store.db.execute("INSERT INTO project_settings(key,value) VALUES(?1,?2)",params![format!("action:{}",action_id),run.id])?;for task in &mut p.tasks{if closure.contains(&task.id){task.review="pending".into();if task.id!=target{task.reason=None;}}}p.actions.push(ProjectActionSnapshot{id:action_id,kind:kind.into(),state:"requested".into(),session:None,detail:"已持久派发目标任务及传递下游重做；等待独立执行，不代表自动开工或模型已执行".into()});p.phase="redoing".into();p.blockers.clear();return self.save(p)}if kind=="repair"&&self.config.is_some(){let path=self.config.clone().context("host config missing")?;if self.engine.is_none(){self.engine=Some(ProjectEngine::open(&path)?);}let action_id=uuid::Uuid::new_v4().to_string();let d=p.document.as_ref().context("confirmed document required")?;ensure!(p.phase=="test_failed"&&!p.tasks.is_empty(),"failed test with approved tasks required");let round=p.actions.iter().filter(|a|a.kind=="repair").count() as u32+1;ensure!(round<=2,"repair round limit reached");let load=|key:String|->Result<super::project_work::FrozenInput>{let raw:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[key],|r|r.get(0)).context("frozen project input missing")?;let value:Value=serde_json::from_str(&raw)?;ensure!(value["document_version"]==d.version&&value["document_sha256"]==d.sha256,"frozen project input document identity mismatch");let input:super::project_work::FrozenInput=serde_json::from_value(value["input"].clone())?;ensure!(input.sha256==hex::encode(Sha256::digest(input.text.as_bytes())),"frozen project input hash mismatch");Ok(input)};let spec=load(format!("test-spec:{}",p.id))?;let artifact=load(format!("test-artifact:{}",p.id))?;let raw:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[format!("tasks:{}",p.id)],|r|r.get(0)).context("split task input missing")?;let inputs:Value=serde_json::from_str(&raw)?;let tasks=p.tasks.iter().map(|t|{let source=inputs.as_array().and_then(|items|items.iter().find(|item|item["id"]==t.id)).context("split task identity missing")?;Ok(super::project_work::WorkTask{id:t.id.clone(),title:t.title.clone(),description:t.description.clone(),acceptance:t.acceptance.clone(),depends_on:serde_json::from_value(source.get("depends_on").cloned().unwrap_or_else(||json!([])))?,review:t.review.clone(),reason:t.reason.clone()})}).collect::<Result<Vec<_>>>()?;let work=super::project_work::ProjectWork{schema:"project-work/1".into(),project_id:p.id.clone(),action_id:action_id.clone(),stage:super::project_work::ProjectStage::Repair,document:super::project_work::ConfirmedDocument{version:d.version,sha256:d.sha256.clone(),text:d.text.clone()},tasks,target_task:None,repair_reason:repair_reason,repair_round:round,test_spec:Some(spec),artifact:Some(artifact)};let run=self.engine.as_mut().context("engine unavailable")?.store.dispatch_project(&crate::config::Actor::Local(p.id.clone()),&work,timestamp())?;self.store.db.execute("INSERT INTO project_settings(key,value) VALUES(?1,?2)",params![format!("action:{}",action_id),run.id])?;p.actions.push(ProjectActionSnapshot{id:action_id,kind:kind.into(),state:"requested".into(),session:None,detail:"已持久派工返修；复用冻结测试规范，等待签名Runner/AO项目就绪，不代表模型已执行".into()});p.phase="repairing".into();p.blockers.clear();return self.save(p)}if kind=="test"&&self.config.is_some(){let path=self.config.clone().context("host config missing")?;if self.engine.is_none(){self.engine=Some(ProjectEngine::open(&path)?);}let action_id=uuid::Uuid::new_v4().to_string();let d=p.document.as_ref().context("confirmed document required")?;ensure!(p.phase=="developed"&&!p.tasks.is_empty(),"developed project with approved tasks required");let load=|key:String|->Result<Option<super::project_work::FrozenInput>>{let raw:Option<String>=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[key],|r|r.get(0)).optional()?;raw.map(|raw|{let value:Value=serde_json::from_str(&raw)?;ensure!(value["document_version"]==d.version&&value["document_sha256"]==d.sha256,"frozen project input document identity mismatch");let input:super::project_work::FrozenInput=serde_json::from_value(value["input"].clone())?;ensure!(input.sha256==hex::encode(Sha256::digest(input.text.as_bytes())),"frozen project input hash mismatch");Ok(input)}).transpose()};let spec=load(format!("test-spec:{}",p.id))?.unwrap_or(frozen_spec(&p)?);let artifact=load(format!("test-artifact:{}",p.id))?.unwrap_or(frozen_artifact(&p,&self.state)?);let tx=self.store.db.transaction_with_behavior(TransactionBehavior::Immediate)?;for(key,input)in [(format!("test-spec:{}",p.id),&spec),(format!("test-artifact:{}",p.id),&artifact)]{let value=json!({"document_version":d.version,"document_sha256":d.sha256,"input":input});tx.execute("INSERT INTO project_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO NOTHING",params![key,value.to_string()])?;}tx.commit()?;let raw:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[format!("tasks:{}",p.id)],|r|r.get(0)).context("split task input missing")?;let inputs:Value=serde_json::from_str(&raw)?;let tasks=p.tasks.iter().map(|t|{let source=inputs.as_array().and_then(|items|items.iter().find(|item|item["id"]==t.id)).context("split task identity missing")?;Ok(super::project_work::WorkTask{id:t.id.clone(),title:t.title.clone(),description:t.description.clone(),acceptance:t.acceptance.clone(),depends_on:serde_json::from_value(source.get("depends_on").cloned().unwrap_or_else(||json!([])))?,review:t.review.clone(),reason:t.reason.clone()})}).collect::<Result<Vec<_>>>()?;let work=super::project_work::ProjectWork{schema:"project-work/1".into(),project_id:p.id.clone(),action_id:action_id.clone(),stage:super::project_work::ProjectStage::Test,document:super::project_work::ConfirmedDocument{version:d.version,sha256:d.sha256.clone(),text:d.text.clone()},tasks,target_task:None,repair_reason:None,repair_round:0,test_spec:Some(spec),artifact:Some(artifact)};let run=self.engine.as_mut().context("engine unavailable")?.store.dispatch_project(&crate::config::Actor::Local(p.id.clone()),&work,timestamp())?;self.store.db.execute("INSERT INTO project_settings(key,value) VALUES(?1,?2)",params![format!("action:{}",action_id),run.id])?;p.actions.push(ProjectActionSnapshot{id:action_id,kind:kind.into(),state:"requested".into(),session:None,detail:"已持久化独立测试规范和产物身份；等待签名Runner/AO项目就绪，不代表模型已执行".into()});p.phase="testing".into();p.blockers.clear();return self.save(p)}if kind=="develop"&&self.config.is_some(){let path=self.config.clone().context("host config missing")?;if self.engine.is_none(){self.engine=Some(ProjectEngine::open(&path)?);}let action_id=uuid::Uuid::new_v4().to_string();let d=p.document.as_ref().context("confirmed document required")?;let raw:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[format!("tasks:{}",p.id)],|r|r.get(0)).context("split task input missing")?;let inputs:Value=serde_json::from_str(&raw)?;let tasks=p.tasks.iter().map(|t|{let source=inputs.as_array().and_then(|items|items.iter().find(|item|item["id"]==t.id)).context("split task identity missing")?;Ok(super::project_work::WorkTask{id:t.id.clone(),title:t.title.clone(),description:t.description.clone(),acceptance:t.acceptance.clone(),depends_on:serde_json::from_value(source.get("depends_on").cloned().unwrap_or_else(||json!([])))?,review:t.review.clone(),reason:t.reason.clone()})}).collect::<Result<Vec<_>>>()?;let work=super::project_work::ProjectWork{schema:"project-work/1".into(),project_id:p.id.clone(),action_id:action_id.clone(),stage:super::project_work::ProjectStage::Develop,document:super::project_work::ConfirmedDocument{version:d.version,sha256:d.sha256.clone(),text:d.text.clone()},tasks,target_task:None,repair_reason:None,repair_round:0,test_spec:None,artifact:None};let run=self.engine.as_mut().context("engine unavailable")?.store.dispatch_project(&crate::config::Actor::Local(p.id.clone()),&work,timestamp())?;self.store.db.execute("INSERT INTO project_settings(key,value) VALUES(?1,?2)",params![format!("action:{}",action_id),run.id])?;p.actions.push(ProjectActionSnapshot{id:action_id,kind:kind.into(),state:"requested".into(),session:None,detail:"已持久派工；等待签名Runner/AO项目就绪，不代表模型已执行".into()});p.phase="developing".into();p.blockers.clear();return self.save(p)}let message=if self.config.is_none(){"未配置真实执行主机，且完整文档执行链尚未实现；仅加载 AO/Reason/节点配置不能启用执行。导入/确认不会调用模型。"}else{"主机配置已加载但尚未建立文档任务 Runner 的实际就绪与报告接线；未发送模型请求，请保留项目并等待执行链路完成。"};p.blockers=vec![message.into()];p.actions.push(ProjectActionSnapshot{id:uuid::Uuid::new_v4().to_string(),kind:kind.into(),state:"blocked".into(),session:None,detail:message.into()});self.save(p)?;anyhow::bail!("{message}")}
  }self.save(p)
 }
 pub fn tick(&mut self)->Result<()> {
  if self.engine.is_none(){let path=self.config.as_ref().context("host config unavailable")?;self.engine=Some(ProjectEngine::open(path)?);}
  let mut engine=self.engine.take().context("project engine unavailable")?;
  let transport=engine.tick();
  let result=(||->Result<()> {
   for mut p in self.items.clone(){
    let artifact_project=p.clone();
    let mut changed=false;let mut settings:Vec<(String,String)>=vec![];
    for a in &mut p.actions {
     if !matches!(a.state.as_str(),"requested"|"running"|"unknown"|"report_pending_exit"){continue}
     let run_id:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[format!("action:{}",a.id)],|r|r.get(0))?;
     let run=engine.store.run(&run_id)?;a.session=run.session.clone();a.state=if run.status=="unknown"{"unknown"}else{"running"}.into();a.detail=format!("Controller run {}: {}",run.id,run.status);
     if let Some(result)=run.detail.get("report").and_then(|r|r.get("project_result")) {
      let d=p.document.as_ref().context("document absent")?;
      ensure!(result["project_id"]==p.id&&result["action_id"]==a.id&&result["document_version"]==d.version&&result["document_sha256"]==d.sha256,"project stage report scope mismatch");
      if a.kind=="develop" {
       let package=run.detail.get("report").and_then(|r|r.get("artifact")).context("verified develop artifact package missing")?;
       let released=engine.store.assignment(&run.id,run.attempt)?.is_some_and(|assignment|assignment.released);
       if released { materialize_artifact(&self.state,&artifact_project,a,package)?; a.state="succeeded".into(); a.detail="真实产物包已验证、写入托管产品目录且执行退出已独立证明".into(); p.phase="developed".into(); p.blockers.clear(); } else { a.state="report_pending_exit".into(); a.detail="真实产物包已读取；执行退出未获独立证明，保留名额，禁止投影完成".into(); p.blockers=vec![a.detail.clone()]; }
      } else if a.kind=="repair" {
       let package=run.detail.get("report").and_then(|r|r.get("artifact")).context("verified repair artifact package missing")?;
       let released=engine.store.assignment(&run.id,run.attempt)?.is_some_and(|assignment| assignment.released);
       if released { materialize_artifact(&self.state,&artifact_project,a,package)?; let fresh_artifact=frozen_artifact(&artifact_project,&self.state)?; let document=artifact_project.document.as_ref().context("document absent")?; settings.push((format!("test-artifact:{}",p.id),json!({"document_version":document.version,"document_sha256":document.sha256,"input":fresh_artifact}).to_string())); a.state="succeeded".into(); a.detail="返修产物包已验证、写入托管产品目录且执行退出已独立证明；等待显式复测".into(); p.phase="developed".into(); p.blockers.clear(); } else { a.state="report_pending_exit".into(); a.detail="返修产物包已读取；执行退出未获独立证明，保留名额，禁止投影完成".into(); p.blockers=vec![a.detail.clone()]; }
      } else if a.kind=="redo" {
       let work=run.detail.get("project_work").context("redo project work missing")?;
       ensure!(result["stage"]=="redo"&&work["stage"]=="redo","redo stage mismatch");
       let target=work["target_task"].as_str().context("redo target missing")?;
       let expected=work["tasks"].as_array().context("redo task closure missing")?;
       let expected_ids:std::collections::BTreeSet<String>=expected.iter().map(|task|task["id"].as_str().map(str::to_owned).context("redo task id missing")).collect::<Result<_>>()?;
       ensure!(!expected_ids.is_empty()&&expected_ids.contains(target),"redo task closure invalid");
       let reported=result["tasks"].as_array().context("redo tasks missing")?;
       let mut reported_ids=std::collections::BTreeSet::new();
       for task in reported {let id=task["id"].as_str().context("redo result task id missing")?;ensure!(expected_ids.contains(id)&&reported_ids.insert(id.to_owned()),"redo result escaped target closure or duplicated task");let depends=task["depends_on"].as_array().context("redo result dependencies missing")?;ensure!(depends.iter().all(|dependency|dependency.as_str().is_some_and(|dependency|dependency!=id&&expected_ids.contains(dependency))),"redo result dependency escaped closure or self-dependency");}
       ensure!(reported_ids==expected_ids,"redo result must cover exact target dependency closure");
       let released=engine.store.assignment(&run.id,run.attempt)?.is_some_and(|assignment|assignment.released);
       if released {let raw:String=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[format!("tasks:{}",p.id)],|r|r.get(0)).context("split task input missing")?;let mut inputs:Value=serde_json::from_str(&raw)?;for reported_task in reported{let id=reported_task["id"].as_str().context("redo result task id missing")?;let title=reported_task["title"].as_str().context("redo result task title missing")?;let description=reported_task["description"].as_str().context("redo result task description missing")?;let acceptance=reported_task["acceptance"].as_str().context("redo result task acceptance missing")?;let input=inputs.as_array_mut().context("split task inputs must be an array")?.iter_mut().find(|input|input["id"]==id).context("redo task input missing")?;input["title"]=json!(title);input["description"]=json!(description);input["acceptance"]=json!(acceptance);input["depends_on"]=reported_task["depends_on"].clone();let task=p.tasks.iter_mut().find(|task|task.id==id).context("redo task snapshot missing")?;task.title=title.into();task.description=description.into();task.acceptance=acceptance.into();task.review="pending".into();if id!=target{task.reason=None;}}settings.push((format!("tasks:{}",p.id),serde_json::to_string(&inputs)?));a.state="succeeded".into();a.detail="重做结果与目标依赖闭包已验证且执行退出已独立证明；新任务内容已持久化，等待逐项复审".into();p.phase="task_review".into();p.blockers.clear();} else {a.state="report_pending_exit".into();a.detail="重做结果已读取；执行退出未获独立证明，保留名额，禁止投影复审完成".into();p.blockers=vec![a.detail.clone()];}
      } else if a.kind=="test" {
       let work=run.detail.get("project_work").context("test project work missing")?;
       let spec_hash=result["test_spec_sha256"].as_str().context("test specification identity missing")?;
       let artifact_hash=result["artifact_sha256"].as_str().context("test artifact identity missing")?;
       ensure!(spec_hash==work["test_spec"]["sha256"]&&artifact_hash==work["artifact"]["sha256"],"test result input identity mismatch");
       let passed=result["passed"].as_bool().context("test result outcome missing")?;
       let released=engine.store.assignment(&run.id,run.attempt)?.is_some_and(|assignment|assignment.released);
       if released { a.state="succeeded".into(); a.detail=if passed{"独立测试报告与退出证据均已验证；等待人工验收".into()}else{"独立测试失败与退出证据均已验证；等待显式返修".into()}; p.phase=if passed{"awaiting_acceptance"}else{"test_failed"}.into(); p.blockers.clear(); } else { a.state="report_pending_exit".into(); a.detail="独立测试报告已读取；执行退出未获独立证明，保留名额，禁止投影结论".into(); p.blockers=vec![a.detail.clone()]; }
      } else if a.kind=="split" {
       let key=format!("ingested:{}",a.id);let digest=hex::encode(Sha256::digest(serde_json::to_vec(result)?));
       let prior:Option<String>=self.store.db.query_row("SELECT value FROM project_settings WHERE key=?1",[&key],|r|r.get(0)).optional()?;
       if let Some(prior)=prior {ensure!(prior==digest,"split result changed after user review; retained previous task decisions");}
       else {
        let tasks=result["tasks"].as_array().context("split tasks missing")?;
        p.tasks=tasks.iter().map(|t|Ok(TaskSnapshot{id:t["id"].as_str().context("task id absent")?.into(),title:t["title"].as_str().context("task title absent")?.into(),description:t["description"].as_str().context("task description absent")?.into(),acceptance:t["acceptance"].as_str().context("task acceptance absent")?.into(),review:"pending".into(),reason:None})).collect::<Result<_>>()?;
        settings.push((format!("tasks:{}",p.id),serde_json::to_string(tasks)?));settings.push((key,digest));p.phase="task_review".into();
       }
       let released=engine.store.assignment(&run.id,run.attempt)?.is_some_and(|a|a.released);
       a.state=if released{"succeeded"}else{"report_pending_exit"}.into();a.detail=if released{"真实拆分结果已验证且执行退出"}else{"真实拆分结果已读取；执行退出未获独立证明，保留名额，禁止后续开工"}.into();p.blockers=if released{vec![]}else{vec![a.detail.clone()]};
      }
     }
     changed=true;
    }
    if changed {
     project(&mut p);
     // Result identity, dependency input and initial task reviews commit together.
     let tx=self.store.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
     for(key,value)in settings{tx.execute("INSERT INTO project_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![key,value])?;}
     tx.execute("UPDATE projects SET body=?1 WHERE id=?2",params![serde_json::to_string(&p)?,p.id])?;tx.commit()?;
     if let Some(old)=self.items.iter_mut().find(|old|old.id==p.id){*old=p;}
    }
   }Ok(())
  })();self.engine=Some(engine);result?;
  for item in &mut self.items {
   if transport.is_err(){
    if !item.blockers.iter().any(|blocker|blocker==TRANSPORT_BLOCKER){item.blockers.push(TRANSPORT_BLOCKER.into());}
   }else{item.blockers.retain(|blocker|blocker!=TRANSPORT_BLOCKER);}
   project(item);
  }
  transport
 }
 pub fn unfinished(&self)->usize{self.items.iter().flat_map(|p|&p.actions).filter(|a|matches!(a.state.as_str(),"requested"|"running"|"unknown"|"report_pending_exit")).count()}
}
pub fn is_project_command(c:&UiCommand)->bool{use UiCommand::*;matches!(c,ProjectCreate{..}|ProjectImport{..}|ProjectConfirm{..}|ProjectSplit{..}|TaskReview{..}|TaskRedo{..}|ProjectStart{..}|ProjectTest{..}|ProjectRepair{..}|ProjectAccept{..}|ProjectRefresh{..}|ProjectConfigure{..})}

#[cfg(test)]
mod readiness_tests {
    use super::*;
    use crate::{config::{NodeSpec, ServiceLock}, core::protocol::{Envelope, PROTOCOL}};

    #[test]
    fn failed_reason_inbox_does_not_assign_queued_work() {
        let temp = tempfile::tempdir().unwrap();
        let node = NodeSpec {
            id: "B".into(), address: "owned-B/runner".into(), key_hex: "07".repeat(32),
            slots: 1, admitted: true, healthy: false, draining: false,
            templates: vec!["sum-v1".into()],
        };
        let missing = if cfg!(windows) {
            format!(r"\\.\pipe\missing-ordering-{}", uuid::Uuid::new_v4())
        } else {
            temp.path().join("missing-reason.sock").display().to_string()
        };
        let mut engine = ProjectEngine {
            store: Store::open(&temp.path().join("controller.sqlite"), "ordering-test", &[node]).unwrap(),
            reason: crate::adapters::reason::Reason::new(missing).unwrap(),
            endpoint: "owned-A/controller".into(),
            _lock: ServiceLock::acquire(temp.path(), "controller").unwrap(),
        };
        let now = timestamp();
        engine.store.probe_nodes(now).unwrap();
        let (_, probe) = engine.store.pending_outbox(10).unwrap().into_iter()
            .find(|(_, message)| message.kind == "probe").unwrap();
        let mut ready = Envelope {
            schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
            deployment: probe.deployment.clone(), from: "B".into(), to: "A".into(),
            run_id: probe.run_id.clone(), attempt: probe.attempt, epoch: probe.epoch.clone(),
            kind: "ready".into(), sent_at: now, expires_at: now + 60,
            payload: json!({"address":"owned-B/runner","templates":["sum-v1"],"ao_project_ready":true}),
            mac: String::new(),
        };
        ready.sign(&hex::decode("07".repeat(32)).unwrap()).unwrap();
        engine.store.receive(&ready, now).unwrap();
        assert!(engine.store.readiness(now).unwrap()[0].healthy);
        let run = engine.store.dispatch(&Actor::Local("queued-owner".into()), "sum-v1", now).unwrap();
        let error = engine.tick().expect_err("missing Reason transport must be observable");
        assert!(error.to_string().contains("project transport unavailable"));
        let assignment = engine.store.assignment(&run.id, run.attempt).unwrap();
        assert!(assignment.is_none(), "failed inbox must invalidate readiness before assignment: {assignment:?}");
        assert_eq!(engine.store.run(&run.id).unwrap().status, "queued");
        assert!(!engine.store.readiness(now).unwrap()[0].healthy);
    }
}
