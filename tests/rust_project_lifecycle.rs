//! Public Projects/Store boundary regression. All signed ingress and ExitProof
//! values below are synthetic fixture data, NOT native exit/ownership evidence.
//! No Runner, AO/model request, GUI, native worker, or host verifier is started.
use agentranch::{
    config::NodeSpec,
    core::{
        project_work::{ProjectStage, ProjectWork, WorkTask},
        projects::Projects,
        protocol::{canonical, Envelope, PROTOCOL},
        service::UiCommand,
        store::{ExitProof, Run, Store},
        worker_owner::{self, ExpectedOwnerBinding, OwnerEvidenceReference, OwnerVerification},
    },
};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::path::Path;

fn node() -> NodeSpec {
    NodeSpec {
        id: "B".into(), address: "owned-B/runner".into(), key_hex: "07".repeat(32),
        slots: 1, admitted: true, healthy: true, draining: false,
        templates: vec!["project-work-v1".into()],
    }
}

fn now() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH).unwrap().as_secs()
}

fn signed_fixture(run: &Run, kind: &str, payload: Value) -> Envelope {
    let time = now();
    let mut message = Envelope {
        schema: PROTOCOL.into(), message_id: uuid::Uuid::new_v4().to_string(),
        deployment: "project-lifecycle-fixture".into(), from: "B".into(), to: "A".into(),
        run_id: run.id.clone(), attempt: run.attempt, epoch: run.epoch.clone(),
        kind: kind.into(), sent_at: time, expires_at: time + 300, payload,
        mac: String::new(),
    };
    message.sign(&hex::decode(node().key_hex).unwrap()).unwrap();
    message
}

fn canonical_digest(value: &Value) -> String {
    hex::encode(Sha256::digest(canonical(value).unwrap()))
}

fn scripted_owner_expected(host: &Store, run: &Run) -> ExpectedOwnerBinding {
    let execution = host.run(&run.id).unwrap().execution_identity("project-lifecycle-fixture").unwrap();
    let instance_id = "123e4567-e89b-42d3-a456-426614174000";
    let boot_id = "123e4567-e89b-12d3-a456-426614174001";
    let owner_handle = format!("ow-{}", canonical_digest(&json!({
        "schema": "linux-worker-owner-evidence/1",
        "execution": execution,
        "node_id": "B",
        "instance_id": instance_id,
        "boot_id": boot_id,
    })));
    serde_json::from_value(json!({
        "execution": execution,
        "owner": {"node_id": "B", "instance_id": instance_id, "boot_id": boot_id,
            "owner_handle": owner_handle},
        "request_sha256": "ab".repeat(32),
        "owner_process": {"pid": 100, "start_ticks": 1000},
        "process": {"pid": 200, "parent_pid": 100, "start_ticks": 2000,
            "pgid": 200, "cgroup_path": format!("workers/{instance_id}/{owner_handle}")}
    })).unwrap()
}

fn scripted_owner_terminal(host: &mut Store, run: &Run, session: &str) {
    let current = host.run(&run.id).unwrap();
    let report = current.detail["report"].clone();
    let expected = scripted_owner_expected(host, &current);
    host.register_local_owner_binding(&current.id, current.attempt, &expected, now()).unwrap();
    let empty_sha = hex::encode(Sha256::digest(b""));
    let stream = json!({"prefix_len": 0, "total_read_bytes": 0, "truncated": false,
        "eof": true, "error": null, "prefix_sha256": empty_sha});
    let owner_prefix = format!("owners/{}/{}", expected.owner.instance_id, expected.owner.owner_handle);
    let record = json!({
        "schema": "linux-worker-owner-evidence/1", "execution": expected.execution,
        "owner": expected.owner, "request_sha256": expected.request_sha256,
        "owner_process": {"pid": 100, "start_ticks": 1000},
        "process": {"pid": 200, "parent_pid": 100, "start_ticks": 2000, "pgid": 200,
            "cgroup_path": format!("workers/{}/{}", expected.owner.instance_id, expected.owner.owner_handle)},
        "direct_wait": {"wait_completed": true, "method": "child_wait", "raw_wait_status": 0,
            "exit_code": 0, "termination_signal": null, "numeric_exit": 0},
        "stdout": stream, "stderr": stream,
        "cleanup": {"proc_scan_complete": true, "process_group_scan_complete": true,
            "cgroup_scan_complete": true, "pgid": 200,
            "cgroup_path": format!("workers/{}/{}", expected.owner.instance_id, expected.owner.owner_handle),
            "process_group_members": 0, "cgroup_members": 0, "error": null},
        "evidence_refs": {"record": format!("{owner_prefix}/record.json"),
            "stdout": format!("{owner_prefix}/stdout.bin"), "stderr": format!("{owner_prefix}/stderr.bin")}
    });
    let bundle_value = json!({"record": record, "record_sha256": canonical_digest(&record),
        "stdout_prefix_hex": "", "stderr_prefix_hex": ""});
    let bundle: worker_owner::OwnerEvidenceBundle = serde_json::from_value(bundle_value).unwrap();
    let verification = OwnerVerification {passed: true, state: "verified".into(),
        details: json!({"source": "scripted lifecycle contract", "native": false})};
    let reference = OwnerEvidenceReference {
        schema: "owner-evidence-reference/1".into(),
        binding_sha256: worker_owner::owner_binding_sha256(&expected).unwrap(),
        bundle_sha256: worker_owner::owner_bundle_sha256(&bundle).unwrap(),
        record_sha256: bundle.record_sha256.clone(),
        report_sha256: worker_owner::owner_report_sha256(&expected.execution, &report, &verification).unwrap(),
    };
    let exit_proof = ExitProof {run_id: current.id.clone(), attempt: current.attempt,
        epoch: current.epoch.clone(), session: Some(session.into()), empty: true,
        observer_alive: true, not_started: false, owner_evidence: Some(reference.clone())};
    let message = signed_fixture(&current, "report", json!({
        "schema": "owner-evidence-report/1", "phase": "terminal", "session": session,
        "report": report, "verification": verification,
        "owner_evidence": {"reference": reference,
            "bundle_json": serde_json::to_string(&bundle).unwrap()},
        "exit_proof": exit_proof,
    }));
    assert!(host.receive(&message, now()).unwrap());
}

fn scripted_release_fixture_paused(host: &mut Store, run: &Run, session: &str) {
    // Paused adaptation: retain this helper without changing the original test flow.
    // The first legacy report remains pending. Only this separate, typed,
    // scripted owner contract may complete the existing assignment.
    scripted_owner_terminal(host, run, session);
    assert!(host.assignment(&run.id, run.attempt).unwrap().unwrap().released,
        "scripted owner terminal admission must release the current assignment");
}

