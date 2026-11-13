//! Controller-side direct SSH manager seam. It owns the controller lock and durable Store.
use anyhow::{Context, Result, ensure};
use std::{io::Read, path::Path};
use crate::{config::{Actor, Config, ServiceLock}, core::{project_work::ProjectWork, ssh_gateway::{GatewayRequest, GatewayResult, SshGatewayConfig, SshWorkerTransport, WorkerTransport, run_once}, store::{Run, Store}}};

pub struct SshManager {
    config: SshGatewayConfig,
    store: Store,
    _controller_lock: ServiceLock,
    _identity_lock: ServiceLock,
}

fn load_config(path: &Path) -> Result<Config> {
    let mut raw=Vec::new();
    std::fs::File::open(path)?.take(65537).read_to_end(&mut raw)?;
    ensure!(raw.len()<=65536,"controller config exceeds 64KiB");
    let config:Config=serde_json::from_slice(&raw)?;
    config.validate()?;
    Ok(config)
}

impl SshManager {
    pub fn open(gateway_path: &Path) -> Result<Self> {
        let config=SshGatewayConfig::load(gateway_path)?;
        let gateway_base=gateway_path.parent().context("gateway config parent missing")?;
        let controller_path=if config.controller_config.is_absolute() {config.controller_config.clone()} else {gateway_base.join(&config.controller_config)};
        let controller=load_config(&controller_path)?;
        ensure!(controller.role=="controller"&&controller.deployment==config.deployment,"controller/gateway identity mismatch");
        let controller_base=controller_path.parent().context("controller config parent missing")?;
        let nodes=controller.load_nodes(controller_base)?;
        let worker=nodes.iter().find(|node|node.id==config.worker_id).context("configured worker is not pinned")?;
        ensure!(worker.templates.iter().any(|template|template=="sum-v1"),"configured worker lacks sum-v1 capability");
        let state=controller.prepare_state(controller_base)?;
        let identity_lock=ServiceLock::acquire(&state,"identity")?;
        let controller_lock=ServiceLock::acquire(&state,"controller")?;
        let mut store=Store::open(&state.join("controller.sqlite"),&config.deployment,&nodes)?;
        store.invalidate_readiness()?;
        Ok(Self{config,store,_controller_lock:controller_lock,_identity_lock:identity_lock})
    }

    pub fn dispatch_with_transport<T:WorkerTransport>(&mut self,actor:&Actor,transport:&mut T,now:u64)->Result<GatewayResult> {
        let run=self.store.dispatch(actor,"sum-v1",now)?;
        self.store.assign_run_with_worker_artifact(now,&run.id,&self.config.worker_id,self.config.artifact_sha256.clone())?.context("configured worker has no available slot")?;
        let request=GatewayRequest::from_store(&self.store,&self.config,&run.id)?;
        run_once(&self.config,&request,&mut self.store,transport,now)
    }
    /// Dispatch a project-work-v1 run (confirmed document + approved tasks) to
    /// the pinned worker: same assignment, signature and exchange flow as
    /// sum-v1, but the start payload carries the project_work JSON.
    pub fn dispatch_project_with_transport<T:WorkerTransport>(&mut self,actor:&Actor,work:&ProjectWork,transport:&mut T,now:u64)->Result<GatewayResult> {
        let run=self.store.dispatch_project(actor,work,now)?;
        self.store.assign_run_with_worker_artifact(now,&run.id,&self.config.worker_id,self.config.artifact_sha256.clone())?.context("configured worker has no available slot")?;
        let request=GatewayRequest::from_store(&self.store,&self.config,&run.id)?;
        run_once(&self.config,&request,&mut self.store,transport,now)
    }

    pub fn dispatch(&mut self,actor:&Actor,now:u64)->Result<GatewayResult> {
        let mut transport=SshWorkerTransport::new(self.config.clone())?;
        self.dispatch_with_transport(actor,&mut transport,now)
    }
    pub fn execute_request(&mut self,request:&GatewayRequest,now:u64)->Result<GatewayResult> {
        let mut transport=SshWorkerTransport::new(self.config.clone())?;
        run_once(&self.config,request,&mut self.store,&mut transport,now)
    }
    pub fn execute_request_with_transport<T:WorkerTransport>(&mut self,request:&GatewayRequest,transport:&mut T,now:u64)->Result<GatewayResult> {
        run_once(&self.config,request,&mut self.store,transport,now)
    }
    pub fn config(&self)->&SshGatewayConfig { &self.config }
    pub fn used_slots(&self)->Result<u32> {
        let count:i64=self.store.db.query_row("SELECT count(*) FROM assignments WHERE node=?1 AND released=0",[&self.config.worker_id],|row|row.get(0))?;
        Ok(u32::try_from(count)?)
    }
    pub fn runs(&self)->Result<Vec<Run>> { self.store.list_runs(200) }
    pub fn run(&self,run_id:&str)->Result<Run> { self.store.run(run_id) }
    pub fn annotate_run(&mut self,run_id:&str,key:&str,value:serde_json::Value)->Result<()> { self.store.annotate_run(run_id,key,value) }
    pub fn request_for(&self,run_id:&str)->Result<GatewayRequest> { GatewayRequest::from_store(&self.store,&self.config,run_id) }
}
