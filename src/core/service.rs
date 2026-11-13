//! Typed, bounded, nonblocking GUI bridge; domain truth stays in the worker's core.
use std::{fmt,path::PathBuf,sync::Arc};
use crate::config::{Mode,Actor};
use super::store::Run;
pub use super::projects::{ProjectSnapshot,DocumentSnapshot,TaskSnapshot,ProjectActionSnapshot};
pub struct ServiceOptions {pub mode:Mode,pub state_dir:PathBuf,pub worker_exe:PathBuf,pub gateway_key:Option<Vec<u8>>,pub gateway_model_token:Option<Vec<u8>>}
impl fmt::Debug for ServiceOptions {
 fn fmt(&self, formatter:&mut fmt::Formatter<'_>)->fmt::Result {
  formatter.debug_struct("ServiceOptions")
   .field("mode",&self.mode)
   .field("state_dir",&self.state_dir)
   .field("worker_exe",&self.worker_exe)
   .field("gateway_key",&self.gateway_key.as_ref().map(|_| "[REDACTED]"))
   .field("gateway_model_token",&self.gateway_model_token.as_ref().map(|_| "[REDACTED]"))
   .finish()
 }
}
impl Drop for ServiceOptions { fn drop(&mut self) { if let Some(key)=self.gateway_key.as_mut() { key.fill(0); } if let Some(token)=self.gateway_model_token.as_mut() { token.fill(0); } } }
#[derive(Clone, Debug)]
pub enum UiCommand {
 GatewayConfigure{config_path:PathBuf},GatewayDispatch{worker_id:String},GatewayDispatchProject{worker_id:String,doc_path:PathBuf},
 StartLocal,Dispatch{template:String},Status{run_id:String},Approvals{run_id:String},
 Decide{run_id:String,attempt:u32,session:String,request_id:String,generation:u64,scope_hash:String,decision_id:String},
 Report{run_id:String},Retry{run_id:String,attempt:u32},Cancel{run_id:String},
 ProjectCreate{name:String},ProjectImport{project_id:String,path:PathBuf},ProjectConfirm{project_id:String,version:u32,sha256:String},ProjectSplit{project_id:String},TaskReview{project_id:String,task_id:String,approve:bool,reason:String},TaskRedo{project_id:String,task_id:String},ProjectStart{project_id:String},ProjectTest{project_id:String},ProjectRepair{project_id:String,reason:String},ProjectAccept{project_id:String},ProjectRefresh{project_id:String},ProjectConfigure{config_path:PathBuf},
}
#[derive(Clone, Debug, PartialEq, Eq)] pub enum SubmitError{Busy,Stopped}
#[derive(Clone, Debug)] pub struct ServiceError{pub code:String,pub message:String,pub next_step:String}
#[derive(Clone, Debug)] pub struct CommandResult{pub request_id:u64,pub result:Result<serde_json::Value,ServiceError>}
#[derive(Clone, Debug, PartialEq, Eq)] pub enum Readiness{NotStarted,Starting,Ready,NotReady,Stopped}
#[derive(Clone, Debug)] pub struct DependencyView{pub id:String,pub state:String,pub detail:String,pub source:String,pub observed_at:u64}
#[derive(Clone, Debug)] pub struct RunView{pub run:Run,pub read_only:bool,pub can_status:bool,pub can_approvals:bool,pub can_decide:bool,pub can_report:bool,pub can_retry:bool,pub can_cancel:bool,pub disabled_reason:Option<String>}
#[derive(Clone, Debug)] pub struct NodeView{pub id:String,pub address:String,pub used:u32,pub slots:u32,pub admitted:bool,pub healthy:bool,pub draining:bool,pub source:String,pub observed_at:u64}
#[derive(Clone, Debug, PartialEq, Eq)] pub enum DecisionKind{AllowOnce,RejectOnce}
#[derive(Clone, Debug)] pub struct OfferedDecision{pub id:String,pub kind:DecisionKind,pub label:String}
#[derive(Clone, Debug)] pub struct ApprovalView{pub run_id:String,pub attempt:u32,pub session:String,pub request_id:String,pub generation:u64,pub scope_hash:String,pub expires_at:u64,pub offered:Vec<OfferedDecision>,pub summary:String,pub simulated:bool,pub can_decide:bool}
#[derive(Clone, Debug)] pub struct GatewayView { pub worker_id:String,pub host:String,pub remote_verified:bool,pub used_slots:u32,pub artifact_sha256:String }
#[derive(Clone, Debug)] pub struct AppSnapshot{pub mode:Mode,pub role:String,pub readiness:Readiness,pub actor:Option<Actor>,pub observed_at:u64,pub running:bool,pub dependencies:Vec<DependencyView>,pub runs:Vec<RunView>,pub nodes:Vec<NodeView>,pub approvals:Vec<ApprovalView>,pub errors:Vec<ServiceError>,pub unfinished:usize,pub projects:Vec<ProjectSnapshot>,pub gateway:Option<GatewayView>}
use std::{sync::{Mutex,atomic::{AtomicBool,AtomicUsize,Ordering},mpsc::{self,SyncSender,Receiver,TrySendError}},time::{Duration,SystemTime,UNIX_EPOCH},io::{Read,Write},path::Path};
use anyhow::{Result,Context,ensure};
use serde_json::{Value,json};
use crate::{config::{Config,NodeSpec,ServiceLock,valid_id},adapters::local::{LocalAo,pump_local}};
use super::{controller::ControlService,runner::Runner,store::Store};
const BOUND:usize=32;
struct Accepted {id:u64,command:UiCommand}
struct Gate {next:u64,closed:bool}
struct Shared {gate:Mutex<Gate>,credits:AtomicUsize,stop:AtomicBool,snapshot:Mutex<Arc<AppSnapshot>>}
pub struct ServiceHandle {commands:SyncSender<Accepted>,results:Receiver<CommandResult>,shared:Arc<Shared>,cached:Arc<AppSnapshot>}
fn clock()->u64 {SystemTime::now().duration_since(UNIX_EPOCH).map_or(0,|d|d.as_secs())}
fn error(code:&str,message:&str,next:&str)->ServiceError {ServiceError{code:code.into(),message:message.into(),next_step:next.into()}}
fn stopped()->ServiceError {error("stopped","后台服务已停止；此请求未执行","重新打开服务；关闭窗口不代表取消任务")}
fn initial(mode:Mode)->AppSnapshot {
 let real=mode==Mode::Real;
 AppSnapshot{mode,role:"controller + local fixture runner".into(),readiness:if real{Readiness::NotReady}else{Readiness::NotStarted},actor:None,observed_at:clock(),running:true,dependencies:vec![DependencyView{id:if real{"real-ports"}else{"local-fixture"}.into(),state:if real{"not_ready"}else{"not_started"}.into(),detail:if real{"真实 AO/Reason/模型尚未接通；不自动回退本地"}else{"本地体验未启动；无真实 AO/Reason/模型"}.into(),source:"explicit configured mode; no external probe".into(),observed_at:0}],runs:vec![],nodes:vec![],approvals:vec![],errors:vec![],unfinished:0,projects:vec![],gateway:None}
}
impl ServiceHandle {
 pub fn spawn(options:ServiceOptions,repaint:Arc<dyn Fn()+Send+Sync>)->Result<Self> {
  let cached=Arc::new(initial(options.mode.clone()));
  let shared=Arc::new(Shared{gate:Mutex::new(Gate{next:1,closed:false}),credits:AtomicUsize::new(0),stop:AtomicBool::new(false),snapshot:Mutex::new(cached.clone())});
  let(commands,rx)=mpsc::sync_channel(BOUND);let(tx,results)=mpsc::sync_channel(BOUND);
  let state=shared.clone();std::thread::Builder::new().name("native-core-service".into()).spawn(move||worker(options,state,rx,tx,repaint))?;
  Ok(Self{commands,results,shared,cached})
 }
 pub fn try_submit(&self,command:UiCommand)->std::result::Result<u64,SubmitError>{
  if self.shared.stop.load(Ordering::Acquire){return Err(SubmitError::Stopped)}
  let mut gate=self.shared.gate.try_lock().map_err(|_|SubmitError::Busy)?;
  if gate.closed||self.shared.stop.load(Ordering::Acquire){return Err(SubmitError::Stopped)}
  if self.shared.credits.try_update(Ordering::AcqRel,Ordering::Acquire,|n|(n<BOUND).then_some(n+1)).is_err(){return Err(SubmitError::Busy)}
  let id=gate.next;
  let Some(next)=id.checked_add(1) else{self.shared.credits.fetch_sub(1,Ordering::AcqRel);return Err(SubmitError::Stopped)};
  match self.commands.try_send(Accepted{id,command}) {
   Ok(())=>{gate.next=next;Ok(id)},
   Err(e)=>{self.shared.credits.fetch_sub(1,Ordering::AcqRel);Err(match e{TrySendError::Full(_)=>SubmitError::Busy,TrySendError::Disconnected(_)=>SubmitError::Stopped})}
  }
 }
 pub fn drain_results(&mut self)->Vec<CommandResult>{let results:Vec<_>=self.results.try_iter().collect();self.shared.credits.fetch_sub(results.len(),Ordering::AcqRel);results}
 pub fn snapshot(&mut self)->Arc<AppSnapshot>{if let Ok(snapshot)=self.shared.snapshot.try_lock(){self.cached=snapshot.clone();}self.cached.clone()}
 pub fn stop(&self){self.shared.stop.store(true,Ordering::Release);}
}
impl Drop for ServiceHandle {fn drop(&mut self){self.stop();}}
struct LocalRuntime {control:ControlService,runner:Runner,ao:LocalAo,_locks:Vec<ServiceLock>,observed_at:u64}
fn bounded_file(path:&Path,max:usize)->Result<Vec<u8>> {let mut raw=Vec::new();std::fs::File::open(path)?.take((max+1) as u64).read_to_end(&mut raw)?;ensure!(raw.len()<=max,"local identity size bound");Ok(raw)}
fn create_file(path:&Path,bytes:&[u8])->Result<()> {let mut file=std::fs::OpenOptions::new().write(true).create_new(true).open(path)?;file.write_all(bytes)?;file.sync_all()?;Ok(())}
impl LocalRuntime {
 fn open(options:&ServiceOptions)->Result<Self>{
  ensure!(options.mode==Mode::Local,"explicit local mode required");
  ensure!(options.worker_exe.is_file(),"bundled Rust fixture missing");
  let config=Config{schema:"ao-reason-config/1".into(),mode:Mode::Local,role:"controller".into(),deployment:"native-local".into(),state_dir:options.state_dir.clone(),nodes:vec![],ao:None,reason_endpoint:None,endpoint:None,runner_node:None,controller_address:None,workspace_root:None};
  let state=config.prepare_state(Path::new("."))?;
  let controller=ServiceLock::acquire(&state,"controller")?;let runner=ServiceLock::acquire(&state,"runner")?;
  let _identity=ServiceLock::acquire(&state,"identity")?;
  let host_path=state.join("local-host.json");
  let actor=if host_path.exists(){
   let host:Value=serde_json::from_slice(&bounded_file(&host_path,1024)?)?;
   ensure!(host["schema"]=="native-local-host/1"&&host.as_object().is_some_and(|o|o.len()==2),"invalid local host identity");
   let id=host["actor"].as_str().context("missing local actor")?;ensure!(valid_id(id),"invalid local actor");Actor::Local(id.into())
  }else{let id=uuid::Uuid::new_v4().to_string();create_file(&host_path,&serde_json::to_vec(&json!({"schema":"native-local-host/1","actor":id}))?)?;Actor::Local(id)};
  let key_path=state.join("local-node.key");
  if !key_path.exists(){let key=format!("{}{}",uuid::Uuid::new_v4().simple(),uuid::Uuid::new_v4().simple());create_file(&key_path,key.as_bytes())?;}
  let key_hex=String::from_utf8(bounded_file(&key_path,64)?)?;
  let node=NodeSpec{id:"local-B".into(),address:"local://fixture-B".into(),key_hex,slots:1,admitted:true,healthy:true,draining:false,templates:vec!["sum-native-v1".into(),"control-native-v1".into()]};node.validate()?;
  let store=Store::open(&state.join("controller.sqlite"),"native-local",std::slice::from_ref(&node))?;
  let runner_db=Runner::open(&state.join("runner.sqlite"),"native-local",node)?;
  let ao=LocalAo::new(&options.worker_exe,&state.join("fixtures"))?;
  Ok(Self{control:ControlService::new(store,actor),runner:runner_db,ao,_locks:vec![controller,runner],observed_at:clock()})
 }
 fn command(&mut self,command:UiCommand,now:u64)->Result<Value>{
  let(tool,args)=match command {
   UiCommand::StartLocal=>return Ok(json!({"state":"ready","simulated":true})),
   UiCommand::Dispatch{template}=>{ensure!(matches!(template.as_str(),"sum-native-v1"|"control-native-v1"),"real template unavailable in local mode");("dispatch",json!({"template":template}))},
   UiCommand::Status{run_id}=>("status",json!({"run_id":run_id})),
   UiCommand::Approvals{run_id}=>("approvals",json!({"run_id":run_id})),
   UiCommand::Report{run_id}=>("report",json!({"run_id":run_id})),
   UiCommand::Retry{run_id,attempt}=>("retry",json!({"run_id":run_id,"attempt":attempt})),
   UiCommand::Cancel{run_id}=>("cancel",json!({"run_id":run_id})),
   UiCommand::Decide{run_id,attempt,session,request_id,generation,scope_hash,decision_id}=>{
    let actor=self.control.actor().clone();
    let decision=super::controller::Decision{run_id,request_id,generation,scope_hash,decision_id};
    return super::controller::decide_with_binding(&mut self.control.store,&actor,&decision,Some((attempt,&session)),now)
   }
   _=>anyhow::bail!("project command belongs to project workflow"),
  };self.control.call(tool,args,now)
 }
 fn project(&self,snapshot:&mut AppSnapshot,now:u64)->Result<()> {
  let store=&self.control.store;let actor=self.control.actor();let mut runs=Vec::new();let mut approvals=Vec::new();
  for run in store.list_runs(200)? {
   let owned=run.owner==*actor;let terminal=matches!(run.status.as_str(),"done"|"failed"|"cancelled");
   let assignment=store.assignment(&run.id,run.attempt)?;
   let can_retry=owned&&matches!(run.status.as_str(),"failed"|"cancelled")&&run.detail["retry_safe"]==true&&assignment.as_ref().is_none_or(|a|a.released);
   let pending=if owned {store.approvals(actor,&run.id,now)?} else {Vec::new()};let mut can_decide=false;
   for p in pending.into_iter().take(400_usize.saturating_sub(approvals.len())) {
    let offered=p["offered"].as_array().context("approval decisions missing")?.iter().map(|v|{let id=v.as_str().context("invalid offered decision")?;let(kind,label)=match id{"allow_once"=>(DecisionKind::AllowOnce,"仅允许本次"),"reject_once"=>(DecisionKind::RejectOnce,"仅拒绝本次"),_=>anyhow::bail!("unsupported decision")};Ok(OfferedDecision{id:id.into(),kind,label:label.into()})}).collect::<Result<Vec<_>>>()?;
    can_decide=true;
    approvals.push(ApprovalView{run_id:run.id.clone(),attempt:run.attempt,session:run.session.clone().context("approval session missing")?,request_id:p["request_id"].as_str().context("request missing")?.into(),generation:p["generation"].as_u64().context("generation missing")?,scope_hash:p["scope_hash"].as_str().context("scope missing")?.into(),expires_at:p["expires_at"].as_u64().context("expiry missing")?,offered,summary:p["summary"].as_str().unwrap_or("模拟审批").chars().take(1024).collect(),simulated:true,can_decide:owned&&now<run.deadline});
   }
   let reason=if !owned{Some("任务属于其他可信入口，只读".into())}else if terminal{Some("已终止；重试仍需精确退出证据".into())}else if run.status=="unknown"{Some("执行结果未知；保留名额，不能自动重跑".into())}else{None};
   runs.push(RunView{read_only:!owned,can_status:owned,can_approvals:owned&&can_decide,can_decide:owned&&can_decide&&now<run.deadline,can_report:owned&&(terminal||run.detail.get("report").is_some()||run.detail.get("failure").is_some()),can_retry,can_cancel:owned&&!terminal&&run.status!="cancel_pending",disabled_reason:reason,run});
  }
  let nodes=store.nodes.iter().map(|n|{let used:i64=store.db.query_row("SELECT count(*) FROM assignments WHERE node=?1 AND released=0",[&n.id],|r|r.get(0))?;Ok(NodeView{id:n.id.clone(),address:n.address.clone(),used:u32::try_from(used)?,slots:n.slots,admitted:n.admitted,healthy:n.healthy,draining:n.draining,source:"explicit local fixture configuration; not real readiness".into(),observed_at:self.observed_at})}).collect::<Result<Vec<_>>>()?;
  let unfinished:i64=store.db.query_row("SELECT count(*) FROM runs WHERE status NOT IN ('done','failed','cancelled') OR EXISTS(SELECT 1 FROM assignments a WHERE a.run=runs.id AND a.released=0)",[],|r|r.get(0))?;
  snapshot.actor=Some(actor.clone());snapshot.runs=runs;snapshot.approvals=approvals;snapshot.nodes=nodes;snapshot.unfinished=usize::try_from(unfinished)?;Ok(())
 }
}
fn push_error(snapshot:&mut AppSnapshot,e:ServiceError){if snapshot.errors.last().is_none_or(|old|old.code!=e.code||old.message!=e.message){snapshot.errors.push(e);if snapshot.errors.len()>20{snapshot.errors.remove(0);}}}
fn repaint_safely(repaint:&Arc<dyn Fn()+Send+Sync>){let _=std::panic::catch_unwind(std::panic::AssertUnwindSafe(||repaint()));}
fn publish(shared:&Shared,snapshot:&AppSnapshot,repaint:&Arc<dyn Fn()+Send+Sync>,last:&mut String){
 let mut compared=snapshot.clone();compared.observed_at=0;let fingerprint=format!("{compared:?}");
 if *last!=fingerprint{*last=fingerprint;let mut value=snapshot.clone();value.observed_at=clock();if let Ok(mut slot)=shared.snapshot.lock(){*slot=Arc::new(value);}repaint_safely(repaint);}
}
fn worker(mut options:ServiceOptions,shared:Arc<Shared>,rx:Receiver<Accepted>,tx:SyncSender<CommandResult>,repaint:Arc<dyn Fn()+Send+Sync>){
 let mut snapshot=initial(options.mode.clone());let mut runtime:Option<LocalRuntime>=None;let mut gateway:Option<super::ssh_manager::SshManager>=None;let mut projects:Option<super::projects::Projects>=None;let mut last=String::new();
 if options.state_dir.join("project-workflow/controller.sqlite").is_file(){match super::projects::Projects::open(&options.state_dir){Ok(p)=>{snapshot.projects=p.snapshots();snapshot.unfinished=p.unfinished();projects=Some(p)},Err(_)=>push_error(&mut snapshot,error("project_state_unavailable","已有项目账本读取失败；未创建新项目或覆盖旧状态","保留状态目录并检查访问权限及数据库版本"))}}
 loop {
  if shared.stop.load(Ordering::Acquire){
   // Serialize closure with try_submit, so no accepted command can arrive after this drain.
   if let Ok(mut gate)=shared.gate.lock(){gate.closed=true;}
   for request in rx.try_iter(){let _=tx.try_send(CommandResult{request_id:request.id,result:Err(stopped())});}
   drop(runtime.take());snapshot.running=false;snapshot.readiness=Readiness::Stopped;
   for r in &mut snapshot.runs {r.can_status=false;r.can_approvals=false;r.can_decide=false;r.can_report=false;r.can_retry=false;r.can_cancel=false;r.disabled_reason=Some("服务已停止；关闭不等于任务已取消".into());}
   for a in &mut snapshot.approvals{a.can_decide=false;}
   publish(&shared,&snapshot,&repaint,&mut last);repaint_safely(&repaint);return
  }
  match rx.recv_timeout(Duration::from_millis(20)) {
   Ok(request)=>{
    let result=if shared.stop.load(Ordering::Acquire){Err(stopped())}else if matches!(&request.command,UiCommand::GatewayConfigure{..}|UiCommand::GatewayDispatch{..}|UiCommand::GatewayDispatchProject{..}) {
     gateway_command(&mut options,&mut gateway,request.command).map_err(|e|error("ssh_gateway_rejected",&format!("{e:#}"),"核对显式Worker配置及保留名额；UNKNOWN不自动重跑，配置成功不表示远端验证"))
    }else if super::projects::is_project_command(&request.command){
     let action=(||->Result<Value>{if projects.is_none(){projects=Some(super::projects::Projects::open(&options.state_dir)?);}projects.as_mut().context("project state unavailable")?.command(request.command)})();
     action.map_err(|e|error("project_action_rejected",&e.to_string(),"核对上传版本/审核门槛与项目阻塞原因；未执行的阶段不会标为成功"))
    }else if matches!(&request.command,UiCommand::Decide{run_id,..} if options.mode==Mode::Real||projects.as_ref().is_some_and(|p|p.has_run_link(run_id).unwrap_or(true))){
     // Resolve the project route before LocalRuntime. Real mode never falls
     // back to a fixture, even when the host or exact binding is unavailable.
     if let Some(p)=projects.as_mut().filter(|p|p.host_configured()) {
      if let UiCommand::Decide{run_id,attempt,session,request_id,generation,scope_hash,decision_id}=request.command {
       let decision=super::controller::Decision{run_id,request_id,generation,scope_hash,decision_id};
       p.decide(&decision,attempt,&session,clock()).map_err(|e|error("project_decision_rejected",&e.to_string(),"刷新项目审批并核对 owner、action、attempt、session 及当前授权范围；未排队的决定不会生效"))
      }else{unreachable!()}
     }else{Err(error("project_host_unavailable","项目执行主机不可用；未路由到本地替代","配置并打开对应项目主机后刷新审批；不要启动 fixture 代替真实项目"))}
    }else if matches!(request.command,UiCommand::StartLocal)&&runtime.is_none(){
     if options.mode==Mode::Real {Err(error("real_not_ready","真实 AO/Reason/模型尚未接通；不会启动本地替代","完成真实依赖配置和独立验收后再使用真实模式"))}
     else {
      snapshot.readiness=Readiness::Starting;publish(&shared,&snapshot,&repaint,&mut last);
      match LocalRuntime::open(&options){Ok(local)=>{snapshot.dependencies=vec![DependencyView{id:"native-local-fixture".into(),state:"ready".into(),detail:"本地 Rust 子进程 + 独立 verifier；无真实 AO/Reason/模型".into(),source:"worker opened native ledgers and bundled fixture; not a live probe".into(),observed_at:local.observed_at}];runtime=Some(local);snapshot.readiness=Readiness::Ready;Ok(json!({"state":"ready","simulated":true}))},Err(_)=>{snapshot.readiness=Readiness::NotReady;Err(error("state_unavailable","本地原生状态或固定执行器不可用；未报告启动成功","检查目录权限、模式、同目录单实例锁及同版 aw-cli；不要删除账本绕过错误"))}}
     }
    }else if let Some(local)=runtime.as_mut(){local.command(request.command,clock()).map_err(|_|error("action_rejected","核心拒绝此操作；未报告成功","刷新实际状态并核对所属任务、attempt、session、审批范围及退出证据"))}
    else{Err(error("not_started","尚未启动本地体验，或真实模式尚未就绪","显式启动本地体验；真实模式不会自动回退"))};
    if let Err(e)=&result{push_error(&mut snapshot,e.clone());}
    // A result credit was reserved before acceptance. At most BOUND results can be outstanding.
    if tx.try_send(CommandResult{request_id:request.id,result}).is_err(){shared.stop.store(true,Ordering::Release);}
    repaint_safely(&repaint);
   },
   Err(mpsc::RecvTimeoutError::Disconnected)=>{shared.stop.store(true,Ordering::Release);},
   Err(mpsc::RecvTimeoutError::Timeout)=>{},
  }
  if !shared.stop.load(Ordering::Acquire){if let Some(local)=runtime.as_mut(){
   if pump_local(&mut local.control.store,&mut local.runner,&mut local.ao,clock()).is_err(){snapshot.readiness=Readiness::NotReady;push_error(&mut snapshot,error("lifecycle_error","本地执行或持久化失败；未推断退出或释放名额","查看保留的任务状态并人工核对；不要自动重跑 UNKNOWN 任务"));}
   if local.project(&mut snapshot,clock()).is_err(){snapshot.readiness=Readiness::NotReady;push_error(&mut snapshot,error("snapshot_error","核心状态读取失败；保留上次快照","停止操作并检查状态目录；上次快照不代表当前进度"));}
  }}
  // Replace durable project cards on every refresh; stale scope must not stay
  // actionable after an error, expiry, decision, or revoked workflow binding.
  snapshot.approvals.retain(|approval|approval.simulated&&runtime.is_some());
  if let Some(p)=projects.as_mut(){
   if p.host_configured(){
    match p.node_readiness(clock()) {
     Ok(nodes)=>{snapshot.nodes=nodes.into_iter().map(|node|NodeView{id:node.id,address:node.address,used:node.used,slots:node.slots,admitted:node.admitted,healthy:node.healthy,draining:node.draining,source:if node.verified{"signed readiness probe/ready matched current node, deployment, address, epoch and capability".into()}else{"configured real node; readiness not verified or expired".into()},observed_at:node.observed_at}).collect();snapshot.readiness=if !snapshot.nodes.is_empty()&&snapshot.nodes.iter().all(|node|node.healthy){Readiness::Ready}else{Readiness::NotReady};for project in &mut snapshot.projects{if snapshot.readiness!=Readiness::Ready&&!project.blockers.iter().any(|blocker|blocker.contains("readiness")){project.blockers.push("真实节点未通过当前签名 readiness probe/ready；配置有效不等于可派工".into());project.can_split=false;project.can_start=false;project.can_test=false;project.can_repair=false;}}}
     Err(_)=>{snapshot.readiness=Readiness::NotReady;push_error(&mut snapshot,error("readiness_unavailable","真实节点 readiness 尚未验证或状态读取失败；未允许新派工","检查当前 probe/ready、部署、地址、attempt/epoch、有效期和模板能力；不释放既有名额"));}
    }
   }
   if p.host_configured()&&p.tick().is_err(){snapshot.readiness=Readiness::NotReady;for node in &mut snapshot.nodes{node.healthy=false;node.source="project transport unavailable; readiness invalidated".into();}push_error(&mut snapshot,error("project_execution_pending","项目执行端口或结果暂不可用；账本保留，未推断完成或释放名额","刷新核对项目阻塞原因；不要自动重派 UNKNOWN 动作"));}
   match p.approvals(clock()) {
    Ok(approvals)=>snapshot.approvals.extend(approvals),
    Err(_)=>push_error(&mut snapshot,error("project_approvals_unavailable","项目审批读取失败；已移除上次项目审批卡片","保留账本并刷新对应主机；未读取到审批不代表任务完成"))
   }
   snapshot.projects=p.snapshots();if snapshot.readiness!=Readiness::Ready{for project in &mut snapshot.projects{let blocker="真实节点未通过当前签名 readiness probe/ready；配置有效不等于可派工";if !project.blockers.iter().any(|existing|existing==blocker){project.blockers.push(blocker.into());}project.can_split=false;project.can_start=false;project.can_test=false;project.can_repair=false;}}snapshot.unfinished=runtime.as_ref().map_or(0,|_|snapshot.unfinished)+p.unfinished();
  }
  if let Some(manager)=gateway.as_ref() {
   if let Err(e)=project_gateway(manager,&mut snapshot) {push_error(&mut snapshot,error("gateway_snapshot_failed",&e.to_string(),"保留账本并检查实际执行证据"));}
  }
  publish(&shared,&snapshot,&repaint,&mut last);
 }
}

