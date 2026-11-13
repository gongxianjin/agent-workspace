//! Linux-only direct owner of the bundled gated child. Fail closed without cgroup v2.
#[cfg(not(target_os = "linux"))]
use anyhow::Result;
#[cfg(not(target_os = "linux"))]
use std::path::PathBuf;
pub mod policy;
#[cfg(target_os = "linux")]
mod unit;

#[cfg(target_os = "linux")]
mod linux {
    use anyhow::{Context, Result, ensure};
    use super::policy;
    use std::{fs, io::{self, Read, Write}, path::{Path, PathBuf}, process::{Child, Command, Stdio}, thread::JoinHandle, time::{Duration, Instant}};
    use std::os::unix::{fs::OpenOptionsExt, process::{CommandExt, ExitStatusExt}};
    use crate::core::{owner_producer::{CleanupFacts, LaunchFacts, OwnedProcessPort, OwnerLaunchPlan, StreamReadOutcome, TerminalFacts}, worker_owner::{OwnerProcessIdentity, derive_owner_handle}, native_worker::CgroupRootMode};
    const PREFIX: usize = 4096;
    const CGROUP2_MAGIC: libc::c_long = 0x63677270;

    pub struct LinuxOwnedProcessPort {
        command: PathBuf,
        args: Vec<String>,
        root: PathBuf,
        timeout: Duration,
        owner: OwnerProcessIdentity,
        child: Option<Child>,
        stdout: Option<JoinHandle<StreamReadOutcome>>,
        stderr: Option<JoinHandle<StreamReadOutcome>>,
        pgid: Option<u32>,
        cgroup: Option<PathBuf>,
        private: policy::PrivateDirectories,
        mode: CgroupRootMode,
    }

    fn proc_identity(pid: u32) -> Result<(u32, u32, u64)> {
        let raw = fs::read_to_string(format!("/proc/{pid}/stat"))?;
        let rest = raw.rsplit_once(')').context("invalid /proc stat")?.1;
        let fields: Vec<_> = rest.split_whitespace().collect();
        let parent = fields.get(1).context("missing PPID")?.parse()?;
        let group = fields.get(2).context("missing PGID")?.parse()?;
        let ticks = fields.get(19).context("missing start ticks")?.parse()?;
        Ok((parent, group, ticks))
    }

    pub(super) fn no_symlink_components(path: &Path) -> Result<()> {
        ensure!(path.is_absolute(), "absolute delegated cgroup path required");
        let mut current = PathBuf::new();
        for component in path.components() {
            ensure!(!matches!(component, std::path::Component::ParentDir), "cgroup parent traversal forbidden");
            current.push(component.as_os_str());
            if current.exists() { ensure!(!fs::symlink_metadata(&current)?.file_type().is_symlink(), "cgroup symlink forbidden"); }
        }
        Ok(())
    }

    fn cgroup_file(path: &Path) -> Result<fs::File> {
        Ok(fs::OpenOptions::new().write(true).custom_flags(libc::O_NOFOLLOW).open(path)?)
    }

    pub(super) fn validate_root(root: &Path) -> Result<()> {
        use std::os::unix::ffi::OsStrExt;
        no_symlink_components(root)?;
        let bytes = std::ffi::CString::new(root.as_os_str().as_bytes())?;
        let mut stat: libc::statfs = unsafe { std::mem::zeroed() };
        ensure!(unsafe { libc::statfs(bytes.as_ptr(), &mut stat) } == 0, "cgroup filesystem check failed");
        ensure!(stat.f_type == CGROUP2_MAGIC, "NOT_READY: delegated root is not cgroup v2");
        ensure!(root.join("cgroup.controllers").is_file(), "NOT_READY: cgroup v2 controllers unavailable");
        cgroup_file(&root.join("cgroup.procs")).context("NOT_READY: cgroup delegation is not writable")?;
        Ok(())
    }

    fn empty_capture(error: String) -> StreamReadOutcome {
        StreamReadOutcome { prefix:vec![], total_read_bytes:0, eof:false, error:Some(error) }
    }

    fn capture(mut input: impl Read) -> StreamReadOutcome {
        let mut value = StreamReadOutcome { prefix:vec![], total_read_bytes:0, eof:false, error:None };
        let mut buffer = [0_u8;8192];
        loop {
            match input.read(&mut buffer) {
                Ok(0) => { value.eof = true; break; }
                Ok(count) => {
                    value.total_read_bytes = value.total_read_bytes.saturating_add(count as u64);
                    let keep = PREFIX.saturating_sub(value.prefix.len()).min(count);
                    value.prefix.extend_from_slice(&buffer[..keep]);
                }
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => { value.error = Some(error.to_string()); break; }
            }
        }
        value
    }

