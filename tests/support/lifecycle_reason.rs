//! Owned local IPC fixture only; no AO/Runner/model or OS-exit claims.
use ao_reason_workbench::core::protocol::{Envelope, PROTOCOL};
use serde_json::{json, Value};
use std::{io::{Read, Write}, path::Path, sync::{Arc, Mutex, atomic::{AtomicBool, Ordering}}, time::{Duration, Instant}};

#[derive(Default)]
struct Bus {
    probes: Vec<Envelope>,
    messages: Vec<(String, String)>,
    ready_enabled: bool,
    fail_inbox: bool,
    starts: usize,
}
pub struct LocalReason {
    pub endpoint: String,
    bus: Arc<Mutex<Bus>>,
    stop: Arc<AtomicBool>,
    thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}
impl LocalReason {
    pub fn new(root: &Path, ready_enabled: bool) -> Self {
        let endpoint = if cfg!(windows) { format!(r"\\.\pipe\lifecycle-{}", uuid::Uuid::new_v4()) }
            else { root.join("lifecycle.sock").display().to_string() };
        let mut listener = platform::listen(&endpoint).unwrap();
        let bus = Arc::new(Mutex::new(Bus { ready_enabled, ..Default::default() }));
        let stop = Arc::new(AtomicBool::new(false));
        let (state, cancelled) = (bus.clone(), stop.clone());
        let address = endpoint.clone();
        let thread = std::thread::spawn(move || {
            while !cancelled.load(Ordering::Acquire) {
                let Some(mut stream) = platform::accept(&mut listener, &address, &cancelled)? else { break };
                if cancelled.load(Ordering::Acquire) { break; }
                let request = read_frame(&mut stream, &cancelled)?;
                let response = respond(&state, &request)?;
                writeln!(stream, "{response}")?;
                platform::complete(stream, &cancelled)?;
            }
            Ok(())
        });
        Self { endpoint, bus, stop, thread: Some(thread) }
    }
    pub fn probes(&self) -> Vec<Envelope> { self.bus.lock().unwrap().probes.clone() }
    pub fn starts(&self) -> usize { self.bus.lock().unwrap().starts }
    pub fn replay_ready(&self, probe: &Envelope) { queue_ready(&mut self.bus.lock().unwrap(), probe); }
    pub fn fail_inbox(&self, fail: bool) { self.bus.lock().unwrap().fail_inbox = fail; }
    pub fn queue_run_event(&self, run: &ao_reason_workbench::core::store::Run, kind: &str, payload: Value) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_secs();
        let mut event = Envelope { schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
            deployment: "lifecycle-test".into(), from: "B".into(), to: "A".into(),
            run_id: run.id.clone(), attempt: run.attempt, epoch: run.epoch.clone(), kind: kind.into(),
            sent_at: now, expires_at: now + 60, payload, mac: String::new() };
        event.sign(&[7;32]).unwrap();
        self.bus.lock().unwrap().messages.push((event.message_id.clone(), serde_json::to_string(&event).unwrap()));
    }
    pub fn enable_ready(&self) {
        let mut bus = self.bus.lock().unwrap();
        bus.ready_enabled = true;
        let probe = bus.probes.last().unwrap().clone();
        queue_ready(&mut bus, &probe);
    }
}
impl Drop for LocalReason {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        platform::wake(&self.endpoint);
        self.thread.take().unwrap().join().expect("local fixture thread").expect("local fixture IPC");
    }
}
fn read_frame(stream: &mut impl Read, stop: &AtomicBool) -> anyhow::Result<Value> {
    let end = Instant::now() + Duration::from_secs(5);
    let mut raw = Vec::new();
    loop {
        anyhow::ensure!(Instant::now() < end && !stop.load(Ordering::Acquire), "fixture read deadline");
        let mut byte = [0];
        match stream.read(&mut byte) {
            Ok(0) => anyhow::bail!("fixture EOF"),
            Ok(_) if byte[0] == b'\n' => return Ok(serde_json::from_slice(&raw)?),
            Ok(_) => { raw.push(byte[0]); anyhow::ensure!(raw.len() <= 65536, "fixture frame bound"); }
            Err(e) if platform::would_block(&e) => std::thread::sleep(Duration::from_millis(1)),
            Err(e) => return Err(e.into()),
        }
    }
}
fn respond(state: &Mutex<Bus>, request: &Value) -> anyhow::Result<Value> {
    let mut bus = state.lock().unwrap();
    if request["op"] == "inbox" && bus.fail_inbox {
        return Ok(json!({"id":request["id"],"ok":false,"result":{}}));
    }
    let result = match request["op"].as_str() {
        Some("inbox") => json!({"messages":bus.messages.iter().map(|(id,body)|json!({"id":id,"body":body})).collect::<Vec<_>>()}),
        Some("delivered") => {
            let ids = request["ids"].as_array().unwrap();
            bus.messages.retain(|(id,_)|!ids.contains(&json!(id)));
            json!({})
        }
        Some("send") => {
            let message: Envelope = serde_json::from_str(request["body"].as_str().unwrap())?;
            message.verify(&[7;32], "lifecycle-test", "B", message.sent_at)?;
            if message.kind == "probe" {
                if !bus.probes.iter().any(|probe|probe.message_id == message.message_id) {
                    bus.probes.push(message.clone());
                    if bus.ready_enabled { queue_ready(&mut bus, &message); }
                }
            } else if message.kind == "start" { bus.starts += 1; }
            json!({"recipients":[{"address":request["to"],"state":"queued"}],"ids":[request["idempotency_key"]]})
        }
        _ => anyhow::bail!("unexpected fixture operation"),
    };
    Ok(json!({"id":request["id"],"ok":true,"result":result}))
}
fn queue_ready(bus: &mut Bus, probe: &Envelope) {
    let mut ready = Envelope { schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
        deployment: probe.deployment.clone(), from: "B".into(), to: "A".into(),
        run_id: probe.run_id.clone(), attempt: probe.attempt, epoch: probe.epoch.clone(), kind: "ready".into(),
        sent_at: probe.sent_at, expires_at: probe.sent_at + 60,
        payload: json!({"address":"owned-B/runner","templates":["sum-v1","project-work-v1"],"ao_project_ready":true}), mac: String::new() };
    ready.sign(&[7;32]).unwrap();
    bus.messages.push((ready.message_id.clone(), serde_json::to_string(&ready).unwrap()));
}
#[cfg(windows)]
mod platform {
    use super::*;
    use std::{fs::File, os::windows::io::{AsRawHandle, FromRawHandle, RawHandle}};
    use windows_sys::Win32::{Foundation::{GetLastError, ERROR_PIPE_CONNECTED, ERROR_NO_DATA, INVALID_HANDLE_VALUE}, Storage::FileSystem::PIPE_ACCESS_DUPLEX, System::Pipes::{CreateNamedPipeW, ConnectNamedPipe, PIPE_TYPE_BYTE, PIPE_WAIT}};
    pub fn listen(endpoint: &str) -> anyhow::Result<File> {
        let wide: Vec<u16> = endpoint.encode_utf16().chain(Some(0)).collect();
        let handle = unsafe { CreateNamedPipeW(wide.as_ptr(), PIPE_ACCESS_DUPLEX, PIPE_TYPE_BYTE | PIPE_WAIT, 4, 65536, 65536, 3000, std::ptr::null()) };
        anyhow::ensure!(handle != INVALID_HANDLE_VALUE, "fixture pipe creation");
        Ok(unsafe { File::from_raw_handle(handle as RawHandle) })
    }
    pub fn accept(listener: &mut File, endpoint: &str, stop: &AtomicBool) -> anyhow::Result<Option<File>> {
        if stop.load(Ordering::Acquire) { return Ok(None); }
        let ok = unsafe { ConnectNamedPipe(listener.as_raw_handle() as _, std::ptr::null_mut()) };
        let code = unsafe { GetLastError() };
        anyhow::ensure!(ok != 0 || code == ERROR_PIPE_CONNECTED, "fixture connect {code}");
        let next = listen(endpoint)?;
        Ok(Some(std::mem::replace(listener, next)))
    }
    pub fn complete(_: File, _: &AtomicBool) -> anyhow::Result<()> { Ok(()) }
    pub fn wake(endpoint: &str) {
        let _ = std::fs::OpenOptions::new().read(true).write(true).open(endpoint);
    }
    pub fn would_block(e: &std::io::Error) -> bool { e.kind() == std::io::ErrorKind::WouldBlock || e.raw_os_error() == Some(ERROR_NO_DATA as i32) }
}
#[cfg(unix)]
mod platform {
    use super::*;
    use std::os::unix::net::{UnixListener, UnixStream};
    pub fn listen(endpoint: &str) -> anyhow::Result<UnixListener> { let listener = UnixListener::bind(endpoint)?; listener.set_nonblocking(true)?; Ok(listener) }
    pub fn accept(listener: &mut UnixListener, _: &str, stop: &AtomicBool) -> anyhow::Result<Option<UnixStream>> {
        while !stop.load(Ordering::Acquire) {
            match listener.accept() {
                Ok((stream,_)) => { stream.set_nonblocking(true)?; return Ok(Some(stream)); }
                Err(e) if would_block(&e) => std::thread::sleep(Duration::from_millis(1)),
                Err(e) => return Err(e.into()),
            }
        }
        Ok(None)
    }
    pub fn complete(_: UnixStream, _: &AtomicBool) -> anyhow::Result<()> { Ok(()) }
    pub fn wake(_: &str) {}
    pub fn would_block(e: &std::io::Error) -> bool { e.kind() == std::io::ErrorKind::WouldBlock }
}