/// Append one manager-local diagnostic line to `<state_dir>/manager-diag.log`.
/// Shape: `<unix_ts>\t<event>\t<detail>` — ids, statuses and paths only, never
/// secrets. Tabs/newlines in event or detail are rejected so a field value can
/// never forge extra lines or extra columns.
pub fn append_diag(state_dir:&Path,event:&str,detail:&str)->Result<()> {
 ensure!(!event.is_empty()&&event.chars().all(|c|c.is_ascii_alphanumeric()||c=='_'||c=='-'),"diag event must be a plain token");
 ensure!(!event.contains('\t')&&!detail.contains('\t')&&!event.contains('\n')&&!detail.contains('\n')&&!detail.contains('\r'),"diag fields must not contain tabs or newlines");
 // Real-mode managers never create --state-dir at startup (Projects::open is
 // read-only; only materialize_artifact_package builds it after a done run),
 // so the first diag lines of a session are written before the dir exists.
 // Create it here: diagnostics must survive, not drop silently via `let _ =`.
 std::fs::create_dir_all(state_dir)?;
 let now=clock();
 let line=format!("{now}\t{event}\t{detail}\n");
 let mut file=std::fs::OpenOptions::new().create(true).append(true).open(state_dir.join("manager-diag.log"))?;
 file.write_all(line.as_bytes())?;Ok(())
}

