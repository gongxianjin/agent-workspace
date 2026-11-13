use ao_reason_workbench::core::{projects::Projects,service::UiCommand};
use serde_json::Value;
use sha2::{Digest,Sha256};

// The callback gate keeps the published snapshot stale while the result carries
// the imported document; the regression must consume that returned contract.
#[test]
fn service_import_result_document_does_not_require_snapshot_publication() {
 use ao_reason_workbench::{config::Mode,core::service::{DocumentSnapshot,ServiceHandle,ServiceOptions}};
 use std::sync::{Arc,Mutex,atomic::{AtomicBool,Ordering},mpsc};
 use std::time::{Duration,Instant};
 struct RepaintGate {
  handle:ServiceHandle,
  entered:mpsc::Receiver<()>,
  release:Option<mpsc::SyncSender<()>>,
  expired:Arc<AtomicBool>,
 }
 impl RepaintGate {
  fn next_callback(&self,deadline:Instant) {
   self.entered.recv_timeout(deadline.saturating_duration_since(Instant::now())).expect("callback entry deadline");
   assert!(!self.expired.load(Ordering::Acquire),"callback gate expired; stale schedule not established");
  }
  fn release_callback(&self) {
   self.release.as_ref().unwrap().try_send(()).expect("exactly one permit per callback");
  }
 }
 impl Drop for RepaintGate {
  fn drop(&mut self) {
   self.handle.stop();
   // Disconnect releases the current callback and every later callback on unwind.
   drop(self.release.take());
  }
 }
 let t=tempfile::tempdir().unwrap();
 let path=t.path().join("confirmed.md");
 let text="# 已确认\n订单原子扣减库存；退款幂等补偿。\n";
 std::fs::write(&path,text).unwrap();
 let (entered_tx,entered)=mpsc::channel();
 let (release_tx,release_rx)=mpsc::sync_channel(1);
 let release_rx=Mutex::new(release_rx);
 let expired=Arc::new(AtomicBool::new(false));
 let callback_expired=expired.clone();
 let repaint=Arc::new(move|| {
  if entered_tx.send(()).is_err(){return}
  if matches!(release_rx.lock().unwrap().recv_timeout(Duration::from_secs(10)),Err(mpsc::RecvTimeoutError::Timeout)) {
   callback_expired.store(true,Ordering::Release);
  }
 });
 let handle=ServiceHandle::spawn(ServiceOptions{mode:Mode::Real,state_dir:t.path().join("state"),worker_exe:t.path().join("unused.exe"),gateway_key:None,gateway_model_token:None},repaint).unwrap();
 let mut gate=RepaintGate{handle,entered,release:Some(release_tx),expired};
 let create_request=gate.handle.try_submit(UiCommand::ProjectCreate{name:"stale snapshot negative control".into()}).unwrap();
 let mut project_id=None;
 let deadline=Instant::now()+Duration::from_secs(5);
 let id=loop {
  gate.next_callback(deadline);
  for reply in gate.handle.drain_results() {
   assert_eq!(reply.request_id,create_request);
   let created=reply.result.expect("ProjectCreate succeeds without external dependencies");
   project_id=Some(created["id"].as_str().unwrap().to_owned());
  }
  let snapshot=gate.handle.snapshot();
  if let Some(id)=project_id.as_ref() {
   if let Some(project)=snapshot.projects.iter().find(|project|&project.id==id) {
    assert!(project.document.is_none());
    break id.clone()
   }
  }
  // Also handles a possible initial publication: observe state, not callback counts.
  gate.release_callback();
 };
 // Queue the import while the worker is still inside ProjectCreate's publication
 // callback. The next command result callback precedes import refresh/publish.
 let import_request=gate.handle.try_submit(UiCommand::ProjectImport{project_id:id.clone(),path}).unwrap();
 gate.release_callback();
 let deadline=Instant::now()+Duration::from_secs(5);
 let imported=loop {
  gate.next_callback(deadline);
  let replies=gate.handle.drain_results();
  if let Some(reply)=replies.into_iter().next() {
   assert_eq!(reply.request_id,import_request);
   break reply.result.expect("ProjectImport must succeed before exercising the extractor")
  }
  gate.release_callback();
 };
 assert_eq!(imported["id"].as_str(),Some(id.as_str()));
 let document:DocumentSnapshot=serde_json::from_value(imported["document"].clone()).expect("successful import document shape");
 assert_eq!(document.text,text);
 assert_eq!(document.sha256,hex::encode(Sha256::digest(text.as_bytes())));
 assert_eq!(document.version,1);
 assert_eq!(document.filename,"confirmed.md");
 assert!(!document.confirmed);
 let stale=gate.handle.snapshot();
 assert_eq!(stale.projects.len(),1);
 assert_eq!(stale.projects[0].id,id);
 assert!(stale.projects[0].document.is_none(),"forced-stale precondition missing");
 assert!(!gate.expired.load(Ordering::Acquire),"callback gate expired; negative control invalid");
 eprintln!("WAVE7_FORCED_STALE_SNAPSHOT: successful ProjectImport result; matching published project document=None");
}

