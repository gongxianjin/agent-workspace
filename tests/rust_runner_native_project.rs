//! Runner native-claim contract for project-work-v1: the Linux worker's
//! runner.sqlite must accept and claim a signed project-work start the same
//! way it does sum-v1, keeping the artifact and session binding.
use ao_reason_workbench::{
    config::NodeSpec,
    core::{protocol::{Envelope, PROTOCOL}, project_work::{ConfirmedDocument, ProjectStage, ProjectWork, WorkTask}, runner::Runner},
};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::time::{SystemTime, UNIX_EPOCH};

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(),
        address: "native-B/runner".into(),
        key_hex: "07".repeat(32),
        slots: 1,
        admitted: true,
        healthy: false,
        draining: false,
        templates: vec!["sum-v1".into(), "project-work-v1".into()],
    }
}

fn now() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_secs() }

fn work() -> ProjectWork {
    let text = "# 设计\n稳定选择器 #app .card\n".to_owned();
    ProjectWork {
        schema: "project-work/1".into(), project_id: "doc-project".into(), action_id: "a1".into(),
        stage: ProjectStage::Develop,
        document: ConfirmedDocument { version: 1, sha256: hex::encode(Sha256::digest(text.as_bytes())), text },
        tasks: vec![WorkTask {
            id: "impl-page".into(), title: "实现设计静态页".into(), description: "按设计文档生成静态文件".into(),
            acceptance: "产物包含稳定选择器；总大小 ≤16KB".into(), depends_on: vec![], review: "approved".into(), reason: None,
        }],
        target_task: Some("impl-page".into()), repair_reason: None, repair_round: 0, test_spec: None, artifact: None,
    }
}

fn signed_start(template: &str, artifact: &str, work: Option<ProjectWork>) -> Envelope {
    let mut payload = json!({"template": template, "session": "session-1", "deadline": now() + 900,
        "worker_id": "B", "worker_artifact_sha256": artifact});
    if let Some(work) = work {
        payload["project_work"] = serde_json::to_value(&work).unwrap();
    }
    let mut message = Envelope {
        schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
        deployment: "runner-native-project".into(), from: "A".into(), to: "B".into(),
        run_id: format!("run-{}", uuid::Uuid::new_v4()), attempt: 1, epoch: "epoch-1".into(),
        kind: "start".into(), sent_at: now(), expires_at: now() + 300, payload, mac: String::new(),
    };
    message.sign(&[0x07; 32]).unwrap();
    message
}

#[test]
fn runner_accepts_and_claims_signed_project_work_start() {
    let root = tempfile::tempdir().unwrap();
    let mut runner = Runner::open(&root.path().join("runner.sqlite"), "runner-native-project", node()).unwrap();
    let artifact = "ab".repeat(32);
    let start = signed_start("project-work-v1", &artifact, Some(work()));
    runner.accept(&start, now()).unwrap();
    let identity = runner.claim_native(&start, now()).unwrap().expect("project start must be claimable");
    assert_eq!(identity.session.as_deref(), Some("session-1"));
    assert_eq!(identity.artifact_sha256.as_deref(), Some(artifact.as_ref()));
    // The controller reconstructs the run identity from the start payload
    // (from_detail) with source_sha256 = project_work.document.sha256; the
    // worker's claimed identity must carry the same field or the owner
    // launch binding is rejected ("owner launch execution mismatch" —
    // linux221 run b2029560). Six fields, all equal.
    assert_eq!(identity.source_sha256.as_deref(), Some(work().document.sha256.as_ref()),
        "claimed identity must bind the confirmed document digest");
}

#[test]
fn runner_claimed_identity_matches_controller_from_detail_reconstruction() {
    // Field-by-field equivalence with ExecutionIdentity::from_detail over the
    // same start payload — the exact check store.rs admission performs.
    let root = tempfile::tempdir().unwrap();
    let mut runner = Runner::open(&root.path().join("runner.sqlite"), "runner-native-project", node()).unwrap();
    let artifact = "ab".repeat(32);
    let start = signed_start("project-work-v1", &artifact, Some(work()));
    runner.accept(&start, now()).unwrap();
    let identity = runner.claim_native(&start, now()).unwrap().unwrap();
    // Mirror the controller's real run detail shape: the start payload's
    // project_work rides in detail.project_work and the assigned worker
    // artifact sha in detail.worker_artifact_sha256 (verified against the
    // linux221-25f12a654904 controller DB for run b2029560).
    let detail = json!({
        "project_work": serde_json::to_value(work()).unwrap(),
        "worker_artifact_sha256": artifact,
    });
    let rebuilt = ao_reason_workbench::core::protocol::ExecutionIdentity::from_detail(
        "runner-native-project", &start.run_id, start.attempt, &start.epoch,
        Some("session-1"), &detail,
    ).unwrap();
    assert_eq!(identity, rebuilt);
}

