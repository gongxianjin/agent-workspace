//! One signed start frame over stdio. No caller report, shell, AO or Reason.
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{io::{BufRead, Read, Write}, path::{Path, PathBuf}};
use sha2::{Digest, Sha256};
use super::protocol::Envelope;
#[cfg(target_os = "linux")]
use super::runner::Runner;

#[derive(Clone, Copy, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "kebab-case")]
pub enum CgroupRootMode {
    #[default]
    Explicit,
    CurrentUnit,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct NativeWorkerConfig {
    pub schema: String,
    pub deployment: String,
    pub worker_id: String,
    pub key_file: PathBuf,
    pub cgroup_root: PathBuf,
    #[serde(default)]
    pub cgroup_root_mode: CgroupRootMode,
    pub artifact_sha256: String,
    pub timeout_secs: u64,
}

pub fn digest_file(path: &Path) -> Result<String> {
    let mut input = std::fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buf = [0_u8; 65536];
    loop {
        let n = input.read(&mut buf)?;
        if n == 0 { break; }
        hash.update(&buf[..n]);
    }
    Ok(hex::encode(hash.finalize()))
}

pub fn load(path: &Path) -> Result<NativeWorkerConfig> {
    let mut raw = Vec::new();
    std::fs::File::open(path)?.take(65537).read_to_end(&mut raw)?;
    ensure!(raw.len() <= 65536, "worker config exceeds 64KiB");
    let cfg: NativeWorkerConfig = serde_json::from_slice(&raw)?;
    cfg.validate()?;
    Ok(cfg)
}

impl NativeWorkerConfig {
    pub fn validate_owner_marker(&self, marker: Option<&std::ffi::OsStr>) -> Result<()> {
        match (self.cgroup_root_mode, marker) {
            (CgroupRootMode::Explicit, None) => Ok(()),
            (CgroupRootMode::Explicit, Some(_)) => anyhow::bail!("owner timeout marker forbidden with explicit mode"),
            (CgroupRootMode::CurrentUnit, Some(value)) => {
                let raw=value.to_str().context("invalid owner timeout marker")?;
                ensure!(raw == self.timeout_secs.to_string(), "owner timeout marker mismatch");
                Ok(())
            }
            (CgroupRootMode::CurrentUnit, None) => anyhow::bail!("owner timeout marker required with current-unit"),
        }
    }
    pub fn validate(&self) -> Result<()> {
        ensure!(self.schema == "native-worker-config/1", "unsupported worker config schema");
        ensure!(crate::config::valid_id(&self.deployment) && crate::config::valid_id(&self.worker_id), "invalid worker identity");
        ensure!(self.artifact_sha256.len() == 64 && self.artifact_sha256.bytes().all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b)), "invalid worker artifact SHA256");
        ensure!((1..=300).contains(&self.timeout_secs), "worker timeout must be 1..300 seconds");
        ensure!(!self.key_file.as_os_str().is_empty() && self.cgroup_root.is_absolute(), "explicit key file and absolute cgroup root required");
        Ok(())
    }
}

pub fn fixed_child(args: &[String]) -> Result<()> {
    // Fixed owner argv, matching the launcher contract in owner_exec:
    // sum-v1            => [template, SESSION]
    // project-work-v1   => [template, ABSOLUTE_WORK_DIR] (directory holds work.json)
    ensure!(args.len() == 2 && matches!(args[0].as_str(), "sum-v1" | "project-work-v1"), "owner-child requires a fixed template and session");
    if args[0] == "project-work-v1" {
        // The work directory is validated as a path, never as a session id;
        // the session inside work.json is checked by the project child.
        let work_dir = std::path::Path::new(&args[1]);
        ensure!(work_dir.is_absolute(), "work directory must be absolute");
        let mut gate = [0_u8; 1];
        std::io::stdin().read_exact(&mut gate)?;
        ensure!(gate == [b'G'], "owner gate rejected");
        // Work binding and model call are Linux-worker-only: the manager
        // host never talks to the model. On other platforms the fixed
        // template is recognized but refuses to run without the Linux
        // owner-exec environment. The child reads the session from the
        // work.json binding, so no session travels in argv here.
        #[cfg(target_os = "linux")]
        return super::owner_project_child::run_in(work_dir);
        #[cfg(not(target_os = "linux"))]
        { let _ = work_dir; anyhow::bail!("project-work-v1 work.json binding requires the Linux worker environment"); }
    }
    let session = &args[1];
    ensure!(crate::config::valid_id(session), "invalid owner session");
    let mut gate = [0_u8; 1];
    std::io::stdin().read_exact(&mut gate)?;
    ensure!(gate == [b'G'], "owner gate rejected");
    let inputs = [1_i64, 2, 3];
    let sum: i64 = inputs.into_iter().sum();
    let report = json!({"schema":"native-sum/1","session":session,"inputs":inputs,"sum":sum});
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, &report)?;
    out.write_all(b"\n")?;
    out.flush()?;
    eprintln!("owner-child template=sum-v1 session={session}");
    Ok(())
}