fn gateway_command(options:&mut ServiceOptions,gateway:&mut Option<super::ssh_manager::SshManager>,command:UiCommand)->Result<Value> {
 ensure!(options.mode==Mode::Real,"direct SSH requires explicit real mode");
 match command {
  UiCommand::GatewayConfigure{config_path}=>{
   ensure!(gateway.is_none(),"gateway already configured; restart before changing pinned configuration");
   let manager=super::ssh_manager::SshManager::open(&config_path)?;
   let result=json!({"worker_id":manager.config().worker_id,"remote_verified":false,"state":"config_only"});
   let _=append_diag(&options.state_dir,"gateway_configured",&format!("worker={} artifact={}",manager.config().worker_id,&manager.config().artifact_sha256[..8]));
   *gateway=Some(manager);Ok(result)
  }
  UiCommand::GatewayDispatch{worker_id}=>{
   let manager=gateway.as_mut().context("explicit SSH worker configuration required")?;
   ensure!(manager.config().worker_id==worker_id,"selected worker differs from pinned config");
   let key=options.gateway_key.take().context("gateway credential unavailable; launch with --gateway-key-stdin")?;
   let helper=std::env::current_exe()?.parent().map(|path|path.join("tools/paramiko_gateway.py")).context("gateway helper path unavailable")?;
   let mut transport=super::ssh_gateway::SshWorkerTransport::with_paramiko(manager.config().clone(),helper,key)?;
   let result=manager.dispatch_with_transport(&Actor::Local("direct-ssh-manager".into()),&mut transport,clock())?;
   let _=append_diag(&options.state_dir,"dispatch_result",&format!("run_id={} status={}",result.run_id,result.status));
   Ok(serde_json::to_value(result)?)
  }
  UiCommand::GatewayDispatchProject{worker_id,doc_path}=>{
   let manager=gateway.as_mut().context("explicit SSH worker configuration required")?;
   ensure!(manager.config().worker_id==worker_id,"selected worker differs from pinned config");
   // Read the confirmed design document from an explicit operator path only.
   let raw=std::fs::read(&doc_path).with_context(||format!("文档读取失败: {}",doc_path.display()))?;
   ensure!(!raw.is_empty(),"文档为空；project-work 派工需要已确认的设计文档");
   ensure!(raw.len()<=65536,"文档超过 64KiB 上限；过大文档请拆分后重新派工");
   let text=String::from_utf8(raw).context("文档必须为 UTF-8 文本")?;
   let key=options.gateway_key.take().context("gateway credential unavailable; launch with --gateway-key-stdin")?;
   let token=options.gateway_model_token.take().context("model token unavailable; launch with framed --gateway-key-stdin (key + token)")?;
   let document=super::project_work::ConfirmedDocument{version:1,sha256:encode_sha256(text.as_bytes()),text};
   let tasks=vec![super::project_work::WorkTask{id:"impl-page".into(),title:"实现设计静态页".into(),description:"按设计文档生成静态文件（index.html、css/style.css、js/data.js、js/app.js）".into(),acceptance:"产物包含设计文档要求的稳定选择器与交互；总大小 ≤16KB".into(),depends_on:vec![],review:"approved".into(),reason:None}];
   let work=super::project_work::ProjectWork{schema:"project-work/1".into(),project_id:"doc-project".into(),action_id:"a1".into(),stage:super::project_work::ProjectStage::Develop,document,tasks,target_task:Some("impl-page".into()),repair_reason:None,repair_round:0,test_spec:None,artifact:None};
   let helper=std::env::current_exe()?.parent().map(|path|path.join("tools/paramiko_gateway.py")).context("gateway helper path unavailable")?;
   let mut transport=super::ssh_gateway::SshWorkerTransport::with_paramiko_token(manager.config().clone(),helper,key,token)?;
   let result=manager.dispatch_project_with_transport(&Actor::Local("direct-ssh-manager".into()),&work,&mut transport,clock())?;
   let _=append_diag(&options.state_dir,"dispatch_result",&format!("run_id={} status={} project=doc-project",result.run_id,result.status));
   // Materialize the admitted artifact package (if any) into the local state
   // directory so the GUI can point at real files on disk.
   let mut value=serde_json::to_value(result)?;
   if value["status"]=="done" {
    let run_id=value["run_id"].as_str().context("gateway result run id missing")?.to_owned();
    let run=manager.run(&run_id)?;
    if let Some(package)=run.detail.get("report").and_then(|report|report.get("artifact")) {
     let target=options.state_dir.join("artifacts").join(&run_id);
     match super::owner_project::materialize_artifact_package(package,&target) {
      Ok(written)=>{manager.annotate_run(&run_id,"artifact_dir",json!(target.display().to_string()))?;value["artifact_dir"]=json!(target.display().to_string());value["artifact_files"]=json!(written);let _=append_diag(&options.state_dir,"artifact_materialized",&format!("run_id={} files={}",run_id,written.len()));}
      Err(error)=>{let _=append_diag(&options.state_dir,"artifact_error",&format!("run_id={} error={}",run_id,error.root_cause()));value["artifact_error"]=json!(format!("{error:#}"));}
     }
    }
   }
   Ok(value)
  }
  _=>anyhow::bail!("not a gateway command"),
 }
}

fn encode_sha256(bytes:&[u8])->String { use sha2::Digest; hex::encode(sha2::Sha256::digest(bytes)) }

fn project_gateway(manager:&super::ssh_manager::SshManager,snapshot:&mut AppSnapshot)->Result<()> { let config=manager.config();let used=manager.used_slots()?;
 snapshot.gateway=Some(GatewayView{worker_id:config.worker_id.clone(),host:config.host.clone(),remote_verified:false,used_slots:used,artifact_sha256:config.artifact_sha256.clone()});
 snapshot.role="direct SSH Manager; config-only, remote not verified".into();
 snapshot.runs=manager.runs()?.into_iter().map(|run|RunView{run,read_only:true,can_status:false,can_approvals:false,can_decide:false,can_report:false,can_retry:false,can_cancel:false,disabled_reason:Some("SSH执行证据来自同一账本；未确认终态保留名额，不自动重跑".into())}).collect();
 snapshot.unfinished=snapshot.runs.iter().filter(|view|!matches!(view.run.status.as_str(),"done"|"failed"|"cancelled")).count().max(used as usize);
 Ok(())
}
