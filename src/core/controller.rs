use anyhow::{Result,ensure};
use serde::Deserialize;
use serde_json::{Value,json};
use crate::config::Actor;
use super::store::Store;

pub struct ControlService { pub store:Store, actor:Actor }
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct Dispatch {template:String}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct RunId {run_id:String}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
struct Retry {run_id:String,attempt:u32}
#[derive(Deserialize)] #[serde(deny_unknown_fields)]
pub struct Decision {pub run_id:String,pub request_id:String,pub generation:u64,pub scope_hash:String,pub decision_id:String}

// GUI bindings are checked under the caller's single-worker/controller lock.
// Store::decide owns the transactional scope/expiry/offered checks and writes;
// this precheck is not an atomic guarantee against external database writers.
pub(crate) fn decide_with_binding(store:&mut Store,actor:&Actor,decision:&Decision,binding:Option<(u32,&str)>,now:u64)->Result<Value>{
    if let Some((attempt,session))=binding {
        let run=store.owned(actor,&decision.run_id)?;
        ensure!(run.attempt==attempt&&run.session.as_deref()==Some(session),"stale GUI attempt/session");
    }
    store.decide(actor,decision,now)?;
    Ok(json!({"state":"queued","meaning":"persisted decision intent; not proof of effect"}))
}

impl ControlService {
    pub fn new(store:Store,actor:Actor)->Self {Self{store,actor}}
    pub fn tool_names()->[&'static str;7] {["dispatch","status","approvals","decide","report","retry","cancel"]}
    pub fn actor(&self)->&Actor{&self.actor}
    pub fn call(&mut self,tool:&str,args:Value,now:u64)->Result<Value>{
        ensure!(args.is_object(),"tool arguments must be object");
        match tool {
            "dispatch"=>{let a:Dispatch=serde_json::from_value(args)?;Ok(serde_json::to_value(self.store.dispatch(&self.actor,&a.template,now)?)?)},
            "status"=>{let a:RunId=serde_json::from_value(args)?;Ok(serde_json::to_value(self.store.owned(&self.actor,&a.run_id)?)?)},
            "approvals"=>{let a:RunId=serde_json::from_value(args)?;Ok(json!(self.store.approvals(&self.actor,&a.run_id,now)?))},
            "decide"=>{let a:Decision=serde_json::from_value(args)?;decide_with_binding(&mut self.store,&self.actor,&a,None,now)},
            "report"=>{let a:RunId=serde_json::from_value(args)?;let r=self.store.owned(&self.actor,&a.run_id)?;Ok(json!({"run_id":r.id,"attempt":r.attempt,"report":r.detail.get("report"),"verification":r.detail.get("verification"),"status":r.status}))},
            "retry"=>{let a:Retry=serde_json::from_value(args)?;Ok(serde_json::to_value(self.store.retry(&self.actor,&a.run_id,a.attempt)?)?)},
            "cancel"=>{let a:RunId=serde_json::from_value(args)?;Ok(serde_json::to_value(self.store.cancel_at(&self.actor,&a.run_id,now)?)?)},
            _=>anyhow::bail!("unknown tool"),
        }
    }
}
