//! Platform-neutral parser/policy tests, not Linux runtime evidence.
use agentranch::adapters::owner_exec::policy::*;

#[test]
fn unified_path_is_unique_absolute_and_normalized() {
    assert_eq!(unified_path("0::/user.slice/run-abc.service\n").unwrap(), "/user.slice/run-abc.service");
    for raw in ["1:cpu:/foo\n", "0::/a\n0::/b\n", "0::/a/../b\n", "0::/a//b\n", "0::/a/./b\n", "0::/a/\n", "0::relative\n", "0::/a\\b\n"] {
        assert!(unified_path(raw).is_err(), "{raw:?}");
    }
}

#[test]
fn policy_is_a_strict_component_ancestor_in_fixed_mount() {
    assert_eq!(unit_root("/sys/fs/cgroup/user.slice", "/user.slice/run-a.service").unwrap(), "/sys/fs/cgroup/user.slice/run-a.service");
    for policy in ["/sys/fs/cgroup/user", "/sys/fs/cgroup/user.slice/run-a.service", "/other/user.slice", "/sys/fs/cgroup/../cgroup", "/sys/fs/cgroup//user.slice"] {
        assert!(unit_root(policy, "/user.slice/run-a.service").is_err(), "{policy}");
    }
}

fn properties() -> String {
    "Transient=yes\nDelegate=yes\nMainPID=123\nControlGroup=/user.slice/run-a.service\nInvocationID=0123456789abcdef0123456789abcdef\n".into()
}
#[test]
fn unit_properties_bind_transient_owner_root_and_invocation() {
    let path="/user.slice/run-a.service";
    let invocation="0123456789abcdef0123456789abcdef";
    validate_unit_properties(&properties(), 123, path, invocation).unwrap();
    for (old,new) in [("Transient=yes","Transient=no"),("Delegate=yes","Delegate=no"),("MainPID=123","MainPID=124"),("ControlGroup=/user.slice/run-a.service","ControlGroup=/user.slice"),(invocation,"ffffffffffffffffffffffffffffffff")] {
        assert!(validate_unit_properties(&properties().replace(old,new),123,path,invocation).is_err(), "{old}");
    }
    assert!(validate_unit_properties("Transient=yes\n",123,path,invocation).is_err());
    assert!(validate_unit_properties(&(properties()+"MainPID=123\n"),123,path,invocation).is_err());
    assert!(validate_unit_properties(&properties(),123,path,"").is_err());
    assert!(unit_root("/sys", path).is_err(), "policy itself must stay in fixed cgroup2 mount");
    assert!(unit_root("/other", "/other/u.service").is_err());
}
#[test]
fn ordinary_domain_root_rejects_nonempty_controls_and_unsafe_ownership() {
    validate_root_facts(1000,1000,1000,1000,1000,1000,0o755,0o644,"domain\n","\n").unwrap();
    for (uid,euid,gid,egid,dir,procs,dm,pm,kind,controls) in [
        (0,0,1000,1000,0,0,0o755,0o644,"domain",""),
        (1000,0,1000,1000,1000,1000,0o755,0o644,"domain",""),
        (1000,1000,1000,0,1000,1000,0o755,0o644,"domain",""),
        (1000,1000,1000,1000,0,1000,0o755,0o644,"domain",""),
        (1000,1000,1000,1000,1000,0,0o755,0o644,"domain",""),
        (1000,1000,1000,1000,1000,1000,0o775,0o644,"domain",""),
        (1000,1000,1000,1000,1000,1000,0o755,0o666,"domain",""),
        (1000,1000,1000,1000,1000,1000,0o755,0o644,"threaded",""),
        (1000,1000,1000,1000,1000,1000,0o755,0o644,"domain","cpu")
    ] { assert!(validate_root_facts(uid,euid,gid,egid,dir,procs,dm,pm,kind,controls).is_err()); }
}
#[test]
fn child_birth_and_migration_require_exact_paths_and_single_identity() {
    let identity=(123,234,567_u64);
    validate_child_membership("/unit.service", "/unit.service", identity, identity, "234\n", 234).unwrap();
    assert!(validate_child_membership("/unit.service", "/sibling.service", identity, identity, "234\n",234).is_err());
    assert!(validate_child_membership("/unit.service", "/unit.service", identity,(123,234,568),"234\n",234).is_err());
    assert!(validate_child_membership("/unit.service", "/unit.service", identity,identity,"234\n235\n",234).is_err());
    assert!(empty_procs("123\n").is_err());
    empty_procs("\n").unwrap();
    // Ancestors may be populated through descendants but must have no direct procs.
    validate_target_state("", "populated 0\nfrozen 0\n", None).unwrap();
    validate_target_state("234\n", "populated 1\n", Some(234)).unwrap();
    for (procs,events,pid) in [("234\n","populated 0\n",Some(234)),("","populated 1\n",None),("234\n235\n","populated 1\n",Some(234)),("","populated 2\n",None)] {
        assert!(validate_target_state(procs,events,pid).is_err());
    }
    validate_owner_presence("123\n456\n",123).unwrap();
    assert!(validate_owner_presence("456\n",123).is_err());
}
#[test]
fn private_directories_are_exclusive_and_shared_workers_survive() {
    let temp=tempfile::tempdir().unwrap();
    let shared=temp.path().join("workers");
    std::fs::create_dir(&shared).unwrap();
    let instance=shared.join("instance");
    let target=instance.join("handle");
    let mut private=PrivateDirectories::default();
    private.create(&instance).unwrap();
    private.create(&target).unwrap();
    assert!(PrivateDirectories::default().create(&instance).is_err());
    private.remove().unwrap();
    assert!(shared.is_dir());
    assert!(!instance.exists());
    let mut private=PrivateDirectories::default();
    private.create(&instance).unwrap();
    std::fs::write(instance.join("residual"),"x").unwrap();
    assert!(private.remove().is_err());
    assert!(instance.is_dir());
    std::fs::remove_file(instance.join("residual")).unwrap();
    private.remove().unwrap();
}

