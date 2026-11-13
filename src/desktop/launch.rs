use std::{path::{Path, PathBuf}, process::Command};
use anyhow::ensure;
use ao_reason_workbench::config::Mode;
use serde_json::Value;

const WORKER_BINARY: &str = if cfg!(windows) { "aw-cli.exe" } else { "aw-cli" };

#[derive(Debug)]
pub struct Launch {
    pub mode: Mode,
    pub state_dir: PathBuf,
    pub worker_exe: PathBuf,
    pub gateway_key_stdin: bool,
}
impl Launch {
    pub fn parse<'a>(args: impl IntoIterator<Item = &'a str>, executable: &Path, data_home: &Path) -> anyhow::Result<Self> {
        let mut args = args.into_iter();
        let mut real = false;
        let mut gateway_key_stdin = false;
        let mut state = None;
        while let Some(arg) = args.next() {
            match arg {
                "--real" if !real => real = true,
                "--gateway-key-stdin" if !gateway_key_stdin => gateway_key_stdin = true,
                "--state-dir" if state.is_none() => {
                    let value = args.next().filter(|p| !p.is_empty() && !p.starts_with("--"))
                        .ok_or_else(|| anyhow::anyhow!("--state-dir requires a nonempty path"))?;
                    state = Some(PathBuf::from(value));
                }
                _ => anyhow::bail!("Unsupported or duplicate option: {arg}. Use --real, --gateway-key-stdin and/or --state-dir PATH."),
            }
        }
        ensure!(real || !gateway_key_stdin,"--gateway-key-stdin requires --real");
        let mode = if real { Mode::Real } else { Mode::Local };
        let state_dir = state.unwrap_or_else(|| data_home.join("ao-reason-workbench").join(if real { "real" } else { "local" }));
        let parent = executable.parent().ok_or_else(|| anyhow::anyhow!("application executable has no parent directory"))?;
        let worker_exe = parent.join(WORKER_BINARY);
        if mode == Mode::Local {
            validate_worker(&worker_exe)?;
        }
        Ok(Self { mode, state_dir, worker_exe, gateway_key_stdin })
    }
}

fn validate_worker(worker_exe: &Path) -> anyhow::Result<()> {
    anyhow::ensure!(worker_exe.is_file(), "sibling worker executable is missing");
    let output = Command::new(worker_exe).arg("--identity").output()
        .map_err(|_| anyhow::anyhow!("sibling worker identity could not be read"))?;
    anyhow::ensure!(output.status.success(), "sibling worker identity could not be read");
    let actual: Value = serde_json::from_slice(&output.stdout)
        .map_err(|_| anyhow::anyhow!("sibling worker identity mismatch"))?;
    let expected = serde_json::json!({
        "binary": WORKER_BINARY.trim_end_matches(".exe"),
        "package": env!("CARGO_PKG_NAME"),
        "version": env!("CARGO_PKG_VERSION"),
    });
    anyhow::ensure!(actual == expected, "sibling worker identity mismatch");
    Ok(())
}
