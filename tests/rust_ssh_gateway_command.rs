//! No SSH executable is run: inspect production command construction only.
use ao_reason_workbench::core::ssh_gateway::{SshGatewayConfig, SshWorkerTransport, ssh_arguments};
use std::path::PathBuf;
use serde_json::json;
fn config()->SshGatewayConfig{
    serde_json::from_value(json!({"schema":"native-ssh-gateway/1","deployment":"command-test","worker_id":"B",
        "host":"b.example","port":2222,"user":"worker","known_hosts":"C:/known hosts",
        "identity_file":"C:/keys/id_ed25519","controller_config":"C:/controller.json","key_file":"C:/keys/worker.key","state_dir":"C:/state",
        "remote_worker":"/home/worker/aw-cli","remote_config":"/home/worker/worker.json",
        "artifact_sha256":"ab".repeat(32),"artifact_bytes":123,"timeout_secs":10})).unwrap()
}
fn bundled_helper()->PathBuf { PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tools/paramiko_gateway.py") }
fn synthetic_runtime()->(tempfile::TempDir,PathBuf) {
    let root=tempfile::tempdir().unwrap();
    let interpreter=root.path().join("runtime/python/python.exe");
    std::fs::create_dir_all(interpreter.parent().unwrap()).unwrap();
    std::fs::write(&interpreter,b"synthetic interpreter marker; never launched").unwrap();
    (root,interpreter)
}

#[cfg(not(windows))]
#[test]
fn default_paramiko_transport_fails_closed_without_a_platform_bundle(){
    let error=match SshWorkerTransport::with_paramiko(config(),bundled_helper(),b"short-lived-key".to_vec()) {
        Ok(_) => panic!("unbundled system Python must not be selected through PATH"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("bundled Paramiko runtime unavailable"),"unexpected construction failure: {error:#}");
}

#[test]
fn paramiko_command_uses_exe_relative_bundled_runtime_without_system_fallback(){
    let (_runtime,interpreter)=synthetic_runtime();
    let transport=SshWorkerTransport::with_paramiko_runtime(config(),bundled_helper(),interpreter.clone(),b"short-lived-key".to_vec()).unwrap();
    let command=transport.command_for_test().unwrap();
    let program=PathBuf::from(command.get_program());
    assert_eq!(program,interpreter,"tests inject a synthetic bundled runtime; production default is EXE-relative");
    let args:Vec<String>=command.get_args().map(|arg|arg.to_string_lossy().into_owned()).collect();
    assert!(args.contains(&"-I".into()));
    assert!(args.contains(&"-B".into()),"bundled runtime must not emit bytecode");
    assert!(args.contains(&bundled_helper().display().to_string()),"helper must be EXE-relative/explicit absolute");
    let envs: Vec<(String, String)> = command.get_envs()
        .map(|(key, value)| (key.to_string_lossy().into_owned(), value.unwrap_or_default().to_string_lossy().into_owned()))
        .collect();
    #[cfg(windows)] {
        assert_eq!(envs.len(), 1, "bundled runtime may retain only the required Windows OS variable");
        assert_eq!(envs[0].0, "SystemRoot");
        assert_eq!(envs[0].1, std::env::var("SystemRoot").expect("test host must provide SystemRoot"));
    }
    #[cfg(not(windows))]
    assert!(envs.is_empty(), "non-Windows transport environment remains cleared");
    assert!(!args.iter().any(|arg|arg.contains("ssh.exe")));
}

#[test]
fn relative_helper_is_rejected_before_transport_construction(){
    let (_runtime,interpreter)=synthetic_runtime();
    let result=SshWorkerTransport::with_paramiko_runtime(config(),PathBuf::from("tools/paramiko_gateway.py"),interpreter,b"short-lived-key".to_vec());
    let error=match result { Ok(_) => panic!("relative helper must be rejected"), Err(error) => error };
    assert!(error.to_string().contains("absolute"),"helper must not depend on cwd/PATH: {error:#}");
}
#[test]
fn paramiko_transport_rejects_missing_memory_key(){
    let helper=bundled_helper();
    assert!(helper.is_file());
    let (_runtime,interpreter)=synthetic_runtime();
    let error=match SshWorkerTransport::with_paramiko_runtime(config(),helper,interpreter,Vec::new()) {
        Ok(_) => panic!("missing private key must be rejected"),
        Err(error) => error,
    };
    assert!(error.to_string().contains("private key"),"missing-key guard must be reached: {error:#}");
}

#[test]
fn paramiko_transport_command_uses_python_helper_not_system_ssh(){
    let (_runtime,interpreter)=synthetic_runtime();
    let transport=SshWorkerTransport::with_paramiko_runtime(config(),bundled_helper(),interpreter,b"short-lived-key".to_vec()).unwrap();
    let command=transport.command_for_test().unwrap();
    let program=command.get_program().to_string_lossy();
    assert!(program.ends_with("python.exe")||program=="python"||program=="python3", "unexpected helper interpreter: {program}");
    let args:Vec<String>=command.get_args().map(|arg|arg.to_string_lossy().into_owned()).collect();
    assert!(args.windows(2).any(|pair|pair==["-I","-B"]), "helper must run isolated without bytecode: {args:?}");
    assert!(args.iter().any(|arg|arg.ends_with("tools/paramiko_gateway.py")), "helper must be EXE-relative: {args:?}");
    assert!(!args.iter().any(|arg|arg.contains("ssh.exe")));
    assert!(!args.iter().any(|arg|arg.contains("short-lived-key")));
    let frame=transport.frame_for_test(b"{\"signed\":true}\n").unwrap();
    let offset=b"AO-PARAMIKO-GATEWAY/1\n".len();
    let length=u32::from_be_bytes(frame[offset..offset+4].try_into().unwrap()) as usize;
    let metadata:serde_json::Value=serde_json::from_slice(&frame[offset+4..offset+4+length]).unwrap();
    assert_eq!(metadata.as_object().unwrap().len(),8);
    assert_eq!(metadata["timeout_secs"],10);
    assert!(metadata.get("setup_timeout_secs").is_none());
    assert!(metadata.get("channel_timeout_secs").is_none());
    assert!(frame.windows(b"short-lived-key".len()).any(|window|window==b"short-lived-key"), "key must be carried only in stdin frame");
}

#[test]
fn strict_host_and_identity_options_are_not_optional(){
    let args=ssh_arguments(&config()).unwrap();
    for expected in ["BatchMode=yes","StrictHostKeyChecking=yes","IdentitiesOnly=yes","UserKnownHostsFile=C:/known hosts","GlobalKnownHostsFile=none","PasswordAuthentication=no","KbdInteractiveAuthentication=no"] {
        assert!(args.iter().any(|arg|arg==expected),"missing {expected}: {args:?}");
    }
    assert!(args.windows(2).any(|pair|pair==["-i","C:/keys/id_ed25519"]));
    assert!(args.windows(2).any(|pair|pair==["-p","2222"]));
    assert!(args.windows(2).any(|pair|pair==["-F","none"]), "untrusted user SSH config must be disabled");
    assert!(args.last().unwrap().contains("sha256sum"));
    assert!(args.last().unwrap().contains("wc -c"));
    assert!(args.last().unwrap().contains("exec /home/worker/aw-cli owner-exec --config /home/worker/worker.json"));
}
#[test]
fn remote_shell_metacharacters_and_root_are_rejected(){
    for field in ["host","user","remote_worker","remote_config"]{
        let mut value=serde_json::to_value(config()).unwrap();value[field]=json!("/tmp/worker; touch /tmp/bad");
        let config:SshGatewayConfig=serde_json::from_value(value).unwrap();assert!(config.validate().is_err(),"{field}");
    }
    let mut config=config();config.user="root".into();assert!(config.validate().is_err());
}

#[test]
fn explicit_loader_path_is_applied_before_direct_worker_exec(){
    let mut value=serde_json::to_value(config()).unwrap();
    value["remote_library_path"]=json!("/home/worker/lib");
    value["remote_library_sha256"]=json!("cd".repeat(32));
    value["remote_library_bytes"]=json!(1557000);
    let config:SshGatewayConfig=serde_json::from_value(value).unwrap();
    let command=ssh_arguments(&config).unwrap().pop().unwrap();
    let expected=format!("test \"$(sha256sum /home/worker/aw-cli | cut -d' ' -f1)\" = \"{}\" && test \"$(wc -c < /home/worker/aw-cli)\" -eq 123 && test -d /home/worker/lib && test -r /home/worker/lib/libsqlite3.so.0 && test \"$(sha256sum /home/worker/lib/libsqlite3.so.0 | cut -d' ' -f1)\" = \"{}\" && test \"$(wc -c < /home/worker/lib/libsqlite3.so.0)\" -eq 1557000 && LD_LIBRARY_PATH=/home/worker/lib exec /home/worker/aw-cli owner-exec --config /home/worker/worker.json", "ab".repeat(32), "cd".repeat(32));
    assert_eq!(command,expected,"checks must precede the scoped environment and direct ELF exec");
}

#[test]
fn loader_identity_is_required_as_a_complete_pair(){
    let cases = [
        (None, Some(1557000_u64)),
        (Some("cd".repeat(32)), None),
    ];
    for (sha, bytes) in cases {
        let mut value = serde_json::to_value(config()).unwrap();
        value["remote_library_path"] = json!("/home/worker/lib");
        if let Some(sha) = sha { value["remote_library_sha256"] = json!(sha); }
        if let Some(bytes) = bytes { value["remote_library_bytes"] = json!(bytes); }
        let config: SshGatewayConfig = serde_json::from_value(value).unwrap();
        assert!(ssh_arguments(&config).is_err(), "partial loader identity must be rejected");
    }
}

#[test]
fn loader_identity_rejects_invalid_sha_and_zero_bytes(){
    let cases = [
        ("".to_owned(), 1557000_u64),
        ("CD".repeat(32), 1557000_u64),
        ("cd".repeat(32), 0_u64),
    ];
    for (sha, bytes) in cases {
        let mut value = serde_json::to_value(config()).unwrap();
        value["remote_library_path"] = json!("/home/worker/lib");
        value["remote_library_sha256"] = json!(sha);
        value["remote_library_bytes"] = json!(bytes);
        let config: SshGatewayConfig = serde_json::from_value(value).unwrap();
        assert!(ssh_arguments(&config).is_err(), "invalid loader identity accepted");
    }
}

#[test]
fn omitted_loader_path_preserves_the_existing_command_exactly(){
    let command=ssh_arguments(&config()).unwrap().pop().unwrap();
    let expected=format!("test \"$(sha256sum /home/worker/aw-cli | cut -d' ' -f1)\" = \"{}\" && test \"$(wc -c < /home/worker/aw-cli)\" -eq 123 && exec /home/worker/aw-cli owner-exec --config /home/worker/worker.json", "ab".repeat(32));
    assert_eq!(command,expected);
}

#[test]
fn launch_mode_defaults_to_direct_and_preserves_existing_remote_command(){
    let default_command=ssh_arguments(&config()).unwrap().pop().unwrap();
    let mut value=serde_json::to_value(config()).unwrap();
    value["launch_mode"]=json!("direct");
    let explicit:SshGatewayConfig=serde_json::from_value(value).unwrap();
    let command=ssh_arguments(&explicit).unwrap().pop().unwrap();
    assert_eq!(command,default_command);
    assert!(!command.contains("systemd-run"));
}

#[test]
fn delegated_user_service_mode_wraps_only_the_pinned_worker_exec(){
    let mut value=serde_json::to_value(config()).unwrap();
    value["launch_mode"]=json!("delegated-user-service");
    value["expected_worker_timeout_secs"]=json!(10);
    value["timeout_secs"]=json!(40);
    let gateway:SshGatewayConfig=serde_json::from_value(value).unwrap();
    let command=ssh_arguments(&gateway).unwrap().pop().unwrap();
    let expected=format!("test \"$(sha256sum /home/worker/aw-cli | cut -d' ' -f1)\" = \"{}\" && test \"$(wc -c < /home/worker/aw-cli)\" -eq 123 && exec /usr/bin/systemd-run --user --pipe --wait --collect --quiet --property=Delegate=yes --property=RuntimeMaxSec=15s --property=TimeoutStopSec=5s --setenv=AW_EXPECTED_OWNER_TIMEOUT_SECS=10 -- /home/worker/aw-cli owner-exec --config /home/worker/worker.json", "ab".repeat(32));
    assert_eq!(command, expected);
}

#[test]
fn delegated_user_service_mode_preserves_pinned_library_checks(){
    let mut value=serde_json::to_value(config()).unwrap();
    value["launch_mode"]=json!("delegated-user-service");
    value["expected_worker_timeout_secs"]=json!(10);
    value["timeout_secs"]=json!(40);
    value["remote_library_path"]=json!("/home/worker/lib");
    value["remote_library_sha256"]=json!("cd".repeat(32));
    value["remote_library_bytes"]=json!(1557000);
    let gateway:SshGatewayConfig=serde_json::from_value(value).unwrap();
    let command=ssh_arguments(&gateway).unwrap().pop().unwrap();
    assert!(command.contains("sha256sum /home/worker/aw-cli"));
    assert!(command.contains("sha256sum /home/worker/lib/libsqlite3.so.0"));
    assert!(command.contains("wc -c < /home/worker/lib/libsqlite3.so.0"));
    assert!(command.ends_with("exec /usr/bin/systemd-run --user --pipe --wait --collect --quiet --property=Delegate=yes --property=RuntimeMaxSec=15s --property=TimeoutStopSec=5s --setenv=AW_EXPECTED_OWNER_TIMEOUT_SECS=10 --setenv=LD_LIBRARY_PATH=/home/worker/lib -- /home/worker/aw-cli owner-exec --config /home/worker/worker.json"));
    assert!(!command.contains("LD_LIBRARY_PATH=/home/worker/lib exec"), "loader environment must be passed into the service, not only systemd-run");
}

#[test]
fn unknown_launch_mode_is_rejected_before_command_construction(){
    let mut value=serde_json::to_value(config()).unwrap();
    value["launch_mode"]=json!("systemd-scope");
    let error=serde_json::from_value::<SshGatewayConfig>(value).unwrap_err();
    assert!(error.to_string().contains("systemd-scope"));
}

#[test]
fn delegated_mode_paramiko_frame_preserves_signed_stdin_and_key_separation(){
    let mut value=serde_json::to_value(config()).unwrap();
    value["launch_mode"]=json!("delegated-user-service");
    value["expected_worker_timeout_secs"]=json!(10);
    value["timeout_secs"]=json!(40);
    value["remote_library_path"]=json!("/home/worker/lib");
    value["remote_library_sha256"]=json!("cd".repeat(32));
    value["remote_library_bytes"]=json!(1557000);
    let gateway:SshGatewayConfig=serde_json::from_value(value).unwrap();
    let (_runtime,interpreter)=synthetic_runtime();
    let transport=SshWorkerTransport::with_paramiko_runtime(gateway,bundled_helper(),interpreter,b"short-lived-key".to_vec()).unwrap();
    let request=b"{\"signed\":true}\n";
    let frame=transport.frame_for_test(request).unwrap();
    let magic=b"AO-PARAMIKO-GATEWAY/1\n";
    assert!(frame.starts_with(magic));
    let offset=magic.len();
    let length=u32::from_be_bytes(frame[offset..offset+4].try_into().unwrap()) as usize;
    let metadata:serde_json::Value=serde_json::from_slice(&frame[offset+4..offset+4+length]).unwrap();
    assert_eq!(metadata["timeout_secs"],35);
    assert_eq!(metadata["setup_timeout_secs"],15);
    assert_eq!(metadata["channel_timeout_secs"],20);
    assert_eq!(metadata["request"],"{\"signed\":true}\n");
    assert_eq!(metadata["host"],"b.example");
    assert_eq!(metadata["user"],"worker");
    assert_eq!(metadata["known_hosts"],"C:/known hosts");
    let remote_command=metadata["remote_command"].as_str().unwrap();
    assert!(remote_command.contains("exec /usr/bin/systemd-run --user --pipe --wait --collect --quiet"));
    assert!(remote_command.contains("--setenv=LD_LIBRARY_PATH=/home/worker/lib -- /home/worker/aw-cli"));
    assert!(!remote_command.contains("LD_LIBRARY_PATH=/home/worker/lib exec"));
    assert!(!remote_command.contains("cgroup"), "command seam must not guess or rewrite the explicit worker cgroup_root; topology remains NOT_VERIFIED");
    assert!(!serde_json::to_string(&metadata).unwrap().contains("short-lived-key"));
    let key_offset=offset+4+length;
    let key_length=u32::from_be_bytes(frame[key_offset..key_offset+4].try_into().unwrap()) as usize;
    assert_eq!(key_length,15);
    assert_eq!(&frame[key_offset+4..],b"short-lived-key");
}

#[test]
fn delegated_mode_timeout_bounds_follow_validated_gateway_timeout(){
    for (worker, outer, valid) in [(1,31,true),(270,300,true),(271,300,false),(10,39,false)] {
        let mut value=serde_json::to_value(config()).unwrap();
        value["launch_mode"]=json!("delegated-user-service");
        value["expected_worker_timeout_secs"]=json!(worker);
        value["timeout_secs"]=json!(outer);
        let gateway:Result<SshGatewayConfig,_>=serde_json::from_value(value);
        assert!(gateway.is_ok(), "supported deadline config must deserialize");
        let result=ssh_arguments(&gateway.unwrap());
        assert_eq!(result.is_ok(),valid,"W={worker} T={outer}");
        if let Ok(args)=result {
            let command=args.last().unwrap();
            assert!(command.contains(&format!("--property=RuntimeMaxSec={}s --property=TimeoutStopSec=5s",worker+5)));
            assert!(!command.contains("||"));
        }
    }
    let mut value=serde_json::to_value(config()).unwrap();
    value["launch_mode"]=json!("delegated-user-service");
    assert!(ssh_arguments(&serde_json::from_value(value).unwrap()).is_err(),"delegated W is required");
    let mut value=serde_json::to_value(config()).unwrap();
    value["expected_worker_timeout_secs"]=json!(1);
    let direct:Result<SshGatewayConfig,_>=serde_json::from_value(value);
    assert!(direct.is_ok(),"new field must deserialize before validation");
    assert!(ssh_arguments(&direct.unwrap()).is_err(),"direct rejects W");
}

#[test]
fn loader_path_rejects_search_lists_relative_paths_and_shell_expansion(){
    for path in ["", "/", ".", "relative/lib", "/home/worker/../lib", "/home/worker/./lib",
        "/home/worker/lib/", "/home//worker/lib", "//home/worker/lib", "/tmp/lib:/usr/lib",
        "/tmp/lib;id", "/tmp/$(id)", "/tmp/`id`", "/tmp/${HOME}", "/tmp/lib\nexec id",
        "/tmp/lib with space", "/tmp/lib'", "/tmp/lib\\x", "/tmp/lib=bad"] {
        let mut value=serde_json::to_value(config()).unwrap();
        value["remote_library_path"]=json!(path);
        let config:SshGatewayConfig=serde_json::from_value(value).unwrap();
        assert!(ssh_arguments(&config).is_err(),"unsafe loader path accepted: {path:?}");
    }
}

#[test]
fn loader_path_does_not_weaken_pinned_host_or_artifact_validation(){
    let mut value=serde_json::to_value(config()).unwrap();
    value["remote_library_path"]=json!("/home/worker/lib");
    for (field,invalid) in [("user",json!("root")),("artifact_sha256",json!("")),("artifact_bytes",json!(0))] {
        let mut invalid_config=value.clone();invalid_config[field]=invalid;
        let config:SshGatewayConfig=serde_json::from_value(invalid_config).unwrap();
        assert!(ssh_arguments(&config).is_err(),"{field}");
    }
}
