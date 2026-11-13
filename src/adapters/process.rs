//! OS containment for our fixed, gated Rust fixture only; not a real-AO process oracle.
use anyhow::{anyhow, Context, Result, ensure};
use std::{
    io::{self, Read, Write},
    path::Path,
    process::{Child, Command, ExitStatus, Stdio},
    thread::JoinHandle,
};

// Two 4 KiB prefixes plus metadata remain below the signed 64 KiB frame bound.
const MAX_CAPTURE_BYTES: usize = 4096;

#[derive(Debug)]
struct CapturedOutput {
    bytes: Vec<u8>,
    total_bytes: usize,
    truncated: bool,
    error: Option<String>,
}

#[derive(Clone, Debug)]
pub struct ChildEvidence {
    pub exit_code: Option<i32>,
    pub stdout: Vec<u8>,
    pub stderr: Vec<u8>,
    pub stdout_truncated: bool,
    pub stderr_truncated: bool,
    pub stdout_total_bytes: usize,
    pub stderr_total_bytes: usize,
    pub stdout_error: Option<String>,
    pub stderr_error: Option<String>,
}

pub struct ManagedChild {
    child: Child,
    status: Option<ExitStatus>,
    evidence: Option<ChildEvidence>,
    stdout: Option<JoinHandle<CapturedOutput>>,
    stderr: Option<JoinHandle<CapturedOutput>>,
    #[cfg(windows)] job: Job,
    #[cfg(unix)] group: i32,
}

fn capture(mut reader: impl Read) -> CapturedOutput {
    let mut bytes = Vec::new();
    let mut truncated = false;
    let mut total_bytes = 0_usize;
    let mut error = None;
    let mut chunk = [0_u8; 8192];
    loop {
        let count = match reader.read(&mut chunk) {
            Ok(count) => count,
            Err(read_error) => {
                error = Some(read_error.to_string());
                break;
            }
        };
        if count == 0 {
            break;
        }
        total_bytes = total_bytes.saturating_add(count);
        let remaining = MAX_CAPTURE_BYTES.saturating_sub(bytes.len());
        let keep = remaining.min(count);
        bytes.extend_from_slice(&chunk[..keep]);
        if keep < count {
            truncated = true;
        }
    }
    CapturedOutput { bytes, total_bytes, truncated, error }
}