fn configure_fixture(projects: &mut Projects, root: &Path) {
    #[cfg(windows)]
    let endpoint = format!(r"\\.\pipe\absent-project-lifecycle-{}", uuid::Uuid::new_v4());
    #[cfg(not(windows))]
    let endpoint = root.join("absent-reason.sock").to_string_lossy().into_owned();
    std::fs::write(root.join("node.key"), node().key_hex).unwrap();
    let config = json!({
        "schema": "ao-reason-config/1", "mode": "real", "role": "controller",
        "deployment": "project-lifecycle-fixture", "state_dir": root.join("host"),
        "nodes": [{"id": "B", "address": "owned-B/runner", "key_file": "node.key",
            "slots": 1, "templates": ["project-work-v1"], "admitted": true}],
        "ao": {"base_url": "http://127.0.0.1:1", "project_id": "fixture", "model": "fixture-only"},
        "reason_endpoint": endpoint, "endpoint": "owned-A/controller"
    });
    let path = root.join("controller.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    projects.command(UiCommand::ProjectConfigure { config_path: path }).unwrap();
}

fn confirmed_project(projects: &mut Projects, root: &Path) -> String {
    let id = projects.command(UiCommand::ProjectCreate { name: "manual Start fixture".into() })
        .unwrap()["id"].as_str().unwrap().to_owned();
    let text = "# Reviewed requirements\nDeduct inventory atomically; refund restores it once.\n";
    let path = root.join("reviewed.md");
    std::fs::write(&path, text).unwrap();
    projects.command(UiCommand::ProjectImport { project_id: id.clone(), path }).unwrap();
    projects.command(UiCommand::ProjectConfirm {
        project_id: id.clone(), version: 1,
        sha256: hex::encode(Sha256::digest(text.as_bytes())),
    }).unwrap();
    id
}

fn snapshot(projects: &Projects, id: &str) -> Value {
    serde_json::to_value(projects.snapshots().into_iter().find(|p| p.id == id).unwrap()).unwrap()
}

fn project_tick_with_absent_transport(projects: &mut Projects) {
    // Project projection still runs after the deliberately unavailable IPC.
    // An unavailable transport is never counted as successful dispatch/delivery.
    let error = projects.tick().expect_err("fixture has no Reason server");
    assert!(error.to_string().contains("project transport unavailable"), "{error:#}");
}

fn split_fixture_report(work: &ProjectWork) -> Value {
    json!({
        "schema": "project-result/1", "project_id": work.project_id,
        "action_id": work.action_id, "stage": "split",
        "document_version": work.document.version, "document_sha256": work.document.sha256,
        "tasks": [
            {"id": "inventory", "title": "Inventory", "description": "Deduct atomically",
                "acceptance": "Inventory remains nonnegative", "depends_on": []},
            {"id": "refund", "title": "Refund", "description": "Restore inventory once",
                "acceptance": "Duplicate refunds are idempotent", "depends_on": ["inventory"]}
        ]
    })
}

fn ingest_split_fixture(projects: &mut Projects, host: &mut Store, id: &str) -> (Run, ProjectWork) {
    projects.command(UiCommand::ProjectSplit { project_id: id.into() }).unwrap();
    let run = host.list_runs(10).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["project_id"] == id).unwrap();
    let work: ProjectWork = serde_json::from_value(run.detail["project_work"].clone()).unwrap();
    assert_eq!(work.stage, ProjectStage::Split);
    assert!(work.tasks.is_empty());
    let assignment = host.assign_next(now()).unwrap().unwrap();
    assert_eq!(assignment.run_id, run.id);
    host.receive(&signed_fixture(&run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-split-session"
    })), now()).unwrap();
    host.receive(&signed_fixture(&run, "report", json!({
        "session": "fixture-split-session", "report": {"project_result": split_fixture_report(&work)},
        "verification": {"passed": true, "meaning": "synthetic fixture only"}
    })), now()).unwrap();
    project_tick_with_absent_transport(projects);
    (run, work)
}

fn split_three_fixture_report(work: &ProjectWork) -> Value {
    json!({
        "schema": "project-result/1", "project_id": work.project_id,
        "action_id": work.action_id, "stage": "split",
        "document_version": work.document.version, "document_sha256": work.document.sha256,
        "tasks": [
            {"id": "inventory", "title": "Inventory", "description": "Deduct atomically",
                "acceptance": "Inventory remains nonnegative", "depends_on": []},
            {"id": "refund", "title": "Refund", "description": "Restore inventory once",
                "acceptance": "Duplicate refunds are idempotent", "depends_on": ["inventory"]},
            {"id": "audit", "title": "Audit", "description": "Record audit trail",
                "acceptance": "Audit records are append-only", "depends_on": []}
        ]
    })
}

fn ingest_three_task_split_fixture(projects: &mut Projects, host: &mut Store, id: &str) -> (Run, ProjectWork) {
    projects.command(UiCommand::ProjectSplit { project_id: id.into() }).unwrap();
    let run = host.list_runs(10).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["project_id"] == id).unwrap();
    let work: ProjectWork = serde_json::from_value(run.detail["project_work"].clone()).unwrap();
    assert_eq!(work.stage, ProjectStage::Split);
    host.assign_next(now()).unwrap();
    host.receive(&signed_fixture(&run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-split-three-session"
    })), now()).unwrap();
    host.receive(&signed_fixture(&run, "report", json!({
        "session": "fixture-split-three-session", "report": {"project_result": split_three_fixture_report(&work)},
        "verification": {"passed": true, "meaning": "synthetic fixture only"}
    })), now()).unwrap();
    project_tick_with_absent_transport(projects);
    (run, work)
}

fn approve(projects: &mut Projects, id: &str, task_id: &str) {
    projects.command(UiCommand::TaskReview {
        project_id: id.into(), task_id: task_id.into(), approve: true, reason: String::new(),
    }).unwrap();
}

fn develop_fixture_report(work: &ProjectWork) -> Value {
    let bytes = br#"{"items":[]}"#;
    let file = json!({"path": "inventory.json", "size": bytes.len(),
        "sha256": hex::encode(Sha256::digest(bytes))});
    let mut packed_file = file.clone();
    packed_file["content_hex"] = json!(hex::encode(bytes));
    json!({
        "project_result": {
            "schema": "project-result/1", "project_id": work.project_id,
            "action_id": work.action_id, "stage": "develop",
            "document_version": work.document.version, "document_sha256": work.document.sha256,
            "artifact": {"schema": "project-artifact/1", "files": [file]}
        },
        "artifact": {
            "schema": "project-artifact-package/1", "project_id": work.project_id,
            "action_id": work.action_id, "document_version": work.document.version,
            "document_sha256": work.document.sha256, "files": [packed_file]
        }
    })
}

fn development_fixture(root: &Path) -> (Projects, Store, String, Run, ProjectWork) {
    let mut projects = Projects::open(&root.join("workflow")).unwrap();
    let id = confirmed_project(&mut projects, root);
    configure_fixture(&mut projects, root);
    let mut host = Store::open(&root.join("host/controller.sqlite"),
        "project-lifecycle-fixture", &[node()]).unwrap();
    let (split, _) = ingest_split_fixture(&mut projects, &mut host, &id);
    scripted_release_fixture_paused(&mut host, &split, "fixture-split-session");
    project_tick_with_absent_transport(&mut projects);
    approve(&mut projects, &id, "inventory");
    approve(&mut projects, &id, "refund");
    projects.command(UiCommand::ProjectStart { project_id: id.clone() }).unwrap();
    let run = host.list_runs(10).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "develop").unwrap();
    let work = serde_json::from_value(run.detail["project_work"].clone()).unwrap();
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, run.id);
    host.receive(&signed_fixture(&run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-develop-session"
    })), now()).unwrap();
    (projects, host, id, run, work)
}

fn ingest_develop_fixture(host: &mut Store, run: &Run, report: Value) {
    host.receive(&signed_fixture(run, "report", json!({
        "session": "fixture-develop-session", "report": report,
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "synthetic fixture only, NOT native proof"}
    })), now()).unwrap();
}

fn test_fixture_report(work: &ProjectWork, passed: bool) -> Value {
    let spec = work.test_spec.as_ref().unwrap();
    let artifact = work.artifact.as_ref().unwrap();
    json!({
        "schema": "project-result/1", "project_id": work.project_id,
        "action_id": work.action_id, "stage": "test",
        "document_version": work.document.version, "document_sha256": work.document.sha256,
        "test_spec_sha256": spec.sha256, "artifact_sha256": artifact.sha256,
        "passed": passed,
        "evidence": {"exit_code": if passed { 0 } else { 1 },
            "stdout": "fixture test output", "stderr": "", "meaning": "synthetic fixture only"}
    })
}

fn repair_fixture_report(work: &ProjectWork) -> Value {
    let bytes = br#"{"items":["repaired"]}"#;
    let file = json!({"path": "inventory.json", "size": bytes.len(),
        "sha256": hex::encode(Sha256::digest(bytes))});
    let mut packed_file = file.clone();
    packed_file["content_hex"] = json!(hex::encode(bytes));
    json!({
        "project_result": {
            "schema": "project-result/1", "project_id": work.project_id,
            "action_id": work.action_id, "stage": "repair",
            "document_version": work.document.version, "document_sha256": work.document.sha256,
            "artifact": {"schema": "project-artifact/1", "files": [file]}
        },
        "artifact": {
            "schema": "project-artifact-package/1", "project_id": work.project_id,
            "action_id": work.action_id, "document_version": work.document.version,
            "document_sha256": work.document.sha256, "files": [packed_file]
        }
    })
}

fn ingest_repair_fixture(host: &mut Store, run: &Run, report: Value) {
    host.receive(&signed_fixture(run, "report", json!({
        "session": "fixture-repair-session", "report": report,
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "synthetic fixture only, NOT native proof"}
    })), now()).unwrap();
}

