use anyhow::Result;
use serde::{Deserialize, Serialize};

pub fn valid_id(value: &str) -> bool {
    !value.is_empty() && value.len() <= 128 && value.bytes().all(|b| b.is_ascii_alphanumeric() || b"-_:.".contains(&b)) && !value.contains("..")
}

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", content = "id", rename_all = "snake_case")]
pub enum Actor { Local(String), AoSession(String) }

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Mode { Local, Real }

// Runtime-only trusted material: never serialize the key into a config or snapshot.
#[derive(Clone, PartialEq, Eq)]
pub struct NodeSpec {
    pub id: String,
    pub address: String,
    pub key_hex: String,
    pub slots: u32,
    pub admitted: bool,
    pub healthy: bool,
    pub draining: bool,
    pub templates: Vec<String>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: String,
    pub mode: Mode,
    pub role: String,
    pub deployment: String,
    pub state_dir: std::path::PathBuf,
    pub nodes: Vec<NodeConfig>,
    #[serde(default)] pub ao: Option<AoConfig>,
    #[serde(default)] pub reason_endpoint: Option<String>,
    #[serde(default)] pub endpoint: Option<String>,
    #[serde(default)] pub runner_node: Option<String>,
    #[serde(default)] pub controller_address: Option<String>,
    #[serde(default)] pub workspace_root: Option<std::path::PathBuf>,
}
#[derive(Clone,Debug,Serialize,Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AoConfig {pub base_url:String,pub project_id:String,pub model:String}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NodeConfig {
    pub id: String, pub address: String, pub key_file: std::path::PathBuf,
    pub slots: u32, pub templates: Vec<String>,
    #[serde(default)] pub admitted: bool,
    #[serde(default)] pub healthy: bool,
    #[serde(default)] pub draining: bool,
}
impl std::fmt::Debug for NodeSpec {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("NodeSpec").field("id", &self.id).field("address", &self.address)
            .field("key", &"[REDACTED]").field("slots", &self.slots)
            .field("admitted", &self.admitted).field("healthy", &self.healthy)
            .field("draining", &self.draining).field("templates", &self.templates).finish()
    }
}
impl Config {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(self.schema == "ao-reason-config/1", "unsupported config schema");
        anyhow::ensure!(valid_id(&self.deployment), "invalid deployment");
        anyhow::ensure!(matches!(self.role.as_str(), "controller" | "runner"), "invalid role");
        anyhow::ensure!(!self.state_dir.as_os_str().is_empty(), "missing state directory");
        let (mut ids, mut addresses) = (std::collections::BTreeSet::new(), std::collections::BTreeSet::new());
        for n in &self.nodes {
            anyhow::ensure!(valid_id(&n.id) && ids.insert(&n.id) && addresses.insert(&n.address), "invalid/duplicate node identity");
            anyhow::ensure!(!n.key_file.as_os_str().is_empty(), "missing trusted key-file reference");
            anyhow::ensure!(!n.templates.is_empty() && n.templates.iter().all(|t| match self.mode {
                Mode::Local => matches!(t.as_str(), "sum-native-v1" | "control-native-v1"),
                Mode::Real => matches!(t.as_str(), "sum-v1" | "control-v1" | "project-work-v1"),
            }), "template does not match explicit mode");
            let shape = NodeSpec {id:n.id.clone(),address:n.address.clone(),key_hex:"00".repeat(32),slots:n.slots,admitted:false,healthy:false,draining:n.draining,templates:n.templates.clone()};
            shape.validate()?;
        }
        Ok(())
    }
    pub fn load_nodes(&self, base: &std::path::Path) -> Result<Vec<NodeSpec>> {
        use std::io::Read;
        self.validate()?;
        self.nodes.iter().map(|n| {
            let path = base.join(&n.key_file);
            let mut raw = String::new();
            std::fs::File::open(path)?.take(130).read_to_string(&mut raw)?;
            let key_hex = raw.trim().to_owned();
            anyhow::ensure!(raw.len() <= 66 && key_hex.len() == 64, "invalid key-file length");
            let node = NodeSpec{id:n.id.clone(),address:n.address.clone(),key_hex,slots:n.slots,
                admitted:n.admitted,healthy:self.mode == Mode::Local && n.healthy,draining:n.draining,templates:n.templates.clone()};
            node.validate()?; Ok(node)
        }).collect()
    }
    pub fn prepare_state(&self, base: &std::path::Path) -> Result<std::path::PathBuf> {
        use std::io::Write;
        self.validate()?;
        let path = base.join(&self.state_dir);
        std::fs::create_dir_all(&path)?;
        let _guard = ServiceLock::acquire(&path, "identity")?;
        let pin = path.join("native-identity.json");
        let expected = serde_json::json!({"schema":"ao-reason-state/1","mode":self.mode,"deployment":self.deployment});
        if pin.exists() {
            let actual:serde_json::Value = serde_json::from_slice(&std::fs::read(pin)?)?;
            anyhow::ensure!(actual == expected, "state mode/deployment changed; do not reuse another ledger");
        } else {
            let mut file = std::fs::OpenOptions::new().write(true).create_new(true).open(pin)?;
            file.write_all(&serde_json::to_vec(&expected)?)?; file.sync_all()?;
        }
        Ok(path)
    }
}
pub struct ServiceLock { _file: std::fs::File }
impl ServiceLock {
    pub fn acquire(state: &std::path::Path, role: &str) -> Result<Self> {
        anyhow::ensure!(matches!(role,"controller"|"runner"|"identity"), "invalid lock role");
        let file=std::fs::OpenOptions::new().read(true).write(true).create(true).truncate(false).open(state.join(format!("{role}.lock")))?;
        file.try_lock().map_err(|e| anyhow::anyhow!("service lock unavailable: {e}"))?;
        Ok(Self{_file:file})
    }
}

impl NodeSpec {
    pub fn validate(&self) -> Result<()> {
        anyhow::ensure!(valid_id(&self.id), "invalid node id");
        anyhow::ensure!(!self.address.is_empty() && self.address.len() <= 256 && self.address.is_ascii() && !self.address.chars().any(char::is_whitespace), "invalid node address");
        anyhow::ensure!((1..=65535).contains(&self.slots), "slots must be 1..65535");
        anyhow::ensure!(hex::decode(&self.key_hex)?.len() == 32, "node key must be 32 bytes");
        anyhow::ensure!(!self.templates.is_empty() && self.templates.iter().all(|t| matches!(t.as_str(), "sum-native-v1" | "control-native-v1" | "sum-v1" | "control-v1" | "project-work-v1")), "unsupported template");
        Ok(())
    }
}