    fn group_members(group: u32) -> Result<u64> {
        let mut count = 0_u64;
        for entry in fs::read_dir("/proc")? {
            let entry = entry?;
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
            match proc_identity(pid) {
                Ok((_, pgid, _)) => { if pgid == group { count += 1; } }
                Err(error) if error.downcast_ref::<io::Error>().is_some_and(|e| e.kind() == io::ErrorKind::NotFound) => {}
                Err(error) => return Err(error.context("incomplete /proc cleanup scan")),
            }
        }
        Ok(count)
    }

    fn cgroup_members(path: &Path) -> Result<u64> {
        // cgroup.events populated includes descendants, unlike cgroup.procs alone.
        let events = fs::read_to_string(path.join("cgroup.events"))?;
        let populated = events.lines().find_map(|line| line.strip_prefix("populated ")).context("missing cgroup populated state")?;
        let direct = fs::read_to_string(path.join("cgroup.procs"))?;
        let direct_count = direct.lines().filter(|line| !line.is_empty()).count() as u64;
        match populated { "0" => { ensure!(direct_count == 0, "inconsistent cgroup occupancy"); Ok(0) }, "1" => Ok(direct_count.max(1)), _ => anyhow::bail!("invalid cgroup populated state") }
    }

    impl LinuxOwnedProcessPort {
        pub fn new(command: PathBuf, args: Vec<String>, root: PathBuf, timeout_secs: u64) -> Result<Self> {
            Self::with_mode(command,args,root,timeout_secs,CgroupRootMode::Explicit)
        }

        pub fn with_mode(command: PathBuf, args: Vec<String>, root: PathBuf, timeout_secs: u64, mode: CgroupRootMode) -> Result<Self> {
            ensure!(command == std::env::current_exe()?, "only the current bundled owner child is permitted");
            // Fixed owner argv: sum-v1 carries its session; project-work-v1
            // carries the absolute managed work directory holding work.json.
            ensure!(args.len() == 3 && args[0] == "owner-child" && match args[1].as_str() {
                "sum-v1" => crate::config::valid_id(&args[2]),
                "project-work-v1" => std::path::Path::new(&args[2]).is_absolute(),
                _ => false,
            }, "fixed owner argv required");
            ensure!((1..=300).contains(&timeout_secs), "invalid owner timeout");
            let root = match mode {
                CgroupRootMode::Explicit => { validate_root(&root)?; root }
                CgroupRootMode::CurrentUnit => { super::unit::current_root(&root)?.0 }
            };
            let pid = std::process::id();
            let (_, _, start_ticks) = proc_identity(pid)?;
            Ok(Self {command,args,root,timeout:Duration::from_secs(timeout_secs),owner:OwnerProcessIdentity{pid,start_ticks},child:None,stdout:None,stderr:None,pgid:None,cgroup:None,private:policy::PrivateDirectories::default(),mode})
        }

        fn create_cgroup(&mut self, plan: &OwnerLaunchPlan) -> Result<()> {
            let handle = derive_owner_handle(&plan.execution,&plan.node_id,&plan.instance_id,&plan.boot_id)?;
            let workers = self.root.join("workers");
            no_symlink_components(&workers)?;
            if !workers.is_dir() { fs::create_dir(&workers).context("NOT_READY: shared workers cgroup cannot be created")?; }
            let parent = workers.join(&plan.instance_id);
            no_symlink_components(&parent)?;
            self.private.create(&parent)?;
            let path = parent.join(handle);
            self.private.create(&path)?;
            self.cgroup = Some(path.clone());
            policy::empty_procs(&fs::read_to_string(workers.join("cgroup.procs"))?)?;
            policy::empty_procs(&fs::read_to_string(parent.join("cgroup.procs"))?)?;
            cgroup_file(&path.join("cgroup.procs"))?;
            policy::validate_target_state(&fs::read_to_string(path.join("cgroup.procs"))?,&fs::read_to_string(path.join("cgroup.events"))?,None)?;
            Ok(())
        }