#[test]
fn develop_requires_verified_artifact_and_independent_exit_before_projection() {
    let temp = tempfile::tempdir().unwrap();
    let (mut projects, mut host, id, run, work) = development_fixture(temp.path());
    let artifact_path = Path::new(snapshot(&projects, &id)["product_path"].as_str().unwrap())
        .join(&work.action_id).join("inventory.json");
    ingest_develop_fixture(&mut host, &run, develop_fixture_report(&work));
    project_tick_with_absent_transport(&mut projects);
    let pending = snapshot(&projects, &id);
    assert_eq!(pending["phase"], "developing");
    assert_eq!(pending["can_test"], false);
    assert_eq!(pending["can_accept"], false);
    assert!(!artifact_path.exists(), "report-only input must not materialize a completed product");
    assert!(!host.assignment(&run.id, run.attempt).unwrap().unwrap().released);
    scripted_release_fixture_paused(&mut host, &run, "fixture-develop-session");
    project_tick_with_absent_transport(&mut projects);
    let developed = snapshot(&projects, &id);
    assert_eq!(developed["phase"], "developed",
        "verified actual artifact package plus separate fixture exit must project developed");
    assert_eq!(developed["can_test"], true);
    assert_eq!(developed["can_accept"], false);
    assert_eq!(developed["actions"][1]["state"], "succeeded");
    assert_eq!(std::fs::read(&artifact_path).unwrap(), br#"{"items":[]}"#);
    assert!(host.list_runs(10).unwrap().iter().all(|run| {
        run.detail["project_work"]["stage"] != "test"
    }), "Develop completion must not auto-dispatch independent Test");
    drop(projects);
    let reopened = Projects::open(&temp.path().join("workflow")).unwrap();
    assert_eq!(snapshot(&reopened, &id)["phase"], "developed");
    assert_eq!(std::fs::read(&artifact_path).unwrap(), br#"{"items":[]}"#);
}

#[test]
fn first_project_test_derives_nonempty_spec_before_dispatch_and_pins_artifact() {
    let temp = tempfile::tempdir().unwrap();
    let (mut projects, mut host, id, develop_run, develop_work) = development_fixture(temp.path());
    ingest_develop_fixture(&mut host, &develop_run, develop_fixture_report(&develop_work));
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &develop_run, "fixture-develop-session");
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["phase"], "developed");

    projects.command(UiCommand::ProjectTest { project_id: id.clone() })
        .expect("explicit ProjectTest must derive host-owned inputs before dispatch");
    let test_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test")
        .expect("ProjectTest must dispatch one independent test run");
    let test_work: ProjectWork = serde_json::from_value(test_run.detail["project_work"].clone()).unwrap();
    let spec = test_work.test_spec.as_ref().expect("first test must freeze a nonempty spec");
    let expected = "# Project checks\n- inventory: Inventory remains nonnegative\n- refund: Duplicate refunds are idempotent\n";
    assert_eq!(spec.text, expected);
    assert_eq!(spec.sha256, hex::encode(Sha256::digest(expected.as_bytes())));
    let artifact = test_work.artifact.as_ref().expect("test must pin a nonempty artifact");
    assert!(!artifact.text.trim().is_empty());
    assert_eq!(artifact.sha256, hex::encode(Sha256::digest(artifact.text.as_bytes())));
    assert_eq!(test_work.document, develop_work.document);
    assert_eq!(snapshot(&projects, &id)["phase"], "testing");
}

#[test]
fn test_pass_requires_independent_exit_and_only_reaches_acceptance_queue() {
    let temp = tempfile::tempdir().unwrap();
    let (mut projects, mut host, id, develop_run, develop_work) = development_fixture(temp.path());
    ingest_develop_fixture(&mut host, &develop_run, develop_fixture_report(&develop_work));
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &develop_run, "fixture-develop-session");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    let test_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test").unwrap();
    let test_work: ProjectWork = serde_json::from_value(test_run.detail["project_work"].clone()).unwrap();
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, test_run.id);
    host.receive(&signed_fixture(&test_run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-test-session"
    })), now()).unwrap();
    host.receive(&signed_fixture(&test_run, "report", json!({
        "session": "fixture-test-session", "report": {"project_result": test_fixture_report(&test_work, true)},
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "synthetic fixture only, NOT native proof"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    let pending = snapshot(&projects, &id);
    assert_eq!(pending["phase"], "testing");
    assert_eq!(pending["can_accept"], false);
    assert!(!host.assignment(&test_run.id, test_run.attempt).unwrap().unwrap().released);
    scripted_release_fixture_paused(&mut host, &test_run, "fixture-test-session");
    project_tick_with_absent_transport(&mut projects);
    let awaiting = snapshot(&projects, &id);
    assert_eq!(awaiting["phase"], "awaiting_acceptance");
    assert_eq!(awaiting["can_accept"], true);
    assert_eq!(awaiting["actions"][2]["state"], "succeeded");
    assert_eq!(awaiting["actions"].as_array().unwrap().iter().filter(|a| a["kind"] == "accept").count(), 0);
}

#[test]
fn manual_accept_persists_durable_evidence_and_reopens() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("workflow");
    let (mut projects, mut host, id, develop_run, develop_work) = development_fixture(temp.path());
    ingest_develop_fixture(&mut host, &develop_run, develop_fixture_report(&develop_work));
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &develop_run, "fixture-develop-session");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    let test_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test").unwrap();
    let test_work: ProjectWork = serde_json::from_value(test_run.detail["project_work"].clone()).unwrap();
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, test_run.id);
    host.receive(&signed_fixture(&test_run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-test-accept-session"
    })), now()).unwrap();
    host.receive(&signed_fixture(&test_run, "report", json!({
        "session": "fixture-test-accept-session", "report": {"project_result": test_fixture_report(&test_work, true)},
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "synthetic fixture only, NOT native proof"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &test_run, "fixture-test-accept-session");
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["phase"], "awaiting_acceptance");
    projects.command(UiCommand::ProjectAccept { project_id: id.clone() })
        .expect("manual acceptance must be independently executable after PASS");
    let accepted = snapshot(&projects, &id);
    assert_eq!(accepted["phase"], "accepted");
    assert_eq!(accepted["actions"].as_array().unwrap().iter().filter(|a| a["kind"] == "accept").count(), 1,
        "manual acceptance must create one durable acceptance action");
    let db = rusqlite::Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    let evidence: String = db.query_row(
        "SELECT value FROM project_settings WHERE key=?1",
        [format!("acceptance:{}", id)], |row| row.get(0)).unwrap();
    let evidence: Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(evidence["project_id"], id);
    assert_eq!(evidence["document_version"], accepted["document"]["version"]);
    assert_eq!(evidence["document_sha256"], accepted["document"]["sha256"]);
    assert_eq!(evidence["test_action_id"], accepted["actions"][2]["id"]);
    drop(projects);
    let reopened = Projects::open(&state).unwrap();
    let reopened_snapshot = snapshot(&reopened, &id);
    assert_eq!(reopened_snapshot["phase"], "accepted");
    assert_eq!(reopened_snapshot["actions"].as_array().unwrap().iter().filter(|a| a["kind"] == "accept").count(), 1);
}

#[test]
fn manual_start_requires_each_approval_and_does_not_freeze_spec() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("workflow");
    let mut projects = Projects::open(&state).unwrap();
    let id = confirmed_project(&mut projects, temp.path());
    configure_fixture(&mut projects, temp.path());
    // Split opens the actual configured host ledger. Opening it here first
    // supplies the same trusted node set, not a directly edited project row.
    let mut host = Store::open(&temp.path().join("host/controller.sqlite"),
        "project-lifecycle-fixture", &[node()]).unwrap();
    let (run, split_work) = ingest_split_fixture(&mut projects, &mut host, &id);
    let pending = snapshot(&projects, &id);
    assert_eq!(pending["tasks"].as_array().unwrap().len(), 2);
    assert!(pending["tasks"].as_array().unwrap().iter().all(|t| t["review"] == "pending"));
    assert_eq!(pending["actions"][0]["state"], "report_pending_exit");
    assert!(!host.assignment(&run.id, run.attempt).unwrap().unwrap().released,
        "signed report alone must not release a reservation");
    assert!(projects.command(UiCommand::ProjectStart { project_id: id.clone() }).is_err());

    // Separate public release boundary; this is deliberately synthetic, not
    // actual process/ownership evidence or a native/product acceptance test.
    assert!(!host.release(&ExitProof {
        run_id: run.id.clone(), attempt: run.attempt, epoch: run.epoch.clone(),
        session: Some("fixture-split-session".into()), empty: true,
        observer_alive: true, not_started: false, owner_evidence: None,
    }).unwrap());
    scripted_release_fixture_paused(&mut host, &run, "fixture-split-session");
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["actions"][0]["state"], "succeeded");
    approve(&mut projects, &id, "inventory");
    assert_eq!(snapshot(&projects, &id)["can_start"], false);
    assert!(projects.command(UiCommand::ProjectStart { project_id: id.clone() }).is_err(),
        "one approved task must not authorize unreviewed dependent work");
    approve(&mut projects, &id, "refund");
    let ready = snapshot(&projects, &id);
    assert_eq!(ready["can_start"], true);
    assert_eq!(ready["phase"], "ready");
    assert!(host.list_runs(10).unwrap().iter().all(|run| {
        run.detail["project_work"]["stage"] != "develop"
    }), "approval must not auto-start development");

    projects.command(UiCommand::ProjectStart { project_id: id.clone() })
        .expect("manual Start must dispatch approved development work without freezing a test spec");
    let runs = host.list_runs(10).unwrap();
    let developments: Vec<_> = runs.iter()
        .filter(|run| run.detail["project_work"]["stage"] == "develop").collect();
    assert_eq!(developments.len(), 1);
    let develop: ProjectWork = serde_json::from_value(developments[0].detail["project_work"].clone()).unwrap();
    assert_eq!(develop.document, split_work.document);
    assert_eq!(develop.tasks, vec![
        WorkTask { id: "inventory".into(), title: "Inventory".into(), description: "Deduct atomically".into(),
            acceptance: "Inventory remains nonnegative".into(), depends_on: vec![], review: "approved".into(), reason: None },
        WorkTask { id: "refund".into(), title: "Refund".into(), description: "Restore inventory once".into(),
            acceptance: "Duplicate refunds are idempotent".into(), depends_on: vec!["inventory".into()], review: "approved".into(), reason: None },
    ]);
    assert!(develop.test_spec.is_none());
    assert!(develop.artifact.is_none());
    assert_eq!(snapshot(&projects, &id)["phase"], "developing");
    assert_eq!(snapshot(&projects, &id)["actions"].as_array().unwrap().len(), 2);
    drop(projects);
    let reopened = Projects::open(&state).unwrap();
    let reopened_snapshot = snapshot(&reopened, &id);
    assert_eq!(reopened_snapshot["phase"], "developing");
    assert_eq!(reopened_snapshot["tasks"], ready["tasks"]);
    assert_eq!(reopened_snapshot["actions"].as_array().unwrap().len(), 2);
    assert!(reopened_snapshot["actions"].as_array().unwrap().iter().all(|action| {
        action["kind"] != "accept" && action["kind"] != "test"
    }));
}

