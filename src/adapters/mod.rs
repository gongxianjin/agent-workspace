pub mod local;
pub mod process;
pub mod owner_exec;
pub mod ao_http;
pub mod model_endpoint;
pub mod reason;
use anyhow::Result;
use serde_json::Value;
#[derive(Clone,Debug)]
pub struct WorkRequest {pub run_id:String,pub attempt:u32,pub epoch:String,pub template:String,pub session:String,pub project_work:Option<crate::core::project_work::ProjectWork>}
#[derive(Clone,Debug)]
pub enum Observation {
 Unknown(String),
 Running,
 Approval(Value),
 Reported{report:Value,verification:Value},
 /// A provider-specific observer may emit this only after independently waiting for
 /// the owned process and proving the exact execution identity. AO reports and
 /// termination flags must remain `Reported`/`Unknown` until such proof exists.
 ExitProof{proof:crate::core::store::ExitProof,report:Value,verification:Value},
 Completed{report:Value,verification:Value},
 Exited,
}
pub trait AoPort {
 fn create(&mut self,request:&WorkRequest)->Result<String>;
 fn observe(&mut self,session:&str)->Result<Observation>;
 fn bind(&mut self,_session:&str,_request:&WorkRequest)->Result<()>{Ok(())}
 fn resolve(&mut self,session:&str,request:&str,decision:&str)->Result<()>;
 fn kill(&mut self,session:&str)->Result<()>;
}
