//! Pure delegated topology policy; local tests are not Linux runtime proof.
use anyhow::{Context, ensure, Result};
use std::{fs, io::Write, path::{Path, PathBuf}};
#[cfg(not(unix))]
use std::time::SystemTime;

fn normalized(value: &str) -> Result<()> {
    ensure!(value.starts_with('/') && value.len() > 1, "absolute normalized path required");
    ensure!(value[1..].split('/').all(|p| !p.is_empty() && p != "." && p != ".."), "non-normal path component");
    ensure!(value.bytes().all(|b| b.is_ascii_alphanumeric() || b"/._-:@".contains(&b)), "unsupported cgroup path byte");
    Ok(())
}

pub fn unified_path(raw: &str) -> Result<String> {
    let mut found = None;
    for line in raw.lines() {
        let fields: Vec<_> = line.splitn(3, ':').collect();
        ensure!(fields.len() == 3 && fields[0].parse::<u32>().is_ok(), "invalid cgroup record");
        if fields[0] == "0" {
            ensure!(fields[1].is_empty() && found.is_none(), "ambiguous unified cgroup");
            normalized(fields[2])?;
            found = Some(fields[2].to_owned());
        }
    }
    found.ok_or_else(|| anyhow::anyhow!("unique unified cgroup entry required"))
}

pub fn unit_root(policy: &str, actual: &str) -> Result<String> {
    normalized(policy)?;
    normalized(actual)?;
    const MOUNT: &str = "/sys/fs/cgroup";
    ensure!(policy == MOUNT || policy.starts_with("/sys/fs/cgroup/"), "policy outside fixed cgroup2 mount");
    let root = format!("{MOUNT}{actual}");
    ensure!(root.starts_with(&format!("{policy}/")), "unit root outside strict component ancestor");
    Ok(root)
}

/// Strips the fixed cgroup2 mount prefix while keeping the result absolute,
/// so it can be compared byte-for-byte with /proc/<pid>/cgroup unified paths
/// (which are absolute, e.g. "/user.slice/..."). `Path::strip_prefix` alone
/// would drop the leading separator and the comparison could never succeed.
pub fn strip_mount(root: &str) -> Result<String> {
    const MOUNT: &str = "/sys/fs/cgroup";
    let rest = root.strip_prefix(MOUNT).with_context(|| "root outside fixed cgroup2 mount")?;
    ensure!(rest.starts_with('/'), "root is the bare mount; not a unit root");
    normalized(rest)?;
    Ok(rest.to_owned())
}

/// Builds the birth-check failure message naming both observed operands.
/// `None` means the /proc read itself failed; that must be explicit instead of
/// silently reading as an empty or default value.
pub fn birth_mismatch(expected: &str, self_observed: Option<&str>, child_observed: Option<&str>) -> String {
    let self_value = self_observed.unwrap_or("read-error");
    let child_value = child_observed.unwrap_or("read-error");
    format!("NOT_READY: owner/child birth cgroup mismatch (expected={expected} self={self_value} child={child_value})")
}

/// Attaches `pid` to a cgroup with exactly one write(2). cgroup v2 kernfs
/// processes each write(2) as one complete operation; a formatted `writeln!`
/// reaches it as two writes (digits, then a bare "\n") and the second fails
/// with EINVAL even though the first already moved the pid. Verified live on
/// the delegated unit: newline-probe-resume5.txt (W1 ok / W2b EINVAL).
pub fn attach_line(mut target: impl Write, pid: u32) -> Result<()> {
    let line = format!("{pid}\n");
    target.write_all(line.as_bytes()).map_err(Into::into)
}

pub fn candidate_unit(path: &str) -> Result<&str> {
    normalized(path)?;
    let name = path.rsplit('/').next().unwrap_or_default();
    ensure!(name.ends_with(".service") && name.len() > 8 && name.len() <= 255 && !name.starts_with('-'), "candidate must be a service basename");
    Ok(name)
}

pub fn validate_unit_properties(raw: &str, owner_pid: u32, path: &str, invocation: &str) -> Result<()> {
    candidate_unit(path)?;
    ensure!(invocation.len() == 32 && invocation.bytes().all(|b| b.is_ascii_hexdigit()), "invocation id missing or malformed");
    let mut map = std::collections::BTreeMap::new();
    for line in raw.lines() {
        let (key, value) = line.split_once('=').ok_or_else(|| anyhow::anyhow!("malformed unit property"))?;
        ensure!(map.insert(key, value).is_none(), "duplicate unit property");
    }
    ensure!(map.len() == 5, "unit properties missing or unexpected");
    for (key, expected) in [("Transient","yes"),("Delegate","yes"),("ControlGroup",path),("InvocationID",invocation)] {
        ensure!(map.get(key) == Some(&expected), "unit property {key} mismatch");
    }
    ensure!(map.get("MainPID").and_then(|s| s.parse::<u32>().ok()) == Some(owner_pid), "unit MainPID mismatch");
    Ok(())
}