pub fn run(config_path: &Path) -> Result<()> {
    let cfg = load(config_path)?;
    cfg.validate_owner_marker(std::env::var_os("AW_EXPECTED_OWNER_TIMEOUT_SECS").as_deref())?;
    #[cfg(not(target_os = "linux"))]
    { let _ = cfg; anyhow::bail!("owner-exec requires Linux; no M0 is inferred on this platform") }
    #[cfg(target_os = "linux")]
    { run_linux(config_path, cfg) }
}

pub fn read_frame(input: &mut impl BufRead) -> Result<Envelope> {
    let mut raw = Vec::new();
    input.take(65538).read_until(b'\n', &mut raw)?;
    ensure!(raw.last() == Some(&b'\n') && raw.len() <= 65537, "worker start frame is incomplete or exceeds 64KiB");
    serde_json::from_slice(&raw).context("invalid worker envelope")
}

pub const MAX_MODEL_TOKEN_BYTES: usize = 16 * 1024;

/// Read the length-framed model token that follows the signed start frame on
/// the SSH channel stdin. The token never touches argv, files, or the signed
/// envelope; it is returned to be set as the owner-child environment.
pub fn read_model_token(input: &mut impl Read) -> Result<Vec<u8>> {
    let mut length = [0_u8; 4];
    input.read_exact(&mut length).context("model token length prefix missing")?;
    let size = u32::from_be_bytes(length) as usize;
    ensure!(size > 0 && size <= MAX_MODEL_TOKEN_BYTES, "model token size out of bounds");
    let mut token = vec![0_u8; size];
    input.read_exact(&mut token).context("model token bytes missing")?;
    Ok(token)
}

#[cfg(test)]
mod token_tests {
    use super::{MAX_MODEL_TOKEN_BYTES, read_model_token};
    use std::io::Cursor;

    fn frame(token: &[u8]) -> Cursor<Vec<u8>> {
        let mut raw = (token.len() as u32).to_be_bytes().to_vec();
        raw.extend_from_slice(token);
        Cursor::new(raw)
    }

    #[test]
    fn model_token_round_trips_after_start_frame() {
        let token = b"sk-secret-69-chars".to_vec();
        let mut input = frame(&token);
        let read = read_model_token(&mut input).unwrap();
        assert_eq!(read, token);
    }

    #[test]
    fn empty_model_token_is_rejected() {
        assert!(read_model_token(&mut frame(b"")).unwrap_err().to_string().contains("out of bounds"));
    }

    #[test]
    fn oversized_model_token_is_rejected() {
        assert!(read_model_token(&mut frame(&vec![b't'; MAX_MODEL_TOKEN_BYTES + 1])).unwrap_err().to_string().contains("out of bounds"));
    }

    #[test]
    fn truncated_model_token_is_rejected() {
        let raw = (8_u32).to_be_bytes().to_vec();
        let mut input = Cursor::new(raw);
        assert!(read_model_token(&mut input).is_err());
    }
}

#[cfg(target_os = "linux")]
fn output_pending(runner: &mut Runner) -> Result<()> {
    for (id, frame) in runner.pending_outbox_raw(100)? {
        // Raised with ARTIFACT_TOTAL_BUDGET (run bc1cf37d): the terminal
        // project report envelope carries the artifact package as content_hex
        // (report bound 73728 + bundle + evidence ≈ up to ~192KiB line).
        ensure!(frame.len() <= 196608, "worker output frame exceeds 192KiB");
        let mut out = std::io::stdout().lock();
        out.write_all(&frame)?;
        out.write_all(b"\n")?;
        out.flush()?;
        runner.acknowledge_outbox(&id)?;
    }
    Ok(())
}

