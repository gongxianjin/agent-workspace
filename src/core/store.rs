use anyhow::{Context, Result, ensure};
use crate::{config::{Actor, NodeSpec}, core::protocol::{Envelope, ExecutionIdentity, PROTOCOL}};
use rusqlite::{params, Connection, OptionalExtension, TransactionBehavior};
use serde::{Serialize,Deserialize};
use serde_json::{Value,json};
use sha2::{Digest,Sha256};
use std::{collections::BTreeSet, path::Path, time::Duration};
use uuid::Uuid;
use super::{protocol::canonical, worker_owner::{
    self, ExpectedOwnerBinding, OwnerEvidenceBundle, OwnerEvidenceReference,
    OwnerReportPayload, OwnerVerification,
}};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Run {
    pub id:String, pub owner:Actor, pub template:String, pub status:String,
    pub attempt:u32, pub epoch:String, pub session:Option<String>,
    pub spawn_intent:bool, pub deadline:u64, pub detail:Value,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Assignment { pub run_id:String, pub attempt:u32, pub node_id:String, pub address:String, pub released:bool }
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExitProof {
    pub run_id: String,
    pub attempt: u32,
    pub epoch: String,
    pub session: Option<String>,
    pub empty: bool,
    pub observer_alive: bool,
    pub not_started: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_evidence: Option<OwnerEvidenceReference>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct NodeReadiness {pub id:String,pub address:String,pub used:u32,pub slots:u32,pub admitted:bool,pub healthy:bool,pub draining:bool,pub verified:bool,pub observed_at:u64}

#[derive(Debug)]
pub(crate) struct ObsoleteReady;
impl std::fmt::Display for ObsoleteReady {
    fn fmt(&self,f:&mut std::fmt::Formatter<'_>)->std::fmt::Result{f.write_str("readiness response does not match current probe")}
}
impl std::error::Error for ObsoleteReady {}
pub struct Store {pub(crate) db:Connection,pub(crate) deployment:String,pub(crate) nodes:Vec<NodeSpec>,readiness_required:bool}
fn read_run(db:&Connection,id:&str)->Result<Run>{
    let raw:Option<String>=db.query_row("SELECT body FROM runs WHERE id=?1",[id],|r|r.get(0)).optional()?;
    serde_json::from_str(&raw.context("unknown run")?).context("invalid persisted run")
}
fn save_run(db:&Connection,run:&Run)->Result<()>{
    db.execute("UPDATE runs SET body=?1,status=?2,attempt=?3 WHERE id=?4",params![serde_json::to_string(run)?,run.status,run.attempt,run.id])?;Ok(())
}
fn read_assignment(db:&Connection,id:&str,attempt:u32)->Result<Option<Assignment>>{
    Ok(db.query_row("SELECT node,address,released FROM assignments WHERE run=?1 AND attempt=?2",params![id,attempt],|row|Ok(Assignment{run_id:id.into(),attempt,node_id:row.get(0)?,address:row.get(1)?,released:row.get(2)?})).optional()?)
}
fn enqueue(db:&Connection,deployment:&str,node:&NodeSpec,run:&Run,kind:&str,payload:Value,now:u64)->Result<()> {
    let mut envelope=Envelope{schema:PROTOCOL.into(),message_id:Uuid::new_v4().to_string(),deployment:deployment.into(),from:"A".into(),to:node.id.clone(),run_id:run.id.clone(),attempt:run.attempt,epoch:run.epoch.clone(),kind:kind.into(),sent_at:now,expires_at:now.saturating_add(300),payload,mac:String::new()};
    envelope.sign(&hex::decode(&node.key_hex)?)?;envelope.verify(&hex::decode(&node.key_hex)?,deployment,&node.id,now)?;
    db.execute("INSERT INTO outbox(id,destination,body,sent) VALUES(?1,?2,?3,0)",params![envelope.message_id,node.address,serde_json::to_string(&envelope)?])?;Ok(())
}
fn owned(db:&Connection,actor:&Actor,id:&str)->Result<Run>{let r=read_run(db,id)?;ensure!(&r.owner==actor,"owner mismatch");Ok(r)}
fn read_owner_binding(db: &Connection, run_id: &str, attempt: u32) -> Result<Option<ExpectedOwnerBinding>> {
    let raw: Option<String> = db.query_row(
        "SELECT expected_json FROM owner_bindings WHERE run=?1 AND attempt=?2",
        params![run_id, attempt],
        |row| row.get(0),
    ).optional()?;
    raw.map(|value| serde_json::from_str(&value).context("invalid persisted owner binding"))
        .transpose()
}

fn owner_evidence_row(db: &Connection, run_id: &str, attempt: u32) -> Result<Option<(String, String, String, String, String, String, String)>> {
    db.query_row(
        "SELECT epoch,session,binding_sha256,bundle_sha256,record_sha256,report_sha256,bundle_json FROM owner_evidence WHERE run=?1 AND attempt=?2",
        params![run_id, attempt],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?)),
    ).optional().map_err(Into::into)
}

fn release_tx(db: &Connection, deployment: &str, proof: &ExitProof) -> Result<bool> {
    let mut run = read_run(db, &proof.run_id)?;
    if run.attempt != proof.attempt || run.epoch != proof.epoch || run.session != proof.session
        || !proof.empty || !proof.observer_alive { return Ok(false); }
    if !matches!(run.status.as_str(), "cancel_pending" | "failed" | "done" | "cancelled") { return Ok(false); }
    let assignment = read_assignment(db, &run.id, run.attempt)?;
    let Some(assignment) = assignment else { return Ok(false); };
    if assignment.released { return Ok(false); }

    if proof.not_started {
        if !matches!(run.status.as_str(), "cancel_pending" | "failed" | "cancelled") { return Ok(false); }
        if proof.session.is_some() || run.session.is_some() || run.spawn_intent
            || proof.owner_evidence.is_some()
            || read_owner_binding(db, &run.id, run.attempt)?.is_some()
            || owner_evidence_row(db, &run.id, run.attempt)?.is_some() { return Ok(false); }
    } else {
        let Some(reference) = proof.owner_evidence.as_ref() else { return Ok(false); };
        worker_owner::validate_owner_evidence_reference(reference)?;
        let Some(expected) = read_owner_binding(db, &run.id, run.attempt)? else { return Ok(false); };
        let Some((epoch, session, binding_sha, bundle_sha, record_sha, report_sha, bundle_json)) = owner_evidence_row(db, &run.id, run.attempt)? else { return Ok(false); };
        let pinned_address: String = db.query_row("SELECT address FROM owner_bindings WHERE run=?1 AND attempt=?2", params![run.id, run.attempt], |row| row.get(0))?;
        if expected.execution != run.execution_identity(deployment)?
            || expected.execution.session.is_none() || run.session.is_none()
            || expected.owner.node_id != assignment.node_id || pinned_address != assignment.address
            || epoch != run.epoch || session != proof.session.clone().unwrap_or_default()
            || binding_sha != reference.binding_sha256 || bundle_sha != reference.bundle_sha256
            || record_sha != reference.record_sha256 || report_sha != reference.report_sha256 { return Ok(false); }
        if worker_owner::owner_binding_sha256(&expected)? != reference.binding_sha256 { return Ok(false); }
        let bundle: OwnerEvidenceBundle = serde_json::from_str(&bundle_json)?;
        let record = worker_owner::validate_bundle(bundle_json.as_bytes(), Some(&expected))?;
        let (report_json, verification_json): (String, String) = db.query_row("SELECT report_json,verification_json FROM owner_evidence WHERE run=?1 AND attempt=?2", params![run.id, run.attempt], |row| Ok((row.get(0)?, row.get(1)?)))?;
        let report: Value = serde_json::from_str(&report_json)?;
        let verification: OwnerVerification = serde_json::from_str(&verification_json)?;
        if worker_owner::owner_bundle_sha256(&bundle)? != reference.bundle_sha256
            || record_sha != bundle.record_sha256
            || worker_owner::owner_report_sha256(&expected.execution, &report, &verification)? != reference.report_sha256
            || run.detail.get("report") != Some(&report)
            || run.detail.get("verification") != Some(&serde_json::to_value(&verification)?) { return Ok(false); }
        if record.direct_wait.exit_code != Some(0) && (run.status == "done" || verification.passed) { return Ok(false); }
    }
    let changed = db.execute("UPDATE assignments SET released=1 WHERE run=?1 AND attempt=?2 AND released=0", params![run.id, run.attempt])?;
    if changed != 1 { return Ok(false); }
    if run.status == "cancel_pending" { run.status = if run.detail.get("failure").is_some() { "failed" } else { "cancelled" }.into(); }
    run.detail["retry_safe"] = json!(true);
    run.detail["exit_proof"] = serde_json::to_value(proof)?;
    save_run(db, &run)?;
    Ok(true)
}

impl Run {
    pub fn execution_identity(&self, deployment: &str) -> Result<ExecutionIdentity> {
        ExecutionIdentity::from_detail(
            deployment,
            &self.id,
            self.attempt,
            &self.epoch,
            self.session.as_deref(),
            &self.detail,
        )
    }
}

fn owner_terminal_payload(payload: &Value) -> Result<Option<OwnerReportPayload>> {
    if payload.get("phase").is_none() { return Ok(None); }
    let typed: OwnerReportPayload = serde_json::from_value(payload.clone())?;
    ensure!(match &typed {
        OwnerReportPayload::Launch { schema, .. } => schema == "owner-evidence-launch/1",
        OwnerReportPayload::Terminal { schema, .. } => schema == "owner-evidence-report/1",
    }, "unsupported owner report schema");
    Ok(Some(typed))
}

fn admit_owner_launch_tx(
    tx: &rusqlite::Transaction<'_>,
    deployment: &str,
    message: &Envelope,
    received_at: u64,
) -> Result<bool> {
    let run = read_run(tx, &message.run_id)?;
    let assignment = read_assignment(tx, &run.id, message.attempt)?.context("owner launch has no assignment")?;
    ensure!(message.attempt == run.attempt && message.epoch == run.epoch, "stale owner launch");
    let OwnerReportPayload::Launch { schema, session, binding } = owner_terminal_payload(&message.payload)?.context("owner launch payload missing")? else {
        anyhow::bail!("owner launch payload required");
    };
    ensure!(schema == "owner-evidence-launch/1", "unsupported owner launch schema");
    ensure!(run.session.as_deref() == Some(session.as_str()), "owner launch session mismatch");
    worker_owner::validate_expected_binding(&binding)?;
    ensure!(binding.execution == run.execution_identity(deployment)?, "owner launch execution mismatch");
    ensure!(binding.owner.node_id == assignment.node_id, "owner launch node mismatch");
    let start_payload_raw: String = tx.query_row(
        "SELECT json_extract(body,'$.payload') FROM outbox WHERE json_extract(body,'$.run_id')=?1 AND json_extract(body,'$.attempt')=?2 AND json_extract(body,'$.epoch')=?3 AND json_extract(body,'$.kind')='start' ORDER BY rowid LIMIT 1",
        params![run.id, run.attempt, run.epoch], |row| row.get(0),
    ).optional()?.context("owner launch start request missing")?;
    let start_payload: Value = serde_json::from_str(&start_payload_raw).context("invalid persisted start payload")?;
    ensure!(binding.request_sha256 == hex::encode(Sha256::digest(canonical(&start_payload)?)),
        "owner launch request digest mismatch");
    let binding_sha = worker_owner::owner_binding_sha256(&binding)?;
    let existing: Option<(String, String, String, String)> = tx.query_row(
        "SELECT epoch,binding_sha256,expected_json,address FROM owner_bindings WHERE run=?1 AND attempt=?2",
        params![run.id, run.attempt],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    ).optional()?;
    if let Some((epoch, prior_sha, prior_json, address)) = existing {
        ensure!(epoch == run.epoch && prior_sha == binding_sha && prior_json == serde_json::to_string(&binding)?
            && address == assignment.address, "owner launch binding replacement conflict");
    } else {
        tx.execute("INSERT INTO owner_bindings(run,attempt,epoch,binding_sha256,expected_json,source_message_id,registered_at,node,address) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
            params![run.id, run.attempt, run.epoch, binding_sha, serde_json::to_string(&binding)?, message.message_id, received_at as i64, assignment.node_id, assignment.address])?;
    }
    let mut updated=run;
    updated.detail["owner_launch"]=message.payload.clone();
    save_run(tx,&updated)?;
    tx.execute("INSERT INTO inbox(id,body,handled,received_at) VALUES(?1,?2,1,?3) ON CONFLICT(id) DO UPDATE SET body=excluded.body,handled=1,received_at=excluded.received_at",
        params![message.message_id, serde_json::to_string(message)?, received_at as i64])?;
    Ok(true)
}

fn admit_owner_terminal_tx(
    tx: &rusqlite::Transaction<'_>,
    deployment: &str,
    message: &Envelope,
    received_at: u64,
) -> Result<bool> {
    let run = read_run(tx, &message.run_id)?;
    let assignment = read_assignment(tx, &run.id, message.attempt)?.context("owner message has no assignment")?;
    ensure!(message.attempt == run.attempt && message.epoch == run.epoch, "stale owner execution");
    let payload = owner_terminal_payload(&message.payload)?.context("owner terminal payload missing")?;
    let OwnerReportPayload::Terminal { session, report, verification, owner_evidence, exit_proof, .. } = &payload else {
        anyhow::bail!("owner terminal payload required");
    };
    ensure!(exit_proof.run_id == run.id && exit_proof.attempt == run.attempt && exit_proof.epoch == run.epoch,
        "owner proof execution mismatch");
    ensure!(exit_proof.session.as_deref() == Some(session.as_str()) && run.session.as_deref() == Some(session.as_str()),
        "owner proof session mismatch");
    ensure!(exit_proof.empty && exit_proof.observer_alive && !exit_proof.not_started, "owner proof is not terminal process evidence");
    let Some(reference) = exit_proof.owner_evidence.as_ref() else { anyhow::bail!("owner evidence reference missing"); };
    ensure!(reference == &owner_evidence.reference, "owner evidence reference mismatch");
    worker_owner::validate_owner_evidence_reference(reference)?;
    worker_owner::validate_owner_verification(&verification)?;
    ensure!(report.is_object(), "owner report must be an object");
    // Same bound as worker_owner::owner_report_sha256 (run bc1cf37d raise):
    // the enriched project report carries the artifact package as content_hex.
    ensure!(canonical(report)?.len() <= 73_728, "owner report exceeds 73728 canonical bytes");
    ensure!(owner_evidence.bundle_json.len() <= 36_864, "owner bundle exceeds 36864 bytes");

    let expected = read_owner_binding(tx, &run.id, run.attempt)?
        .context("independently registered local owner binding required")?;
    worker_owner::validate_expected_binding(&expected)?;
    ensure!(expected.execution == run.execution_identity(deployment)?, "owner binding execution mismatch");
    ensure!(expected.owner.node_id == assignment.node_id, "owner binding node mismatch");
    let binding_sha = worker_owner::owner_binding_sha256(&expected)?;
    ensure!(binding_sha == reference.binding_sha256, "owner binding digest mismatch");
    let bundle: OwnerEvidenceBundle = serde_json::from_slice(owner_evidence.bundle_json.as_bytes())?;
    let record = worker_owner::validate_bundle(owner_evidence.bundle_json.as_bytes(), Some(&expected))?;
    let bundle_sha = worker_owner::owner_bundle_sha256(&bundle)?;
    ensure!(bundle_sha == reference.bundle_sha256 && bundle.record_sha256 == reference.record_sha256,
        "owner bundle digest mismatch");
    ensure!(worker_owner::owner_report_sha256(&expected.execution, report, &verification)? == reference.report_sha256,
        "owner report digest mismatch");
    if record.direct_wait.exit_code != Some(0) || record.direct_wait.termination_signal.is_some() {
        ensure!(!verification.passed, "nonzero or signal terminal cannot verify success");
    }
    let existing: Option<(String,String,String,String,String,String)> = tx.query_row(
        "SELECT epoch,session,binding_sha256,bundle_sha256,record_sha256,report_sha256 FROM owner_evidence WHERE run=?1 AND attempt=?2",
        params![run.id, run.attempt], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).optional()?;
    if let Some(existing) = existing {
        ensure!(existing == (run.epoch.clone(), session.clone(), reference.binding_sha256.clone(), reference.bundle_sha256.clone(), reference.record_sha256.clone(), reference.report_sha256.clone()), "owner evidence replacement conflict");
    } else {
        tx.execute("INSERT INTO owner_evidence(run,attempt,epoch,session,binding_sha256,bundle_sha256,record_sha256,report_sha256,bundle_json,report_json,verification_json,source_message_id,admitted_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13)", params![run.id,run.attempt,run.epoch,session,reference.binding_sha256,reference.bundle_sha256,reference.record_sha256,reference.report_sha256,owner_evidence.bundle_json,serde_json::to_string(report)?,serde_json::to_string(&verification)?,message.message_id,received_at as i64])?;
    }
    let mut updated = run;
    if !matches!(updated.status.as_str(), "cancel_pending"|"cancelled"|"failed"|"done") {
        updated.status = if verification.passed { "done" } else { "failed" }.into();
    }
    updated.detail["report"] = report.clone();
    updated.detail["verification"] = serde_json::to_value(&verification)?;
    updated.detail["owner_admission"] = json!("admitted");
    save_run(tx, &updated)?;
    if !assignment.released {
        ensure!(release_tx(tx, deployment, exit_proof)?, "owner admission did not pass release gate");
    }
    tx.execute("INSERT INTO inbox(id,body,handled,received_at) VALUES(?1,?2,1,?3) ON CONFLICT(id) DO UPDATE SET body=excluded.body,handled=1,received_at=excluded.received_at", params![message.message_id,serde_json::to_string(message)?,received_at as i64])?;
    Ok(true)
}

impl Store {
    pub fn open(path:&Path,deployment:&str,nodes:&[NodeSpec])->Result<Self>{
        ensure!(crate::config::valid_id(deployment),"invalid deployment");
        let(mut ids,mut addresses,mut keys)=(BTreeSet::new(),BTreeSet::new(),BTreeSet::new());
        for n in nodes {n.validate()?;ensure!(ids.insert(&n.id)&&addresses.insert(&n.address)&&keys.insert(n.key_hex.to_lowercase()),"duplicate node identity/address/key");}
        if let Some(parent)=path.parent(){std::fs::create_dir_all(parent)?;}
        let mut db=Connection::open(path)?;db.busy_timeout(Duration::from_secs(3))?;
        let version:u32=db.pragma_query_value(None,"user_version",|r|r.get(0))?;
        ensure!(version<=2,"unsupported future native schema");
        if version==0 {let count:i64=db.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'",[],|r|r.get(0))?;ensure!(count==0,"refusing non-native/legacy database");}
        db.pragma_update(None,"journal_mode","WAL")?;db.pragma_update(None,"synchronous","FULL")?;db.pragma_update(None,"foreign_keys",true)?;
        let tx=db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute_batch("CREATE TABLE IF NOT EXISTS metadata(key TEXT PRIMARY KEY,value TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS nodes(id TEXT PRIMARY KEY,address TEXT NOT NULL,fingerprint TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS runs(seq INTEGER PRIMARY KEY AUTOINCREMENT,id TEXT NOT NULL UNIQUE,body TEXT NOT NULL,status TEXT NOT NULL,attempt INTEGER NOT NULL);
            CREATE TABLE IF NOT EXISTS assignments(run TEXT NOT NULL,attempt INTEGER NOT NULL,node TEXT NOT NULL,address TEXT NOT NULL,released INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(run,attempt));
            CREATE TABLE IF NOT EXISTS attempts(run TEXT NOT NULL,attempt INTEGER NOT NULL,body TEXT NOT NULL,PRIMARY KEY(run,attempt));
            CREATE TABLE IF NOT EXISTS outbox(id TEXT PRIMARY KEY,destination TEXT NOT NULL,body TEXT NOT NULL,sent INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS inbox(id TEXT PRIMARY KEY,body TEXT NOT NULL,handled INTEGER NOT NULL DEFAULT 0);
            CREATE TABLE IF NOT EXISTS approvals(run TEXT NOT NULL,attempt INTEGER NOT NULL,session TEXT NOT NULL,request TEXT NOT NULL,body TEXT NOT NULL,decided INTEGER NOT NULL DEFAULT 0,PRIMARY KEY(run,attempt,session,request));
            CREATE TABLE IF NOT EXISTS resolve_intents(run TEXT NOT NULL,attempt INTEGER NOT NULL,session TEXT NOT NULL,request TEXT NOT NULL,decision TEXT NOT NULL,PRIMARY KEY(run,attempt,session,request));
            CREATE TABLE IF NOT EXISTS audit(seq INTEGER PRIMARY KEY AUTOINCREMENT,body TEXT NOT NULL);
            CREATE TABLE IF NOT EXISTS owner_bindings(
                run TEXT NOT NULL, attempt INTEGER NOT NULL, epoch TEXT NOT NULL,
                binding_sha256 TEXT NOT NULL UNIQUE, expected_json TEXT NOT NULL,
                source_message_id TEXT NOT NULL, registered_at INTEGER NOT NULL,
                node TEXT NOT NULL, address TEXT NOT NULL,
                PRIMARY KEY(run,attempt));
            CREATE TABLE IF NOT EXISTS owner_evidence(
                run TEXT NOT NULL, attempt INTEGER NOT NULL, epoch TEXT NOT NULL,
                session TEXT NOT NULL, binding_sha256 TEXT NOT NULL,
                bundle_sha256 TEXT NOT NULL UNIQUE, record_sha256 TEXT NOT NULL UNIQUE,
                report_sha256 TEXT NOT NULL, bundle_json TEXT NOT NULL,
                report_json TEXT NOT NULL, verification_json TEXT NOT NULL,
                source_message_id TEXT NOT NULL, admitted_at INTEGER NOT NULL,
                PRIMARY KEY(run,attempt));")?;
        let existing:Option<String>=tx.query_row("SELECT value FROM metadata WHERE key='deployment'",[],|r|r.get(0)).optional()?;
        if let Some(value)=existing {ensure!(value==deployment,"deployment identity changed");}
        else{tx.execute("INSERT INTO metadata(key,value) VALUES('deployment',?1)",[deployment])?;}
        let stored:i64=tx.query_row("SELECT count(*) FROM nodes",[],|r|r.get(0))?;
        if version>0 {ensure!(stored==nodes.len() as i64,"static node set changed");}
        for n in nodes {
            let fingerprint=hex::encode(Sha256::digest(hex::decode(&n.key_hex)?));
            let prior:Option<(String,String)>=tx.query_row("SELECT address,fingerprint FROM nodes WHERE id=?1",[&n.id],|r|Ok((r.get(0)?,r.get(1)?))).optional()?;
            if let Some((address,key))=prior {ensure!(address==n.address && key==fingerprint,"node address/key identity changed");}
            else {ensure!(version==0,"node identity changed");tx.execute("INSERT INTO nodes VALUES(?1,?2,?3)",params![n.id,n.address,fingerprint])?;}
        }
        if version<2 { tx.execute_batch("ALTER TABLE inbox ADD COLUMN received_at INTEGER;")?; }
        tx.pragma_update(None,"user_version",2)?;tx.commit()?;
        let readiness_required=nodes.iter().any(|node|!node.healthy);
        Ok(Self{db,deployment:deployment.into(),nodes:nodes.to_vec(),readiness_required})
    }
    /// Trusted local fixture seam. The complete expected binding must be supplied
    /// independently before terminal admission; no wire/provider input enrolls it.
    pub fn register_local_owner_binding(
        &mut self,
        run_id: &str,
        attempt: u32,
        expected: &ExpectedOwnerBinding,
        now: u64,
    ) -> Result<()> {
        worker_owner::validate_expected_binding(expected)?;
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let run = read_run(&tx, run_id)?;
        ensure!(run.attempt == attempt && expected.execution == run.execution_identity(&self.deployment)?, "owner binding execution fence mismatch");
        let assignment = read_assignment(&tx, run_id, attempt)?.context("owner binding assignment missing")?;
        ensure!(!assignment.released && assignment.node_id == expected.owner.node_id, "owner binding assignment/node mismatch");
        ensure!(self.nodes.iter().any(|node| node.id == assignment.node_id && node.address == assignment.address), "owner binding pinned node/address missing");
        let binding_sha = worker_owner::owner_binding_sha256(expected)?;
        let prior: Option<(String,String,String,String,String)> = tx.query_row(
            "SELECT epoch,binding_sha256,expected_json,node,address FROM owner_bindings WHERE run=?1 AND attempt=?2",
            params![run_id, attempt], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?))).optional()?;
        if let Some((epoch, prior_sha, prior_json, node, address)) = prior {
            ensure!(epoch == run.epoch && prior_sha == binding_sha && prior_json == serde_json::to_string(expected)?
                && node == assignment.node_id && address == assignment.address, "owner binding is immutable");
        } else {
            tx.execute("INSERT INTO owner_bindings(run,attempt,epoch,binding_sha256,expected_json,source_message_id,registered_at,node,address) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,?9)",
                params![run_id,attempt,run.epoch,binding_sha,serde_json::to_string(expected)?,"local-owner-binding",now as i64,assignment.node_id,assignment.address])?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn receive(&mut self, message:&Envelope, now:u64)->Result<bool>{
        if message.kind=="ready"{return self.receive_ready(message,now)}
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut run=read_run(&tx,&message.run_id)?;
        let a=read_assignment(&tx,&run.id,message.attempt)?.context("message has no assignment")?;
        let node=self.nodes.iter().find(|n|n.id==a.node_id && n.address==a.address).context("pinned node missing")?;
        message.verify(&hex::decode(&node.key_hex)?,&self.deployment,"A",now)?;
        ensure!(message.from==a.node_id && run.attempt==message.attempt && run.epoch==message.epoch,"stale/foreign execution");
        let prior:Option<String>=tx.query_row("SELECT body FROM inbox WHERE id=?1",[&message.message_id],|r|r.get(0)).optional()?;
        let body=serde_json::to_string(message)?;
        if let Some(prior)=prior {ensure!(prior==body,"message id collision");return Ok(false)}
        if message.kind == "report" && message.payload.get("phase").is_some() {
            if message.payload["phase"] == "launch" {
                admit_owner_launch_tx(&tx, &self.deployment, message, now)?;
            } else {
                admit_owner_terminal_tx(&tx, &self.deployment, message, now)?;
            }
            tx.commit()?;
            return Ok(true);
        }
        let p=&message.payload;
        let protected=matches!(run.status.as_str(),"cancel_pending"|"cancelled"|"failed"|"done");
        match message.kind.as_str(){
            "status"=>{
                if p.get("spawn_intent")==Some(&json!(true)){run.spawn_intent=true;}
                if let Some(session)=p.get("session").and_then(Value::as_str){
                    ensure!(crate::config::valid_id(session) && run.session.as_deref().is_none_or(|s|s==session),"session identity changed");run.session=Some(session.into());
                }
                let state=p.get("state").and_then(Value::as_str).context("status missing state")?;
                ensure!(matches!(state,"running"|"waiting_approval"|"unknown"|"failed"|"cancel_pending"|"cancelled"),"invalid runner status");
                if !protected {run.status=state.into();}
                if let Some(reason)=p.get("reason"){run.detail["reason"]=reason.clone();}
                if let Some(failure)=p.get("failure"){run.detail["failure"]=failure.clone();}
            },
            "approval"=>{
                let session=p.get("session").and_then(Value::as_str).context("approval session missing")?;
                ensure!(run.session.as_deref()==Some(session),"approval session mismatch");
                let request=p.get("request_id").and_then(Value::as_str).context("approval request missing")?;
                ensure!(crate::config::valid_id(request),"invalid request id");
                ensure!(p.get("generation").and_then(Value::as_u64).is_some_and(|v|v>0),"invalid generation");
                ensure!(p.get("scope_hash").and_then(Value::as_str).is_some_and(|s|!s.is_empty()&&s.len()<=128),"invalid scope hash");
                ensure!(p.get("expires_at").and_then(Value::as_u64).is_some_and(|v|v>now&&v<=run.deadline),"approval expired");
                let offered=p.get("offered").and_then(Value::as_array).context("offered decisions missing")?;
                ensure!(!offered.is_empty() && offered.len()<=2 && offered.iter().all(|v|matches!(v.as_str(),Some("allow_once"|"reject_once"))),"unsupported decision");
                if !protected {
                    let previous:Option<String>=tx.query_row("SELECT body FROM approvals WHERE run=?1 AND attempt=?2 AND session=?3 AND request=?4",params![run.id,run.attempt,session,request],|r|r.get(0)).optional()?;
                    if let Some(previous)=previous {ensure!(serde_json::from_str::<Value>(&previous)?==*p,"approval identity/scope changed");}
                    else{tx.execute("INSERT INTO approvals(run,attempt,session,request,body) VALUES(?1,?2,?3,?4,?5)",params![run.id,run.attempt,session,request,serde_json::to_string(p)?])?;}
                    run.status="waiting_approval".into();
                }
            },
            "report"=>{
                ensure!(p.get("session").and_then(Value::as_str)==run.session.as_deref()&&run.session.is_some(),"report session mismatch");
                ensure!(p.get("report").is_some_and(Value::is_object)&&p.get("verification").is_some_and(Value::is_object),"report/verifier missing");
                if owner_evidence_row(&tx, &run.id, run.attempt)?.is_none() {
                    run.detail["report"]=p["report"].clone();run.detail["verification"]=p["verification"].clone();
                    if !protected {run.status="unknown".into();}
                }
            },
            "error"=>{run.detail["reason"]=p.clone();if !protected{run.status="unknown".into();}},
            _=>anyhow::bail!("invalid controller event kind"),
        }
        run.detail["contact_at"]=json!(now);run.detail["contact_source"]=json!("authenticated runner envelope");
        if p.get("progress")==Some(&json!(true)){run.detail["progress_at"]=json!(now);run.detail["progress_source"]=json!("runner event");}
        save_run(&tx,&run)?;
        if let Some(proof)=p.get("exit_proof").filter(|v|!v.is_null()) {
            let parsed: ExitProof = serde_json::from_value(proof.clone())?;
            if parsed.not_started && parsed.owner_evidence.is_none() { release_tx(&tx, &self.deployment, &parsed)?; }
        }
        tx.execute("INSERT INTO inbox(id,body,handled) VALUES(?1,?2,1)",params![message.message_id,body])?;
        tx.commit()?;Ok(true)
    }
    fn probe_node(&mut self,node:&NodeSpec,now:u64)->Result<()> {
        let expires_at=now.checked_add(60).context("probe expiry overflow")?;
        let run=Run{id:Uuid::new_v4().to_string(),owner:Actor::Local("native-readiness".into()),template:String::new(),status:"probe".into(),attempt:1,epoch:Uuid::new_v4().to_string(),session:None,spawn_intent:false,deadline:expires_at,detail:json!({})};
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        // Superseded unsent readiness probes must not solicit obsolete ready replies.
        // Execution/cancel/decision messages and all reservations are untouched.
        tx.execute("DELETE FROM outbox WHERE sent=0 AND destination=?1 AND json_extract(body,'$.kind')='probe'",[&node.address])?;
        tx.execute("INSERT INTO metadata(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",params![format!("probe:{}",node.id),json!({"run_id":run.id,"epoch":run.epoch,"expires_at":expires_at,"ready":false}).to_string()])?;
        enqueue(&tx,&self.deployment,node,&run,"probe",json!({"templates":node.templates}),now)?;
        tx.commit()?;Ok(())
    }
    pub fn probe_nodes(&mut self,now:u64)->Result<()> {
        for node in self.nodes.clone(){if node.admitted&&!node.draining{self.probe_node(&node,now)?;}}
        Ok(())
    }
    pub(crate) fn refresh_probes(&mut self,now:u64)->Result<()> {
        for node in self.nodes.clone(){
            if !node.admitted||node.draining{continue}
            let raw:Option<String>=self.db.query_row("SELECT value FROM metadata WHERE key=?1",[format!("probe:{}",node.id)],|row|row.get(0)).optional()?;
            let current=raw.map(|raw|serde_json::from_str::<Value>(&raw)).transpose()?;
            // Both a verified ready and an unanswered, unexpired probe keep identity.
            let pending=current.is_some_and(|value|value["invalidated"]!=true&&value["expires_at"].as_u64().is_some_and(|expires|now<expires));
            if !pending{self.probe_node(&node,now)?;}
        }
        Ok(())
    }
    fn receive_ready(&mut self,m:&Envelope,now:u64)->Result<bool>{let node=self.nodes.iter().find(|n|n.id==m.from).context("unconfigured readiness node")?.clone();m.verify(&hex::decode(&node.key_hex)?,&self.deployment,"A",now)?;let key=format!("probe:{}",node.id);let raw:String=self.db.query_row("SELECT value FROM metadata WHERE key=?1",[&key],|r|r.get(0))?;let p:Value=serde_json::from_str(&raw)?;let probe_expires=p["expires_at"].as_u64().context("probe expiry missing")?;if p["invalidated"]==true||p["run_id"]!=m.run_id||p["epoch"]!=m.epoch||now>=probe_expires{return Err(ObsoleteReady.into())}ensure!(m.attempt==1&&m.expires_at<=probe_expires,"readiness response does not match current probe");ensure!(m.payload["address"]==node.address&&m.payload["ao_project_ready"]==true,"runner AO project not ready");let templates=m.payload["templates"].as_array().context("ready templates missing")?;ensure!(node.templates.iter().all(|t|templates.contains(&json!(t))),"runner template capability mismatch");let body=serde_json::to_string(m)?;let inserted=self.db.execute("INSERT OR IGNORE INTO inbox(id,body,handled) VALUES(?1,?2,1)",params![m.message_id,body])?;if inserted==1{let mut current=p;current["ready"]=json!(true);current["address"]=json!(node.address);current["templates"]=Value::Array(templates.clone());self.db.execute("UPDATE metadata SET value=?1 WHERE key=?2",params![current.to_string(),key])?;self.nodes.iter_mut().find(|n|n.id==node.id).context("node disappeared")?.healthy=true;}Ok(inserted==1)}
    pub fn invalidate_readiness(&mut self)->Result<()> {
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let keys:Vec<String>={let mut stmt=tx.prepare("SELECT key FROM metadata WHERE key LIKE 'probe:%'")?;stmt.query_map([],|row|row.get(0))?.collect::<std::result::Result<_,_>>()?};
        for key in keys {let raw:String=tx.query_row("SELECT value FROM metadata WHERE key=?1",[&key],|row|row.get(0))?;let mut value:Value=serde_json::from_str(&raw)?;value["ready"]=json!(false);value["invalidated"]=json!(true);tx.execute("UPDATE metadata SET value=?1 WHERE key=?2",params![value.to_string(),key])?;}
        tx.commit()?;for node in &mut self.nodes {node.healthy=false;}Ok(())
    }
    pub fn readiness(&self,now:u64)->Result<Vec<NodeReadiness>> {
        self.nodes.iter().map(|node| {
            let used:i64=self.db.query_row("SELECT count(*) FROM assignments WHERE node=?1 AND released=0",[&node.id],|row|row.get(0))?;
            let raw:Option<String>=self.db.query_row("SELECT value FROM metadata WHERE key=?1",[format!("probe:{}",node.id)],|row|row.get(0)).optional()?;
            let verified=match raw.and_then(|raw|serde_json::from_str::<Value>(&raw).ok()) {Some(value)=>value["ready"]==true&&value["expires_at"].as_u64().is_some_and(|expires|now<expires),None=>node.healthy};
            Ok(NodeReadiness{id:node.id.clone(),address:node.address.clone(),used:u32::try_from(used)?,slots:node.slots,admitted:node.admitted,healthy:node.healthy&&verified,draining:node.draining,verified,observed_at:now})
        }).collect()
    }
    pub fn approvals(&self,actor:&Actor,id:&str,now:u64)->Result<Vec<Value>>{
        let run=owned(&self.db,actor,id)?;
        if !matches!(run.status.as_str(),"running"|"waiting_approval"){return Ok(Vec::new())}
        let mut stmt=self.db.prepare("SELECT body FROM approvals WHERE run=?1 AND attempt=?2 AND session=?3 AND decided=0 ORDER BY request")?;
        let rows=stmt.query_map(params![id,run.attempt,run.session],|r|r.get::<_,String>(0))?;
        let mut result=Vec::new();for row in rows {let p:Value=serde_json::from_str(&row?)?;if p["expires_at"].as_u64().is_some_and(|v|v>now){result.push(p);}}
        Ok(result)
    }
    pub fn decide(&mut self,actor:&Actor,d:&super::controller::Decision,now:u64)->Result<()> {
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;let r=owned(&tx,actor,&d.run_id)?;
        ensure!(matches!(r.status.as_str(),"running"|"waiting_approval")&&now<r.deadline,"execution not accepting decisions");
        let session=r.session.as_ref().context("session unknown")?;
        let raw:Option<String>=tx.query_row("SELECT body FROM approvals WHERE run=?1 AND attempt=?2 AND session=?3 AND request=?4 AND decided=0",params![r.id,r.attempt,session,d.request_id],|r|r.get(0)).optional()?;
        let p:Value=serde_json::from_str(&raw.context("approval absent/already decided")?)?;
        ensure!(p["generation"].as_u64()==Some(d.generation)&&p["scope_hash"].as_str()==Some(&d.scope_hash)&&p["expires_at"].as_u64().is_some_and(|v|v>now),"approval scope/generation/expiry mismatch");
        ensure!(matches!(d.decision_id.as_str(),"allow_once"|"reject_once")&&p["offered"].as_array().is_some_and(|v|v.contains(&json!(d.decision_id))),"decision was not offered");
        tx.execute("INSERT INTO resolve_intents VALUES(?1,?2,?3,?4,?5)",params![r.id,r.attempt,session,d.request_id,d.decision_id])?;
        tx.execute("UPDATE approvals SET decided=1 WHERE run=?1 AND attempt=?2 AND session=?3 AND request=?4",params![r.id,r.attempt,session,d.request_id])?;
        let a=read_assignment(&tx,&r.id,r.attempt)?.context("assignment missing")?;let node=self.nodes.iter().find(|n|n.id==a.node_id&&n.address==a.address).context("pinned node missing")?;
        enqueue(&tx,&self.deployment,node,&r,"decision",json!({"session":session,"request_id":d.request_id,"generation":d.generation,"scope_hash":d.scope_hash,"decision_id":d.decision_id}),now)?;
        tx.commit()?;Ok(())
    }
    pub fn pending_outbox_raw(&self,limit:usize)->Result<Vec<(String,String,Vec<u8>)>>{
        let mut stmt=self.db.prepare("SELECT id,destination,body FROM outbox WHERE sent=0 ORDER BY rowid LIMIT ?1")?;
        let rows=stmt.query_map([limit.min(100) as i64],|r|Ok((r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,String>(2)?.into_bytes())))?;
        rows.collect::<std::result::Result<Vec<_>,_>>().map_err(Into::into)
    }
    pub fn pending_outbox(&self,limit:usize)->Result<Vec<(String,Envelope)>>{
        self.pending_outbox_raw(limit)?.into_iter().map(|(_id,destination,body)|Ok((destination,serde_json::from_slice(&body)?))).collect()
    }
    pub fn acknowledge_outbox(&mut self,id:&str)->Result<()> {ensure!(self.db.execute("UPDATE outbox SET sent=1 WHERE id=?1",[id])?==1,"outbox id missing");Ok(())}
    pub fn list_runs(&self,limit:usize)->Result<Vec<Run>>{let mut stmt=self.db.prepare("SELECT body FROM runs ORDER BY seq DESC LIMIT ?1")?;let rows=stmt.query_map([limit.min(200) as i64],|r|r.get::<_,String>(0))?;rows.map(|r|Ok(serde_json::from_str(&r?)?)).collect()}
    pub fn expire(&mut self,now:u64)->Result<usize>{
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let ids:Vec<String>={let mut stmt=tx.prepare("SELECT id FROM runs WHERE status NOT IN ('done','failed','cancelled')")?;stmt.query_map([],|r|r.get(0))?.collect::<std::result::Result<_,_>>()?};
        let mut count=0;for id in ids {let mut r=read_run(&tx,&id)?;if now<r.deadline{continue}count+=1;r.detail["failure"]=json!("deadline expired");
            if read_assignment(&tx,&id,r.attempt)?.is_none()&&!r.spawn_intent&&r.session.is_none(){r.status="failed".into();r.detail["retry_safe"]=json!(true);r.detail["not_started"]=json!(true);}
            else if r.status!="cancel_pending" {r.status="cancel_pending".into();if let Some(a)=read_assignment(&tx,&id,r.attempt)?{let n=self.nodes.iter().find(|n|n.id==a.node_id).context("node missing")?;enqueue(&tx,&self.deployment,n,&r,"cancel",json!({"reason":"deadline expired"}),now)?;}}
            save_run(&tx,&r)?;
        }tx.commit()?;Ok(count)
    }
    pub fn dispatch(&mut self,actor:&Actor,template:&str,now:u64)->Result<Run>{
        let actor_id=match actor{Actor::Local(s)|Actor::AoSession(s)=>s};ensure!(crate::config::valid_id(actor_id),"invalid actor");
        ensure!(matches!(template,"sum-native-v1"|"control-native-v1"|"sum-v1"|"control-v1"),"unknown fixed template");
        let r=Run{id:Uuid::new_v4().to_string(),owner:actor.clone(),template:template.into(),status:"queued".into(),attempt:1,epoch:Uuid::new_v4().to_string(),session:None,spawn_intent:false,deadline:now.checked_add(900).context("deadline overflow")?,detail:json!({"created_at":now,"progress_at":now})};
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute("INSERT INTO runs(id,body,status,attempt) VALUES(?1,?2,?3,?4)",params![r.id,serde_json::to_string(&r)?,r.status,r.attempt])?;tx.commit()?;Ok(r)
    }
    pub fn dispatch_project(&mut self,actor:&Actor,work:&super::project_work::ProjectWork,now:u64)->Result<Run>{
        work.validate()?;let actor_id=match actor{Actor::Local(s)|Actor::AoSession(s)=>s};ensure!(crate::config::valid_id(actor_id),"invalid trusted actor");
        let run=Run{id:Uuid::new_v4().to_string(),owner:actor.clone(),template:"project-work-v1".into(),status:"queued".into(),attempt:1,epoch:Uuid::new_v4().to_string(),session:None,spawn_intent:false,deadline:now.checked_add(900).context("deadline overflow")?,detail:json!({"created_at":now,"progress_at":now,"project_work":work})};
        // Check the actual signed JSON frame for every trusted possible route before queue insertion.
        let capable:Vec<_>=self.nodes.iter().filter(|n|n.templates.iter().any(|t|t=="project-work-v1")).collect();ensure!(!capable.is_empty(),"no project-work capable node configured");
        for node in capable{let mut m=Envelope{schema:PROTOCOL.into(),message_id:Uuid::new_v4().to_string(),deployment:self.deployment.clone(),from:"A".into(),to:node.id.clone(),run_id:run.id.clone(),attempt:run.attempt,epoch:run.epoch.clone(),kind:"start".into(),sent_at:now,expires_at:now.saturating_add(300),payload:json!({"template":run.template,"deadline":run.deadline,"project_work":work}),mac:String::new()};m.sign(&hex::decode(&node.key_hex)?)?;m.verify(&hex::decode(&node.key_hex)?,&self.deployment,&node.id,now).context("document execution exceeds native signed frame limit; import is retained but no task is queued")?;}
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;tx.execute("INSERT INTO runs(id,body,status,attempt) VALUES(?1,?2,?3,?4)",params![run.id,serde_json::to_string(&run)?,run.status,run.attempt])?;tx.commit()?;Ok(run)
    }
    pub fn run(&self,id:&str)->Result<Run>{read_run(&self.db,id)}
    /// Annotate a run's detail with a manager-local observation (e.g. the
    /// materialized artifact directory). Detail-only: status, attempt and
    /// identity are immutable through this path.
    pub fn annotate_run(&mut self,id:&str,key:&str,value:Value)->Result<()>{
        let mut run=self.run(id)?;
        ensure!(!matches!(key,"status"|"attempt"|"epoch"|"identity"),"protected detail key");
        run.detail[key]=value;save_run(&self.db,&run)?;Ok(())
    }
    pub fn owned(&self,actor:&Actor,id:&str)->Result<Run>{owned(&self.db,actor,id)}
    pub fn assign_next(&mut self,now:u64)->Result<Option<Assignment>>{ self.assign_pinned(now,None,"",None) }
    pub fn assign_next_with_worker_artifact(&mut self,now:u64,worker_id:&str,artifact:impl Into<Option<String>>)->Result<Option<Assignment>>{ self.assign_pinned(now,None,worker_id,artifact.into()) }
    pub fn assign_run_with_worker_artifact(&mut self,now:u64,run_id:&str,worker_id:&str,artifact:String)->Result<Option<Assignment>>{ self.assign_pinned(now,Some(run_id),worker_id,Some(artifact)) }
    fn assign_pinned(&mut self,now:u64,run_id:Option<&str>,worker_id:&str,artifact:Option<String>)->Result<Option<Assignment>>{
        if let Some(value)=&artifact { ensure!(!worker_id.is_empty()&&value.len()==64&&value.bytes().all(|b|b.is_ascii_hexdigit()),"invalid pinned worker artifact SHA256"); }
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let id:Option<String>=tx.query_row("SELECT id FROM runs WHERE status='queued' AND (?1 IS NULL OR id=?1) ORDER BY seq LIMIT 1",[run_id],|r|r.get(0)).optional()?;
        let Some(id)=id else{return Ok(None)};
        let mut run=read_run(&tx,&id)?;
        let prior:Option<String>=tx.query_row("SELECT node FROM assignments WHERE run=?1 ORDER BY attempt DESC LIMIT 1",[&id],|r|r.get(0)).optional()?;
        let mut choices=Vec::new();
        for n in &self.nodes {
            let ready:Option<String>=tx.query_row("SELECT value FROM metadata WHERE key=?1",[format!("probe:{}",n.id)],|row|row.get(0)).optional()?;
            let verified=match ready.and_then(|raw|serde_json::from_str::<Value>(&raw).ok()) {Some(value)=>value["ready"]==true&&value["expires_at"].as_u64().is_some_and(|expires|now<expires),None=>n.healthy};
            let direct_pinned=artifact.is_some()&&n.id==worker_id&&matches!(run.template.as_str(),"sum-v1"|"project-work-v1");
            if !n.admitted||(!direct_pinned&&(!n.healthy||(self.readiness_required&&!verified)))||n.draining||!n.templates.contains(&run.template)||prior.as_ref().is_some_and(|p|p!=&n.id)||(!worker_id.is_empty()&&n.id!=worker_id){continue;}
            let used:i64=tx.query_row("SELECT count(*) FROM assignments WHERE node=?1 AND released=0",[&n.id],|r|r.get(0))?;
            if used<i64::from(n.slots){choices.push((n,used));}
        }
        choices.sort_by(|(a,ua),(b,ub)|{((*ua as u128)*u128::from(b.slots)).cmp(&((*ub as u128)*u128::from(a.slots))).then_with(||a.id.cmp(&b.id))});
        let Some((node,_))=choices.first() else{return Ok(None)};
        tx.execute("INSERT INTO assignments(run,attempt,node,address,released) VALUES(?1,?2,?3,?4,0)",params![run.id,run.attempt,node.id,node.address])?;
        run.status="accepted".into();run.detail["assigned_at"]=json!(now);run.detail["worker_id"]=json!(node.id);
        if let Some(value)=artifact {run.detail["worker_artifact_sha256"]=json!(value);}
        save_run(&tx,&run)?;
        let mut payload=json!({"template":run.template,"deadline":run.deadline,"worker_id":node.id});
        if let Some(value)=run.detail.get("worker_artifact_sha256"){payload["worker_artifact_sha256"]=value.clone();}
        if let Some(work)=run.detail.get("project_work"){payload["project_work"]=work.clone();}
        enqueue(&tx,&self.deployment,node,&run,"start",payload,now)?;
        let a=read_assignment(&tx,&run.id,run.attempt)?;tx.commit()?;Ok(a)
    }
    pub fn assignment(&self,id:&str,attempt:u32)->Result<Option<Assignment>>{read_assignment(&self.db,id,attempt)}
    pub fn outbox_count(&self)->Result<u64>{let count:i64=self.db.query_row("SELECT count(*) FROM outbox WHERE sent=0",[],|r|r.get(0))?;Ok(u64::try_from(count)?)}
    pub fn claim(&mut self,id:&str,attempt:u32,epoch:&str)->Result<bool>{
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut r=read_run(&tx,id)?;
        if r.attempt!=attempt||r.epoch!=epoch||r.spawn_intent||r.session.is_some()||!matches!(r.status.as_str(),"accepted"|"running"){return Ok(false)}
        r.spawn_intent=true;save_run(&tx,&r)?;tx.commit()?;Ok(true)
    }
    pub fn bind(&mut self,id:&str,attempt:u32,epoch:&str,session:&str)->Result<()>{
        ensure!(crate::config::valid_id(session),"invalid session");
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut r=read_run(&tx,id)?;
        ensure!(r.attempt==attempt&&r.epoch==epoch&&r.spawn_intent&&r.session.as_ref().is_none_or(|s|s==session),"create response does not match claimed execution");
        r.session=Some(session.into());if matches!(r.status.as_str(),"accepted"|"running"){r.status="running".into();}save_run(&tx,&r)?;tx.commit()?;Ok(())
    }
    /// Authenticate a gateway request against the exact persisted signed start.
    pub fn validate_gateway_start(&self, message:&Envelope, now:u64)->Result<()> {
        let assignment=read_assignment(&self.db,&message.run_id,message.attempt)?.context("gateway assignment missing")?;
        let node=self.nodes.iter().find(|node|node.id==assignment.node_id&&node.address==assignment.address).context("gateway pinned node missing")?;
        message.verify(&hex::decode(&node.key_hex)?,&self.deployment,&node.id,now)?;
        ensure!(message.from=="A"&&message.kind=="start","gateway requires signed start");
        let persisted:String=self.db.query_row("SELECT body FROM outbox WHERE id=?1",[&message.message_id],|row|row.get(0))?;
        ensure!(serde_json::from_str::<Envelope>(&persisted)?==*message,"gateway start differs from durable outbox");
        Ok(())
    }
    /// Local transport uncertainty is never process-exit authority.
    pub fn mark_gateway_unknown(&mut self, expected:&ExecutionIdentity, reason:&str, now:u64)->Result<()> {
        ensure!(expected.deployment==self.deployment,"gateway uncertainty deployment mismatch");
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut run=read_run(&tx,&expected.run_id)?;
        ensure!(run.execution_identity(&self.deployment)?==*expected,"gateway uncertainty identity mismatch");
        if owner_evidence_row(&tx,&run.id,run.attempt)?.is_none() {
            run.status="unknown".into();
            run.detail["reason"]=json!(reason.chars().take(2048).collect::<String>());
            run.detail["transport_unknown_at"]=json!(now);
            save_run(&tx,&run)?;
        }
        tx.commit()?;Ok(())
    }
    pub fn cancel(&mut self,actor:&Actor,id:&str)->Result<Run>{
        let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
        self.cancel_at(actor,id,now)
    }
    pub fn cancel_at(&mut self,actor:&Actor,id:&str,now:u64)->Result<Run>{
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut r=owned(&tx,actor,id)?;
        if matches!(r.status.as_str(),"cancelled"|"failed"|"done"|"cancel_pending"){return Ok(r)}
        r.detail["cancel_requested"]=json!(true);
        if read_assignment(&tx,id,r.attempt)?.is_none()&&!r.spawn_intent&&r.session.is_none(){r.status="cancelled".into();r.detail["retry_safe"]=json!(true);r.detail["not_started"]=json!(true);}
        else {
            r.status="cancel_pending".into();
            if let Some(a)=read_assignment(&tx,id,r.attempt)?{let node=self.nodes.iter().find(|n|n.id==a.node_id).context("pinned node unavailable")?;
                enqueue(&tx,&self.deployment,node,&r,"cancel",json!({}),now)?;}
        }
        save_run(&tx,&r)?;tx.commit()?;Ok(r)
    }
    pub fn release(&mut self, proof: &ExitProof) -> Result<bool> {
        let tx = self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let released = release_tx(&tx, &self.deployment, proof)?;
        tx.commit()?;
        Ok(released)
    }
    pub fn retry(&mut self,actor:&Actor,id:&str,attempt:u32)->Result<Run>{
        let tx=self.db.transaction_with_behavior(TransactionBehavior::Immediate)?;let mut r=owned(&tx,actor,id)?;
        ensure!(r.attempt==attempt&&matches!(r.status.as_str(),"failed"|"cancelled")&&r.detail.get("retry_safe")==Some(&json!(true)),"execution is not safely retryable");
        if let Some(a)=read_assignment(&tx,id,attempt)?{ensure!(a.released,"previous assignment still reserved");}
        tx.execute("INSERT INTO attempts VALUES(?1,?2,?3)",params![id,attempt,serde_json::to_string(&r)?])?;
        r.attempt=r.attempt.checked_add(1).context("attempt overflow")?;r.epoch=Uuid::new_v4().to_string();r.status="queued".into();r.session=None;r.spawn_intent=false;
        let now=std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();r.deadline=now+900;r.detail=json!({"created_at":now,"progress_at":now});save_run(&tx,&r)?;tx.commit()?;Ok(r)
    }
}
