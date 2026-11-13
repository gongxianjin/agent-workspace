//! Linux-only project-work-v1 owner-child body. Runs inside the cgroup-owned
//! child: reads the work.json binding from stdin (after the owner gate),
//! calls the pinned model endpoint exactly once with the in-memory bearer
//! token, validates the untrusted response, writes static files into the run
//! workspace, and prints the native-project/1 manifest report.
//!
//! The bearer token arrives via the environment variable AO_MODEL_TOKEN and
//! is never written to disk, stdout, stderr, or any report.
#![cfg(target_os = "linux")]
use anyhow::{Context, Result, ensure};
use serde_json::{Value, json};
use std::io::{BufRead, Read, Write};
use std::path::{Path, PathBuf};
use std::time::Instant;
use super::owner_project::{ARTIFACT_TOTAL_BUDGET, ModelFiles, evidence_digest, model_prompt, project_report, strip_outer_fence};
use super::project_work::ProjectWork;

/// Where the manager stages the endpoint identity (certificate pin, model
/// name, timeout). No credential lives here.
const ENDPOINT_CONFIG: &str = "model-endpoint.json";

struct WorkBinding {
    session: String,
    work: ProjectWork,
    work_dir: PathBuf,
}

fn read_binding_in(work_dir: &Path) -> Result<WorkBinding> {
    // The owner passes the managed work directory; work.json inside it
    // carries the session and confirmed document work.
    let mut raw = Vec::new();
    std::fs::File::open(work_dir.join("work.json"))?.take(65537).read_to_end(&mut raw)?;
    ensure!(raw.len() <= 65536, "work binding exceeds 64KiB");
    let binding: Value = serde_json::from_slice(&raw)?;
    let session = binding["session"].as_str().context("work binding session missing")?.to_owned();
    ensure!(crate::config::valid_id(&session), "invalid work binding session");
    let work: ProjectWork = serde_json::from_value(binding["work"].clone()).context("invalid work binding document work")?;
    work.validate()?;
    Ok(WorkBinding { session, work, work_dir: work_dir.to_owned() })
}

fn load_endpoint() -> Result<crate::adapters::model_endpoint::ModelEndpoint> {
    // The endpoint config sits beside the worker config (staged by the
    // manager with the pinned certificate).
    let exe = std::env::current_exe()?;
    let base = exe.parent().context("worker executable directory missing")?;
    let path = base.join(ENDPOINT_CONFIG);
    crate::adapters::model_endpoint::ModelEndpoint::load(&path)
        .with_context(|| format!("model endpoint config {} missing or invalid", path.display()))
}

fn bearer_token() -> Result<String> {
    let token = std::env::var("AO_MODEL_TOKEN").context("AO_MODEL_TOKEN missing; model call requires the in-memory token")?;
    ensure!(!token.trim().is_empty() && token.len() <= 16384, "invalid model token shape");
    Ok(token)
}

fn call_model(endpoint: &crate::adapters::model_endpoint::ModelEndpoint, token: &str, prompt: &str) -> Result<(Value, Value)> {
    let client = endpoint.pinned_client()?;
    let body = json!({
        "model": endpoint.model,
        "messages": [{"role": "user", "content": prompt}],
    });
    let request_bytes = serde_json::to_vec(&body)?;
    let started = Instant::now();
    let response = client
        .post(format!("{}/v1/chat/completions", endpoint.base_url.trim_end_matches('/')))
        .bearer_auth(token)
        .json(&body)
        .send()
        .context("model HTTP request failed")?;
    let status = response.status().as_u16();
    let response_bytes = response.bytes().context("model response body unreadable")?.to_vec();
    ensure!(response_bytes.len() <= 1_048_576, "model response exceeds 1MiB bound");
    ensure!(status == 200, "model HTTP status {status}: response prefix {}", String::from_utf8_lossy(&response_bytes[..response_bytes.len().min(512)]));
    let elapsed = started.elapsed().as_millis() as u64;
    let evidence = json!({
        "model": endpoint.model,
        "request_sha256": evidence_digest(&request_bytes),
        "response_sha256": evidence_digest(&response_bytes),
        "elapsed_ms": elapsed,
        "status": status,
    });
    let parsed: Value = serde_json::from_slice(&response_bytes).context("model response is not JSON")?;
    Ok((parsed, evidence))
}

fn extract_content(response: &Value) -> Result<String> {
    let content = response["choices"][0]["message"]["content"]
        .as_str()
        .context("model response has no chat content")?;
    ensure!(content.len() <= 1_048_576, "model chat content exceeds bound");
    Ok(content.to_owned())
}

pub fn run_in(work_dir: &Path) -> Result<()> {
    let binding = read_binding_in(work_dir)?;
    let endpoint = load_endpoint()?;
    let token = bearer_token()?;
    let prompt = model_prompt(&binding.work)?;
    let (response, evidence) = call_model(&endpoint, &token, &prompt)?;
    let content = extract_content(&response)?;
    let cleaned = strip_outer_fence(&content)?;
    let files = ModelFiles::parse(cleaned, ARTIFACT_TOTAL_BUDGET)?;
    let workspace = binding.work_dir.join("files");
    std::fs::create_dir_all(&workspace)?;
    let written = files.write_within(&workspace)?;
    // Manifest entries carry the exact shape the worker-side packaging
    // cross-checks; sizes/hashes come from the parsed model content and are
    // re-verified against disk before any admission.
    let manifest: Vec<Value> = files.files.iter().map(|file| {
        let bytes = file.content.as_bytes();
        json!({"path": file.path, "size": bytes.len(), "sha256": crate::core::owner_project::evidence_digest(bytes)})
    }).collect();
    let report = project_report(&binding.session, &binding.work, &evidence, &manifest)?;
    let mut out = std::io::stdout().lock();
    serde_json::to_writer(&mut out, &report)?;
    out.write_all(b"\n")?;
    out.flush()?;
    eprintln!("owner-child template=project-work-v1 session={} files={}", binding.session, written.len());
    Ok(())
}

#[allow(unused)]
fn _unused_reads() { let _ = (Path::new(".").read_dir(), std::io::stdin().lock().fill_buf()); }
