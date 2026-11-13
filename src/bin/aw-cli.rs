use anyhow::{Result,Context,ensure};
use agentranch::{config::{Config,Actor,Mode,valid_id},core::{store::Store,controller::ControlService,service::{ServiceOptions,ServiceHandle,UiCommand,Readiness},mcp,ssh_gateway::{GatewayRequest,SshWorkerTransport}}};
use std::{path::{Path,PathBuf},io::Read,sync::Arc,time::{Duration,Instant}};
use serde_json::json;

fn owner_exec(args: &[String]) -> Result<()> {
    let path = PathBuf::from(option(args, "--config")?);
    agentranch::core::native_worker::run(&path)
}

fn owner_child(args: &[String]) -> Result<()> {
    agentranch::core::native_worker::fixed_child(&args[1..])
}
fn gateway_helper(args:&[String])->Result<PathBuf>{
    match args.iter().position(|value|value=="--helper").and_then(|index|args.get(index+1)) {
        Some(value)=>{
            let path=PathBuf::from(value);
            ensure!(path.is_absolute(),"--helper path must be absolute; relative paths are rejected");
            Ok(path)
        }
        None=>{
            let executable=std::env::current_exe().context("current executable path unavailable")?;
            let parent=executable.parent().context("current executable directory unavailable")?;
            Ok(parent.join("tools/paramiko_gateway.py"))
        }
    }
}

fn ssh_gateway(args:&[String])->Result<()> {
    let config_path=PathBuf::from(option(args,"--config")?);let request_path=PathBuf::from(option(args,"--request")?);let evidence=PathBuf::from(option(args,"--evidence-dir")?);
    let helper=gateway_helper(args)?;let key_stdin=args.iter().any(|value|value=="--key-stdin");
    ensure!(key_stdin,"--key-stdin is required; private key files are not supported");
    ensure!(!args.iter().any(|value|value=="--key-file"),"--key-file is forbidden; use --key-stdin");
    let mut key=Vec::new();std::io::stdin().lock().take(65537).read_to_end(&mut key)?;ensure!(key.len()<=65536,"private key exceeds 64KiB");
    let mut raw=Vec::new();std::fs::File::open(&request_path)?.take(131073).read_to_end(&mut raw)?;ensure!(raw.len()<=131072,"gateway request exceeds 128KiB");
    let request:GatewayRequest=serde_json::from_slice(&raw)?;std::fs::create_dir(&evidence)?;
    let mut manager=agentranch::core::ssh_manager::SshManager::open(&config_path)?;
    let mut transport=SshWorkerTransport::with_paramiko(manager.config().clone(),helper,key)?;
    let result=manager.execute_request_with_transport(&request,&mut transport,agentranch::core::native_worker::clock())?;
    std::fs::write(evidence.join("result.json"),serde_json::to_vec_pretty(&result)?)?;println!("{}",serde_json::to_string(&result)?);Ok(())
}

