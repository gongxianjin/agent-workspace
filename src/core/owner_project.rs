//! project-work-v1 owner-child support: strict model-response parsing and
//! the terminal report contract. The model call itself is Linux-worker-only
//! (user-confirmed); everything here is untrusted-input validation that must
//! hold on every platform, so it is compiled and tested on the host too.
use anyhow::{Context, Result, ensure};
use std::io::Read;
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::path::Path;

/// Total artifact content budget. The artifact package rides inside the
/// terminal report envelope hex-encoded, and Store admission caps the report
/// at 73728 canonical bytes: 32768 content bytes double to 65536 hex chars,
/// leaving margin for manifests and evidence. Sized for a real generated
/// static page (linux221 run bc1cf37d: a 4-file CJK page exceeded the old
/// 7168-byte sum-fixture budget). Every layer (model output parse, disk
/// write, workspace packaging) enforces the same single budget.
pub const ARTIFACT_TOTAL_BUDGET: usize = 32768;

/// A single static file claimed by the model response. Content is model
/// output: untrusted bytes that must never be executed, only written as
/// static artifacts inside the managed run workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFile {
    pub path: String,
    pub content: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ModelFiles {
    pub files: Vec<ModelFile>,
}

/// Managed-path rules shared with the AO artifact pipeline: portable
/// relative ASCII paths, no escapes, no drive letters, bounded depth.
fn safe_path(relative: &str) -> Result<()> {
    ensure!(!relative.is_empty() && relative.len() <= 120 && relative.is_ascii(), "artifact path bound");
    ensure!(!relative.contains(['\\', ':']) && !relative.starts_with('/'), "artifact path must be portable relative UTF-8");
    let parts: Vec<_> = relative.split('/').collect();
    ensure!(parts.len() <= 8 && parts.iter().all(|part| {
        !part.is_empty() && *part != "." && *part != ".." && !part.ends_with(['.', ' '])
            && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || b"._-".contains(&byte))
    }), "unsafe artifact path");
    Ok(())
}

impl ModelFiles {
    /// Parse the strict model output contract: exactly `{"files":[{"path","content"}]}`.
    /// Unknown fields fail closed. Total content budget (sum of content bytes)
    /// is enforced against `budget`.
    pub fn parse(raw: &str, budget: usize) -> Result<Self> {
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawFile { path: String, content: String }
        #[derive(serde::Deserialize)]
        #[serde(deny_unknown_fields)]
        struct RawFiles { files: Vec<RawFile> }
        let parsed: RawFiles = serde_json::from_str(raw).context("model response must be exact files JSON")?;
        ensure!(!parsed.files.is_empty() && parsed.files.len() <= 32, "model file count out of bounds");
        let mut seen = std::collections::BTreeSet::new();
        let mut total = 0usize;
        let mut files = Vec::with_capacity(parsed.files.len());
        for file in parsed.files {
            safe_path(&file.path)?;
            ensure!(seen.insert(file.path.to_ascii_lowercase()), "duplicate model file path");
            total = total.checked_add(file.content.len()).context("model content size overflow")?;
            ensure!(total <= budget, "model output exceeds artifact budget");
            files.push(ModelFile { path: file.path, content: file.content });
        }
        Ok(Self { files })
    }

    /// Write every file inside `workspace` (which the caller has already
    /// bound to the run). Creates parent directories; refuses symlinked
    /// targets; returns the written relative paths in sorted order.
    pub fn write_within(&self, workspace: &Path) -> Result<Vec<String>> {
        let mut written = Vec::new();
        for file in &self.files {
            safe_path(&file.path)?;
            let mut target = workspace.to_path_buf();
            for part in file.path.split('/') { target.push(part); }
            if let Some(parent) = target.parent() { std::fs::create_dir_all(parent)?; }
            ensure!(std::fs::symlink_metadata(&target).map(|m| !m.file_type().is_symlink()).unwrap_or(true),
                "model file target is a symlink");
            std::fs::write(&target, file.content.as_bytes())?;
            written.push(file.path.clone());
        }
        written.sort();
        Ok(written)
    }
}

/// The prompt handed to the model: ProjectWork::prompt() (host-owned rules,
/// document as data) plus the strict output contract with the size budget
/// and file-set expectation. No credential ever appears here.
pub fn model_prompt(work: &super::project_work::ProjectWork) -> Result<String> {
    let base = work.prompt()?;
    Ok(format!(
        "{base}\n\nOUTPUT CONTRACT: reply with a single JSON object and nothing else, exactly {{\"files\":[{{\"path\":\"...\",\"content\":\"...\"}}]}}. Produce the static page files required by the approved task (expected file set: index.html, css/style.css, js/data.js, js/app.js). Paths are portable relative ASCII (letters, digits, dot, underscore, hyphen; forward slashes; no .. or absolute paths). Total content across all files must stay within 32768 bytes. Output raw JSON only: no markdown fences, no commentary."
    ))
}