impl ManagedChild {
    pub fn spawn(worker: &Path, template: &str, artifact: &Path, session: &str) -> Result<Self> {
        ensure!(matches!(template, "sum-native-v1" | "control-native-v1"), "unsupported fixture");
        let mut cmd = Command::new(worker);
        cmd.arg("fixture-worker")
            .arg(template)
            .arg(artifact)
            .arg(session)
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        #[cfg(windows)] {
            use std::os::windows::process::CommandExt;
            cmd.creation_flags(0x08000000);
        }
        #[cfg(unix)] {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0);
        }
        let mut child = cmd.spawn().context("spawn bundled Rust fixture")?;
        #[cfg(windows)] let job = match Job::attach(&child) {
            Ok(job) => job,
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(e);
            }
        };
        #[cfg(unix)] let group = i32::try_from(child.id())?;

        let stdout = child.stdout.take().context("fixture stdout pipe missing")?;
        let stderr = child.stderr.take().context("fixture stderr pipe missing")?;
        #[cfg(test)] let stdout = test_capture::wrap(stdout, "stdout");
        #[cfg(test)] let stderr = test_capture::wrap(stderr, "stderr");
        let stdout = std::thread::spawn(move || capture(stdout));
        let stderr = std::thread::spawn(move || capture(stderr));

        // The trusted fixture reads this byte before any work/descendant spawn. A failed
        // containment operation never opens the gate; EOF makes the fixture exit.
        if let Err(e) = child.stdin.take().context("fixture gate missing")?.write_all(b"G") {
            let _ = child.kill();
            let _ = child.wait();
            return Err(e.into());
        }
        Ok(Self {
            child,
            status: None,
            evidence: None,
            stdout: Some(stdout),
            stderr: Some(stderr),
            #[cfg(windows)] job,
            #[cfg(unix)] group,
        })
    }

    fn poll_status(&mut self) -> Result<bool> {
        if self.status.is_none() {
            self.status = self.child.try_wait()?;
            #[cfg(test)]
            if let Some(status) = &self.status {
                println!("MANAGED_CHILD pid={} wait_completed=true numeric_exit={:?}", self.child.id(), status.code());
            }
        }
        Ok(self.status.is_some())
    }

    pub fn evidence(&mut self) -> Result<Option<ChildEvidence>> {
        if let Some(evidence) = &self.evidence {
            return Ok(Some(evidence.clone()));
        }
        if !self.poll_status()? {
            return Ok(None);
        }
        if !self.stdout.as_ref().is_some_and(|reader| reader.is_finished())
            || !self.stderr.as_ref().is_some_and(|reader| reader.is_finished()) {
            return Ok(None);
        }
        let stdout = self.stdout.take().context("fixture stdout reader missing")?.join()
            .map_err(|_| anyhow!("fixture stdout reader panicked"))?;
        let stderr = self.stderr.take().context("fixture stderr reader missing")?.join()
            .map_err(|_| anyhow!("fixture stderr reader panicked"))?;
        let status = self.status.as_ref().context("fixture exit status missing")?;
        let evidence = ChildEvidence {
            exit_code: status.code(),
            stdout: stdout.bytes,
            stderr: stderr.bytes,
            stdout_truncated: stdout.truncated,
            stderr_truncated: stderr.truncated,
            stdout_total_bytes: stdout.total_bytes,
            stderr_total_bytes: stderr.total_bytes,
            stdout_error: stdout.error,
            stderr_error: stderr.error,
        };
        self.evidence = Some(evidence.clone());
        Ok(Some(evidence))
    }

    pub fn stop(&mut self) -> Result<()> {
        #[cfg(windows)] self.job.stop()?;
        #[cfg(unix)] {
            let rc = unsafe { libc::kill(-self.group, libc::SIGKILL) };
            if rc != 0 && io::Error::last_os_error().raw_os_error() != Some(libc::ESRCH) {
                return Err(io::Error::last_os_error().into());
            }
        }
        Ok(())
    }

    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    pub fn active(&mut self) -> Result<u32> {
        let _ = self.poll_status()?;
        #[cfg(windows)] { self.job.active() }
        #[cfg(target_os = "linux")] {
            let mut count = 0;
            for e in std::fs::read_dir("/proc")? {
                let e = e?;
                if e.file_name().to_string_lossy().parse::<u32>().is_err() { continue; }
                let raw = match std::fs::read_to_string(e.path().join("stat")) {
                    Ok(s) => s,
                    Err(e) if e.kind() == io::ErrorKind::NotFound => continue,
                    Err(e) => return Err(e.into()),
                };
                let fields = raw.rsplit_once(')').context("invalid proc stat")?.1
                    .split_whitespace().collect::<Vec<_>>();
                if fields.get(2).and_then(|s| s.parse::<i32>().ok()) == Some(self.group) { count += 1; }
            }
            Ok(count)
        }
        #[cfg(all(unix, not(target_os = "linux")))] {
            anyhow::bail!("fixture process observation unsupported on this OS")
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        let _ = self.stop();
        if self.status.is_none() {
            self.status = self.child.wait().ok();
        }
        let _ = self.stdout.take().map(|reader| reader.join());
        let _ = self.stderr.take().map(|reader| reader.join());
    }
}