        fn launch_inner(&mut self, plan: &OwnerLaunchPlan) -> Result<LaunchFacts> {
            self.create_cgroup(plan)?;
            let mut command = Command::new(&self.command);
            command.args(&self.args).process_group(0)
                .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped());
            if self.args.get(1).map(String::as_str) == Some("project-work-v1") {
                // The model bearer token travels owner memory -> child env only
                // for the project template; it is never argv, a file, or a log.
                let token = std::env::var("AO_MODEL_TOKEN")
                    .context("AO_MODEL_TOKEN missing; project owner-child requires the in-memory token")?;
                ensure!(!token.trim().is_empty() && token.len() <= 16384, "invalid model token shape");
                command.env("AO_MODEL_TOKEN", token);
            }
            let child = command.spawn()?;
            let pid = child.id();
            self.pgid = Some(pid);
            self.child = Some(child);
            let (parent, pgid, ticks) = proc_identity(pid)?;
            ensure!(parent == self.owner.pid && pgid == pid, "owner PID/PPID/PGID mismatch");
            let path = self.cgroup.as_ref().context("owner cgroup missing")?;
            if self.mode == CgroupRootMode::CurrentUnit {
                let expected = policy::strip_mount(self.root.to_str().context("invalid unit root")?)?;
                let self_observed = super::unit::self_cgroup().ok();
                let child_observed = super::unit::process_cgroup(pid).ok();
                if self_observed.as_deref() != Some(expected.as_str()) || child_observed.as_deref() != Some(expected.as_str()) {
                    // Start evidence capture before destroying the state: a dead
                    // child's stderr and exit status are the only diagnostic for
                    // this failure, and terminate() below would discard them.
                    let child_status = self.child.as_mut().and_then(|c| c.try_wait().ok().flatten()).map(|s| format!("{s}"));
                    let stderr_probe = self.child_stderr_probe();
                    anyhow::bail!("{} child_status={:?} child_stderr_prefix={:?}", policy::birth_mismatch(&expected, self_observed.as_deref(), child_observed.as_deref()), child_status, stderr_probe);
                }
            }
            let attach_target = path.join("cgroup.procs");
            let child_alive = self.child.as_mut().and_then(|c| c.try_wait().ok().flatten().map(|s| s.to_string())).unwrap_or_else(|| "alive-or-unwaited".into());
            if let Err(error) = policy::attach_line(cgroup_file(&attach_target)?, pid) {
                anyhow::bail!("NOT_READY: writing child pid to cgroup.procs failed ({path:?} child={pid} child_status={child_alive}: {error})");
            }
            let procs = fs::read_to_string(path.join("cgroup.procs")).context("NOT_READY: reading cgroup.procs after attach failed")?;
            let after = proc_identity(pid)?;
            if self.mode == CgroupRootMode::CurrentUnit {
                let expected = policy::strip_mount(path.to_str().context("invalid target cgroup")?)?;
                policy::validate_child_membership(&expected,&super::unit::process_cgroup(pid)?,(parent,pgid,ticks),after,&procs,pid)?;
                policy::validate_target_state(&procs,&fs::read_to_string(path.join("cgroup.events"))?,Some(pid))?;
                let owner_root = policy::strip_mount(self.root.to_str().context("invalid unit root")?)?;
                ensure!(super::unit::self_cgroup()? == owner_root, "owner cgroup changed");
                self.owner_identity()?;
            } else {
                ensure!(procs.lines().any(|line| line == pid.to_string()), "child cgroup membership not observed");
                ensure!(after == (parent,pgid,ticks), "child identity changed before gate");
            }
            let child = self.child.as_mut().context("owner child missing")?;
            let stdout = child.stdout.take().context("stdout pipe missing")?;
            let stderr = child.stderr.take().context("stderr pipe missing")?;
            self.stdout = Some(std::thread::spawn(move || capture(stdout)));
            self.stderr = Some(std::thread::spawn(move || capture(stderr)));
            child.stdin.take().context("owner gate missing")?.write_all(b"G").context("NOT_READY: writing owner gate failed")?;
            Ok(LaunchFacts {child_pid:pid,child_start_ticks:ticks,pgid})
        }

        fn terminate(&mut self) {
            if let Some(group) = self.pgid { unsafe { libc::kill(-(group as i32),libc::SIGKILL); } }
            if let Some(path) = &self.cgroup { let _ = cgroup_file(&path.join("cgroup.kill")).and_then(|mut f| { f.write_all(b"1")?; Ok(()) }); }
            if let Some(child) = self.child.as_mut() { let _=child.kill(); let _=child.wait(); }
        }