/// Model-call evidence carried in the terminal report. Bounded, no bodies,
/// no credentials.
#[derive(Clone, Debug, Serialize)]
pub struct ModelEvidence {
    pub model: String,
    pub request_sha256: String,
    pub response_sha256: String,
    pub elapsed_ms: u64,
    pub status: u16,
}

pub fn project_report(
    session: &str,
    work: &super::project_work::ProjectWork,
    evidence: &Value,
    manifest: &[Value],
) -> Result<Value> {
    work.validate()?;
    // The manifest entries must be exactly what package_workspace_files
    // cross-checks on the worker side (run bc1cf37d follow-up: the child used
    // to emit {path,bytes} and the packaging step demanded {path,size,sha256}).
    // Entries arrive from the caller, which holds the written content in
    // memory; every byte is still re-read and re-hashed from disk by the
    // packaging step before admission.
    let files: Vec<Value> = manifest.to_vec();
    ensure!(!files.is_empty() && files.len() <= 32, "manifest file count out of bounds");
    for file in &files {
        ensure!(file["path"].is_string() && file["size"].is_u64() && file["sha256"].is_string(),
            "manifest entries must carry path, size and sha256");
    }
    let report = json!({
        "schema": "native-project/1",
        "session": session,
        "project_id": work.project_id,
        "action_id": work.action_id,
        "stage": "develop",
        "document_version": work.document.version,
        "document_sha256": work.document.sha256,
        "files": files,
        "model": evidence,
    });
    let serialized = serde_json::to_string(&report)?;
    ensure!(serialized.len() <= 73728, "project report exceeds canonical bound");
    Ok(report)
}

/// SHA-256 helper for request/response evidence digests.
pub fn evidence_digest(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}

/// Strip exactly one outer markdown fence (``` or ```lang) around a single
/// JSON object. Anything else — leading text, trailing text, a second fence,
/// no fence at all on a non-JSON body — fails closed.
pub fn strip_outer_fence(raw: &str) -> Result<&str> {
    let trimmed = raw.trim();
    let Some(rest) = trimmed.strip_prefix("```") else {
        ensure!(trimmed.starts_with('{') && trimmed.ends_with('}'), "model output is not a JSON object");
        return Ok(trimmed);
    };
    // optional language tag on the opening fence line
    let after_open = rest.split_once('\n').context("fence has no closing line")?;
    let lang = after_open.0.trim();
    ensure!(lang.is_empty() || lang.chars().all(|c| c.is_ascii_alphanumeric()), "invalid fence language tag");
    let body = after_open.1;
    let stripped = body.strip_suffix("```").context("fence is not closed")?;
    let inner = stripped.trim();
    ensure!(!inner.contains("```"), "multiple fences are not a single JSON object");
    ensure!(inner.starts_with('{') && inner.ends_with('}'), "fenced body is not a JSON object");
    Ok(inner)
}

/// Cross-check the child's stdout file manifest against the actual managed
/// files in the run workspace and package them. The child's declared sizes
/// and hashes are never trusted: every byte is re-read, re-hashed and
/// bounded; any file present in the workspace but missing from the manifest
/// (or vice versa) fails closed. Mirrors the ao_http artifact package shape.
pub fn package_workspace_files(
    manifest: &Value,
    workspace: &Path,
    work: &super::project_work::ProjectWork,
) -> Result<Value> {
    let files = manifest.as_array().context("child manifest must be a file array")?;
    ensure!(!files.is_empty() && files.len() <= 32, "manifest file count out of bounds");
    let mut seen = std::collections::BTreeSet::new();
    let mut packed = Vec::with_capacity(files.len());
    let mut total = 0usize;
    let mut manifest_paths = std::collections::BTreeSet::new();
    for file in files {
        let relative = file["path"].as_str().context("manifest path missing")?;
        ensure!(seen.insert(relative.to_ascii_lowercase()), "duplicate manifest path");
        manifest_paths.insert(relative.to_owned());
        safe_path(relative)?;
        let declared_size = file["size"].as_u64().context("manifest size missing")?;
        let declared_hash = file["sha256"].as_str().context("manifest hash missing")?;
        let target = resolve_managed_file(workspace, relative)?;
        let mut bytes = Vec::new();
        std::fs::File::open(&target)?.take(ARTIFACT_TOTAL_BUDGET as u64 + 1).read_to_end(&mut bytes)?;
        ensure!(bytes.len() as u64 == declared_size, "managed file size does not match manifest");
        ensure!(bytes.len() <= ARTIFACT_TOTAL_BUDGET, "managed file exceeds per-file bound");
        total = total.checked_add(bytes.len()).context("package size overflow")?;
        ensure!(total <= ARTIFACT_TOTAL_BUDGET, "artifact package exceeds total budget");
        let digest = hex::encode(Sha256::digest(&bytes));
        ensure!(declared_hash == digest, "managed file hash does not match manifest");
        packed.push(json!({"path": relative, "size": declared_size, "sha256": digest, "content_hex": hex::encode(&bytes)}));
    }
    // The workspace must contain exactly the manifest set: no strays.
    let mut disk_paths = std::collections::BTreeSet::new();
    collect_managed_files(workspace, workspace, &mut disk_paths)?;
    let expected: std::collections::BTreeSet<String> = manifest_paths.iter().cloned().collect();
    ensure!(disk_paths == expected, "workspace files do not exactly match the child manifest");
    packed.sort_by(|a, b| a["path"].as_str().cmp(&b["path"].as_str()));
    Ok(json!({"schema":"project-artifact-package/1", "project_id":work.project_id,
        "action_id":work.action_id, "document_version":work.document.version,
        "document_sha256":work.document.sha256, "files":packed}))
}