#[test]
fn reopened_service_loads_existing_projects_without_new_command(){use ao_reason_workbench::{config::Mode,core::service::{ServiceHandle,ServiceOptions}};use std::{sync::Arc,time::{Instant,Duration}};let t=tempfile::tempdir().unwrap();let state=t.path().join("state");let mut p=Projects::open(&state).unwrap();let id=create(&mut p);drop(p);let mut h=ServiceHandle::spawn(ServiceOptions{mode:Mode::Real,state_dir:state,worker_exe:t.path().join("unused"),gateway_key:None,gateway_model_token:None},Arc::new(||{})).unwrap();let end=Instant::now()+Duration::from_secs(3);loop{if h.snapshot().projects.iter().any(|p|p.id==id){break}assert!(Instant::now()<end,"historical projects must load without a new command");std::thread::sleep(Duration::from_millis(10));}h.stop();}
#[test]
fn managed_project_and_product_directories_are_actual_owned_persistent_locations(){let t=tempfile::tempdir().unwrap();let state=t.path().join("state");let mut p=Projects::open(&state).unwrap();let value=p.command(UiCommand::ProjectCreate{name:"managed project".into()}).unwrap();let root=std::path::PathBuf::from(value["project_path"].as_str().expect("actual project path"));let product=std::path::PathBuf::from(value["product_path"].as_str().expect("actual product path"));assert!(root.is_dir()&&product.is_dir());assert!(root.starts_with(state.canonicalize().unwrap()));assert_eq!(product,root.join("product"));assert_eq!(std::fs::read_dir(&product).unwrap().count(),0,"directory creation is not artifact generation");let id=value["id"].as_str().unwrap().to_owned();drop(p);let mut p=Projects::open(&state).unwrap();let refreshed=p.command(UiCommand::ProjectRefresh{project_id:id}).unwrap();assert_eq!(refreshed["project_path"],value["project_path"]);assert_eq!(refreshed["product_path"],value["product_path"]);assert!(refreshed["actions"].as_array().unwrap().is_empty());}
#[cfg(windows)]
#[test]
fn service_split_sends_confirmed_document_through_reason_runner_http_and_ingests_real_workspace_tasks(){
 use ao_reason_workbench::{config::{Config,Mode},core::{service::{ServiceHandle,ServiceOptions},runtime},adapters::reason::Reason};
 use std::{sync::{Arc,Mutex,atomic::{AtomicBool,Ordering}},io::{Read,Write},time::{Instant,Duration}};
 let t=tempfile::tempdir().unwrap();let workspace=t.path().join("ao-workspaces");std::fs::create_dir(&workspace).unwrap();let session_workspace=workspace.join("owned-document-worker");std::fs::create_dir(&session_workspace).unwrap();
 let bus:Arc<Mutex<std::collections::BTreeMap<String,Vec<(String,String)>>>>=Arc::new(Mutex::new(std::collections::BTreeMap::new()));let stop=Arc::new(AtomicBool::new(false));
 let endpoint=format!(r"\\.\pipe\project-vertical-{}",uuid::Uuid::new_v4());let pipe_endpoint=endpoint.clone();let pipe_stop=stop.clone();let pipe_bus=bus.clone();
 use windows_sys::Win32::{System::Pipes::{CreateNamedPipeW,ConnectNamedPipe,PIPE_TYPE_BYTE,PIPE_WAIT},Storage::FileSystem::PIPE_ACCESS_DUPLEX,Foundation::{INVALID_HANDLE_VALUE,GetLastError,ERROR_PIPE_CONNECTED}};
 use std::os::windows::io::{FromRawHandle,RawHandle};
 let make_pipe=|endpoint:&str|{let wide:Vec<u16>=endpoint.encode_utf16().chain(Some(0)).collect();let h=unsafe{CreateNamedPipeW(wide.as_ptr(),PIPE_ACCESS_DUPLEX,PIPE_TYPE_BYTE|PIPE_WAIT,4,1024*1024,1024*1024,3000,std::ptr::null())};assert_ne!(h,INVALID_HANDLE_VALUE);h as usize};
 let first=make_pipe(&endpoint);let pipe=std::thread::spawn(move||{let mut next=first;loop{let h=next as windows_sys::Win32::Foundation::HANDLE;let ok=unsafe{ConnectNamedPipe(h,std::ptr::null_mut())};assert!(ok!=0||unsafe{GetLastError()}==ERROR_PIPE_CONNECTED);let mut file=unsafe{std::fs::File::from_raw_handle(h as RawHandle)};next=make_pipe(&pipe_endpoint);let mut frame=Vec::new();loop{let mut b=[0];if file.read_exact(&mut b).is_err(){break}frame.push(b[0]);if b[0]==b'\n'{break}}if frame.is_empty(){continue}let r:Value=serde_json::from_slice(&frame).unwrap();let mut bus=pipe_bus.lock().unwrap();let result=match r["op"].as_str().unwrap(){"send"=>{let to=r["to"].as_str().unwrap();let id=r["idempotency_key"].as_str().unwrap();let q=bus.entry(to.into()).or_default();if !q.iter().any(|(old,_)|old==id){q.push((id.into(),r["body"].as_str().unwrap().into()))}serde_json::json!({"recipients":[{"address":to,"state":"queued"}],"ids":[id]})},"inbox"=>serde_json::json!({"messages":bus.get(r["key"].as_str().unwrap()).into_iter().flatten().map(|(id,body)|serde_json::json!({"id":id,"body":body})).collect::<Vec<_>>()}),"delivered"=>{if let Some(q)=bus.get_mut(r["key"].as_str().unwrap()){q.retain(|(id,_)|!r["ids"].as_array().unwrap().contains(&serde_json::json!(id)));}serde_json::json!({})},_=>panic!("unexpected IPC")};drop(bus);writeln!(file,"{}",serde_json::json!({"id":r["id"],"ok":true,"result":result})).unwrap();if pipe_stop.load(Ordering::Acquire){unsafe{windows_sys::Win32::Foundation::CloseHandle(next as windows_sys::Win32::Foundation::HANDLE);}break}}});
 let listener=std::net::TcpListener::bind("127.0.0.1:0").unwrap();listener.set_nonblocking(true).unwrap();let address=listener.local_addr().unwrap();let http_stop=stop.clone();let received=Arc::new(Mutex::new(None));let http_received=received.clone();let actual_workspace=session_workspace.clone();let reports_seen=Arc::new(std::sync::atomic::AtomicUsize::new(0));let http_reports=reports_seen.clone();let http=std::thread::spawn(move||{while !http_stop.load(Ordering::Acquire){let(mut s,_)=match listener.accept(){Ok(v)=>v,Err(e)if e.kind()==std::io::ErrorKind::WouldBlock=>{std::thread::sleep(Duration::from_millis(5));continue},Err(e)=>panic!("{e}")};s.set_nonblocking(false).unwrap();s.set_read_timeout(Some(Duration::from_secs(3))).unwrap();let mut header=Vec::new();loop{let mut b=[0];s.read_exact(&mut b).unwrap();header.push(b[0]);if header.ends_with(b"\r\n\r\n"){break}}let header=String::from_utf8(header).unwrap();let n:usize=header.lines().find_map(|l|l.to_ascii_lowercase().strip_prefix("content-length:").map(|n|n.trim().parse().unwrap())).unwrap_or(0);let mut body=vec![0;n];s.read_exact(&mut body).unwrap();let route=header.lines().next().unwrap().split_whitespace().nth(1).unwrap();let reply=if route=="/api/v1/projects"{serde_json::json!({"projects":[{"id":"owned-project"}]})}else if route=="/api/v1/sessions"{let r:Value=serde_json::from_slice(&body).unwrap();let w:Value=serde_json::from_str(r["prompt"].as_str().unwrap().split_once("\nPROJECT_WORK_JSON\n").unwrap().1).unwrap();let result=serde_json::json!({"schema":"project-result/1","project_id":w["project_id"],"action_id":w["action_id"],"stage":"split","document_version":w["document"]["version"],"document_sha256":w["document"]["sha256"],"tasks":[{"id":"inventory","title":"库存扣减","description":"订单创建原子扣减库存","acceptance":"并发后库存不为负","depends_on":[]},{"id":"refund","title":"退款补偿","description":"退款仅恢复一次库存","acceptance":"重复退款幂等","depends_on":["inventory"]}]});std::fs::write(actual_workspace.join("project-result.json"),serde_json::to_vec(&result).unwrap()).unwrap();*http_received.lock().unwrap()=Some(w);serde_json::json!({"session":{"id":"owned-document-worker","kind":"worker","harness":"claude-code"}})}else if route.starts_with("/api/v1/sessions/owned-document-worker/conversation"){serde_json::json!({"sessionId":"owned-document-worker","activities":[]})}else if route.starts_with("/api/v1/reports?"){serde_json::json!({"reports":[{"id":"owned-report","sessionId":"owned-document-worker","state":"done"}]})}else if route=="/api/v1/desktop/sessions/owned-document-worker/workspace"{http_reports.fetch_add(1,Ordering::AcqRel);serde_json::json!({"sessionId":"owned-document-worker","workspacePath":actual_workspace})}else{serde_json::json!({"session":{"id":"owned-document-worker","kind":"worker","harness":"claude-code","isTerminated":false}})};let reply=reply.to_string();write!(s,"HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",reply.len(),reply).unwrap();}});
 let key=t.path().join("node.key");std::fs::write(&key,"07".repeat(32)).unwrap();let config_path=t.path().join("controller.json");let app_state=t.path().join("app-state");let base=serde_json::json!({"schema":"ao-reason-config/1","mode":"real","role":"controller","deployment":"project-vertical","state_dir":t.path().join("host-state"),"nodes":[{"id":"B","address":"owned-B/runner","key_file":key,"slots":1,"templates":["project-work-v1"],"admitted":true,"healthy":true}],"ao":{"base_url":format!("http://{address}"),"project_id":"owned-project","model":"claude-opus-5-5"},"reason_endpoint":endpoint,"endpoint":"owned-A/controller","workspace_root":workspace});std::fs::write(&config_path,serde_json::to_vec(&base).unwrap()).unwrap();let mut runner_cfg=base.clone();runner_cfg["role"]=serde_json::json!("runner");runner_cfg["state_dir"]=serde_json::json!(t.path().join("runner-state"));runner_cfg["runner_node"]=serde_json::json!("B");runner_cfg["controller_address"]=serde_json::json!("owned-A/controller");let runner_path=t.path().join("runner.json");std::fs::write(&runner_path,serde_json::to_vec(&runner_cfg).unwrap()).unwrap();
 let runner_stop=stop.clone();let runner=std::thread::spawn(move||{let cfg:Config=serde_json::from_value(runner_cfg).unwrap();while !runner_stop.load(Ordering::Acquire){let _=runtime::runner(&runner_path,&cfg,true);std::thread::sleep(Duration::from_millis(10));}});
 let mut h=ServiceHandle::spawn(ServiceOptions{mode:Mode::Real,state_dir:app_state.clone(),worker_exe:t.path().join("not-used"),gateway_key:None,gateway_model_token:None},Arc::new(||{})).unwrap();fn submit(h:&mut ServiceHandle,c:UiCommand)->Result<Value,String>{let id=h.try_submit(c).unwrap();let end=Instant::now()+Duration::from_secs(8);loop{if let Some(r)=h.drain_results().into_iter().find(|r|r.request_id==id){return r.result.map_err(|e|e.message)}assert!(Instant::now()<end,"service result deadline");std::thread::sleep(Duration::from_millis(5));}}
 let outcome=(||->Result<(),String>{let id=submit(&mut h,UiCommand::ProjectCreate{name:"production split fixture".into()})?["id"].as_str().unwrap().to_owned();let doc=t.path().join("confirmed.md");let text="# 已确认\n订单原子扣减库存；退款幂等补偿。\n";std::fs::write(&doc,text).unwrap();let imported=submit(&mut h,UiCommand::ProjectImport{project_id:id.clone(),path:doc})?;assert_eq!(imported["id"].as_str(),Some(id.as_str()));let d:ao_reason_workbench::core::service::DocumentSnapshot=serde_json::from_value(imported["document"].clone()).unwrap();assert_eq!(d.text,text);assert_eq!(d.sha256,hex::encode(sha2::Sha256::digest(text.as_bytes())));assert_eq!(d.version,1);assert_eq!(d.filename,"confirmed.md");assert!(!d.confirmed);submit(&mut h,UiCommand::ProjectConfirm{project_id:id.clone(),version:d.version,sha256:d.sha256.clone()})?;submit(&mut h,UiCommand::ProjectConfigure{config_path:config_path.clone()})?;submit(&mut h,UiCommand::ProjectSplit{project_id:id.clone()})?;let end=Instant::now()+Duration::from_secs(12);loop{let s=h.snapshot();let p=s.projects.iter().find(|p|p.id==id).unwrap();if p.tasks.len()==2{assert_eq!(p.tasks[0].id,"inventory");assert_eq!(p.tasks[1].id,"refund");assert!(p.tasks.iter().all(|t|t.review=="pending"));assert!(!p.can_start,"unobserved exit cannot unlock next execution");assert!(p.actions.iter().any(|a|a.session.as_deref()==Some("owned-document-worker")));let got=received.lock().unwrap().clone().unwrap();assert_eq!(got["document"]["text"],d.text);assert_eq!(got["document"]["sha256"],d.sha256);break}if Instant::now()>=end{return Err(format!("actual split tasks absent: phase={} blockers={:?}",p.phase,p.blockers))}std::thread::sleep(Duration::from_millis(15));}
 submit(&mut h,UiCommand::TaskReview{project_id:id.clone(),task_id:"inventory".into(),approve:true,reason:String::new()})?;
 submit(&mut h,UiCommand::TaskReview{project_id:id.clone(),task_id:"refund".into(),approve:false,reason:"需要补充重复回调验收".into()})?;
 let seen=reports_seen.load(Ordering::Acquire);let end=Instant::now()+Duration::from_secs(4);while reports_seen.load(Ordering::Acquire)<seen+3{if Instant::now()>=end{return Err("repeated result observation missing".into())}std::thread::sleep(Duration::from_millis(10));}
 let reviewed=submit(&mut h,UiCommand::ProjectRefresh{project_id:id.clone()})?;
 if reviewed["tasks"][0]["review"]!="approved"||reviewed["tasks"][1]["review"]!="rejected"||reviewed["tasks"][1]["reason"]!="需要补充重复回调验收"{return Err(format!("polling erased user review: {}",reviewed["tasks"]))}
 let reopened=Projects::open(&app_state).map_err(|e|e.to_string())?;let saved=reopened.snapshots().pop().unwrap();if saved.tasks[0].review!="approved"||saved.tasks[1].reason.as_deref()!=Some("需要补充重复回调验收"){return Err("review decisions not durable".into())}drop(reopened);
 let replacement=t.path().join("replacement.json");std::fs::write(&replacement,serde_json::to_vec(&base).unwrap()).unwrap();if submit(&mut h,UiCommand::ProjectConfigure{config_path:replacement}).is_ok(){return Err("unresolved run allowed config replacement".into())}let db=rusqlite::Connection::open(app_state.join("project-workflow/controller.sqlite")).unwrap();let active:String=db.query_row("SELECT value FROM project_settings WHERE key='config'",[],|r|r.get(0)).unwrap();if std::path::PathBuf::from(active)!=config_path{return Err("rejected config replacement mutated durable settings".into())}drop(db);
 let approved=submit(&mut h,UiCommand::TaskReview{project_id:id.clone(),task_id:"refund".into(),approve:true,reason:String::new()})?;if approved["can_start"]!=false{return Err("unobserved exit unlocked development".into())}if submit(&mut h,UiCommand::ProjectStart{project_id:id}).is_ok(){return Err("Start accepted without credible exit".into())}
 Ok(())})();h.stop();std::thread::sleep(Duration::from_millis(60));stop.store(true,Ordering::Release);let _=Reason::new(endpoint).unwrap().inbox("shutdown");runner.join().unwrap();http.join().unwrap();pipe.join().unwrap();outcome.expect("real ServiceHandle split path");
}
fn create(p:&mut Projects)->String{p.command(UiCommand::ProjectCreate{name:"真实设计文档项目".into()}).unwrap()["id"].as_str().unwrap().into()}
#[test]
fn uploaded_utf8_document_versions_and_confirmation_persist_without_starting_work(){let t=tempfile::tempdir().unwrap();let state=t.path().join("state");let doc=t.path().join("design.md");let text="# 已确认设计\n实现库存扣减与退款补偿。\n";std::fs::write(&doc,text).unwrap();let mut p=Projects::open(&state).unwrap();let id=create(&mut p);p.command(UiCommand::ProjectImport{project_id:id.clone(),path:doc.clone()}).unwrap();let s=p.snapshots().pop().unwrap();let d=s.document.unwrap();assert_eq!(d.version,1);assert_eq!(d.text,text);assert_eq!(d.sha256,hex::encode(Sha256::digest(text.as_bytes())));assert!(!d.confirmed);assert!(s.actions.is_empty());assert!(!s.can_start);assert!(p.command(UiCommand::ProjectConfirm{project_id:id.clone(),version:1,sha256:"wrong".into()}).is_err());p.command(UiCommand::ProjectConfirm{project_id:id.clone(),version:1,sha256:d.sha256}).unwrap();drop(p);let mut p=Projects::open(&state).unwrap();assert!(p.snapshots()[0].document.as_ref().unwrap().confirmed);std::fs::write(&doc,"# 第二版\n保留原文").unwrap();p.command(UiCommand::ProjectImport{project_id:id,path:doc}).unwrap();let s=p.snapshots().pop().unwrap();assert_eq!(s.document.unwrap().version,2);assert!(s.tasks.is_empty());assert!(s.actions.is_empty());}
#[test]
fn document_boundary_rejects_non_utf8_oversize_and_wrong_extension(){let t=tempfile::tempdir().unwrap();let mut p=Projects::open(&t.path().join("state")).unwrap();let id=create(&mut p);for(name,bytes)in [("bad.md",vec![255]),("huge.txt",vec![b'a';256*1024+1]),("script.exe",b"text".to_vec())]{let path=t.path().join(name);std::fs::write(&path,bytes).unwrap();assert!(p.command(UiCommand::ProjectImport{project_id:id.clone(),path}).is_err());}assert!(p.snapshots()[0].document.is_none());}
#[test]
fn missing_host_blocks_real_split_and_manual_start_without_fake_tasks(){let t=tempfile::tempdir().unwrap();let doc=t.path().join("design.txt");std::fs::write(&doc,"订单系统确认版").unwrap();let mut p=Projects::open(&t.path().join("state")).unwrap();let id=create(&mut p);p.command(UiCommand::ProjectImport{project_id:id.clone(),path:doc}).unwrap();let d=p.snapshots()[0].document.clone().unwrap();assert!(p.command(UiCommand::ProjectSplit{project_id:id.clone()}).is_err());p.command(UiCommand::ProjectConfirm{project_id:id.clone(),version:d.version,sha256:d.sha256}).unwrap();assert!(p.command(UiCommand::ProjectSplit{project_id:id.clone()}).is_err());assert!(p.command(UiCommand::ProjectStart{project_id:id.clone()}).is_err());assert!(p.command(UiCommand::ProjectAccept{project_id:id}).is_err());let s=&p.snapshots()[0];assert!(s.tasks.is_empty());assert!(!s.blockers.is_empty());assert!(!s.can_start);assert!(s.actions.iter().all(|a|a.state!="succeeded"));assert_eq!(p.unfinished(),0);}
#[test]
fn project_service_import_does_not_require_or_start_fixture(){use ao_reason_workbench::{config::Mode,core::service::{ServiceHandle,ServiceOptions}};use std::{sync::Arc,time::{Instant,Duration}};let t=tempfile::tempdir().unwrap();let mut h=ServiceHandle::spawn(ServiceOptions{mode:Mode::Real,state_dir:t.path().join("state"),worker_exe:t.path().join("not-used.exe"),gateway_key:None,gateway_model_token:None},Arc::new(||{})).unwrap();let request=h.try_submit(UiCommand::ProjectCreate{name:"文档工作流".into()}).unwrap();let end=Instant::now()+Duration::from_secs(5);loop{let replies=h.drain_results();if let Some(reply)=replies.into_iter().find(|r|r.request_id==request){let value:Value=reply.result.expect("project command independent from external readiness");assert!(value["id"].is_string());break}assert!(Instant::now()<end);std::thread::sleep(Duration::from_millis(10));}assert_eq!(h.snapshot().projects.len(),1);assert!(h.snapshot().runs.is_empty());assert!(!t.path().join("state/fixtures").exists());h.stop();}
