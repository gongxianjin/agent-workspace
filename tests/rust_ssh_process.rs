//! Local real subprocess I/O only. Never starts SSH or a Linux owner.
use agentranch::core::ssh_gateway::exchange_process;
use std::{io::{Read, Write}, process::Command, time::{Duration, Instant}};

#[test]
fn inherited_pipe_fixture() {
    if std::env::var("AW_GATEWAY_INHERITED_PIPE").is_ok() {std::thread::sleep(Duration::from_secs(10));}
}
#[test]
fn ipc_child_fixture() {
    let Ok(mode)=std::env::var("AW_GATEWAY_IPC_FIXTURE") else{return};
    if mode=="no-read" {
        std::thread::sleep(Duration::from_secs(10));
        std::process::exit(0);
    }
    let mut request=Vec::new();std::io::stdin().read_to_end(&mut request).unwrap();
    assert_eq!(request,b"signed-start\n");
    if mode=="inherited" {
        Command::new(std::env::current_exe().unwrap()).args(["--exact","inherited_pipe_fixture","--nocapture"])
            .env("AW_GATEWAY_INHERITED_PIPE","1").spawn().unwrap();
    } else if mode=="flood" {
        std::io::stdout().write_all(&vec![b'o';131072]).unwrap();
        std::io::stderr().write_all(&vec![b'e';131072]).unwrap();
    } else if mode=="exact" {
        std::io::stdout().write_all(b"ok\n").unwrap();
        std::io::stderr().write_all(&vec![b'e';65536]).unwrap();
    } else {
        std::io::stdout().write_all(b"launch-prefix\n").unwrap();std::io::stdout().flush().unwrap();
        std::io::stderr().write_all(b"diagnostic\n").unwrap();std::io::stderr().flush().unwrap();
        std::thread::sleep(Duration::from_secs(10));
    }
    std::process::exit(0);
}
fn command(mode:&str)->Command {
    let mut command=Command::new(std::env::current_exe().unwrap());
    command.args(["--exact","ipc_child_fixture","--nocapture"]).env("AW_GATEWAY_IPC_FIXTURE",mode);command
}
#[test]
fn continuously_drains_both_pipes_without_wait_before_read_deadlock() {
    let mut command=command("flood");
    let output=exchange_process(&mut command,b"signed-start\n",Duration::from_secs(3)).unwrap();
    assert_eq!(output.exit_code,Some(0),"{:?}",output.failure);
    assert!(output.stdout.windows(100).any(|window|window==[b'o';100]));
    assert!(output.stderr.windows(100).any(|window|window==[b'e';100]));
    assert!(output.failure.is_some(),"bounded stderr capture must report overflow");
    assert!(output.stdout.len()<=1024*1024 && output.stderr.len()<=64*1024);
}
#[test]
fn timeout_returns_real_prefixes_and_waits_for_local_process() {
    let started=Instant::now();let mut command=command("timeout");
    let output=exchange_process(&mut command,b"signed-start\n",Duration::from_millis(400)).unwrap();
    assert!(started.elapsed()<Duration::from_secs(3));
    assert!(String::from_utf8_lossy(&output.stdout).contains("launch-prefix"));
    assert!(String::from_utf8_lossy(&output.stderr).contains("diagnostic"));
    assert!(output.failure.unwrap().contains("timeout"));
    assert!(output.exit_code.is_some(),"killed subprocess must be directly waited");
}
#[test]
fn exact_stderr_limit_is_not_an_overflow() {
    let output=exchange_process(&mut command("exact"),b"signed-start\n",Duration::from_secs(2)).unwrap();
    assert_eq!(output.stderr.len(),65536);
    assert_eq!(output.failure,None,"exact capacity must be accepted");
}
#[test]
fn inherited_pipe_eof_is_bounded_by_the_same_deadline() {
    let started=Instant::now();
    let output=exchange_process(&mut command("inherited"),b"signed-start\n",Duration::from_millis(300)).unwrap();
    assert!(started.elapsed()<Duration::from_secs(2),"inherited pipe exceeded deadline");
    assert_eq!(output.exit_code,Some(0));
    assert!(output.failure.unwrap().contains("timeout"));
}
#[test]
fn blocked_stdin_is_bounded_by_the_same_deadline() {
    let started=Instant::now();
    let output=exchange_process(&mut command("no-read"),&vec![b'x';1024*1024],Duration::from_millis(300));
    assert!(started.elapsed()<Duration::from_secs(2),"stdin write escaped deadline");
    assert!(output.unwrap().failure.unwrap().contains("timeout"));
}