#[cfg(windows)] struct Job(windows_sys::Win32::Foundation::HANDLE);
#[cfg(windows)] impl Job {
    fn attach(child: &Child) -> Result<Self> {
        use windows_sys::Win32::System::JobObjects::*;
        use std::os::windows::io::AsRawHandle;
        let handle = unsafe { CreateJobObjectW(std::ptr::null(), std::ptr::null()) };
        ensure!(!handle.is_null(), "CreateJobObjectW: {}", io::Error::last_os_error());
        let job = Self(handle);
        let mut limits = JOBOBJECT_EXTENDED_LIMIT_INFORMATION::default();
        limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
        ensure!(unsafe { SetInformationJobObject(handle, JobObjectExtendedLimitInformation, (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(), std::mem::size_of_val(&limits) as u32) } != 0, "SetInformationJobObject: {}", io::Error::last_os_error());
        ensure!(unsafe { AssignProcessToJobObject(handle, child.as_raw_handle()) } != 0, "AssignProcessToJobObject: {}", io::Error::last_os_error());
        Ok(job)
    }
    fn active(&self) -> Result<u32> {
        use windows_sys::Win32::System::JobObjects::*;
        let mut info = JOBOBJECT_BASIC_ACCOUNTING_INFORMATION::default();
        ensure!(unsafe { QueryInformationJobObject(self.0, JobObjectBasicAccountingInformation, (&mut info as *mut JOBOBJECT_BASIC_ACCOUNTING_INFORMATION).cast(), std::mem::size_of_val(&info) as u32, std::ptr::null_mut()) } != 0, "job observation failed: {}", io::Error::last_os_error());
        Ok(info.ActiveProcesses)
    }
    fn stop(&self) -> Result<()> {
        ensure!(unsafe { windows_sys::Win32::System::JobObjects::TerminateJobObject(self.0, 2) } != 0, "job termination failed: {}", io::Error::last_os_error());
        Ok(())
    }
}
#[cfg(windows)] impl Drop for Job { fn drop(&mut self) { unsafe { windows_sys::Win32::Foundation::CloseHandle(self.0); } } }

#[cfg(test)]
pub(crate) mod test_capture {
    use std::{cell::RefCell, io::{self, Read}};
    thread_local! { static FAULT: RefCell<Option<String>> = const { RefCell::new(None) }; }
    pub(crate) fn fail(stream: Option<&str>) { FAULT.with(|fault| *fault.borrow_mut() = stream.map(str::to_owned)); }
    pub(crate) struct FaultReader<R> { inner: R, fail: bool, read_prefix: bool, stream: &'static str }
    pub(crate) fn wrap<R: Read>(inner: R, stream: &'static str) -> FaultReader<R> {
        let fail = FAULT.with(|fault| fault.borrow().as_deref() == Some(stream));
        FaultReader { inner, fail, read_prefix: false, stream }
    }
    impl<R: Read> Read for FaultReader<R> {
        fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
            if self.fail && self.read_prefix {
                eprintln!("READ_BOUNDARY stream={} error=injected-reader-error-after-real-prefix", self.stream);
                return Err(io::Error::other("injected reader error after real prefix"));
            }
            let count = self.inner.read(buffer)?;
            if count > 0 {
                eprintln!("READ_BOUNDARY stream={} bytes={} hex={}", self.stream, count, hex::encode(&buffer[..count]));
            }
            self.read_prefix |= count > 0;
            Ok(count)
        }
    }
}

pub fn fixture_worker(args: &[String]) -> Result<()> {
    use std::io::{Read, Write};
    ensure!(args.len() == 3, "fixture worker requires fixed template/artifact/session");
    let (template, artifact, session) = (&args[0], Path::new(&args[1]), &args[2]);
    ensure!(crate::config::valid_id(session), "invalid fixture session");
    eprintln!("fixture-worker template={template} session={session}");
    let mut gate = [0_u8];
    std::io::stdin().read_exact(&mut gate)?;
    ensure!(gate == [b'G'], "fixture gate rejected");
    let value = match template.as_str() {
        "sum-native-v1" => serde_json::json!({"schema":"native-sum/1","session":session,"inputs":[1,2,3],"sum":6}),
        "control-native-v1" => {
            let mut cmd = Command::new(std::env::current_exe()?);
            cmd.arg("fixture-descendant").stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null());
            #[cfg(windows)] { use std::os::windows::process::CommandExt; cmd.creation_flags(0x08000000); }
            let child = cmd.spawn()?;
            let value = serde_json::json!({"schema":"native-control/1","session":session,"parent":std::process::id(),"descendant":child.id()});
            let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(artifact)?;
            f.write_all(&serde_json::to_vec(&value)?)?;
            f.sync_all()?;
            loop { std::thread::sleep(std::time::Duration::from_secs(1)); }
        }
        _ => anyhow::bail!("unknown fixture template"),
    };
    let serialized = serde_json::to_vec(&value)?;
    let mut f = std::fs::OpenOptions::new().write(true).create_new(true).open(artifact)?;
    f.write_all(&serialized)?;
    f.sync_all()?;
    println!("{}", String::from_utf8_lossy(&serialized));
    Ok(())
}