#[cfg(target_os = "linux")]
fn run_linux(config_path: &Path, cfg: NativeWorkerConfig) -> Result<()> {
    use super::owner_producer::{OwnedProcessPort, OwnerLaunchPlan, complete_binding, finish_owner_evidence};
    use crate::adapters::owner_exec::LinuxOwnedProcessPort;
    let exe = std::env::current_exe()?;
    ensure!(digest_file(&exe)? == cfg.artifact_sha256, "worker artifact SHA mismatch");
    let base = config_path.parent().context("worker config parent missing")?;
    let mut raw_key = String::new();
    std::fs::File::open(base.join(&cfg.key_file))?.take(67).read_to_string(&mut raw_key)?;
    let key = raw_key.trim().to_owned();
    ensure!(raw_key.len() <= 66 && hex::decode(&key)?.len() == 32, "worker key must contain 32 bytes");
    let m = read_frame(&mut std::io::stdin().lock())?;
    let now = clock();
    m.verify(&hex::decode(&key)?, &cfg.deployment, &cfg.worker_id, now)?;
    ensure!(m.from == "A" && m.kind == "start" && matches!(m.payload["template"].as_str(), Some("sum-v1") | Some("project-work-v1")), "only signed sum-v1 or project-work-v1 starts are supported");
    if m.payload["template"] == "project-work-v1" {
        // The in-memory bearer token follows the signed start frame on the
        // same stdin: read it now, hold it only in this process's
        // environment, and pass it to the owner-child. Never logged, never
        // persisted, never inside the signed envelope.
        let token = read_model_token(&mut std::io::stdin().lock())?;
        ensure!(std::env::var_os("AO_MODEL_TOKEN").is_none(), "AO_MODEL_TOKEN already set; refusing to overwrite");
        // Safety: single-threaded worker startup before any thread spawn; the
        // token stays in this process's environment only.
        unsafe {
            std::env::set_var("AO_MODEL_TOKEN", {
                use std::os::unix::ffi::OsStringExt;
                std::ffi::OsString::from_vec(token)
            });
        }
    }
    ensure!(m.payload["worker_id"] == cfg.worker_id && m.payload["worker_artifact_sha256"] == cfg.artifact_sha256, "worker route/artifact mismatch");
    let state = base.join("worker-state");
    std::fs::create_dir_all(&state)?;
    let _lock = crate::config::ServiceLock::acquire(&state, "runner")?;
    let template = m.payload["template"].as_str().context("template missing")?.to_owned();
    let node = crate::config::NodeSpec { id:cfg.worker_id.clone(), address:cfg.worker_id.clone(), key_hex:key, slots:1, admitted:true, healthy:false, draining:false, templates:vec![template.clone()] };
    let mut runner = Runner::open(&state.join("runner.sqlite"), &cfg.deployment, node)?;
    runner.accept(&m, now)?;
    let Some(identity) = runner.claim_native(&m, now)? else {
        output_pending(&mut runner)?;
        anyhow::bail!("duplicate start retained; no second child was launched");
    };
    let plan = OwnerLaunchPlan { execution:identity.clone(), node_id:cfg.worker_id,
        instance_id:uuid::Uuid::new_v4().to_string(), boot_id:std::fs::read_to_string("/proc/sys/kernel/random/boot_id")?.trim().into(),
        request_bytes:super::protocol::canonical(&m.payload)? };
    let outcome = (|| -> Result<()> {
        let mut child_args = vec!["owner-child".into(), template.clone()];
        let mut files_workspace = None;
        if template == "project-work-v1" {
            // The confirmed work binding is staged as work.json in a managed
            // per-run directory; the child receives the directory as argv,
            // never a payload blob, and writes artifacts under files/.
            let work_dir = state.join("runs").join(identity.run_id.as_str()).join(identity.attempt.to_string());
            std::fs::create_dir_all(&work_dir)?;
            let work = runner.project_work(&identity)?;
            std::fs::write(work_dir.join("work.json"), serde_json::to_vec(&json!({"session":identity.session,"work":work}))?)?;
            child_args.push(work_dir.to_string_lossy().into_owned());
            files_workspace = Some(work_dir.join("files"));
        } else {
            child_args.push(identity.session.clone().context("session missing")?);
        }
        let mut port = LinuxOwnedProcessPort::with_mode(exe.clone(), child_args, cfg.cgroup_root, cfg.timeout_secs, cfg.cgroup_root_mode)?;
        let owner = port.owner_identity()?;
        let facts = port.launch(&plan)?;
        let binding = complete_binding(&plan, &owner, &facts)?;
        runner.native_launch(&binding, clock())?;
        output_pending(&mut runner)?;
        let production = finish_owner_evidence(&mut port, &plan, &owner, &facts)?;
        runner.native_terminal_for(&production, files_workspace.as_deref(), clock())?;
        output_pending(&mut runner)
    })();
    if let Err(error) = outcome {
        runner.native_unknown(&identity, &format!("native owner UNKNOWN: {error:#}"), clock())?;
        output_pending(&mut runner)?;
        return Err(error);
    }
    Ok(())
}

pub fn clock() -> u64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |v| v.as_secs())
}
