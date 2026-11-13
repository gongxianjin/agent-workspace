//! Loopback-only artifact tests. Synthetic AO reports are NOT exit evidence.
use ao_reason_workbench::{
    adapters::{ao_http::AoHttp, AoPort, Observation, WorkRequest},
    config::AoConfig,
    core::project_work::{ConfirmedDocument, ProjectStage, ProjectWork, WorkTask},
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{io::{Read, Write}, net::TcpListener, path::Path, thread};

fn work() -> ProjectWork {
    let text = "# Confirmed\nInventory is never negative.\n".to_owned();
    ProjectWork {
        schema: "project-work/1".into(), project_id: "project-1".into(), action_id: "develop-1".into(),
        stage: ProjectStage::Develop,
        document: ConfirmedDocument { version: 1, sha256: hex::encode(Sha256::digest(text.as_bytes())), text },
        tasks: vec![WorkTask {
            id: "inventory".into(), title: "Inventory".into(), description: "Deduct atomically".into(),
            acceptance: "Inventory remains nonnegative".into(), depends_on: vec![], review: "approved".into(), reason: None,
        }],
        target_task: None, repair_reason: None, repair_round: 0, test_spec: None, artifact: None,
    }
}

fn declaration(path: &str, bytes: &[u8]) -> Value {
    json!({"schema":"project-artifact/1", "files":[{"path":path,
        "size":bytes.len(), "sha256":hex::encode(Sha256::digest(bytes))}]})
}

fn fixture_result(work: &ProjectWork, artifact: Value) -> Value {
    json!({"schema":"project-result/1", "project_id":work.project_id,
        "action_id":work.action_id, "stage":work.stage,
        "document_version":work.document.version, "document_sha256":work.document.sha256,
        "artifact":artifact})
}

fn serve_workspace(listener: TcpListener, workspace: std::path::PathBuf) {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    for _ in 0..3 {
        let mut stream = loop {
            match listener.accept() {
                Ok((stream, _)) => break stream,
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    assert!(std::time::Instant::now() < deadline, "AO fixture request deadline");
                    thread::sleep(std::time::Duration::from_millis(5));
                }
                Err(error) => panic!("AO fixture accept failed: {error}"),
            }
        };
        stream.set_nonblocking(false).unwrap();
        stream.set_read_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        stream.set_write_timeout(Some(std::time::Duration::from_secs(3))).unwrap();
        let mut request = Vec::new();
        loop {
            let mut byte = [0u8; 1];
            stream.read_exact(&mut byte).unwrap();
            request.push(byte[0]);
            assert!(request.len() < 8192, "fixture request header bound");
            if request.ends_with(b"\r\n\r\n") { break; }
        }
        let header = String::from_utf8(request).unwrap();
        let path = header.lines().next().unwrap().split_whitespace().nth(1).unwrap();
        let body = if path.starts_with("/api/v1/sessions/worker-1/conversation") {
            json!({"sessionId":"worker-1","activities":[]})
        } else if path.starts_with("/api/v1/reports?") {
            json!({"reports":[{"id":"report-1","sessionId":"worker-1","state":"done"}]})
        } else if path == "/api/v1/desktop/sessions/worker-1/workspace" {
            json!({"sessionId":"worker-1","workspacePath":workspace})
        } else { panic!("unexpected AO fixture path: {path}"); };
        let body = body.to_string();
        write!(stream, "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}", body.len(), body).unwrap();
    }
}

fn observe(root: &Path, workspace: &Path, artifact: Value) -> anyhow::Result<Observation> {
    let work = work();
    std::fs::write(workspace.join("project-result.json"),
        serde_json::to_vec(&fixture_result(&work, artifact)).unwrap()).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let workspace_for_server = workspace.to_owned();
    let server = thread::spawn(move || serve_workspace(listener, workspace_for_server));
    let config = AoConfig { base_url:format!("http://{address}"), project_id:"fixture".into(), model:"fixture-model".into() };
    let mut ao = AoHttp::new(config).unwrap().with_workspace_root(root).unwrap();
    let request = WorkRequest { run_id:"run-1".into(), attempt:1, epoch:"epoch-1".into(),
        template:"project-work-v1".into(), session:"worker-1".into(), project_work:Some(work) };
    ao.bind("worker-1", &request).unwrap();
    let result = ao.observe("worker-1");
    server.join().unwrap();
    result
}