#[test]
fn runner_claims_sum_start_unchanged() {
    let root = tempfile::tempdir().unwrap();
    let mut runner = Runner::open(&root.path().join("runner.sqlite"), "runner-native-project", node()).unwrap();
    let artifact = "cd".repeat(32);
    let start = signed_start("sum-v1", &artifact, None);
    runner.accept(&start, now()).unwrap();
    let identity = runner.claim_native(&start, now()).unwrap().expect("sum start still claimable");
    assert_eq!(identity.session.as_deref(), Some("session-1"));
}

#[test]
fn runner_rejects_project_work_on_sum_only_node() {
    let root = tempfile::tempdir().unwrap();
    let mut sum_node = node();
    sum_node.templates = vec!["sum-v1".into()];
    let mut runner = Runner::open(&root.path().join("runner.sqlite"), "runner-native-project", sum_node).unwrap();
    let start = signed_start("project-work-v1", &"ab".repeat(32), Some(work()));
    let accepted = runner.accept(&start, now());
    assert!(accepted.is_err(), "template not offered by node must fail: {accepted:?}");
}

#[test]
fn runner_rejects_project_start_without_document_payload() {
    let root = tempfile::tempdir().unwrap();
    let mut runner = Runner::open(&root.path().join("runner.sqlite"), "runner-native-project", node()).unwrap();
    let start = signed_start("project-work-v1", &"ab".repeat(32), None);
    let accepted = runner.accept(&start, now());
    assert!(accepted.is_err(), "project template without project_work payload must fail: {accepted:?}");
}

#[test]
fn terminal_parse_failure_surfaces_child_stderr_prefix() {
    // linux221 run 1152214d: the owner-child died before printing its JSON
    // report (TLS NotValidForName) and the only diagnostic — the child's
    // stderr — was discarded on the parse-failure path, forcing remote
    // forensics. The parse error must carry the retained stderr prefix so
    // native_unknown persists the real cause.
    use ao_reason_workbench::core::owner_producer::{
        CleanupFacts, LaunchFacts, OwnedProcessPort, OwnerLaunchPlan, StreamReadOutcome,
        TerminalFacts, finish_owner_evidence,
    };
    use ao_reason_workbench::core::protocol::canonical;

    struct DeadChildPort;
    impl OwnedProcessPort for DeadChildPort {
        fn owner_identity(&self) -> anyhow::Result<ao_reason_workbench::core::worker_owner::OwnerProcessIdentity> {
            Ok(ao_reason_workbench::core::worker_owner::OwnerProcessIdentity { pid: 100, start_ticks: 1000 })
        }
        fn launch(&mut self, _: &OwnerLaunchPlan) -> anyhow::Result<LaunchFacts> {
            Ok(LaunchFacts { child_pid: 200, child_start_ticks: 2000, pgid: 200 })
        }
        fn wait_terminal(&mut self) -> anyhow::Result<TerminalFacts> {
            Ok(TerminalFacts {
                wait_completed: true,
                wait_method: "child_wait".into(),
                raw_wait_status: 512, // exit code 2, the observed shape
                stdout: StreamReadOutcome { prefix: vec![], total_read_bytes: 0, eof: true, error: None },
                stderr: StreamReadOutcome {
                    prefix: b"Error: model HTTP request failed: invalid certificate: NotValidForName\n".to_vec(),
                    total_read_bytes: 71, eof: true, error: None,
                },
                cleanup: CleanupFacts {
                    proc_scan_complete: true, process_group_scan_complete: true, cgroup_scan_complete: true,
                    process_group_members: 0, cgroup_members: 0, error: None,
                },
            })
        }
    }

    let root = tempfile::tempdir().unwrap();
    let mut runner = Runner::open(&root.path().join("runner.sqlite"), "runner-native-project", node()).unwrap();
    let artifact = "ab".repeat(32);
    let start = signed_start("project-work-v1", &artifact, Some(work()));
    runner.accept(&start, now()).unwrap();
    let identity = runner.claim_native(&start, now()).unwrap().expect("claimable");
    let plan = OwnerLaunchPlan {
        execution: identity.clone(),
        node_id: "B".into(),
        instance_id: "123e4567-e89b-42d3-a456-426614174000".into(),
        boot_id: "123e4567-e89b-12d3-a456-426614174001".into(),
        request_bytes: canonical(&start.payload).unwrap(),
    };
    let mut port = DeadChildPort;
    let owner = port.owner_identity().unwrap();
    let facts = port.launch(&plan).unwrap();
    let binding = ao_reason_workbench::core::owner_producer::complete_binding(&plan, &owner, &facts).unwrap();
    runner.native_launch(&binding, now()).unwrap();
    let production = finish_owner_evidence(&mut port, &plan, &owner, &facts).unwrap();

    let workspace = root.path().join("files-workspace");
    std::fs::create_dir_all(&workspace).unwrap();
    let failure = runner.native_terminal_for(&production, Some(&workspace), now()).unwrap_err();
    let message = format!("{failure:#}");
    assert!(
        message.contains("NotValidForName"),
        "the child stderr prefix must ride the parse failure so native_unknown persists the real cause, got: {message}"
    );
}
