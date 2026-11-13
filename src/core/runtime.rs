//! Headless service entrypoints; transport errors never suppress a lifecycle tick.
mod stop;
#[cfg(all(test, windows))]
mod reason_transport_tests;
use anyhow::{Result,Context,ensure};
use std::{path::Path,time::{Duration,SystemTime,UNIX_EPOCH}};
use crate::{config::{Config,Mode,Actor,ServiceLock},adapters::{local::{LocalAo,pump_local},ao_http::AoHttp,reason::{Reason,ReasonRelayTransport}}};
use super::{store::Store,runner::Runner,protocol::Envelope,relay::{RelayBridge,RelayEndpoint,RelayTransport}};

struct ReasonEndpoint<'a,'b>{transport:&'a mut ReasonRelayTransport<'b>,destination:&'a str}
impl RelayEndpoint for ReasonEndpoint<'_,'_>{fn deliver(&mut self,frame:&[u8])->Result<()>{self.transport.send(self.destination,frame)}}

pub(crate) fn forward_reason_frame(
 reason:&Reason,from:&str,destination:&str,outbox_id:&str,frame:&[u8],bridge:&mut RelayBridge,now:u64,
)->Result<()>{
 ensure!(frame.len()<=64*1024,"relay frame exceeds 64KiB");
 let message:Envelope=serde_json::from_slice(frame).context("invalid relay envelope")?;
 ensure!(message.message_id==outbox_id,"outbox id does not match envelope message id");
 let mut transport=ReasonRelayTransport::new(reason,from)?;
 let mut endpoint=ReasonEndpoint{transport:&mut transport,destination};
 bridge.forward(frame,&mut endpoint,now)?;
 Ok(())
}

pub(crate) fn forward_controller_outbox_entry(
 store:&mut Store,reason:&Reason,from:&str,outbox_id:&str,destination:&str,frame:&[u8],now:u64,
)->Result<()>{
 ensure!(frame.len()<=64*1024,"relay frame exceeds 64KiB");
 let message:Envelope=serde_json::from_slice(frame).context("invalid controller outbox envelope")?;
 ensure!(message.from=="A","foreign controller outbox sender");
 let matches:Vec<_>=store.nodes.iter().filter(|node|node.address==destination).collect();
 ensure!(matches.len()==1,"outbox destination must match exactly one configured node");
 let node=matches[0];
 let key:[u8;32]=hex::decode(&node.key_hex)?.try_into().map_err(|_|anyhow::anyhow!("node key must be 32 bytes"))?;
 let mut bridge=RelayBridge::new(&store.deployment,&node.id,key)?;
 forward_reason_frame(reason,from,destination,outbox_id,frame,&mut bridge,now)?;
 store.acknowledge_outbox(outbox_id)?;
 Ok(())
}
fn now()->u64{SystemTime::now().duration_since(UNIX_EPOCH).map_or(0,|v|v.as_secs())}
pub fn controller(path:&Path,cfg:&Config,once:bool)->Result<()> {
 ensure!(cfg.role=="controller","controller role required");let base=path.parent().unwrap_or(Path::new("."));let nodes=cfg.load_nodes(base)?;let state=cfg.prepare_state(base)?;let _lock=ServiceLock::acquire(&state,"controller")?;let mut store=Store::open(&state.join("controller.sqlite"),&cfg.deployment,&nodes)?;
 if cfg.mode==Mode::Local {ensure!(nodes.len()==1,"local CLI expects one explicit fixture node");let _runner_lock=ServiceLock::acquire(&state,"runner")?;let mut runner=Runner::open(&state.join("runner.sqlite"),&cfg.deployment,nodes[0].clone())?;let mut ao=LocalAo::new(&std::env::current_exe()?,&state.join("fixtures"))?;
  loop{pump_local(&mut store,&mut runner,&mut ao,now())?;if once{return Ok(())}std::thread::sleep(Duration::from_millis(25));}
 }
 let reason=Reason::new(cfg.reason_endpoint.clone().context("explicit reason_endpoint required")?)?;let endpoint=cfg.endpoint.as_deref().context("explicit controller endpoint required")?;
 loop {store.expire(now())?;store.assign_next(now())?;
  let result=(||->Result<()>{for(id,body)in reason.inbox(endpoint)?{ensure!(body.len()<=65536,"native frame bound");let message:Envelope=serde_json::from_str(&body)?;store.receive(&message,now())?;reason.delivered(endpoint,&id)?;}Ok(())})();
  if let Err(e)=result{if once{return Err(e)}eprintln!("controller inbox unavailable; durable state retained");}
  let result=(||->Result<()>{for(id,destination,frame)in store.pending_outbox_raw(32)?{forward_controller_outbox_entry(&mut store,&reason,endpoint,&id,&destination,&frame,now())?;}Ok(())})();
  if let Err(e)=result{if once{return Err(e)}eprintln!("controller outbox pending; no success inferred");}
  if once{return Ok(())}std::thread::sleep(Duration::from_millis(100));
 }
}
// Preserve the existing API and default/once behavior: no stdin reader is started.
pub fn runner(path:&Path,cfg:&Config,once:bool)->Result<()> { runner_main(path,cfg,once,false) }

/// Explicit process-level CLI opt-in. Stopping ends only local supervision; it
/// neither cancels an AO session nor proves its exit or releases its reservation.
pub fn runner_with_stdin_stop(path:&Path,cfg:&Config)->Result<()> { runner_main(path,cfg,false,true) }

fn runner_stop_requested(control:&mut Option<stop::StdinStop>)->Result<bool> {
 match control { Some(control)=>control.requested().context("runner control"), None=>Ok(false) }
}

fn runner_main(path:&Path,cfg:&Config,once:bool,stop_on_stdin:bool)->Result<()> {
 ensure!(!(once&&stop_on_stdin),"runner control: --once and --stop-on-stdin are incompatible");
 ensure!(cfg.role=="runner","runner role required");let base=path.parent().unwrap_or(Path::new("."));let nodes=cfg.load_nodes(base)?;let node=nodes.iter().find(|n|Some(n.id.as_str())==cfg.runner_node.as_deref()).context("configured runner_node missing")?.clone();let state=cfg.prepare_state(base)?;let _lock=ServiceLock::acquire(&state,"runner")?;let mut runner=Runner::open(&state.join("runner.sqlite"),&cfg.deployment,node.clone())?;
 if cfg.mode==Mode::Local{anyhow::bail!("local runner is embedded in local controller; run controller or native GUI")}
 let reason=Reason::new(cfg.reason_endpoint.clone().context("explicit reason_endpoint required")?)?;let controller=cfg.controller_address.as_deref().context("explicit controller_address required")?;let mut ao=AoHttp::new(cfg.ao.clone().context("trusted AO config required")?)?;if let Some(root)=&cfg.workspace_root{ao=ao.with_workspace_root(&base.join(root))?;}
 let mut control=if stop_on_stdin{Some(stop::StdinStop::start().context("runner control: start stdin reader")?)}else{None};
 eprintln!("real Runner ports configured; process observer/verifier incomplete: UNKNOWN retains slot, no real readiness claim");
 loop {
  if runner_stop_requested(&mut control)?{return Ok(())}
  let inbox=(||->Result<()>{
   let messages=reason.inbox(&node.address)?;
   if runner_stop_requested(&mut control)?{return Ok(())}
   for(id,body)in messages{
    if runner_stop_requested(&mut control)?{return Ok(())}
    let m:Envelope=serde_json::from_str(&body)?;
    if m.kind=="probe"{let health=ao.probe()?;if runner_stop_requested(&mut control)?{return Ok(())}runner.accept_probe(&m,now(),health)?;}else{runner.accept(&m,now())?;}
    if runner_stop_requested(&mut control)?{return Ok(())}
    reason.delivered(&node.address,&id)?;
   }
   Ok(())
  })();
  if runner_stop_requested(&mut control)?{return Ok(())}
  let lifecycle=runner.tick(&mut ao,now());
  if runner_stop_requested(&mut control)?{return Ok(())}
  let outbound=(||->Result<()> {for(id,frame) in runner.pending_outbox_raw(32)?{if runner_stop_requested(&mut control)?{return Ok(())}ensure!(frame.len()<=64*1024,"relay frame exceeds 64KiB");let message:Envelope=serde_json::from_slice(&frame).context("invalid runner outbox envelope")?;ensure!(message.from==node.id,"foreign runner outbox sender");let key:[u8;32]=hex::decode(&node.key_hex)?.try_into().map_err(|_|anyhow::anyhow!("node key must be 32 bytes"))?;let mut bridge=RelayBridge::new(&cfg.deployment,"A",key)?;forward_reason_frame(&reason,&node.address,controller,&id,&frame,&mut bridge,now())?;if runner_stop_requested(&mut control)?{return Ok(())}runner.acknowledge_outbox(&id)?;}Ok(())})();
  if runner_stop_requested(&mut control)?{return Ok(())}
  if once{inbox?;lifecycle?;outbound?;return Ok(())}
  if inbox.is_err(){eprintln!("runner inbox unavailable; state retained");}if lifecycle.is_err(){eprintln!("runner observation unavailable; no exit inferred");}if outbound.is_err(){eprintln!("runner outbox retained");}std::thread::sleep(Duration::from_millis(100));
 }
}
// Keep AO session actor distinct from local CLI/GUI actor at every entrypoint.
pub fn host_actor(id:&str)->Actor{Actor::AoSession(id.into())}
