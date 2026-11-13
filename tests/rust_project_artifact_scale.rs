//! Real-page artifact scale contract (linux221 run bc1cf37d regression).
//!
//! The 7168-byte ARTIFACT_TOTAL_BUDGET was sized for sum-v1 fixtures; a real
//! generated static page (project-work-v1, four files, CJK content) exceeded
//! it and the child failed with "model output exceeds artifact budget".
//! These tests pin the raised chain end to end at host-test scope:
//! parse accepts a 4-file ~30KiB page; the enriched terminal report (artifact
//! package rides inside as content_hex) fits the shared canonical report
//! bound; the manager-side materializer round-trips it; over-budget output
//! still fails closed.

use agentranch::core::owner_project::{self, ARTIFACT_TOTAL_BUDGET};
use agentranch::core::protocol::ExecutionIdentity;
use agentranch::core::project_work::ProjectWork;
use agentranch::core::worker_owner::{self, OwnerVerification};
use serde_json::json;
use sha2::{Digest, Sha256};

/// Four-file real page ~`total` bytes (CJK-heavy, like the Chengdu outing
/// page that failed in run bc1cf37d).
fn real_page_json(total: usize) -> String {
    let per_file = total / 4;
    let unit_len = "成都遛娃好去处推荐".len(); // 27 bytes UTF-8
    let filler = "成都遛娃好去处推荐".repeat(per_file / unit_len + 1);
    let mut files = Vec::new();
    for path in ["index.html", "css/style.css", "js/data.js", "js/app.js"] {
        let content = format!("/* {path} */ {filler}");
        files.push(json!({"path": path, "content": content}));
    }
    json!({"files": files}).to_string()
}

fn workspace() -> std::path::PathBuf {
    let dir = std::env::temp_dir().join(format!("aw-scale-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

/// A minimal but fully valid ProjectWork at the develop stage with a real
/// confirmed document (hash matches text, version >= 1).
fn work() -> ProjectWork {
    let text = "# 设计\n\n成都遛娃静态页面任务。\n".to_owned();
    let sha256 = hex::encode(Sha256::digest(text.as_bytes()));
    serde_json::from_value(json!({
        "schema": "project-work/1",
        "project_id": "p1", "action_id": "a1",
        "stage": "develop",
        "document": {"version": 1, "sha256": sha256, "text": text},
        "tasks": [{
            "id": "t1", "title": "生成页面", "description": "生成静态页面文件",
            "acceptance": "index.html 存在且引用样式与数据", "depends_on": [],
            "review": "approved", "reason": null
        }],
        "target_task": null, "repair_reason": null, "repair_round": 0,
        "test_spec": null, "artifact": null
    })).unwrap()
}

fn execution() -> ExecutionIdentity {
    ExecutionIdentity {
        deployment: "d".into(), run_id: "r1".into(), attempt: 1, epoch: "e".into(),
        session: Some("native-s1".into()), source_sha256: None,
        artifact_sha256: Some("46949ce7".repeat(8)),
    }
}

#[test]
fn real_page_scale_parse_and_budget() {
    assert!(ARTIFACT_TOTAL_BUDGET >= 32768, "artifact budget must carry a real 4-file page");
    let parsed = owner_project::ModelFiles::parse(&real_page_json(30000), ARTIFACT_TOTAL_BUDGET)
        .expect("30KiB real page must parse within the raised budget");
    assert_eq!(parsed.files.len(), 4);
    let over = real_page_json(ARTIFACT_TOTAL_BUDGET + 4096);
    assert!(owner_project::ModelFiles::parse(&over, ARTIFACT_TOTAL_BUDGET).is_err(),
        "over-budget output must still fail closed");
}

#[test]
fn real_page_scale_report_and_materialize_roundtrip() {
    let work = work();
    work.validate().unwrap();
    let dir = workspace();
    let parsed = owner_project::ModelFiles::parse(&real_page_json(30000), ARTIFACT_TOTAL_BUDGET).unwrap();
    let written = parsed.write_within(&dir).unwrap();
    assert_eq!(written.len(), 4);
    let manifest: Vec<serde_json::Value> = parsed.files.iter().map(|file| {
        let bytes = file.content.as_bytes();
        json!({"path": file.path, "size": bytes.len(), "sha256": owner_project::evidence_digest(bytes)})
    }).collect();
    let evidence = json!({"model":"m","request_sha256":"0".repeat(64),"response_sha256":"0".repeat(64),
        "elapsed_ms":1,"status":200});
    let report = owner_project::project_report("native-s1", &work, &evidence, &manifest).unwrap();
    let manifest = report["files"].clone();
    let package = owner_project::package_workspace_files(&manifest, &dir, &work)
        .expect("workspace packaging must accept the real page scale");
    let mut enriched = report.clone();
    enriched["artifact"] = package;

    // The enriched report (content_hex inside) is what worker_owner hashes and
    // the manager store revalidates; both share one canonical bound that must
    // admit this payload.
    let verification: OwnerVerification = serde_json::from_value(json!({
        "passed": true, "state": "verified",
        "details": {"source": "native-project-verifier"}
    })).unwrap();
    worker_owner::owner_report_sha256(&execution(), &enriched, &verification)
        .expect("enriched terminal report with a real page must fit the report bound");

    // Manager-side materialization round-trips the same package.
    let target = dir.join("out");
    let names = owner_project::materialize_artifact_package(&enriched["artifact"], &target)
        .expect("materializer must accept the real page scale");
    assert_eq!(names.len(), 4);
    let _ = std::fs::remove_dir_all(&dir);
}