#[test]
fn replaced_private_identity_must_not_be_removed() {
    let temp=tempfile::tempdir().unwrap();
    let path=temp.path().join("instance");
    let mut private=PrivateDirectories::default();
    private.create(&path).unwrap();
    std::fs::rename(&path,temp.path().join("original")).unwrap();
    std::thread::sleep(std::time::Duration::from_millis(20));
    std::fs::create_dir(&path).unwrap();
    assert!(private.remove().is_err(), "replacement must not inherit cleanup authority");
    assert!(path.is_dir());
}


#[test]
fn birth_mismatch_message_names_both_observed_operands() {
    let message = birth_mismatch("/user.slice/run-a.service", Some("/user.slice/run-a.service"), None);
    assert!(message.contains("birth cgroup mismatch"), "{message}");
    assert!(message.contains("self="), "owner-side observed value must be named: {message}");
    assert!(message.contains("child=read-error"), "failed child read must be explicit: {message}");
    let both = birth_mismatch("/user.slice/run-a.service", Some("/root"), Some("/user.slice/run-b.service"));
    assert!(both.contains("self=/root") && both.contains("child=/user.slice/run-b.service"), "{both}");
    let read_fail = birth_mismatch("/user.slice/run-a.service", None, Some("/user.slice/run-a.service"));
    assert!(read_fail.contains("self=read-error"), "{read_fail}");
}

#[test]
fn strip_mount_preserves_the_leading_separator_for_absolute_comparison() {
    // /proc/<pid>/cgroup unified entries are absolute ("/user.slice/...").
    // A mount-relative expected value without the leading '/' can never equal
    // them, so the birth/membership checks would fail deterministically.
    assert_eq!(strip_mount("/sys/fs/cgroup/user.slice/run-a.service").unwrap(), "/user.slice/run-a.service");
    assert!(strip_mount("/sys/fs/cgroup").is_err(), "the bare mount itself is not a unit root");
    assert!(strip_mount("/other/user.slice/x").is_err());
    assert!(strip_mount("/sys/fs/cgroup/../cgroup/x").is_err());
}

#[test]
fn attach_line_issues_exactly_one_write_call() {
    // cgroup v2 kernfs processes each write(2) as one complete operation; a
    // formatted writeln! reaches it as two writes (digits, then a bare "\n")
    // and the second one fails with EINVAL while the first already attached
    // the pid. Verified live on the target: newline-probe-resume5.txt.
    #[derive(Default)]
    struct Recorder { calls: std::sync::Mutex<Vec<Vec<u8>>> }
    impl std::io::Write for Recorder {
        fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
            self.calls.lock().unwrap().push(buf.to_vec());
            Ok(buf.len())
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut recorder = Recorder::default();
    attach_line(&mut recorder, 866869).unwrap();
    let calls = recorder.calls.lock().unwrap();
    assert_eq!(calls.len(), 1, "kernfs cgroup writes must be a single write(2): {calls:?}");
    assert_eq!(calls[0], b"866869\n".to_vec());
}

#[test]
fn attach_line_propagates_the_underlying_write_error() {
    struct Failer(std::sync::Mutex<u32>);
    impl std::io::Write for Failer {
        fn write(&mut self, _buf: &[u8]) -> std::io::Result<usize> {
            *self.0.lock().unwrap() += 1;
            Err(std::io::Error::from_raw_os_error(22))
        }
        fn flush(&mut self) -> std::io::Result<()> { Ok(()) }
    }
    let mut failer = Failer(std::sync::Mutex::new(0));
    let error = attach_line(&mut failer, 42).unwrap_err();
    assert_eq!(*failer.0.lock().unwrap(), 1);
    assert!(error.to_string().contains("os error 22"), "{error}");
}
