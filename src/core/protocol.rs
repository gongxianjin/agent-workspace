use anyhow::{ensure, Context, Result};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

pub const PROTOCOL: &str = "ao-reason-native/1";

/// Stable lifecycle identity shared by Controller, Runner and evidence records.
///
/// `source_sha256` and `artifact_sha256` are optional so pre-project tasks and
/// older durable records retain the core lifecycle identity contract.
#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct ExecutionIdentity {
    pub deployment: String,
    pub run_id: String,
    pub attempt: u32,
    pub epoch: String,
    #[serde(default)]
    pub session: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_sha256: Option<String>,
}

impl ExecutionIdentity {
    pub fn new(
        deployment: &str,
        run_id: &str,
        attempt: u32,
        epoch: &str,
        session: Option<&str>,
        source_sha256: Option<&str>,
        artifact_sha256: Option<&str>,
    ) -> Self {
        Self {
            deployment: deployment.into(),
            run_id: run_id.into(),
            attempt,
            epoch: epoch.into(),
            session: session.map(str::to_owned),
            source_sha256: source_sha256.map(str::to_owned),
            artifact_sha256: artifact_sha256.map(str::to_owned),
        }
    }

    pub fn from_detail(
        deployment: &str,
        run_id: &str,
        attempt: u32,
        epoch: &str,
        session: Option<&str>,
        detail: &Value,
    ) -> Result<Self> {
        let project_work = detail.get("project_work");
        let source_sha256 = project_work
            .and_then(|work| work.get("document"))
            .and_then(|document| document.get("sha256"))
            .map(|value| value.as_str().context("project source digest is not a string"))
            .transpose()?;
        let artifact_sha256 = project_work
            .and_then(|work| work.get("artifact"))
            .and_then(|artifact| artifact.get("sha256"))
            .or_else(|| detail.get("worker_artifact_sha256"))
            .map(|value| value.as_str().context("artifact digest is not a string"))
            .transpose()?;
        let identity = Self::new(
            deployment,
            run_id,
            attempt,
            epoch,
            session,
            source_sha256,
            artifact_sha256,
        );
        identity.validate()?;
        Ok(identity)
    }

    pub fn validate(&self) -> Result<()> {
        ensure!(crate::config::valid_id(&self.deployment), "invalid identity deployment");
        ensure!(crate::config::valid_id(&self.run_id), "invalid identity run");
        ensure!(self.attempt > 0, "invalid identity attempt");
        ensure!(crate::config::valid_id(&self.epoch), "invalid identity epoch");
        if let Some(session) = &self.session {
            ensure!(crate::config::valid_id(session), "invalid identity session");
        }
        for digest in [&self.source_sha256, &self.artifact_sha256] {
            if let Some(digest) = digest {
                ensure!(digest.len() == 64 && digest.bytes().all(|b| b.is_ascii_hexdigit()), "invalid identity digest");
            }
        }
        Ok(())
    }

    pub fn canonical_bytes(&self) -> Result<Vec<u8>> {
        self.validate()?;
        canonical(&serde_json::to_value(self)?)
    }

