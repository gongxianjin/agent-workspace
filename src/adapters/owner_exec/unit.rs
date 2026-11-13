//! Linux syscall boundary; intentionally not compiled as Linux evidence on Windows.
use anyhow::{ensure, Context, Result};
use std::{fs, io::Read, os::unix::fs::MetadataExt, path::{Path, PathBuf}, process::{Command, Stdio}, time::{Duration, Instant}};
use super::policy;

pub fn self_cgroup() -> Result<String> {
    process_cgroup(std::process::id())
}
pub fn process_cgroup(pid:u32) -> Result<String> {
    let mut raw=String::new();
    fs::File::open(format!("/proc/{pid}/cgroup"))?.take(65537).read_to_string(&mut raw)?;
    ensure!(raw.len() <= 65536, "cgroup entry exceeds bound");
    policy::unified_path(&raw)
}

pub fn current_root(ancestor:&Path) -> Result<(PathBuf,String)> {
    let actual=self_cgroup()?;
    let ancestor=ancestor.to_str().context("cgroup policy is not UTF-8")?;
    let root=PathBuf::from(policy::unit_root(ancestor,&actual)?);
    super::linux::no_symlink_components(&root)?;
    super::linux::validate_root(&root)?;
    let dir=fs::symlink_metadata(&root)?;
    let procs=fs::symlink_metadata(root.join("cgroup.procs"))?;
    ensure!(dir.is_dir() && procs.is_file() && !procs.file_type().is_symlink(), "invalid delegated root files");
    policy::validate_root_facts(unsafe{libc::getuid()},unsafe{libc::geteuid()},unsafe{libc::getgid()},unsafe{libc::getegid()},dir.uid(),procs.uid(),dir.mode(),procs.mode(),
        &fs::read_to_string(root.join("cgroup.type"))?,&fs::read_to_string(root.join("cgroup.subtree_control"))?)?;
    policy::validate_owner_presence(&fs::read_to_string(root.join("cgroup.procs"))?,std::process::id())?;
    attest_unit(&actual)?;
    ensure!(self_cgroup()? == actual, "NOT_READY: owner cgroup changed during attestation");
    Ok((root,actual))
}

fn capture_nonblocking(pipe:&mut impl Read, bytes:&mut Vec<u8>) -> Result<bool> {
    let mut buffer=[0_u8;2048];
    match pipe.read(&mut buffer) {
        Ok(0) => Ok(true),
        Ok(count) => {
            ensure!(bytes.len()+count <= 16384, "unit property output exceeded bound");
            bytes.extend_from_slice(&buffer[..count]);
            Ok(false)
        }
        Err(e) if e.kind()==std::io::ErrorKind::WouldBlock || e.kind()==std::io::ErrorKind::Interrupted => Ok(false),
        Err(e) => Err(e.into()),
    }
}
fn nonblocking(pipe:&impl std::os::fd::AsRawFd) -> Result<()> {
    let fd=pipe.as_raw_fd();
    let flags=unsafe { libc::fcntl(fd,libc::F_GETFL) };
    ensure!(flags >= 0 && unsafe{libc::fcntl(fd,libc::F_SETFL,flags|libc::O_NONBLOCK)} == 0, "unit property pipe cannot be bounded");
    Ok(())
}

fn query_properties(candidate:&str) -> Result<String> {
    let deadline=Instant::now()+Duration::from_secs(2);
    let mut child=Command::new("/usr/bin/systemctl").args(["--user","show","--no-pager","--property=Transient,Delegate,MainPID,ControlGroup,InvocationID","--",candidate])
        .stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().context("NOT_READY: unit property query unavailable")?;
    let result=(|| -> Result<String> {
        let mut stdout=child.stdout.take().context("unit stdout missing")?;
        let mut stderr=child.stderr.take().context("unit stderr missing")?;
        nonblocking(&stdout)?; nonblocking(&stderr)?;
        let(mut out,mut err)=(Vec::new(),Vec::new());
        let(mut out_eof,mut err_eof)=(false,false);
        loop {
            ensure!(Instant::now()<deadline, "NOT_READY: unit property query timeout");
            if !out_eof { out_eof=capture_nonblocking(&mut stdout,&mut out)?; }
            if !err_eof { err_eof=capture_nonblocking(&mut stderr,&mut err)?; }
            if let Some(status)=child.try_wait()? {
                ensure!(status.success(), "NOT_READY: unit property query failed");
                if out_eof && err_eof { return String::from_utf8(out).context("invalid unit property encoding"); }
            }
            std::thread::sleep(Duration::from_millis(2));
        }
    })();
    if result.is_err() { let _=child.kill(); }
    let waited=child.wait().context("NOT_READY: failed to reap unit property query");
    waited?;
    result
}

fn attest_unit(actual:&str) -> Result<()> {
    let candidate=policy::candidate_unit(actual)?;
    let invocation=std::env::var("INVOCATION_ID").context("NOT_READY: INVOCATION_ID unavailable")?;
    let raw=query_properties(candidate)?;
    policy::validate_unit_properties(&raw,std::process::id(),actual,&invocation).context("NOT_READY: unit attestation rejected")
}
