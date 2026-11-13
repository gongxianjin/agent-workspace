//! RUST-RUNNER-STOP-01. Real regular CLI, owned IPC/HTTP endpoints and durable ledgers.
//! Prepared tests are NOT_RUN until an admitted compiler/runtime window records actual exits.
//! Default continuous/EOF survival is deliberately NOT tested by leaving an uncleanable child.
use ao_reason_workbench::{
    adapters::{AoPort, Observation, WorkRequest},
    config::{Actor, Config, NodeSpec, ServiceLock},
    core::{runner::Runner, store::Store},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, OpenOptions},
    io::{Read, Write},
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::{atomic::{AtomicBool, Ordering}, mpsc, Arc, Mutex, MutexGuard},
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

// Even a harness invoked without --test-threads=1 must not launch concurrent
// children. An unresolved child exits THIS TEST PROCESS with 86: no later case,
// retry or forced child termination. A panic after a known exit poisons the batch.
static BATCH: Mutex<()> = Mutex::new(());
fn batch_guard() -> MutexGuard<'static, ()> {
    BATCH.lock().unwrap_or_else(|_| {
        eprintln!("runner-stop batch aborted after an earlier failed case; no further child launch");
        std::process::exit(86)
    })
}
fn hash(bytes: &[u8]) -> String { format!("{:x}", Sha256::digest(bytes)) }

// Break caught: accepting --once with the opt-in flag runs a lifecycle tick or
// prepares a durable ledger before rejecting an incompatible control contract.
#[test]
fn incompatible_once_and_stdin_stop_are_rejected_before_state_side_effects() {
    let _batch = batch_guard();
    for malformed in [false, true] {
        let f = Fixture::new();
        if malformed { std::fs::write(&f.config, b"{not-json").unwrap(); }
        let mut child = f.spawn(&["--once", "--stop-on-stdin"]);
        let output = child.finish(Duration::from_secs(6));
        assert_eq!(output.code, Some(2), "{}", output.stderr);
        assert!(!f.state.exists(), "incompatible flags must fail before state initialization: {}", output.stderr);
        // This is the feature's explicit startup category, not a generic exit2.
        assert!(output.stderr.contains("runner control: --once and --stop-on-stdin are incompatible"),
            "flag conflict must precede config loading (malformed={malformed}): {}", output.stderr);
        assert!(output.stdout.is_empty());
    }
}

// Break caught: legal bounded framing is ignored, requires EOF, stops with an
// error, or leaves the process-owned runner lock held after its actual exit.
#[test]
fn explicit_stdin_stop_lf_and_crlf_exit_normally_without_waiting_for_eof() {
    let _batch = batch_guard();
    for control in [b"stop\n".as_slice(), b"stop\r\n".as_slice(), b"stop\nJUNK".as_slice(), b"stop\r\nJUNK".as_slice()] {
        let f = Fixture::new();
        let mut child = f.spawn(&["--stop-on-stdin"]);
        child.wait_for_role_lock(&f.state);
        child.write(control);
        // Keep the control pipe open: a valid line, not parent EOF, requests stop.
        let output = child.finish(Duration::from_secs(6));
        assert_eq!(output.code, Some(0), "{}", output.stderr);
        assert!(output.stdout.is_empty());
        drop(ServiceLock::acquire(&f.state, "runner").expect("normal process exit releases only its OS role lock"));
        assert!(!f.http_was_contacted(), "empty runner stop must not call AO");
    }
}

// Break caught: EOF, partial/invalid/oversize commands become successful stops,
// or an unbounded line reader keeps waiting rather than rejecting the prefix.
#[test]
fn opted_in_eof_and_invalid_control_exit_two_without_claiming_normal_stop() {
    let _batch = batch_guard();
    for (bytes, close) in [
        (b"".as_slice(), true),
        (b"stop".as_slice(), true),
        (b"STOP\n".as_slice(), false),
        (b"stopJUNK\n".as_slice(), false),
        (b"stop \n".as_slice(), false),
        (b"stop\rX".as_slice(), false),
        (b"xxxxxxxxxxxxxxxxxxxxxxxxxxxxxxxx".as_slice(), false),
        (b"\n".as_slice(), false),
        (b" stop\n".as_slice(), false),
        (b"{\"stop\":true}\n".as_slice(), false),
        (b"stop\rX\n".as_slice(), false),
        (b"bad\nstop\n".as_slice(), false),
    ] {
        let f = Fixture::new();
        let mut child = f.spawn(&["--stop-on-stdin"]);
        child.wait_for_role_lock(&f.state);
        child.write(bytes);
        if close { child.close_control(); }
        let output = child.finish(Duration::from_secs(6));
        assert_eq!(output.code, Some(2), "control={bytes:?}: {}", output.stderr);
        assert!(output.stderr.contains("runner control"), "control error must be distinguishable: {}", output.stderr);
        assert!(output.stdout.is_empty());
        drop(ServiceLock::acquire(&f.state, "runner").unwrap());
        assert!(!f.http_was_contacted());
    }
}