fn fixture() -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let temp = tempfile::tempdir().unwrap();
    let root = temp.path().join("owned-workspaces");
    let workspace = root.join("worker-1");
    std::fs::create_dir_all(&workspace).unwrap();
    (temp, root, workspace)
}

#[test]
fn develop_observation_rejects_manifest_without_actual_managed_bytes() {
    let (_temp, root, workspace) = fixture();
    let artifact = json!({"schema":"project-artifact/1",
        "files":[{"path":"inventory.json", "size":13, "sha256":"aa".repeat(32)}]});
    assert!(observe(&root, &workspace, artifact).is_err(),
        "manifest-only report must not count as delivered artifact bytes");
}

#[test]
fn develop_observation_collects_exact_managed_bytes_but_not_exit() {
    let (_temp, root, workspace) = fixture();
    let bytes = br#"{"items":[]}"#;
    std::fs::write(workspace.join("inventory.json"), bytes).unwrap();
    let observation = observe(&root, &workspace, declaration("inventory.json", bytes)).unwrap();
    let Observation::Reported { report, verification } = observation else {
        panic!("managed bytes must remain Reported without independent exit");
    };
    assert_eq!(report["artifact"]["schema"], "project-artifact-package/1");
    assert_eq!(report["artifact"]["project_id"], "project-1");
    assert_eq!(report["artifact"]["action_id"], "develop-1");
    assert_eq!(report["artifact"]["files"][0]["content_hex"], hex::encode(bytes));
    assert_eq!(verification["artifact_bytes_verified"], true);
    assert!(verification["passed"].is_null());
    assert_eq!(verification["state"], "report_pending_exit");
}

#[test]
fn develop_observation_rejects_directory_artifact() {
    let (_temp, root, workspace) = fixture();
    std::fs::create_dir(workspace.join("inventory.json")).unwrap();
    assert!(observe(&root, &workspace, declaration("inventory.json", b"")).is_err());
}

#[test]
fn develop_observation_rejects_incorrect_size() {
    let (_temp, root, workspace) = fixture();
    std::fs::write(workspace.join("inventory.json"), b"actual").unwrap();
    let mut artifact = declaration("inventory.json", b"actual");
    artifact["files"][0]["size"] = json!(1);
    assert!(observe(&root, &workspace, artifact).is_err());
}

#[test]
fn develop_observation_rejects_incorrect_sha256() {
    let (_temp, root, workspace) = fixture();
    std::fs::write(workspace.join("inventory.json"), b"actual").unwrap();
    let mut artifact = declaration("inventory.json", b"actual");
    artifact["files"][0]["sha256"] = json!("aa".repeat(32));
    assert!(observe(&root, &workspace, artifact).is_err());
}

#[test]
fn develop_observation_rejects_parent_traversal() {
    let (_temp, root, workspace) = fixture();
    std::fs::write(root.join("outside.json"), b"outside").unwrap();
    assert!(observe(&root, &workspace, declaration("../outside.json", b"outside")).is_err());
}

#[test]
fn develop_observation_rejects_missing_declaration() {
    let (_temp, root, workspace) = fixture();
    assert!(observe(&root, &workspace, Value::Null).is_err());
}

#[test]
fn develop_observation_rejects_link_when_creation_is_supported() {
    let (_temp, root, workspace) = fixture();
    let target = root.join("outside.json");
    std::fs::write(&target, b"outside").unwrap();
    #[cfg(windows)]
    let created = std::os::windows::fs::symlink_file(&target, workspace.join("inventory.json"));
    #[cfg(unix)]
    let created = std::os::unix::fs::symlink(&target, workspace.join("inventory.json"));
    if let Err(error) = created {
        if error.kind() == std::io::ErrorKind::PermissionDenied || error.raw_os_error() == Some(1314) {
            eprintln!("UNVERIFIED: local symlink creation privilege unavailable: {error}");
            return;
        }
        panic!("unexpected symlink fixture error: {error}");
    }
    assert!(observe(&root, &workspace, declaration("inventory.json", b"outside")).is_err());
}