#[allow(clippy::too_many_arguments)]
pub fn validate_root_facts(uid:u32,euid:u32,gid:u32,egid:u32,dir_uid:u32,procs_uid:u32,dir_mode:u32,procs_mode:u32,kind:&str,controls:&str)->Result<()> {
    ensure!(uid != 0 && gid != 0 && uid == euid && gid == egid, "ordinary nonroot identity required");
    ensure!(dir_uid == uid && procs_uid == uid, "cgroup ownership mismatch");
    ensure!(dir_mode & 0o022 == 0 && procs_mode & 0o022 == 0, "cgroup group/other writable");
    ensure!(procs_mode & 0o200 != 0, "cgroup.procs is not owner writable");
    ensure!(kind.trim() == "domain", "domain cgroup required");
    ensure!(controls.trim().is_empty(), "subtree_control must be empty");
    Ok(())
}

pub fn empty_procs(procs: &str) -> Result<()> {
    ensure!(procs.trim().is_empty(), "intermediate/target cgroup has direct processes");
    Ok(())
}

pub fn validate_child_membership(expected:&str, observed:&str, before:(u32,u32,u64), after:(u32,u32,u64), procs:&str, pid:u32)->Result<()> {
    ensure!(expected == observed, "child cgroup path mismatch");
    ensure!(before == after, "child identity changed");
    let expected_pid = pid.to_string();
    ensure!(procs.lines().collect::<Vec<_>>() == [expected_pid.as_str()], "target must contain only child");
    Ok(())
}

pub fn validate_target_state(procs: &str, events: &str, child: Option<u32>) -> Result<()> {
    let populated:Vec<_>=events.lines().filter_map(|v|v.strip_prefix("populated ")).collect();
    let expected=if child.is_some() { "1" } else { "0" };
    ensure!(populated == [expected], "target populated state mismatch");
    if let Some(pid)=child {
        ensure!(procs.lines().collect::<Vec<_>>() == [pid.to_string()], "target must contain only child");
    } else { empty_procs(procs)?; }
    Ok(())
}
pub fn validate_owner_presence(procs: &str, owner: u32) -> Result<()> {
    let pids:Result<Vec<u32>,_>=procs.lines().map(str::parse).collect();
    ensure!(pids?.contains(&owner), "owner not in actual unit cgroup.procs");
    Ok(())
}

#[derive(PartialEq)]
struct DirectoryIdentity {
    #[cfg(unix)] dev: u64,
    #[cfg(unix)] ino: u64,
    #[cfg(not(unix))] created: SystemTime,
}
impl DirectoryIdentity {
    fn read(path:&Path)->Result<Self> {
        let metadata=fs::symlink_metadata(path)?;
        ensure!(metadata.is_dir() && !metadata.file_type().is_symlink(), "private directory replaced");
        #[cfg(unix)] {
            use std::os::unix::fs::MetadataExt;
            Ok(Self{dev:metadata.dev(),ino:metadata.ino()})
        }
        #[cfg(not(unix))] { Ok(Self{created:metadata.created()?}) }
    }
}

#[derive(Default)]
pub struct PrivateDirectories { paths: Vec<(PathBuf, DirectoryIdentity)> }
impl PrivateDirectories {
    pub fn create(&mut self, path:&Path)->Result<()> {
        ensure!(path.is_absolute(), "private path must be absolute");
        fs::create_dir(path).map_err(|e| anyhow::anyhow!("private directory must be newly exclusive: {e}"))?;
        // Only a successfully created and identified private directory is eligible for removal.
        let identity=DirectoryIdentity::read(path)?;
        self.paths.push((path.to_owned(),identity));
        Ok(())
    }
    pub fn remove(&mut self)->Result<()> {
        while let Some((path, expected))=self.paths.last() {
            ensure!(DirectoryIdentity::read(path)? == *expected, "private directory identity changed");
            fs::remove_dir(path).map_err(|e| anyhow::anyhow!("private directory cleanup failed: {e}"))?;
            self.paths.pop();
        }
        Ok(())
    }
}
