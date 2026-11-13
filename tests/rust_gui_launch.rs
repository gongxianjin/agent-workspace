#[path = "../src/desktop/launch.rs"]
mod launch;
use agentranch::config::Mode;
use launch::Launch;
use std::{path::{Path, PathBuf}, process::Command};

fn worker_name() -> &'static str {
    if cfg!(windows) { "aw-cli.exe" } else { "aw-cli" }
}

fn app_path(root: &Path) -> PathBuf {
    root.join(if cfg!(windows) { "agentranch.exe" } else { "agentranch" })
}

fn install_matching_worker(root: &Path) -> PathBuf {
    let worker = root.join(worker_name());
    std::fs::copy(env!("CARGO_BIN_EXE_aw-cli"), &worker).expect("copy matching worker");
    worker
}

fn install_fake_worker(root: &Path, package: &str, version: &str) -> PathBuf {
    let worker = root.join(worker_name());
    let source = root.join("fake-worker.rs");
    let identity = serde_json::json!({"binary": "aw-cli", "package": package, "version": version});
    let source_code = ["fn main() { print!(\"{}\", r###\"", &identity.to_string(), "\"###); }"]
        .concat();
    std::fs::write(&source, source_code).expect("write fake worker");
    let status = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--edition=2024", source.to_str().unwrap(), "-o"])
        .arg(&worker)
        .status()
        .expect("compile fake worker");
    assert!(status.success(), "fake worker compilation failed: {status}");
    worker
}

fn parse(root: &Path, args: impl IntoIterator<Item = &'static str>) -> anyhow::Result<Launch> {
    Launch::parse(args, &app_path(root), &root.join("data"))
}

#[test]
fn gateway_key_stdin_is_an_explicit_real_mode_launch_flag() {
    let root = tempfile::tempdir().unwrap();
    let opts = parse(root.path(), ["--real", "--gateway-key-stdin"]).unwrap();
    assert!(opts.gateway_key_stdin);
    assert_eq!(opts.mode, Mode::Real);
}

#[test]
fn gateway_key_stdin_is_rejected_without_real_mode() {
    let root = tempfile::tempdir().unwrap();
    let error = parse(root.path(), ["--gateway-key-stdin"]).unwrap_err();
    assert!(error.to_string().contains("--gateway-key-stdin requires --real"));
}

#[test]
fn no_gateway_key_flag_leaves_memory_credential_unconfigured() {
    let root = tempfile::tempdir().unwrap();
    let opts = parse(root.path(), ["--real"]).unwrap();
    assert!(!opts.gateway_key_stdin);
}

#[test]
fn missing_sibling_worker_is_rejected_with_deterministic_error() {
    let root = tempfile::tempdir().unwrap();
    let error = match parse(root.path(), []) {
        Ok(_) => panic!("missing sibling worker must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "sibling worker executable is missing");
}

#[test]
fn wrong_worker_version_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    install_fake_worker(root.path(), env!("CARGO_PKG_NAME"), "999.999.999");
    let error = match parse(root.path(), []) {
        Ok(_) => panic!("wrong worker version must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "sibling worker identity mismatch");
}

#[test]
fn wrong_worker_package_identity_is_rejected() {
    let root = tempfile::tempdir().unwrap();
    install_fake_worker(root.path(), "untrusted-package", env!("CARGO_PKG_VERSION"));
    let error = match parse(root.path(), []) {
        Ok(_) => panic!("wrong worker package must be rejected"),
        Err(error) => error,
    };
    assert_eq!(error.to_string(), "sibling worker identity mismatch");
}

#[test]
fn matching_sibling_worker_is_accepted() {
    let root = tempfile::tempdir().unwrap();
    let worker = install_matching_worker(root.path());
    let opts = parse(root.path(), []).unwrap();
    assert_eq!(opts.mode, Mode::Local);
    assert_eq!(opts.worker_exe, worker);
}

#[test]
fn real_manager_launch_does_not_require_a_local_fixture_worker() {
    let root = tempfile::tempdir().unwrap();
    let opts = parse(root.path(), ["--real"])
        .unwrap_or_else(|error| panic!("real manager must not require a local fixture worker: {error}"));
    assert_eq!(opts.mode, Mode::Real);
    assert_eq!(opts.state_dir, root.path().join("data/agentranch/real"));
    assert!(!root.path().join(worker_name()).exists());
}

#[test]
fn real_manager_launch_ignores_non_executable_local_fixture_sentinel() {
    let root = tempfile::tempdir().unwrap();
    let worker = root.path().join(worker_name());
    let sentinel = b"NOT_AN_EXECUTABLE_LOCAL_FIXTURE_SENTINEL\n";
    std::fs::write(&worker, sentinel).unwrap();
    let opts = parse(root.path(), ["--real"])
        .unwrap_or_else(|error| panic!("real manager must not validate a local fixture sentinel: {error}"));
    assert_eq!(opts.mode, Mode::Real);
    assert_eq!(std::fs::read(&worker).unwrap(), sentinel);
}

#[test]
fn real_manager_launch_never_executes_a_present_local_fixture_worker() {
    let root = tempfile::tempdir().unwrap();
    let marker = root.path().join("unexpected-local-execution.txt");
    let source = root.path().join("side-effect-worker.rs");
    let identity = serde_json::json!({
        "binary": "aw-cli", "package": env!("CARGO_PKG_NAME"), "version": env!("CARGO_PKG_VERSION")
    });
    let source_code = format!(
        r####"fn main() {{ std::fs::write({marker:?}, b"LOCAL_FIXTURE_WAS_EXECUTED").unwrap(); print!("{{}}", r###"{identity}"###); }}"####,
        marker = marker.to_str().unwrap(), identity = identity,
    );
    std::fs::write(&source, source_code).unwrap();
    let status = Command::new(std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into()))
        .args(["--edition=2024", source.to_str().unwrap(), "-o"])
        .arg(root.path().join(worker_name()))
        .status()
        .expect("compile owned side-effect worker");
    assert!(status.success(), "side-effect worker compilation failed: {status}");
    let opts = parse(root.path(), ["--real"]).unwrap();
    assert_eq!(opts.mode, Mode::Real);
    assert!(!marker.exists(), "real manager must not execute the local fixture worker");
}

#[test]
fn default_and_real_modes_keep_isolated_state_defaults() {
    let root = tempfile::tempdir().unwrap();
    install_matching_worker(root.path());
    let local = parse(root.path(), []).unwrap();
    let real = parse(root.path(), ["--real"]).unwrap();
    assert_eq!(local.state_dir, root.path().join("data/agentranch/local"));
    assert_eq!(real.mode, Mode::Real);
    assert_eq!(real.state_dir, root.path().join("data/agentranch/real"));
}

#[test]
fn explicit_host_state_path_is_preserved_without_executable_or_shell_override() {
    let root = tempfile::tempdir().unwrap();
    let worker = install_matching_worker(root.path());
    let opts = parse(root.path(), ["--state-dir", "D:/isolated/local ledger"]).unwrap();
    assert_eq!(opts.state_dir, Path::new("D:/isolated/local ledger"));
    assert_eq!(opts.mode, Mode::Local);
    assert_eq!(opts.worker_exe, worker);
    for args in [vec!["--state-dir"], vec!["--state-dir", ""], vec!["--worker", "arbitrary.exe"], vec!["--state-dir", "a", "--state-dir", "b"], vec!["--real", "--real"]] {
        assert!(parse(root.path(), args).is_err());
    }
}