// Break caught: the original --once path starts consuming stdin or stops before
// its one real inbox exchange. This is not proof of default EOF long-term survival.
#[test]
fn original_once_ignores_stdin_control_and_completes_its_single_tick() {
    let _batch = batch_guard();
    let f = Fixture::new();
    let mut gate = ReasonGate::new(&f.reason_endpoint);
    let mut child = f.spawn(&["--once"]);
    child.write(b"not-a-runner-control\n");
    child.close_control();
    gate.wait_for_inbox();
    gate.release();
    let output = child.finish(Duration::from_secs(6));
    gate.finish();
    assert_eq!(output.code, Some(0), "{}", output.stderr);
    assert!(output.stdout.is_empty());
    assert!(!f.http_was_contacted());
}

// The original --once path must propagate its actual transport error rather than
// accidentally accepting EOF/control as a successful stop.
#[test]
fn original_once_propagates_reason_transport_error_without_stdin_control() {
    let _batch = batch_guard();
    let f = Fixture::new();
    let mut child = f.spawn(&["--once"]);
    child.close_control();
    let output = child.finish(Duration::from_secs(6));
    assert_eq!(output.code, Some(2), "{}", output.stderr);
    assert!(!output.stderr.contains("runner control"), "{}", output.stderr);
    assert!(!output.stderr.contains("runner stop requested"), "{}", output.stderr);
    assert!(f.state.join("runner.sqlite").is_file(), "once must reach real runtime initialization");
    drop(ServiceLock::acquire(&f.state, "runner").unwrap());
    assert!(!f.http_was_contacted());
}

// Break caught: a received stop proceeds from a just-completed inbox into tick,
// AO observation/kill or outbox publication, mutates unfinished execution, emits
// a false exit, or releases Controller reservation. Seeding is a unit boundary,
// NOT a claim of actual AO process-exit evidence or a completed product workflow.
#[test]
fn stop_received_during_inbox_wait_preserves_unknown_execution_and_pending_outbox() {
    let _batch = batch_guard();
    let f = Fixture::new();
    let (controller, run_id) = f.seed_unobserved_execution();
    let before = ledger(&f.state.join("runner.sqlite"));
    assert!(!before["executions"].as_array().unwrap().is_empty());
    assert!(!before["outbox"].as_array().unwrap().is_empty());
    let mut gate = ReasonGate::new(&f.reason_endpoint);
    let mut child = f.spawn(&["--stop-on-stdin"]);
    gate.wait_for_inbox();
    child.write(b"stop\n");
    child.wait_for_control_receipt();
    assert!(child.is_running(), "in-flight transport is not force-cancelled by local stop");
    gate.release();
    let output = child.finish(Duration::from_secs(6));
    gate.finish();
    assert_eq!(output.code, Some(0), "{}", output.stderr);
    assert_eq!(ledger(&f.state.join("runner.sqlite")), before, "stop must not tick, cancel, acknowledge or manufacture exit proof");
    assert!(!controller.assignment(&run_id, 1).unwrap().unwrap().released);
    assert!(!f.http_was_contacted(), "stop must not observe or kill the remote session");
    drop(ServiceLock::acquire(&f.state, "runner").unwrap());
}