        /// Best-effort stderr prefix of a just-failed child, taken before
        /// terminate() destroys the evidence. Starts a capture thread, waits a
        /// bounded instant, and returns whatever the pipe held. Empty when the
        /// child produced nothing before dying or is still blocked at the gate.
        fn child_stderr_probe(&mut self) -> Option<String> {
            let child = self.child.as_mut()?;
            if self.stderr.is_none() {
                let stderr = child.stderr.take()?;
                self.stderr = Some(std::thread::spawn(move || capture(stderr)));
            }
            std::thread::sleep(Duration::from_millis(50));
            let finished = self.stderr.as_ref().is_some_and(|h| h.is_finished());
            if !finished { return Some("<blocked>".into()); }
            let outcome = self.stderr.take().map(|h| h.join().ok())??;
            Some(String::from_utf8_lossy(&outcome.prefix).into_owned())
        }

        fn read_completed(reader: &mut Option<JoinHandle<StreamReadOutcome>>) -> StreamReadOutcome {
            let Some(handle) = reader.take() else { return empty_capture("stream reader missing".into()); };
            if !handle.is_finished() { return empty_capture("stream EOF deadline exceeded".into()); }
            handle.join().unwrap_or_else(|_| empty_capture("stream reader panicked".into()))
        }

        fn terminal(&mut self) -> Result<TerminalFacts> {
            let deadline = Instant::now()+self.timeout;
            let status = loop {
                if let Some(status) = self.child.as_mut().context("owner child missing")?.try_wait()? { break status; }
                if Instant::now() >= deadline { self.terminate(); anyhow::bail!("owner timeout; UNKNOWN without terminal evidence"); }
                std::thread::sleep(Duration::from_millis(10));
            };
            while self.stdout.as_ref().is_some_and(|h| !h.is_finished()) || self.stderr.as_ref().is_some_and(|h| !h.is_finished()) {
                if Instant::now() >= deadline { self.terminate(); break; }
                std::thread::sleep(Duration::from_millis(5));
            }
            let stdout = Self::read_completed(&mut self.stdout);
            let stderr = Self::read_completed(&mut self.stderr);
            let group_members = group_members(self.pgid.context("PGID missing")?)?;
            let path = self.cgroup.as_ref().context("cgroup missing")?;
            let members = cgroup_members(path)?;
            let error = if group_members != 0 || members != 0 { Some("cleanup retains owned processes".into()) } else { None };
            if error.is_none() {
                policy::validate_target_state(&fs::read_to_string(path.join("cgroup.procs"))?,&fs::read_to_string(path.join("cgroup.events"))?,None)?;
                self.private.remove()?;
                self.cgroup = None;
            }
            Ok(TerminalFacts {wait_completed:true,wait_method:"child_try_wait".into(),raw_wait_status:status.into_raw(),stdout,stderr,
                cleanup:CleanupFacts{proc_scan_complete:true,process_group_scan_complete:true,cgroup_scan_complete:true,process_group_members:group_members,cgroup_members:members,error}})
        }
    }

    impl OwnedProcessPort for LinuxOwnedProcessPort {
        fn owner_identity(&self) -> Result<OwnerProcessIdentity> {
            let (_,_,ticks)=proc_identity(self.owner.pid)?;
            ensure!(ticks==self.owner.start_ticks,"owner identity changed");Ok(self.owner.clone())
        }
        fn launch(&mut self,plan:&OwnerLaunchPlan)->Result<LaunchFacts> {
            ensure!(self.child.is_none(),"owner already launched");
            let result=self.launch_inner(plan);if result.is_err(){self.terminate();}result
        }
        fn wait_terminal(&mut self)->Result<TerminalFacts>{self.terminal()}
    }
    impl Drop for LinuxOwnedProcessPort {
        fn drop(&mut self) { self.terminate(); let _=self.private.remove(); }
    }
}
#[cfg(target_os = "linux")]
pub use linux::LinuxOwnedProcessPort;
#[cfg(not(target_os = "linux"))]
pub struct LinuxOwnedProcessPort;
#[cfg(not(target_os = "linux"))]
impl LinuxOwnedProcessPort {
    pub fn new(_:PathBuf,_:Vec<String>,_:PathBuf,_:u64)->Result<Self>{anyhow::bail!("Linux owner provider is unavailable on this platform")}
}
