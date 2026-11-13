#![cfg_attr(target_os = "windows", windows_subsystem = "windows")]
mod desktop;
use anyhow::Context;
use agentranch::core::service::ServiceOptions;
use eframe::egui;
use std::io::Read;

const MAX_GATEWAY_KEY: usize = 64 * 1024;
const MAX_MODEL_TOKEN: usize = 16 * 1024;
const GATEWAY_SECRETS_MAGIC: &[u8] = b"AW-GATEWAY-SECRETS/1\n";

/// Gateway secrets from stdin, held only in memory. The model token is
/// optional: sum-v1 dispatch needs no token, project-work dispatch does.
#[derive(Default)]
pub struct GatewaySecrets {
    pub key: Vec<u8>,
    pub model_token: Vec<u8>,
}

impl std::fmt::Debug for GatewaySecrets {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Never render secret bytes; lengths only.
        f.debug_struct("GatewaySecrets")
            .field("key_len", &self.key.len())
            .field("model_token_len", &self.model_token.len())
            .finish()
    }
}

impl Drop for GatewaySecrets {
    fn drop(&mut self) {
        self.key.fill(0);
        self.model_token.fill(0);
    }
}

impl GatewaySecrets {
    fn into_parts(mut self) -> (Vec<u8>, Vec<u8>) {
        let key = std::mem::take(&mut self.key);
        let token = std::mem::take(&mut self.model_token);
        (key, token)
    }
}

fn read_gateway_secrets(mut input: impl Read) -> anyhow::Result<GatewaySecrets> {
    let mut raw = Vec::new();
    input
        .by_ref()
        .take((MAX_GATEWAY_KEY + 4 + MAX_MODEL_TOKEN + 4 + GATEWAY_SECRETS_MAGIC.len() + 1) as u64)
        .read_to_end(&mut raw)
        .context("read gateway secrets from stdin")?;
    if !raw.starts_with(GATEWAY_SECRETS_MAGIC) {
        // Legacy single-field stdin: the whole input is the key.
        anyhow::ensure!(!raw.is_empty(), "gateway key stdin is empty");
        anyhow::ensure!(raw.len() <= MAX_GATEWAY_KEY, "gateway key exceeds 64 KiB");
        return Ok(GatewaySecrets { key: raw, model_token: Vec::new() });
    }
    let mut offset = GATEWAY_SECRETS_MAGIC.len();
    let read_field = |offset: &mut usize| -> anyhow::Result<Vec<u8>> {
        let length = raw
            .get(*offset..*offset + 4)
            .and_then(|bytes| <[u8; 4]>::try_from(bytes).ok())
            .map(u32::from_be_bytes)
            .context("truncated gateway secrets frame: length prefix missing")?;
        *offset += 4;
        let end = offset
            .checked_add(length as usize)
            .context("gateway secrets field length overflows")?;
        let field = raw
            .get(*offset..end)
            .context("truncated gateway secrets frame: field bytes missing")?
            .to_vec();
        *offset = end;
        Ok(field)
    };
    let key = read_field(&mut offset)?;
    let model_token = read_field(&mut offset)?;
    anyhow::ensure!(!key.is_empty(), "gateway key stdin is empty");
    anyhow::ensure!(key.len() <= MAX_GATEWAY_KEY, "gateway key exceeds 64 KiB");
    anyhow::ensure!(model_token.len() <= MAX_MODEL_TOKEN, "model token exceeds 16 KiB");
    anyhow::ensure!(*&offset == raw.len(), "trailing bytes in gateway secrets frame");
    Ok(GatewaySecrets { key, model_token })
}

fn main() -> eframe::Result {
    let executable = std::env::current_exe().unwrap_or_else(|_| "agentranch.exe".into());
    let home = std::env::var_os("LOCALAPPDATA").or_else(|| std::env::var_os("XDG_DATA_HOME"))
        .map(std::path::PathBuf::from).or_else(|| std::env::var_os("HOME").map(|p| std::path::PathBuf::from(p).join(".local/share")))
        .unwrap_or_else(|| executable.parent().unwrap_or(std::path::Path::new(".")).join("state"));
    let args: Vec<String> = std::env::args().skip(1).collect();
    let launch = desktop::launch::Launch::parse(args.iter().map(String::as_str), &executable, &home)
        .and_then(|launch| {
            let gateway_secrets = launch.gateway_key_stdin
                .then(|| read_gateway_secrets(std::io::stdin().lock()))
                .transpose()?;
            Ok((launch, gateway_secrets))
        });
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_title("agentranch 工作台").with_inner_size([1100., 800.]).with_min_inner_size([720., 540.]),
        ..Default::default()
    };
    eframe::run_native("agentranch 工作台", options, Box::new(move |cc| {
        let result = launch.and_then(|(l, gateway_secrets)| {
            let (gateway_key, gateway_model_token) = match gateway_secrets {
                Some(secrets) => {
                    let (key, token) = secrets.into_parts();
                    (Some(key), Some(token))
                }
                None => (None, None),
            };
            desktop::Workbench::new(ServiceOptions { mode: l.mode, state_dir: l.state_dir, worker_exe: l.worker_exe, gateway_key, gateway_model_token }, &cc.egui_ctx)
        });
        Ok(Box::new(NativeApp { app: result.map_err(|e| e.to_string()) }))
    }))
}

#[cfg(test)]
mod tests {
    use super::{GatewaySecrets, read_gateway_secrets};
    use std::io::{self, Cursor, Read};