#[test]
fn repair_report_updates_artifact_then_retest_reuses_exact_spec() {
    let temp = tempfile::tempdir().unwrap();
    let (mut projects, mut host, id, develop_run, develop_work) = development_fixture(temp.path());
    ingest_develop_fixture(&mut host, &develop_run, develop_fixture_report(&develop_work));
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &develop_run, "fixture-develop-session");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    let test_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test").unwrap();
    let test_work: ProjectWork = serde_json::from_value(test_run.detail["project_work"].clone()).unwrap();
    let frozen_spec = test_work.test_spec.clone().unwrap();
    let old_artifact = test_work.artifact.clone().unwrap();
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, test_run.id);
    host.receive(&signed_fixture(&test_run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-test-failure-session"
    })), now()).unwrap();
    host.receive(&signed_fixture(&test_run, "report", json!({
        "session": "fixture-test-failure-session",
        "report": {"project_result": test_fixture_report(&test_work, false)},
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "synthetic fixture only, NOT native proof"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &test_run, "fixture-test-failure-session");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectRepair {
        project_id: id.clone(), reason: "fix failing inventory check".into()
    }).unwrap();
    let repair_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "repair").unwrap();
    let repair_work: ProjectWork = serde_json::from_value(repair_run.detail["project_work"].clone()).unwrap();
    assert_eq!(repair_work.test_spec.as_ref().unwrap(), &frozen_spec);
    assert_eq!(repair_work.artifact.as_ref().unwrap(), &old_artifact);
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, repair_run.id);
    host.receive(&signed_fixture(&repair_run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-repair-session"
    })), now()).unwrap();
    ingest_repair_fixture(&mut host, &repair_run, repair_fixture_report(&repair_work));
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &repair_run, "fixture-repair-session");
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["phase"], "developed",
        "released Repair with A2 bytes must reopen explicit Test gate");
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    let retest_run = host.list_runs(30).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test" && run.id != test_run.id)
        .expect("Repair must permit explicit retest");
    let retest_work: ProjectWork = serde_json::from_value(retest_run.detail["project_work"].clone()).unwrap();
    let expected_spec = "# Project checks\n- inventory: Inventory remains nonnegative\n- refund: Duplicate refunds are idempotent\n";
    assert_eq!(retest_work.test_spec.as_ref().unwrap().text, expected_spec);
    assert_eq!(retest_work.test_spec.as_ref().unwrap().sha256,
        hex::encode(Sha256::digest(expected_spec.as_bytes())));
    assert_ne!(retest_work.artifact.as_ref().unwrap(), &old_artifact,
        "retest must bind repaired artifact A2, not stale artifact A1");
    assert!(retest_work.artifact.as_ref().unwrap().text.contains(
        &hex::encode(Sha256::digest(br#"{"items":["repaired"]}"#))));
}

#[test]
fn third_repair_is_rejected_after_two_bounded_rounds() {
    let temp = tempfile::tempdir().unwrap();
    let (mut projects, mut host, id, develop_run, develop_work) = development_fixture(temp.path());
    ingest_develop_fixture(&mut host, &develop_run, develop_fixture_report(&develop_work));
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &develop_run, "fixture-develop-session");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    let first_test = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test").unwrap();
    let first_work: ProjectWork = serde_json::from_value(first_test.detail["project_work"].clone()).unwrap();
    let spec = first_work.test_spec.clone().unwrap();
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, first_test.id);
    host.receive(&signed_fixture(&first_test, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-test-1"
    })), now()).unwrap();
    host.receive(&signed_fixture(&first_test, "report", json!({
        "session": "fixture-test-1", "report": {"project_result": test_fixture_report(&first_work, false)},
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "synthetic fixture only, NOT native proof"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &first_test, "fixture-test-1");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectRepair { project_id: id.clone(), reason: "repair round one".into() }).unwrap();
    let repair_one = host.list_runs(30).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "repair").unwrap();
    let work_one: ProjectWork = serde_json::from_value(repair_one.detail["project_work"].clone()).unwrap();
    assert_eq!(work_one.repair_round, 1);
    assert_eq!(work_one.test_spec.as_ref(), Some(&spec));
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, repair_one.id);
    host.receive(&signed_fixture(&repair_one, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-repair-session"
    })), now()).unwrap();
    ingest_repair_fixture(&mut host, &repair_one, repair_fixture_report(&work_one));
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &repair_one, "fixture-repair-session");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    let second_test = host.list_runs(40).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test" && run.id != first_test.id).unwrap();
    let second_work: ProjectWork = serde_json::from_value(second_test.detail["project_work"].clone()).unwrap();
    assert_eq!(second_work.test_spec.as_ref(), Some(&spec));
    assert_ne!(second_work.artifact.as_ref(), first_work.artifact.as_ref());
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, second_test.id);
    host.receive(&signed_fixture(&second_test, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-test-2"
    })), now()).unwrap();
    host.receive(&signed_fixture(&second_test, "report", json!({
        "session": "fixture-test-2", "report": {"project_result": test_fixture_report(&second_work, false)},
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "synthetic fixture only, NOT native proof"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &second_test, "fixture-test-2");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectRepair { project_id: id.clone(), reason: "repair round two".into() }).unwrap();
    let repair_two = host.list_runs(50).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "repair" && run.id != repair_one.id).unwrap();
    let work_two: ProjectWork = serde_json::from_value(repair_two.detail["project_work"].clone()).unwrap();
    assert_eq!(work_two.repair_round, 2);
    assert_eq!(work_two.test_spec.as_ref(), Some(&spec));
    assert!(projects.command(UiCommand::ProjectRepair {
        project_id: id.clone(), reason: "repair round three must fail".into()
    }).is_err(), "third Repair must be rejected, not bypassed by another action");
}