fn resolve_managed_file(workspace: &Path, relative: &str) -> Result<std::path::PathBuf> {
    safe_path(relative)?;
    let mut candidate = workspace.to_path_buf();
    for part in relative.split('/') { candidate.push(part); }
    let metadata = std::fs::symlink_metadata(&candidate).context("manifest file missing from workspace")?;
    ensure!(metadata.file_type().is_file(), "manifest path is not a regular file");
    let canonical = candidate.canonicalize()?;
    ensure!(canonical.starts_with(workspace.canonicalize()?), "managed file escaped workspace");
    Ok(canonical)
}

fn collect_managed_files(root: &Path, dir: &Path, out: &mut std::collections::BTreeSet<String>) -> Result<()> {
    for entry in std::fs::read_dir(dir)? {
        let entry = entry?;
        let path = entry.path();
        let name = entry.file_name();
        let name = name.to_string_lossy().into_owned();
        if path.is_dir() {
            collect_managed_files(root, &path, out)?;
        } else {
            let relative = path.strip_prefix(root)?.to_string_lossy().replace('\\', "/");
            out.insert(relative.clone());
            let _ = name;
        }
    }
    Ok(())
}

/// Manager-side: materialize an admitted artifact package (content_hex from
/// the signed terminal report) into the local artifacts directory. Every
/// file is hash-verified against the package before anything is written;
/// on any inconsistency nothing is left on disk. The target directory must
/// not pre-exist (no overwriting of prior evidence).
pub fn materialize_artifact_package(package: &Value, target: &Path) -> Result<Vec<String>> {
    ensure!(package["schema"] == "project-artifact-package/1", "artifact package schema mismatch");
    ensure!(!target.exists(), "artifact target already exists; refusing to overwrite");
    let files = package["files"].as_array().context("artifact package files missing")?;
    ensure!(!files.is_empty() && files.len() <= 32, "artifact package file count out of bounds");
    // Verify everything first, collect writes, then commit atomically-ish:
    // staging directory then rename.
    let staging = target.with_extension("staging");
    ensure!(!staging.exists(), "artifact staging path already exists");
    std::fs::create_dir_all(&staging)?;
    let result = (|| -> Result<Vec<String>> {
        let mut seen = std::collections::BTreeSet::new();
        let mut written = Vec::new();
        let mut total = 0usize;
        for file in files {
            let relative = file["path"].as_str().context("package path missing")?;
            ensure!(seen.insert(relative.to_ascii_lowercase()), "duplicate package path");
            safe_path(relative)?;
            let content_hex = file["content_hex"].as_str().context("package content_hex missing")?;
            let bytes = hex::decode(content_hex).context("package content_hex invalid")?;
            ensure!(bytes.len() <= ARTIFACT_TOTAL_BUDGET, "package file exceeds budget");
            total = total.checked_add(bytes.len()).context("package size overflow")?;
            ensure!(total <= ARTIFACT_TOTAL_BUDGET, "package exceeds total budget");
            ensure!(file["size"].as_u64() == Some(bytes.len() as u64), "package size mismatch");
            let digest = hex::encode(Sha256::digest(&bytes));
            ensure!(file["sha256"].as_str() == Some(digest.as_str()), "package hash mismatch");
            let mut destination = staging.clone();
            for part in relative.split('/') { destination.push(part); }
            if let Some(parent) = destination.parent() { std::fs::create_dir_all(parent)?; }
            ensure!(std::fs::symlink_metadata(&destination).is_err(), "package path collision");
            std::fs::write(&destination, &bytes)?;
            written.push(relative.to_owned());
        }
        Ok(written)
    })();
    match result {
        Ok(written) => {
            std::fs::rename(&staging, target).context("artifact commit failed")?;
            Ok(written)
        }
        Err(error) => {
            let _ = std::fs::remove_dir_all(&staging);
            Err(error)
        }
    }
}