    const MAX_KEY: usize = 64 * 1024;

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buffer: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::BrokenPipe, "injected stdin failure"))
        }
    }

    #[test]
    fn empty_gateway_key_stdin_is_rejected_explicitly() {
        let error = read_gateway_secrets(Cursor::new(Vec::new())).unwrap_err();
        assert!(error.to_string().contains("empty"), "unexpected error: {error:#}");
    }

    #[test]
    fn gateway_key_stdin_accepts_exactly_64_kib() {
        let secrets = read_gateway_secrets(Cursor::new(vec![b'k'; MAX_KEY])).unwrap();
        assert_eq!(secrets.key.len(), MAX_KEY);
        assert!(secrets.model_token.is_empty());
    }

    #[test]
    fn gateway_key_stdin_rejects_the_65537th_byte() {
        let error = read_gateway_secrets(Cursor::new(vec![b'k'; MAX_KEY + 1])).unwrap_err();
        assert!(error.to_string().contains("64 KiB"), "unexpected error: {error:#}");
    }

    #[test]
    fn framed_stdin_magic_only_reports_key_error() {
        // Magic line with no key field: the key length prefix is missing, so
        // this must fail (truncated), not silently yield an empty key.
        let error = read_gateway_secrets(Cursor::new(b"AW-GATEWAY-SECRETS/1\n".to_vec())).unwrap_err();
        assert!(error.to_string().contains("truncated"), "unexpected error: {error:#}");
    }

    #[test]
    fn gateway_key_stdin_propagates_io_failure() {
        let error = read_gateway_secrets(FailingReader).unwrap_err();
        assert!(error.to_string().contains("read gateway secrets from stdin"), "unexpected error: {error:#}");
    }

    #[test]
    fn legacy_raw_stdin_still_yields_key_without_token() {
        let secrets = read_gateway_secrets(Cursor::new(b"07".repeat(32))).unwrap();
        assert_eq!(secrets.key, b"07".repeat(32));
        assert!(secrets.model_token.is_empty());
    }

    #[test]
    fn framed_stdin_yields_key_and_token() {
        let key: Vec<u8> = b"07".repeat(32);
        let token = b"sk-test-token".to_vec();
        let mut frame = b"AW-GATEWAY-SECRETS/1\n".to_vec();
        frame.extend_from_slice(&(key.len() as u32).to_be_bytes());
        frame.extend_from_slice(&key);
        frame.extend_from_slice(&(token.len() as u32).to_be_bytes());
        frame.extend_from_slice(&token);
        let secrets = read_gateway_secrets(Cursor::new(frame)).unwrap();
        assert_eq!(secrets.key, key);
        assert_eq!(secrets.model_token, token);
    }

    #[test]
    fn framed_stdin_allows_zero_length_token() {
        let key: Vec<u8> = b"07".repeat(32);
        let mut frame = b"AW-GATEWAY-SECRETS/1\n".to_vec();
        frame.extend_from_slice(&(key.len() as u32).to_be_bytes());
        frame.extend_from_slice(&key);
        frame.extend_from_slice(&0u32.to_be_bytes());
        let secrets = read_gateway_secrets(Cursor::new(frame)).unwrap();
        assert_eq!(secrets.key, key);
        assert!(secrets.model_token.is_empty());
    }

    #[test]
    fn framed_stdin_rejects_truncated_token_bytes() {
        let key: Vec<u8> = b"07".repeat(32);
        let mut frame = b"AW-GATEWAY-SECRETS/1\n".to_vec();
        frame.extend_from_slice(&(key.len() as u32).to_be_bytes());
        frame.extend_from_slice(&key);
        frame.extend_from_slice(&5u32.to_be_bytes());
        frame.extend_from_slice(b"abc"); // 2 bytes short
        let error = read_gateway_secrets(Cursor::new(frame)).unwrap_err();
        assert!(error.to_string().contains("truncated"), "unexpected error: {error:#}");
    }

    #[test]
    fn framed_stdin_rejects_empty_key() {
        // Complete frame (both length prefixes present), key length 0: the
        // explicit "empty" rejection must win, not a truncation error.
        let mut frame = b"AW-GATEWAY-SECRETS/1\n".to_vec();
        frame.extend_from_slice(&0u32.to_be_bytes());
        frame.extend_from_slice(&0u32.to_be_bytes());
        let error = read_gateway_secrets(Cursor::new(frame)).unwrap_err();
        assert!(error.to_string().contains("empty"), "unexpected error: {error:#}");
    }

    #[test]
    fn framed_stdin_rejects_trailing_bytes() {
        let key: Vec<u8> = b"07".repeat(32);
        let mut frame = b"AW-GATEWAY-SECRETS/1\n".to_vec();
        frame.extend_from_slice(&(key.len() as u32).to_be_bytes());
        frame.extend_from_slice(&key);
        frame.extend_from_slice(&0u32.to_be_bytes());
        frame.extend_from_slice(b"trailing");
        let error = read_gateway_secrets(Cursor::new(frame)).unwrap_err();
        assert!(error.to_string().contains("trailing"), "unexpected error: {error:#}");
    }
}

struct NativeApp { app: Result<desktop::Workbench, String> }
impl eframe::App for NativeApp {
    fn logic(&mut self, _ctx: &egui::Context, _frame: &mut eframe::Frame) {
        if let Ok(app) = &mut self.app { app.poll(); }
    }
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ui, |ui| match &mut self.app {
            Ok(app) => app.show(ui),
            Err(error) => {
                ui.heading("工作台未能启动");
                ui.label(error.as_str());
                ui.label("检查启动参数与本地运行环境；关闭后重试。不会自动启动外部 AO、Reason 或模型。");
            }
        });
    }
}