struct Fixture {
    root: PathBuf,
    config: PathBuf,
    state: PathBuf,
    reason_endpoint: String,
    http: std::net::TcpListener,
}
impl Fixture {
    fn new() -> Self {
        let root = std::env::var_os("AW_RUNNER_STOP_TEST_ROOT").map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap().join("dist/runner-stop-tests"));
        std::fs::create_dir_all(&root).unwrap();
        // Keep owned evidence and any uncertain execution state even on a failed test.
        let root = tempfile::Builder::new().prefix("owned-").tempdir_in(root).unwrap().keep();
        let state = root.as_path().join("runner-state");
        let config = root.as_path().join("runner.json");
        let http = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        http.set_nonblocking(true).unwrap();
        #[cfg(windows)]
        let reason_endpoint = format!(r"\\.\pipe\runner-stop-{}", uuid::Uuid::new_v4());
        #[cfg(unix)]
        let reason_endpoint = root.as_path().join("reason.sock").to_str().unwrap().to_owned();
        std::fs::write(root.as_path().join("B.key"), "07".repeat(32)).unwrap();
        std::fs::write(&config, serde_json::to_vec(&json!({
            "schema":"ao-reason-config/1", "mode":"real", "role":"runner",
            "deployment":"runner-stop-test", "state_dir":state,
            "nodes":[{"id":"B","address":"owned-B/runner","key_file":"B.key","slots":1,"templates":["sum-v1"],"admitted":true,"healthy":true}],
            "runner_node":"B", "controller_address":"owned-A/controller",
            "reason_endpoint":reason_endpoint,
            "ao":{"base_url":format!("http://{}",http.local_addr().unwrap()),"project_id":"owned-project","model":"controlled-no-model-call"}
        })).unwrap()).unwrap();
        Self { root, config, state, reason_endpoint, http }
    }
    fn spawn(&self, options: &[&str]) -> OwnedChild {
        let stdout = self.root.as_path().join("child.stdout.txt");
        let stderr = self.root.as_path().join("child.stderr.txt");
        let executable = PathBuf::from(env!("CARGO_BIN_EXE_aw-cli"));
        let mut argv = vec![executable.to_string_lossy().into_owned(), "runner".into(), "--config".into(), self.config.to_string_lossy().into_owned()];
        argv.extend(options.iter().map(|s| s.to_string()));
        let mut evidence = Evidence::new(&self.root);
        let bytes = std::fs::read(&executable).unwrap();
        evidence.record(json!({"event":"prepared", "argv":argv, "cwd":self.root, "root":self.root,
            "executable":{"path":executable,"bytes":bytes.len(),"sha256":hash(&bytes)},
            "config_sha256":hash(&std::fs::read(&self.config).unwrap()),
            "stdout":stdout,"stderr":stderr,"pid":null,"actual_wait_exit":null}));
        let child = Command::new(&executable)
            .args(["runner", "--config"]).arg(&self.config).args(options)
            .current_dir(self.root.as_path()).stdin(Stdio::piped())
            .stdout(File::create(&stdout).unwrap()).stderr(File::create(&stderr).unwrap())
            .spawn().unwrap();
        evidence.record(json!({"event":"spawned","pid":child.id(),"actual_wait_exit":null}));
        OwnedChild { child, stdout, stderr, retained_root:self.root.clone(), exited:false, evidence }
    }
    fn http_was_contacted(&self) -> bool {
        match self.http.accept() {
            Ok(_) => true,
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => false,
            Err(e) => panic!("owned listener: {e}"),
        }
    }
    fn seed_unobserved_execution(&self) -> (Store, String) {
        let cfg: Config = serde_json::from_slice(&std::fs::read(&self.config).unwrap()).unwrap();
        cfg.prepare_state(self.root.as_path()).unwrap();
        let node = NodeSpec { id:"B".into(), address:"owned-B/runner".into(), key_hex:"07".repeat(32), slots:1, admitted:true, healthy:true, draining:false, templates:vec!["sum-v1".into()] };
        let mut controller = Store::open(&self.root.as_path().join("controller.sqlite"), "runner-stop-test", &[node.clone()]).unwrap();
        let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs();
        let run = controller.dispatch(&Actor::Local("owned-host".into()), "sum-v1", now).unwrap();
        controller.assign_next(now).unwrap().unwrap();
        let mut runner = Runner::open(&self.state.join("runner.sqlite"), "runner-stop-test", node).unwrap();
        for (_, message) in controller.pending_outbox(32).unwrap() { runner.accept(&message, now).unwrap(); }
        struct Unobserved;
        impl AoPort for Unobserved {
            fn create(&mut self, _: &WorkRequest) -> anyhow::Result<String> { Ok("owned-unobserved-session".into()) }
            fn observe(&mut self, _: &str) -> anyhow::Result<Observation> { Ok(Observation::Unknown("owned unobserved execution".into())) }
            fn resolve(&mut self, _: &str, _: &str, _: &str) -> anyhow::Result<()> { anyhow::bail!("no approval in fixture") }
            fn kill(&mut self, _: &str) -> anyhow::Result<()> { anyhow::bail!("no cancellation in fixture") }
        }
        runner.tick(&mut Unobserved, now).unwrap();
        for message in runner.pending_outbox(32).unwrap() { controller.receive(&message, now).unwrap(); }
        assert_eq!(controller.run(&run.id).unwrap().session.as_deref(), Some("owned-unobserved-session"));
        assert!(!controller.assignment(&run.id, 1).unwrap().unwrap().released);
        drop(runner);
        (controller, run.id)
    }
}
struct Evidence { file: File, started: Instant, sequence: u64 }
impl Evidence {
    fn new(root: &Path) -> Self {
        Self { file:OpenOptions::new().create_new(true).write(true).open(root.join("child.events.jsonl")).unwrap(), started:Instant::now(), sequence:0 }
    }
    fn record(&mut self, mut event: Value) {
        event["sequence"] = json!(self.sequence);
        // UTC epoch time is stored without local timezone conversion; monotonic
        // offsets establish ordering even if the wall clock is adjusted.
        event["utc_unix_ns"] = json!(SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos().to_string());
        event["monotonic_elapsed_ns"] = json!(self.started.elapsed().as_nanos().to_string());
        self.sequence += 1;
        let result = (|| -> std::io::Result<()> {
            serde_json::to_writer(&mut self.file, &event)?;
            self.file.write_all(b"\n")?;
            self.file.sync_all()
        })();
        if let Err(error) = result {
            eprintln!("runner-stop evidence failure: {error}; batch failstop, child exit UNKNOWN, no kill");
            std::process::exit(86);
        }
    }
}
struct Captured { code: Option<i32>, stdout: Vec<u8>, stderr: String }
struct OwnedChild { child: Child, stdout: PathBuf, stderr: PathBuf, retained_root: PathBuf, exited: bool, evidence: Evidence }
impl OwnedChild {
    fn write(&mut self, bytes: &[u8]) {
        self.evidence.record(json!({"event":"control_write_attempt","pid":self.child.id(),"bytes_hex":hex::encode(bytes)}));
        self.child.stdin.as_mut().unwrap().write_all(bytes).unwrap();
        self.evidence.record(json!({"event":"control_write_completed","pid":self.child.id(),"bytes_hex":hex::encode(bytes)}));
    }
    fn close_control(&mut self) {
        drop(self.child.stdin.take());
        self.evidence.record(json!({"event":"control_eof_sent","pid":self.child.id()}));
    }
    fn observed_exit(&mut self) -> Option<std::process::ExitStatus> {
        let observed = match self.child.try_wait() {
            Ok(status) => status,
            Err(error) => self.failstop(&format!("try_wait failed: {error}")),
        }?;
        let waited = match self.child.wait() {
            Ok(status) => status,
            Err(error) => self.failstop(&format!("wait failed: {error}")),
        };
        self.exited = true;
        let out = std::fs::read(&self.stdout).unwrap();
        let err = std::fs::read(&self.stderr).unwrap();
        self.evidence.record(json!({"event":"actual_wait_completed","pid":self.child.id(),
            "actual_wait_exit":waited.code(),"try_wait_matches":waited==observed,
            "stdout":{"path":self.stdout,"bytes":out.len(),"sha256":hash(&out)},
            "stderr":{"path":self.stderr,"bytes":err.len(),"sha256":hash(&err)},
            "descendants":"UNOBSERVED"}));
        assert_eq!(waited, observed);
        if waited.code().is_none() { self.failstop("wait returned no numeric exit code"); }
        Some(waited)
    }
    fn is_running(&mut self) -> bool { self.observed_exit().is_none() }
    fn wait_for_role_lock(&mut self, state: &Path) {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            assert!(self.is_running(), "runner exited before initialization: {}", self.stderr_text());
            if state.join("runner.lock").exists() && ServiceLock::acquire(state,"runner").is_err() {
                self.evidence.record(json!({"event":"role_lock_contention_observed","pid":self.child.id()}));
                return;
            }
            if Instant::now() >= end { self.failstop("runner never held its role lock within observation budget"); }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn stderr_text(&self) -> String { String::from_utf8_lossy(&std::fs::read(&self.stderr).unwrap()).into_owned() }
    fn wait_for_control_receipt(&mut self) {
        let end = Instant::now() + Duration::from_secs(2);
        loop {
            // Synchronization only: this acknowledges receipt, never actual exit.
            if self.stderr_text().contains("runner stop requested; local supervision only") {
                self.evidence.record(json!({"event":"control_receipt_observed_not_exit","pid":self.child.id()}));
                return;
            }
            assert!(self.is_running(), "runner exited before acknowledging control: {}", self.stderr_text());
            if Instant::now() >= end { self.failstop("no control receipt; held inbox not released early"); }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn finish(&mut self, budget: Duration) -> Captured {
        self.evidence.record(json!({"event":"wait_observation_started","pid":self.child.id(),"budget_ms":budget.as_millis()}));
        let end = Instant::now() + budget;
        loop {
            if let Some(waited) = self.observed_exit() {
                return Captured { code:waited.code(), stdout:std::fs::read(&self.stdout).unwrap(), stderr:self.stderr_text() };
            }
            if Instant::now() >= end { self.failstop("fixture process observation budget exceeded; not a product shutdown deadline"); }
            std::thread::sleep(Duration::from_millis(5));
        }
    }
    fn failstop(&mut self, reason: &str) -> ! {
        self.evidence.record(json!({"event":"BATCH_FAILSTOP","pid":self.child.id(),"reason":reason,
            "actual_wait_exit":null,"child_state":"UNKNOWN","no_kill":true,"retained_root":self.retained_root}));
        eprintln!("runner-stop batch failstop: {reason}; PID={} root={}; actual exit/descendants UNKNOWN, no kill",self.child.id(),self.retained_root.display());
        std::process::exit(86)
    }
}
impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !self.exited {
            // Reap an already-exited owned child if possible. Do not treat a
            // best-effort stdin write, dropping a handle, or ACK as cleanup.
            if self.observed_exit().is_none() { self.failstop("scope ended/panic with an unreaped runner"); }
        }
    }
}
fn ledger(path: &Path) -> Value {
    let db = rusqlite::Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let executions = db.prepare("SELECT run,attempt,body FROM executions ORDER BY run,attempt").unwrap()
        .query_map([], |r| Ok(json!([r.get::<_,String>(0)?,r.get::<_,u32>(1)?,r.get::<_,String>(2)?]))).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap();
    let outbox = db.prepare("SELECT id,body,sent FROM outbox ORDER BY rowid").unwrap()
        .query_map([], |r| Ok(json!([r.get::<_,String>(0)?,r.get::<_,String>(1)?,r.get::<_,i64>(2)?]))).unwrap()
        .collect::<rusqlite::Result<Vec<_>>>().unwrap();
    json!({"executions":executions,"outbox":outbox})
}

struct ReasonGate {
    received: mpsc::Receiver<()>, release: Option<mpsc::SyncSender<()>>,
    done: mpsc::Receiver<anyhow::Result<()>>, thread: Option<std::thread::JoinHandle<()>>,
    cancelled: Arc<AtomicBool>,
}
impl ReasonGate {
    fn new(endpoint: &str) -> Self {
        let listener = gate_platform::listen(endpoint).unwrap();
        let (received_tx, received) = mpsc::sync_channel(1);
        let (release, release_rx) = mpsc::sync_channel(1);
        let (done_tx, done) = mpsc::sync_channel(1);
        let cancelled = Arc::new(AtomicBool::new(false));
        let stopped = cancelled.clone();
        let thread = std::thread::spawn(move || {
            let result = (|| -> anyhow::Result<()> {
                let end = Instant::now() + Duration::from_secs(5);
                let mut stream = gate_platform::accept(listener, end, &stopped)?;
                let mut raw=Vec::new();
                loop {
                    anyhow::ensure!(Instant::now()<end && !stopped.load(Ordering::Acquire),"owned Reason frame deadline");
                    let mut byte=[0];
                    match stream.read(&mut byte) {
                        Ok(0)=>anyhow::bail!("owned Reason EOF"),
                        Ok(_)=>{ if byte[0]==b'\n' { break; } raw.push(byte[0]); anyhow::ensure!(raw.len()<=65536,"owned frame bound"); },
                        Err(e) if gate_platform::would_block(&e)=>std::thread::sleep(Duration::from_millis(2)),
                        Err(e)=>return Err(e.into()),
                    }
                }
                let request:Value=serde_json::from_slice(&raw)?;
                anyhow::ensure!(request["op"]=="inbox" && request["key"]=="owned-B/runner","wrong owned inbox request");
                received_tx.send(())?;
                release_rx.recv_timeout(Duration::from_secs(5))?;
                writeln!(stream,"{}",json!({"id":request["id"],"ok":true,"result":{"messages":[]}}))?;
                Ok(())
            })();
            let _=done_tx.send(result);
        });
        Self { received, release:Some(release), done, thread:Some(thread), cancelled }
    }
    fn wait_for_inbox(&mut self) { self.received.recv_timeout(Duration::from_secs(5)).expect("actual runner inbox request"); }
    fn release(&mut self) { self.release.take().unwrap().send(()).unwrap(); }
    fn finish(&mut self) {
        self.done.recv_timeout(Duration::from_secs(6)).expect("owned IPC server completion").unwrap();
        self.thread.take().unwrap().join().unwrap();
    }
}
impl Drop for ReasonGate {
    fn drop(&mut self) {
        self.cancelled.store(true,Ordering::Release);
        if let Some(release)=self.release.take() { let _=release.try_send(()); }
        if let Some(thread)=self.thread.take() {
            if self.done.recv_timeout(Duration::from_secs(6)).is_err() {
                eprintln!("owned Reason thread did not finish; whole batch failstop, no next child");
                std::process::exit(86);
            }
            if thread.join().is_err() {
                eprintln!("owned Reason thread panicked; whole batch failstop");
                std::process::exit(86);
            }
        }
    }
}
#[cfg(windows)]
mod gate_platform {
    use super::*;
    use std::os::windows::io::{AsRawHandle, FromRawHandle, RawHandle};
    use windows_sys::Win32::{Foundation::{GetLastError,ERROR_PIPE_CONNECTED,ERROR_PIPE_LISTENING,ERROR_NO_DATA,INVALID_HANDLE_VALUE},Storage::FileSystem::PIPE_ACCESS_DUPLEX,System::Pipes::{CreateNamedPipeW,ConnectNamedPipe,PIPE_TYPE_BYTE,PIPE_NOWAIT}};
    pub fn listen(endpoint:&str)->anyhow::Result<File> {
        let wide:Vec<u16>=endpoint.encode_utf16().chain(Some(0)).collect();
        let h=unsafe{CreateNamedPipeW(wide.as_ptr(),PIPE_ACCESS_DUPLEX,PIPE_TYPE_BYTE|PIPE_NOWAIT,1,65536,65536,3000,std::ptr::null())};
        anyhow::ensure!(h!=INVALID_HANDLE_VALUE,"owned pipe creation failed");
        Ok(unsafe{File::from_raw_handle(h as RawHandle)})
    }
    pub fn accept(file:File,end:Instant,cancelled:&AtomicBool)->anyhow::Result<File> {
        loop {
            anyhow::ensure!(Instant::now()<end && !cancelled.load(Ordering::Acquire),"owned pipe accept deadline");
            let ok=unsafe{ConnectNamedPipe(file.as_raw_handle() as _,std::ptr::null_mut())};
            if ok!=0 { return Ok(file); }
            let code=unsafe{GetLastError()};
            if code==ERROR_PIPE_CONNECTED { return Ok(file); }
            anyhow::ensure!(code==ERROR_PIPE_LISTENING,"owned pipe connect error {code}");
            std::thread::sleep(Duration::from_millis(2));
        }
    }
    pub fn would_block(e:&std::io::Error)->bool { e.kind()==std::io::ErrorKind::WouldBlock || e.raw_os_error()==Some(ERROR_NO_DATA as i32) }
}
#[cfg(unix)]
mod gate_platform {
    use super::*;
    use std::os::unix::net::{UnixListener,UnixStream};
    pub fn listen(endpoint:&str)->anyhow::Result<UnixListener> { let l=UnixListener::bind(endpoint)?; l.set_nonblocking(true)?; Ok(l) }
    pub fn accept(listener:UnixListener,end:Instant,cancelled:&AtomicBool)->anyhow::Result<UnixStream> {
        loop {
            anyhow::ensure!(Instant::now()<end && !cancelled.load(Ordering::Acquire),"owned socket accept deadline");
            match listener.accept() { Ok((s,_))=>{s.set_nonblocking(true)?;return Ok(s)},Err(e) if would_block(&e)=>std::thread::sleep(Duration::from_millis(2)),Err(e)=>return Err(e.into()) }
        }
    }
    pub fn would_block(e:&std::io::Error)->bool { e.kind()==std::io::ErrorKind::WouldBlock }
}