    pub fn canonical_hash(&self) -> Result<String> {
        Ok(hex::encode(Sha256::digest(self.canonical_bytes()?)))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    pub schema: String,
    pub message_id: String,
    pub deployment: String,
    pub from: String,
    pub to: String,
    pub run_id: String,
    pub attempt: u32,
    pub epoch: String,
    pub kind: String,
    pub sent_at: u64,
    pub expires_at: u64,
    pub payload: Value,
    pub mac: String,
}
/// Sorted UTF-8 JSON, with integers only and bounded recursion.
pub fn canonical(value: &Value) -> Result<Vec<u8>> {
    fn ordered(v: &Value, depth: usize) -> Result<Value> {
        anyhow::ensure!(depth <= 32, "JSON nesting exceeds 32");
        Ok(match v {
            Value::Number(n) => { anyhow::ensure!(n.is_i64() || n.is_u64(), "floating-point protocol value"); v.clone() },
            Value::Array(a) => Value::Array(a.iter().map(|x| ordered(x,depth+1)).collect::<Result<_>>()?),
            Value::Object(m) => {
                let mut keys:Vec<_>=m.keys().collect(); keys.sort();
                let mut sorted=serde_json::Map::new();
                for k in keys { sorted.insert(k.clone(),ordered(&m[k],depth+1)?); }
                Value::Object(sorted)
            },
            _ => v.clone(),
        })
    }
    Ok(serde_json::to_vec(&ordered(value,0)?)?)
}
impl Envelope {
    /// Read the shared identity from the existing wire fields and optional
    /// project payload, without adding a second top-level wire identity.
    pub fn execution_identity(&self) -> Result<ExecutionIdentity> {
        let project_work = self.payload.get("project_work");
        let source_sha256 = project_work
            .and_then(|work| work.get("document"))
            .and_then(|document| document.get("sha256"))
            .map(|value| value.as_str().context("project source digest is not a string"))
            .transpose()?;
        let artifact_sha256 = project_work
            .and_then(|work| work.get("artifact"))
            .and_then(|artifact| artifact.get("sha256"))
            .or_else(|| self.payload.get("worker_artifact_sha256"))
            .map(|value| value.as_str().context("artifact digest is not a string"))
            .transpose()?;
        let identity = ExecutionIdentity::new(
            &self.deployment,
            &self.run_id,
            self.attempt,
            &self.epoch,
            self.payload.get("session").and_then(Value::as_str),
            source_sha256,
            artifact_sha256,
        );
        identity.validate()?;
        Ok(identity)
    }

    pub fn preimage(&self) -> Result<Vec<u8>> {
        let mut value=serde_json::to_value(self)?;
        value.as_object_mut().expect("envelope is object").remove("mac");
        canonical(&value)
    }
    pub fn sign(&mut self, key: &[u8]) -> Result<()> {
        use hmac::{Hmac,Mac};
        anyhow::ensure!(key.len()==32,"key must be 32 bytes");
        let mut signer=Hmac::<sha2::Sha256>::new_from_slice(key)?;
        signer.update(&self.preimage()?);
        self.mac=hex::encode(signer.finalize().into_bytes()); Ok(())
    }
    pub fn verify(&self, key: &[u8], deployment: &str, target: &str, now: u64) -> Result<()> {
        use hmac::{Hmac,Mac};
        anyhow::ensure!(key.len()==32,"key must be 32 bytes");
        anyhow::ensure!(self.schema==PROTOCOL && self.deployment==deployment && self.to==target,"wire/deployment/destination mismatch");
        anyhow::ensure!([&self.message_id,&self.deployment,&self.run_id,&self.epoch].iter().all(|s|crate::config::valid_id(s)),"invalid execution id");
        anyhow::ensure!([&self.from,&self.to].iter().all(|s| !s.is_empty() && s.len()<=256 && s.is_ascii() && !s.chars().any(char::is_whitespace)),"invalid route");
        anyhow::ensure!(matches!(self.kind.as_str(),"start"|"cancel"|"decision"|"status"|"approval"|"report"|"error"|"probe"|"ready"),"unsupported message kind");
        anyhow::ensure!(self.attempt>0 && self.payload.is_object(),"invalid attempt/payload");
        anyhow::ensure!(self.sent_at<=now.saturating_add(45) && self.expires_at>now && self.expires_at>self.sent_at && self.expires_at-self.sent_at<=300,"invalid envelope time");
        anyhow::ensure!(serde_json::to_vec(self)?.len()<=65536,"envelope exceeds 64KiB");
        anyhow::ensure!(self.mac.len()==64 && self.mac.bytes().all(|b|b.is_ascii_digit() || (b'a'..=b'f').contains(&b)),"invalid MAC encoding");
        let mut verifier=Hmac::<sha2::Sha256>::new_from_slice(key)?;
        verifier.update(&self.preimage()?);
        verifier.verify_slice(&hex::decode(&self.mac)?)?; Ok(())
    }
}