#[test]
fn redo_rebuilds_only_rejected_task_dependency_closure_without_auto_start() {
    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("workflow");
    let mut projects = Projects::open(&state).unwrap();
    let id = confirmed_project(&mut projects, temp.path());
    configure_fixture(&mut projects, temp.path());
    let mut host = Store::open(&temp.path().join("host/controller.sqlite"),
        "project-lifecycle-fixture", &[node()]).unwrap();
    let (split_run, split_work) = ingest_three_task_split_fixture(&mut projects, &mut host, &id);
    scripted_release_fixture_paused(&mut host, &split_run, "fixture-split-three-session");
    project_tick_with_absent_transport(&mut projects);

    approve(&mut projects, &id, "audit");
    approve(&mut projects, &id, "refund");
    projects.command(UiCommand::TaskReview {
        project_id: id.clone(), task_id: "inventory".into(), approve: false,
        reason: "inventory deduction is unsafe".into(),
    }).unwrap();
    let before = snapshot(&projects, &id);
    assert_eq!(before["document"]["version"], split_work.document.version);
    assert_eq!(before["document"]["sha256"], split_work.document.sha256);
    assert_eq!(before["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "audit").unwrap()["review"], "approved");
    assert_eq!(before["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "refund").unwrap()["review"], "approved");
    assert_eq!(before["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "inventory").unwrap()["review"], "rejected");

    projects.command(UiCommand::TaskRedo {
        project_id: id.clone(), task_id: "inventory".into(),
    }).expect("redo must dispatch a bounded dependency-closure work item");

    let runs = host.list_runs(20).unwrap();
    let redo_run = runs.iter()
        .find(|run| run.detail["project_work"]["stage"] == "redo")
        .expect("redo must create a ProjectStage::Redo run");
    let redo_work: ProjectWork = serde_json::from_value(
        redo_run.detail["project_work"].clone()).unwrap();
    assert_eq!(redo_work.stage, ProjectStage::Redo);
    assert_eq!(redo_work.target_task.as_deref(), Some("inventory"));
    assert_eq!(redo_work.document.version, split_work.document.version);
    assert_eq!(redo_work.document.sha256, split_work.document.sha256);
    let redo_ids: Vec<_> = redo_work.tasks.iter().map(|task| task.id.as_str()).collect();
    assert_eq!(redo_ids, vec!["inventory", "refund"]);
    assert!(!redo_ids.contains(&"audit"));

    let after = snapshot(&projects, &id);
    assert_eq!(after["phase"], "redoing");
    assert_eq!(after["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "audit").unwrap()["review"], "approved",
        "unrelated approved work must remain approved");
    for task_id in ["inventory", "refund"] {
        assert_eq!(after["tasks"].as_array().unwrap().iter()
            .find(|task| task["id"] == task_id).unwrap()["review"], "pending",
            "redo closure must require fresh review");
    }
    assert!(runs.iter().all(|run| run.detail["project_work"]["stage"] != "develop"),
        "redo must not auto-start development");
}

fn redo_fixture_report(work: &ProjectWork, include_unrelated: bool) -> Value {
    let mut tasks = vec![
        json!({"id": "inventory", "title": "Inventory repaired", "description": "Deduct safely",
            "acceptance": "Inventory remains strictly positive after repair", "depends_on": []}),
        json!({"id": "refund", "title": "Refund rechecked", "description": "Restore once after repaired inventory",
            "acceptance": "Duplicate refunds remain idempotent after redo", "depends_on": []}),
    ];
    if include_unrelated {
        tasks.push(json!({"id": "audit", "title": "Audit changed", "description": "Must be rejected",
            "acceptance": "Audit records are append-only", "depends_on": []}));
    }
    json!({
        "schema": "project-result/1", "project_id": work.project_id,
        "action_id": work.action_id, "stage": "redo",
        "document_version": work.document.version, "document_sha256": work.document.sha256,
        "tasks": tasks,
    })
}

#[test]
fn redo_report_requires_exit_and_reopens_only_affected_review_scope() {
    let temp = tempfile::tempdir().unwrap();
    let mut projects = Projects::open(&temp.path().join("workflow")).unwrap();
    let id = confirmed_project(&mut projects, temp.path());
    configure_fixture(&mut projects, temp.path());
    let mut host = Store::open(&temp.path().join("host/controller.sqlite"),
        "project-lifecycle-fixture", &[node()]).unwrap();
    let (split_run, split_work) = ingest_three_task_split_fixture(&mut projects, &mut host, &id);
    scripted_release_fixture_paused(&mut host, &split_run, "fixture-split-three-session");
    project_tick_with_absent_transport(&mut projects);
    approve(&mut projects, &id, "audit");
    approve(&mut projects, &id, "refund");
    projects.command(UiCommand::TaskReview {
        project_id: id.clone(), task_id: "inventory".into(), approve: false,
        reason: "inventory deduction is unsafe".into(),
    }).unwrap();
    projects.command(UiCommand::TaskRedo {
        project_id: id.clone(), task_id: "inventory".into(),
    }).unwrap();
    let redo_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "redo").unwrap();
    let redo_work: ProjectWork = serde_json::from_value(redo_run.detail["project_work"].clone()).unwrap();
    let dispatched = snapshot(&projects, &id);
    let ledger_path = temp.path().join("workflow/project-workflow/controller.sqlite");
    let ledger = rusqlite::Connection::open_with_flags(
        &ledger_path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let read_task_inputs = || -> Value {
        let raw: String = ledger.query_row(
            "SELECT value FROM project_settings WHERE key=?1", [format!("tasks:{id}")],
            |row| row.get(0)).unwrap();
        serde_json::from_str(&raw).unwrap()
    };
    let original_inputs = read_task_inputs();
    assert_eq!(original_inputs.as_array().unwrap().iter()
        .find(|task| task["id"] == "refund").unwrap()["depends_on"], json!(["inventory"]));
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, redo_run.id);
    host.receive(&signed_fixture(&redo_run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-redo-report-session"
    })), now()).unwrap();
    host.receive(&signed_fixture(&redo_run, "report", json!({
        "session": "fixture-redo-report-session", "report": {"project_result": redo_fixture_report(&redo_work, false)},
        "verification": {"passed": true, "stage_result_validated": true, "meaning": "synthetic fixture only"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    let before_release = snapshot(&projects, &id);
    assert_eq!(before_release["tasks"], dispatched["tasks"],
        "report alone must not replace any content or review decision");
    assert_eq!(read_task_inputs(), original_inputs,
        "report alone must not update durable dependency inputs");
    assert_eq!(before_release["can_start"], false);
    assert!(projects.command(UiCommand::ProjectStart { project_id: id.clone() }).is_err());
    assert_eq!(before_release["phase"], "redoing",
        "Redo report without independent release must remain unresolved");
    assert_eq!(before_release["actions"].as_array().unwrap().iter()
        .find(|action| action["kind"] == "redo").unwrap()["state"], "report_pending_exit");
    assert_eq!(before_release["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "inventory").unwrap()["title"], "Inventory");
    assert_eq!(before_release["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "inventory").unwrap()["acceptance"], "Inventory remains nonnegative");
    scripted_release_fixture_paused(&mut host, &redo_run, "fixture-redo-report-session");
    // A failing project-body write must roll back the associated task/dependency KV.
    // This trigger affects only this test's temporary ledger, never product source.
    let body_before: String = ledger.query_row(
        "SELECT body FROM projects WHERE id=?1", [&id], |row| row.get(0)).unwrap();
    let fault_ledger = rusqlite::Connection::open(&ledger_path).unwrap();
    fault_ledger.execute_batch("CREATE TRIGGER reject_redo_projection BEFORE UPDATE ON projects
        BEGIN SELECT RAISE(ABORT, 'fixture projection commit blocked'); END;").unwrap();
    let commit_error = projects.tick().expect_err("fixture blocks the project-body write");
    assert!(commit_error.to_string().contains("fixture projection commit blocked"),
        "{commit_error:#}");
    assert_eq!(snapshot(&projects, &id), before_release,
        "failed transaction must not publish a partial in-memory projection");
    assert_eq!(read_task_inputs(), original_inputs,
        "failed transaction must roll back the task-content/dependency update");
    let body_after: String = ledger.query_row(
        "SELECT body FROM projects WHERE id=?1", [&id], |row| row.get(0)).unwrap();
    assert_eq!(body_after, body_before);
    fault_ledger.execute_batch("DROP TRIGGER reject_redo_projection;").unwrap();
    drop(fault_ledger);
    project_tick_with_absent_transport(&mut projects);
    let after = snapshot(&projects, &id);
    assert_eq!(after["phase"], "task_review",
        "released Redo must reopen explicit review for affected tasks");
    assert_eq!(after["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "audit").unwrap()["review"], "approved");
    let inventory = after["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "inventory").unwrap();
    assert_eq!(inventory["title"], "Inventory repaired");
    assert_eq!(inventory["description"], "Deduct safely");
    assert_eq!(inventory["acceptance"], "Inventory remains strictly positive after repair");
    let refund = after["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "refund").unwrap();
    assert_eq!(refund["title"], "Refund rechecked");
    assert_eq!(refund["description"], "Restore once after repaired inventory");
    assert_eq!(refund["acceptance"], "Duplicate refunds remain idempotent after redo");
    for task_id in ["inventory", "refund"] {
        assert_eq!(after["tasks"].as_array().unwrap().iter()
            .find(|task| task["id"] == task_id).unwrap()["review"], "pending");
    }
    assert_eq!(after["document"]["version"], split_work.document.version);
    assert_eq!(after["document"]["sha256"], split_work.document.sha256);
    assert_eq!(after["actions"].as_array().unwrap().iter()
        .find(|action| action["kind"] == "redo").unwrap()["state"], "succeeded");
    assert_eq!(after["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "audit").unwrap(),
        dispatched["tasks"].as_array().unwrap().iter()
            .find(|task| task["id"] == "audit").unwrap(),
        "unrelated audit content, review and reason must remain unchanged");
    assert_eq!(read_task_inputs(), json!([
        {"id": "inventory", "title": "Inventory repaired", "description": "Deduct safely",
            "acceptance": "Inventory remains strictly positive after repair", "depends_on": []},
        {"id": "refund", "title": "Refund rechecked", "description": "Restore once after repaired inventory",
            "acceptance": "Duplicate refunds remain idempotent after redo", "depends_on": []},
        {"id": "audit", "title": "Audit", "description": "Record audit trail",
            "acceptance": "Audit records are append-only", "depends_on": []}
    ]), "the complete durable task set must include new closure fields and unchanged audit");
    assert!(host.list_runs(30).unwrap().iter().all(|run|
        run.detail["project_work"]["stage"] != "develop"),
        "Redo completion must not auto-start development");
    drop(projects);
    let mut reopened = Projects::open(&temp.path().join("workflow")).unwrap();
    let reopened_snapshot = snapshot(&reopened, &id);
    assert_eq!(reopened_snapshot["tasks"], after["tasks"],
        "Redo content and dependency projection must survive reopen");
    assert!(reopened.command(UiCommand::ProjectStart { project_id: id.clone() }).is_err(),
        "reopen must not bypass pending task reviews");
    approve(&mut reopened, &id, "inventory");
    assert!(reopened.command(UiCommand::ProjectStart { project_id: id.clone() }).is_err(),
        "reapproving only one affected task must not authorize the other");
    approve(&mut reopened, &id, "refund");
    assert!(host.list_runs(30).unwrap().iter().all(|run|
        run.detail["project_work"]["stage"] != "develop"),
        "even all approvals must not substitute for explicit Start");
    reopened.command(UiCommand::ProjectStart { project_id: id.clone() })
        .expect("affected tasks must be explicitly reapproved before Develop");
    let develop_run = host.list_runs(30).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "develop")
        .expect("explicit Start must dispatch Develop after reapproval");
    let develop_work: ProjectWork = serde_json::from_value(
        develop_run.detail["project_work"].clone()).unwrap();
    assert_eq!(develop_work.tasks.iter().find(|task| task.id == "inventory").unwrap(),
        &WorkTask { id: "inventory".into(), title: "Inventory repaired".into(),
            description: "Deduct safely".into(),
            acceptance: "Inventory remains strictly positive after repair".into(),
            depends_on: vec![], review: "approved".into(), reason: None });
    assert_eq!(develop_work.tasks.iter().find(|task| task.id == "refund").unwrap(),
        &WorkTask { id: "refund".into(), title: "Refund rechecked".into(),
            description: "Restore once after repaired inventory".into(),
            acceptance: "Duplicate refunds remain idempotent after redo".into(),
            depends_on: vec![], review: "approved".into(), reason: None });
    assert_eq!(develop_work.tasks.iter().find(|task| task.id == "audit").unwrap(),
        &WorkTask { id: "audit".into(), title: "Audit".into(),
            description: "Record audit trail".into(),
            acceptance: "Audit records are append-only".into(),
            depends_on: vec![], review: "approved".into(), reason: None },
        "Develop must preserve the complete unrelated approved task");
    assert_eq!(develop_work.tasks.len(), 3);
    assert_eq!(develop_work.document, split_work.document);
    assert!(host.list_runs(30).unwrap().into_iter().filter(|run|
        run.detail["project_work"]["stage"] == "develop").count() == 1,
        "Redo completion must not auto-start development");
}

#[test]
fn redo_report_rejects_unrelated_task_without_partial_projection() {
    let temp = tempfile::tempdir().unwrap();
    let mut projects = Projects::open(&temp.path().join("workflow")).unwrap();
    let id = confirmed_project(&mut projects, temp.path());
    configure_fixture(&mut projects, temp.path());
    let mut host = Store::open(&temp.path().join("host/controller.sqlite"),
        "project-lifecycle-fixture", &[node()]).unwrap();
    let (split_run, _) = ingest_three_task_split_fixture(&mut projects, &mut host, &id);
    scripted_release_fixture_paused(&mut host, &split_run, "fixture-split-three-session");
    project_tick_with_absent_transport(&mut projects);
    approve(&mut projects, &id, "audit");
    approve(&mut projects, &id, "refund");
    projects.command(UiCommand::TaskReview {
        project_id: id.clone(), task_id: "inventory".into(), approve: false,
        reason: "inventory deduction is unsafe".into(),
    }).unwrap();
    projects.command(UiCommand::TaskRedo {
        project_id: id.clone(), task_id: "inventory".into(),
    }).unwrap();
    let redo_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "redo").unwrap();
    let redo_work: ProjectWork = serde_json::from_value(redo_run.detail["project_work"].clone()).unwrap();
    host.assign_next(now()).unwrap();
    host.receive(&signed_fixture(&redo_run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-redo-invalid-session"
    })), now()).unwrap();
    host.receive(&signed_fixture(&redo_run, "report", json!({
        "session": "fixture-redo-invalid-session", "report": {"project_result": redo_fixture_report(&redo_work, true)},
        "verification": {"passed": true, "stage_result_validated": true, "meaning": "synthetic fixture only"}
    })), now()).unwrap();
    assert!(projects.tick().is_err(),
        "out-of-scope Redo report must be rejected before any projection");
    assert_eq!(snapshot(&projects, &id)["phase"], "redoing");
    scripted_release_fixture_paused(&mut host, &redo_run, "fixture-redo-invalid-session");
    assert!(projects.tick().is_err(),
        "out-of-scope Redo report must remain rejected after release");
    let after = snapshot(&projects, &id);
    assert_eq!(after["phase"], "redoing");
    assert_eq!(after["tasks"].as_array().unwrap().iter()
        .find(|task| task["id"] == "audit").unwrap()["review"], "approved");
}

#[test]
fn failed_test_requires_reason_and_dispatches_bounded_repair_with_exact_spec() {
    let temp = tempfile::tempdir().unwrap();
    let (mut projects, mut host, id, develop_run, develop_work) = development_fixture(temp.path());
    ingest_develop_fixture(&mut host, &develop_run, develop_fixture_report(&develop_work));
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &develop_run, "fixture-develop-session");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    let test_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test").unwrap();
    let test_work: ProjectWork = serde_json::from_value(test_run.detail["project_work"].clone()).unwrap();
    let frozen_spec = test_work.test_spec.clone().unwrap();
    let frozen_artifact = test_work.artifact.clone().unwrap();
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, test_run.id);
    host.receive(&signed_fixture(&test_run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-test-failure-session"
    })), now()).unwrap();
    host.receive(&signed_fixture(&test_run, "report", json!({
        "session": "fixture-test-failure-session",
        "report": {"project_result": test_fixture_report(&test_work, false)},
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "synthetic fixture only, NOT native proof"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    scripted_release_fixture_paused(&mut host, &test_run, "fixture-test-failure-session");
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["phase"], "test_failed");
    assert!(projects.command(UiCommand::ProjectRepair {
        project_id: id.clone(), reason: "   ".into()
    }).is_err(), "empty repair reason must be rejected");

    projects.command(UiCommand::ProjectRepair {
        project_id: id.clone(), reason: "fix failing inventory check".into()
    }).expect("explicit repair reason must dispatch a repair run");
    let repair_run = host.list_runs(20).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "repair")
        .expect("repair command must create a Repair work item");
    let repair_work: ProjectWork = serde_json::from_value(repair_run.detail["project_work"].clone()).unwrap();
    assert_eq!(repair_work.stage, ProjectStage::Repair);
    assert_eq!(repair_work.repair_round, 1);
    assert_eq!(repair_work.repair_reason.as_deref(), Some("fix failing inventory check"));
    assert_eq!(repair_work.document, test_work.document);
    assert_eq!(repair_work.test_spec.as_ref(), Some(&frozen_spec));
    assert_eq!(repair_work.artifact.as_ref(), Some(&frozen_artifact));
    assert_eq!(snapshot(&projects, &id)["phase"], "repairing");
}

// Continuous synthetic Projects/Store regression; NOT native worker or human acceptance proof.
#[test]
fn continuous_repair_retest_pass_requires_manual_accept_and_pins_pass_evidence() {
    use rusqlite::OptionalExtension;

    let temp = tempfile::tempdir().unwrap();
    let state = temp.path().join("workflow");
    let (mut projects, mut host, id, develop_run, develop_work) = development_fixture(temp.path());
    let run_count = |host: &Store| host.list_runs(200).unwrap().len();
    let ledger = rusqlite::Connection::open_with_flags(
        state.join("project-workflow/controller.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    ).unwrap();
    let setting = |prefix: &str| -> Option<Value> {
        let raw: Option<String> = ledger.query_row(
            "SELECT value FROM project_settings WHERE key=?1", [format!("{prefix}:{id}")],
            |row| row.get(0),
        ).optional().unwrap();
        raw.map(|raw| serde_json::from_str(&raw).unwrap())
    };
    let expected_spec = "# Project checks\n- inventory: Inventory remains nonnegative\n- refund: Duplicate refunds are idempotent\n";
    let expected_spec_hash = hex::encode(Sha256::digest(expected_spec.as_bytes()));
    let a1_bytes = br#"{"items":[]}"#;
    let a2_bytes = br#"{"items":["repaired"]}"#;
    let product = std::path::PathBuf::from(
        snapshot(&projects, &id)["product_path"].as_str().unwrap(),
    );
    let a1_path = product.join(&develop_work.action_id).join("inventory.json");
    assert_eq!(run_count(&host), 2, "fixture must have only Split and explicit Develop");
    assert!(setting("test-spec").is_none());
    assert!(setting("acceptance").is_none());

    // 1. Deliver real literal A1 bytes via the existing synthetic report boundary.
    ingest_develop_fixture(&mut host, &develop_run, develop_fixture_report(&develop_work));
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["phase"], "developing");
    assert!(!a1_path.exists(), "report without release must not materialize A1");
    scripted_release_fixture_paused(&mut host, &develop_run, "fixture-develop-session");
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(std::fs::read(&a1_path).unwrap().as_slice(), a1_bytes);
    assert_eq!(snapshot(&projects, &id)["phase"], "developed");
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectRefresh { project_id: id.clone() }).unwrap();
    assert_eq!(run_count(&host), 2, "Develop must not auto-dispatch Test");

    // 2. First explicit Test freezes S, then FAIL waits for a separate fixture release.
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    assert_eq!(run_count(&host), 3);
    let first_test = host.list_runs(200).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "test").unwrap();
    let first_work: ProjectWork = serde_json::from_value(first_test.detail["project_work"].clone()).unwrap();
    let spec = first_work.test_spec.clone().unwrap();
    let a1 = first_work.artifact.clone().unwrap();
    assert_eq!(first_work.project_id, id);
    assert_eq!(first_work.document, develop_work.document);
    assert_eq!(spec.text, expected_spec);
    assert_eq!(spec.sha256, expected_spec_hash);
    assert_eq!(a1.sha256, hex::encode(Sha256::digest(a1.text.as_bytes())));
    let expected_a1 = json!({
        "schema": "project-artifact-manifest/1", "files": [{
            "path": format!("{}/inventory.json", develop_work.action_id),
            "size": a1_bytes.len(), "sha256": hex::encode(Sha256::digest(a1_bytes))
        }]
    });
    assert_eq!(serde_json::from_str::<Value>(&a1.text).unwrap(), expected_a1);
    let frozen_spec_record = setting("test-spec").unwrap();
    assert_eq!(frozen_spec_record, json!({
        "document_version": develop_work.document.version,
        "document_sha256": develop_work.document.sha256, "input": spec
    }));
    assert_eq!(setting("test-artifact").unwrap()["input"], serde_json::to_value(&a1).unwrap());
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, first_test.id);
    host.receive(&signed_fixture(&first_test, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-continuous-first-test"
    })), now()).unwrap();
    host.receive(&signed_fixture(&first_test, "report", json!({
        "session": "fixture-continuous-first-test",
        "report": {"project_result": test_fixture_report(&first_work, false)},
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "SYNTHETIC, NOT native execution/exit proof"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    let pending_fail = snapshot(&projects, &id);
    assert_eq!(pending_fail["phase"], "testing");
    assert_eq!(pending_fail["actions"].as_array().unwrap().iter()
        .find(|action| action["id"] == first_work.action_id).unwrap()["state"], "report_pending_exit");
    assert!(projects.command(UiCommand::ProjectRepair {
        project_id: id.clone(), reason: "not released yet".into(),
    }).is_err());
    assert!(projects.command(UiCommand::ProjectAccept { project_id: id.clone() }).is_err());
    assert_eq!(run_count(&host), 3);
    scripted_release_fixture_paused(&mut host, &first_test, "fixture-continuous-first-test");
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["phase"], "test_failed");
    assert_eq!(snapshot(&projects, &id)["can_repair"], true);
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectRefresh { project_id: id.clone() }).unwrap();
    assert_eq!(run_count(&host), 3, "FAIL must not auto-dispatch Repair or Retest");
    assert!(setting("acceptance").is_none());

    // 3. Explicit Repair keeps S and old A1 as input; released A2 must be different.
    projects.command(UiCommand::ProjectRepair {
        project_id: id.clone(), reason: "repair failed inventory check".into(),
    }).unwrap();
    assert_eq!(run_count(&host), 4);
    let repair_run = host.list_runs(200).unwrap().into_iter()
        .find(|run| run.detail["project_work"]["stage"] == "repair").unwrap();
    let repair_work: ProjectWork = serde_json::from_value(repair_run.detail["project_work"].clone()).unwrap();
    assert_eq!(repair_work.project_id, id);
    assert_eq!(repair_work.document, develop_work.document);
    assert_eq!(repair_work.tasks, first_work.tasks);
    assert_eq!(repair_work.test_spec.as_ref(), Some(&spec));
    assert_eq!(repair_work.artifact.as_ref(), Some(&a1));
    assert_eq!(repair_work.repair_round, 1);
    assert_eq!(repair_work.repair_reason.as_deref(), Some("repair failed inventory check"));
    let a2_path = product.join(&repair_work.action_id).join("inventory.json");
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, repair_run.id);
    host.receive(&signed_fixture(&repair_run, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-repair-session"
    })), now()).unwrap();
    ingest_repair_fixture(&mut host, &repair_run, repair_fixture_report(&repair_work));
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["phase"], "repairing");
    assert!(!a2_path.exists(), "unreleased Repair report must not materialize A2");
    assert_eq!(setting("test-spec").unwrap(), frozen_spec_record);
    assert_eq!(setting("test-artifact").unwrap()["input"], serde_json::to_value(&a1).unwrap());
    assert!(projects.command(UiCommand::ProjectTest { project_id: id.clone() }).is_err());
    assert_eq!(run_count(&host), 4);
    scripted_release_fixture_paused(&mut host, &repair_run, "fixture-repair-session");
    project_tick_with_absent_transport(&mut projects);
    assert_eq!(snapshot(&projects, &id)["phase"], "developed");
    assert_eq!(std::fs::read(&a2_path).unwrap().as_slice(), a2_bytes);
    assert_eq!(std::fs::read(&a1_path).unwrap().as_slice(), a1_bytes,
        "Repair must not rewrite the old artifact evidence");
    assert_eq!(setting("test-spec").unwrap(), frozen_spec_record);
    project_tick_with_absent_transport(&mut projects);
    projects.command(UiCommand::ProjectRefresh { project_id: id.clone() }).unwrap();
    assert_eq!(run_count(&host), 4, "Repair completion must not auto-dispatch Retest");
    assert!(setting("acceptance").is_none());
    drop(projects);
    let mut projects = Projects::open(&state).unwrap();
    assert_eq!(snapshot(&projects, &id)["phase"], "developed");

    // 4. Explicit Retest after reopen must use the same S and the changed artifact.
    projects.command(UiCommand::ProjectTest { project_id: id.clone() }).unwrap();
    assert_eq!(run_count(&host), 5);
    let retest = host.list_runs(200).unwrap().into_iter().find(|run|
        run.detail["project_work"]["stage"] == "test" && run.id != first_test.id,
    ).unwrap();
    let retest_work: ProjectWork = serde_json::from_value(retest.detail["project_work"].clone()).unwrap();
    assert_eq!(retest_work.project_id, id);
    assert_eq!(retest_work.document, develop_work.document);
    assert_eq!(retest_work.tasks, first_work.tasks);
    assert_ne!(retest_work.action_id, first_work.action_id);
    assert_eq!(retest_work.test_spec.as_ref(), Some(&spec));
    assert_eq!(retest_work.test_spec.as_ref().unwrap().text, expected_spec);
    assert_eq!(retest_work.test_spec.as_ref().unwrap().sha256, expected_spec_hash);
    let a2 = retest_work.artifact.as_ref().unwrap();
    assert_ne!(a2, &a1, "Retest must not reuse stale A1");
    assert_eq!(a2.sha256, hex::encode(Sha256::digest(a2.text.as_bytes())));
    // Existing managed-product semantics retain immutable per-action artifacts;
    // check the complete two-file manifest, not just a substring containing A2.
    let mut expected_files = vec![
        json!({"path": format!("{}/inventory.json", develop_work.action_id),
            "size": a1_bytes.len(), "sha256": hex::encode(Sha256::digest(a1_bytes))}),
        json!({"path": format!("{}/inventory.json", repair_work.action_id),
            "size": a2_bytes.len(), "sha256": hex::encode(Sha256::digest(a2_bytes))}),
    ];
    expected_files.sort_by(|left, right| left["path"].as_str().unwrap().cmp(right["path"].as_str().unwrap()));
    assert_eq!(serde_json::from_str::<Value>(&a2.text).unwrap(), json!({
        "schema": "project-artifact-manifest/1", "files": expected_files
    }));
    assert_eq!(setting("test-spec").unwrap(), frozen_spec_record);
    assert_eq!(setting("test-artifact").unwrap(), json!({
        "document_version": develop_work.document.version,
        "document_sha256": develop_work.document.sha256, "input": a2
    }));
    assert_eq!(host.assign_next(now()).unwrap().unwrap().run_id, retest.id);
    host.receive(&signed_fixture(&retest, "status", json!({
        "state": "running", "spawn_intent": true, "session": "fixture-continuous-retest"
    })), now()).unwrap();
    host.receive(&signed_fixture(&retest, "report", json!({
        "session": "fixture-continuous-retest",
        "report": {"project_result": test_fixture_report(&retest_work, true)},
        "verification": {"passed": true, "stage_result_validated": true,
            "artifact_bytes_verified": true, "meaning": "SYNTHETIC, NOT native execution/exit proof"}
    })), now()).unwrap();
    project_tick_with_absent_transport(&mut projects);
    let pending_pass = snapshot(&projects, &id);
    assert_eq!(pending_pass["phase"], "testing");
    assert_eq!(pending_pass["can_accept"], false);
    assert_eq!(pending_pass["actions"].as_array().unwrap().iter()
        .find(|action| action["id"] == retest_work.action_id).unwrap()["state"], "report_pending_exit");
    assert!(projects.command(UiCommand::ProjectAccept { project_id: id.clone() }).is_err());
    assert!(setting("acceptance").is_none());
    assert_eq!(run_count(&host), 5);
    scripted_release_fixture_paused(&mut host, &retest, "fixture-continuous-retest");
    project_tick_with_absent_transport(&mut projects);
    for _ in 0..2 {
        project_tick_with_absent_transport(&mut projects);
        projects.command(UiCommand::ProjectRefresh { project_id: id.clone() }).unwrap();
    }
    let awaiting = snapshot(&projects, &id);
    assert_eq!(awaiting["phase"], "awaiting_acceptance");
    assert_eq!(awaiting["can_accept"], true);
    assert_eq!(awaiting["actions"].as_array().unwrap().iter().filter(|a| a["kind"] == "accept").count(), 0,
        "PASS may unlock manual acceptance but must never auto-accept");
    assert!(setting("acceptance").is_none());
    assert_eq!(run_count(&host), 5);
    drop(projects);
    let mut projects = Projects::open(&state).unwrap();
    assert_eq!(snapshot(&projects, &id)["phase"], "awaiting_acceptance");
    assert!(setting("acceptance").is_none(), "reopen must not silently accept");

    // 5. Only the explicit command accepts, without creating another host run.
    projects.command(UiCommand::ProjectAccept { project_id: id.clone() }).unwrap();
    let accepted = snapshot(&projects, &id);
    assert_eq!(accepted["phase"], "accepted");
    assert_eq!(accepted["actions"].as_array().unwrap().iter().filter(|a| a["kind"] == "accept").count(), 1);
    assert_eq!(run_count(&host), 5, "manual Accept must not dispatch work");
    let acceptance = setting("acceptance").unwrap();
    drop(projects);
    let mut reopened = Projects::open(&state).unwrap();
    assert_eq!(snapshot(&reopened, &id), accepted,
        "acceptance phase, action and document must all survive reopen");
    assert!(reopened.command(UiCommand::ProjectAccept { project_id: id.clone() }).is_err());
    assert_eq!(snapshot(&reopened, &id), accepted);
    assert_eq!(setting("acceptance").unwrap(), acceptance);
    assert_eq!(setting("test-spec").unwrap(), frozen_spec_record);
    assert_eq!(run_count(&host), 5, "reopen or duplicate Accept must not enqueue anything");
    assert_ne!(acceptance["test_action_id"], first_work.action_id,
        "the first completed Test was FAIL and cannot be the acceptance evidence");
    assert_eq!(acceptance["test_action_id"], retest_work.action_id,
        "acceptance must cite this project's Retest PASS, never its earlier completed FAIL");
    assert_eq!(acceptance, json!({
        "schema": "project-acceptance/1", "project_id": id,
        "document_version": develop_work.document.version,
        "document_sha256": develop_work.document.sha256,
        "test_action_id": retest_work.action_id,
        "test_spec_sha256": expected_spec_hash, "artifact_sha256": a2.sha256,
        "accepted": true
    }));
}