fn option(args:&[String],name:&str)->Result<String>{let i=args.iter().position(|s|s==name).context(format!("required {name}"))?;args.get(i+1).cloned().context("missing option value")}
fn load(path:&Path)->Result<Config>{let mut raw=Vec::new();std::fs::File::open(path)?.take(65537).read_to_end(&mut raw)?;ensure!(raw.len()<=65536,"configuration exceeds 64KiB");let c:Config=serde_json::from_slice(&raw)?;c.validate()?;Ok(c)}
fn open_control(path:&Path,cfg:&Config,actor:Actor)->Result<ControlService>{ensure!(cfg.role=="controller","controller config required");let base=path.parent().unwrap_or(Path::new("."));let nodes=cfg.load_nodes(base)?;let state=cfg.prepare_state(base)?;Ok(ControlService::new(Store::open(&state.join("controller.sqlite"),&cfg.deployment,&nodes)?,actor))}
fn local_check(state:PathBuf)->Result<()> {
 let options=ServiceOptions{mode:Mode::Local,state_dir:state,worker_exe:std::env::current_exe()?,gateway_key:None,gateway_model_token:None};let mut handle=ServiceHandle::spawn(options,Arc::new(||{}))?;handle.try_submit(UiCommand::StartLocal).map_err(|e|anyhow::anyhow!("{e:?}"))?;
 let end=Instant::now()+Duration::from_secs(20);let mut dispatch_request=None;let mut run_id:Option<String>=None;let mut result=None;
 while Instant::now()<end {let snapshot=handle.snapshot();if snapshot.readiness==Readiness::NotReady{anyhow::bail!("local state unavailable; preserve ledger and inspect configuration")}
  if snapshot.readiness==Readiness::Ready&&dispatch_request.is_none(){dispatch_request=Some(handle.try_submit(UiCommand::Dispatch{template:"sum-native-v1".into()}).map_err(|e|anyhow::anyhow!("{e:?}"))?);}
  for reply in handle.drain_results(){let value=reply.result.map_err(|_|anyhow::anyhow!("local command rejected"))?;if Some(reply.request_id)==dispatch_request{run_id=Some(value["id"].as_str().context("dispatch run id missing")?.to_owned());}}
  if let Some(run)=snapshot.runs.iter().find(|r|Some(r.run.id.as_str())==run_id.as_deref()){if run.run.status=="done"{result=Some(json!({"mode":"local","simulated":true,"meaning":"native fixture only; no real AO/Reason/model","run_id":run.run.id,"report":run.run.detail["report"],"verification":run.run.detail["verification"]}));break;}ensure!(!matches!(run.run.status.as_str(),"failed"|"cancelled"),"current local fixture failed; historical reports do not satisfy this check");}
  std::thread::sleep(Duration::from_millis(15));
 }
 handle.stop();while Instant::now()<end&&handle.snapshot().readiness!=Readiness::Stopped{std::thread::sleep(Duration::from_millis(10));}
 println!("{}",result.context("local fixture timed out; uncertain task remains in ledger")?);Ok(())
}
fn execute()->Result<()> {
 let args:Vec<String>=std::env::args().skip(1).collect();
 match args.first().map(String::as_str){
  Some("--identity") if args.len()==1=>{println!("{}",json!({"binary":"aw-cli","package":env!("CARGO_PKG_NAME"),"version":env!("CARGO_PKG_VERSION")}));Ok(())}

  Some("owner-exec")=>owner_exec(&args),
  Some("owner-child")=>owner_child(&args),
  Some("ssh-gateway")=>ssh_gateway(&args),
  Some("fixture-worker")=>agentranch::adapters::process::fixture_worker(&args[1..]),
  Some("fixture-descendant") if args.len()==1=>loop{std::thread::sleep(Duration::from_secs(1));},
  Some("local-check")=>local_check(option(&args,"--state-dir")?.into()),
  Some("check")=>{let path=PathBuf::from(option(&args,"--config")?);let cfg=load(&path)?;let nodes=cfg.load_nodes(path.parent().unwrap_or(Path::new(".")))?;println!("{}",json!({"schema":"native-check/1","mode":cfg.mode,"role":cfg.role,"deployment":cfg.deployment,"configuration_valid":true,"nodes":nodes.len(),"active_probe":false,"external_readiness":"not_probed"}));Ok(())},
  Some("mcp")=>{let path=PathBuf::from(option(&args,"--config")?);let cfg=load(&path)?;let actor=std::env::var("AO_SESSION_ID").ok().filter(|s|valid_id(s)).map(Actor::AoSession);let controller=actor.map(|a|open_control(&path,&cfg,a)).transpose()?;mcp::serve(&mut std::io::stdin().lock(),&mut std::io::stdout().lock(),controller,cfg.mode==Mode::Local)},
  Some("controller")=>agentranch::core::runtime::controller(&PathBuf::from(option(&args,"--config")?),&load(&PathBuf::from(option(&args,"--config")?))?,args.iter().any(|s|s=="--once")),
  Some("runner")=>{
   let once=args.iter().any(|s|s=="--once");
   let stop_on_stdin=args.iter().any(|s|s=="--stop-on-stdin");
   ensure!(!(once&&stop_on_stdin),"runner control: --once and --stop-on-stdin are incompatible");
   let path=PathBuf::from(option(&args,"--config")?);let cfg=load(&path)?;
   if stop_on_stdin{agentranch::core::runtime::runner_with_stdin_stop(&path,&cfg)}
   else{agentranch::core::runtime::runner(&path,&cfg,once)}
  },

  Some("--help")|Some("help")=>{println!("aw-cli --identity\naw-cli check --config FILE\naw-cli mcp --config FILE\naw-cli local-check --state-dir DIR\naw-cli runner --config FILE [--once | --stop-on-stdin]\naw-cli controller --config FILE [--once]\naw-cli owner-exec --config FILE\naw-cli owner-child sum-v1 SESSION\naw-cli ssh-gateway --config FILE --request FILE --evidence-dir DIR --helper FILE --key-stdin\n--key-stdin: read one short-lived SSH private key from stdin; no private-key file is accepted\n--stop-on-stdin: first stdin frame must be stop LF or stop CRLF; stops local supervision only\ncontroller/runner real service: not ready until explicit adapter configuration");Ok(())},
  _=>anyhow::bail!("usage: aw-cli <check|mcp|local-check|controller|runner> (see --help)"),
 }
}
#[cfg(test)]
mod tests {
    use super::gateway_helper;

    #[test]
    fn explicit_relative_helper_is_rejected_without_cwd_rewrite() {
        let args = vec!["--helper".to_owned(), "tools/paramiko_gateway.py".to_owned()];
        let error = gateway_helper(&args).expect_err("relative helper must be rejected");
        assert!(error.to_string().contains("absolute"), "unexpected error: {error:#}");
    }
}

fn main(){if let Err(e)=execute(){eprintln!("{e:#}");std::process::exit(2);}}
