//! project-work-v1 owner-child contract. These tests never execute Linux,
//! SSH, or a real model: they pin the child-side template routing, the
//! work.json binding, and the model-response file manifest validation that
//! turns untrusted model output into managed static files.
use agentranch::core::project_work::{ConfirmedDocument, ProjectStage, ProjectWork, WorkTask};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::process::Command;

fn work() -> ProjectWork {
    let text = "# 成都遛娃静态页\n## §5 稳定选择器\n#venue-list .venue-card\n".to_owned();
    ProjectWork {
        schema: "project-work/1".into(), project_id: "doc-project".into(), action_id: "a1".into(),
        stage: ProjectStage::Develop,
        document: ConfirmedDocument { version: 1, sha256: hex::encode(Sha256::digest(text.as_bytes())), text },
        tasks: vec![WorkTask {
            id: "impl-page".into(), title: "实现设计静态页".into(),
            description: "按设计文档生成静态文件".into(),
            acceptance: "产物包含稳定选择器；总大小 ≤16KB".into(),
            depends_on: vec![], review: "approved".into(), reason: None,
        }],
        target_task: Some("impl-page".into()), repair_reason: None, repair_round: 0,
        test_spec: None, artifact: None,
    }
}

/// The child must reject an unknown template argv before reading any gate or
/// stdin: only sum-v1 and project-work-v1 are fixed entry points.
#[test]
fn owner_child_rejects_unknown_template_argv() {
    for argv in [["owner-child", "poem-v1", "session-1"].as_slice(), ["owner-child", "project-work-v1"].as_slice()] {
        let output = Command::new(env!("CARGO_BIN_EXE_aw-cli"))
            .args(argv).output().unwrap();
        assert!(!output.status.success(), "argv {argv:?} must be rejected");
    }
}

/// The project child requires the work.json binding (session + project_work)
/// from its managed work directory after the owner gate; a missing or
/// mismatched binding fails closed before any model interaction.
/// Platform-independent CLI boundary test, runs on the host too.
#[test]
fn owner_child_project_mode_requires_work_binding() {
    use std::io::Write;
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    // Absolute directory without work.json: fails closed on the missing
    // binding, never on the directory path shape.
    let empty_dir = root.path().to_string_lossy().into_owned();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aw-cli"))
        .args(["owner-child", "project-work-v1", empty_dir.as_str()])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"G").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "work binding is required");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(error.contains("work.json") || error.contains("work binding")
        || error.contains("binding requires the Linux worker environment"), "{error}");
}

/// The owner launcher (`owner_exec` argv contract) sends the project template
/// as [owner-child, project-work-v1, ABSOLUTE_WORK_DIR] where the directory
/// holds work.json; only sum-v1 carries a session string in that slot. The
/// child must accept the directory form past its argv validation and fail on
/// the *missing work.json binding* instead — an absolute directory path must
/// never be rejected as an "invalid owner session". Regression for the
/// linux221 run a8fc6059 which died with empty stdout ("invalid owner
/// session") before ever reading work.json.
#[test]
fn owner_child_project_mode_accepts_absolute_work_directory() {
    use std::io::Write;
    use std::process::Stdio;
    let root = tempfile::tempdir().unwrap();
    // Deliberately a real absolute directory (with path separators) that does
    // NOT contain work.json: the child must get past argv/session validation
    // and fail on the missing binding, not on the path shape.
    let empty_dir = root.path().join("nested").join("work");
    std::fs::create_dir_all(&empty_dir).unwrap();
    let dir_arg = empty_dir.to_string_lossy().into_owned();
    let mut child = Command::new(env!("CARGO_BIN_EXE_aw-cli"))
        .args(["owner-child", "project-work-v1", dir_arg.as_str()])
        .stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::piped()).spawn().unwrap();
    child.stdin.take().unwrap().write_all(b"G").unwrap();
    let output = child.wait_with_output().unwrap();
    assert!(!output.status.success(), "missing work.json must fail closed");
    let error = String::from_utf8_lossy(&output.stderr);
    assert!(!error.to_string().contains("invalid owner session"),
        "absolute directory must pass argv validation: {error}");
    // On non-Linux hosts the refusal is the platform guard; on Linux it is the
    // missing work.json binding. Both mention the binding/environment, never
    // the session validator.
    assert!(error.contains("work.json") || error.contains("work binding")
        || error.contains("binding requires the Linux worker environment"), "{error}");
}

/// File-manifest validation is pure logic shared by the child: model output
/// is untrusted input and every file entry must pass the managed-path rules
/// and size budget before anything is written.
#[test]
fn model_response_files_manifest_is_validated() {
    use agentranch::core::owner_project::ModelFiles;
    let index_html = b"<html><body id=\"top\"></body></html>";
    let valid = json!({"files": [
        {"path": "index.html", "content": String::from_utf8_lossy(index_html)},
        {"path": "css/style.css", "content": "body{margin:0}"},
        {"path": "js/data.js", "content": "window.DATA=[];"},
        {"path": "js/app.js", "content": "console.log(1);"}
    ]});
    let files = ModelFiles::parse(&valid.to_string(), 16384).unwrap();
    assert_eq!(files.files.len(), 4);
    assert_eq!(files.files[0].path, "index.html");

    // escape attempts
    for bad in [
        json!({"files":[{"path":"../escape.js","content":"x"}]}),
        json!({"files":[{"path":"/abs.js","content":"x"}]}),
        json!({"files":[{"path":"a/../../escape.js","content":"x"}]}),
        json!({"files":[{"path":"C:\\\\win.js","content":"x"}]}),
        json!({"files":[{"path":"","content":"x"}]}),
        json!({"files":[{"path":"index.html","content":"x"},{"path":"INDEX.HTML","content":"y"}]}),
        json!({"files":[]}),
        json!({"files":[{"path":"ok.js","content":"x"}],"extra":"smuggled"}),
        json!({}),
    ] {
        assert!(ModelFiles::parse(&bad.to_string(), 16384).is_err(), "must reject: {bad}");
    }

    // size budget: total across files
    let big = json!({"files":[{"path":"a.js","content":"x".repeat(9000)},{"path":"b.js","content":"y".repeat(9000)}]});
    assert!(ModelFiles::parse(&big.to_string(), 16384).is_err(), "total budget enforced");

    // non-UTF8 content rejected via JSON type error (content must be a string)
    let not_string = json!({"files":[{"path":"a.js","content":123}]});
    assert!(ModelFiles::parse(&not_string.to_string(), 16384).is_err());
}

/// The terminal report the project child prints must bind the run identity
/// and carry the model-call evidence without ever containing the token.
#[test]
fn project_report_shape_has_model_evidence_and_no_token() {
    use agentranch::core::owner_project::project_report;
    use agentranch::core::owner_project::evidence_digest;
    let report = project_report(
        "session-1",
        &work(),
        &json!({"model":"gpt-6.1-sol","request_sha256":"00".repeat(32),"response_sha256":"11".repeat(32),"elapsed_ms":1234,"status":200}),
        &[json!({"path":"index.html","size":13,"sha256":evidence_digest(b"<html></html>")})],
    ).unwrap();
    assert_eq!(report["schema"], "native-project/1");
    assert_eq!(report["session"], "session-1");
    assert_eq!(report["project_id"], "doc-project");
    assert_eq!(report["document_sha256"], work().document.sha256);
    assert_eq!(report["files"][0]["path"], "index.html");
    assert!(report["model"]["model"].as_str().unwrap().contains("gpt"));
    let text = report.to_string();
    assert!(!text.contains("Bearer"), "no credential in report");
    assert!(!text.to_lowercase().contains("token"), "no token in report: {text}");
}

/// fixed_child routes the project template instead of computing the sum.
/// Covered by the CLI boundary test above (owner_child_project_mode_requires_
/// work_binding): the child accepts the argv, consumes the gate, then fails
/// closed demanding the Linux worker environment with a work.json binding.

/// The prompt sent to the model is exactly ProjectWork::prompt() plus the
/// strict output contract; the document text rides inside as data.
#[test]
fn project_prompt_binds_document_and_output_contract() {
    let prompt = agentranch::core::owner_project::model_prompt(&work()).unwrap();
    assert!(prompt.contains("PROJECT_WORK_JSON"));
    assert!(prompt.contains(&work().document.sha256));
    assert!(prompt.contains("files"));
    assert!(!prompt.to_lowercase().contains("bearer"), "no credential in prompt");
    let serialized = serde_json::to_string(&work()).unwrap();
    assert!(prompt.contains(&serialized), "full work JSON travels as data");
}

/// Model output often arrives wrapped in markdown fences despite the
/// contract; stripping exactly one outer fence is allowed, anything else
/// (multiple fences, mixed content) must fail closed.
#[test]
fn model_output_fence_stripping_is_bounded() {
    use agentranch::core::owner_project::strip_outer_fence;
    let inner = r#"{"files":[{"path":"index.html","content":"x"}]}"#;
    assert_eq!(strip_outer_fence(inner).unwrap(), inner);
    assert_eq!(strip_outer_fence(&format!("```json\n{inner}\n```")).unwrap(), inner);
    assert_eq!(strip_outer_fence(&format!("```\n{inner}\n```")).unwrap(), inner);
    assert!(strip_outer_fence(&format!("```json\n{inner}\n```text\nmore")).is_err());
    assert!(strip_outer_fence("before ```json\n{}\n```").is_err(), "{:?}", "leading text");
    assert!(strip_outer_fence(&format!("{inner}\n```")).is_err());
}

/// The runner packages the actual managed files from the run workspace and
/// cross-checks them against the child's stdout manifest: sizes, hashes and
/// the exact path set must agree; the child's claims are never trusted.
#[test]
fn workspace_packaging_cross_checks_child_manifest() {
    use agentranch::core::owner_project::package_workspace_files;
    let root = tempfile::tempdir().unwrap();
    let workspace = root.path();
    std::fs::write(workspace.join("index.html"), b"<html></html>").unwrap();
    std::fs::create_dir(workspace.join("css")).unwrap();
    std::fs::write(workspace.join("css/style.css"), b"body{}").unwrap();
    let manifest = json!([
        {"path":"index.html","size":13,"sha256":hex::encode(Sha256::digest(b"<html></html>"))},
        {"path":"css/style.css","size":6,"sha256":hex::encode(Sha256::digest(b"body{}"))}
    ]);
    let package = package_workspace_files(&manifest, workspace, &work()).unwrap();
    assert_eq!(package["schema"], "project-artifact-package/1");
    assert_eq!(package["document_sha256"], work().document.sha256);
    assert_eq!(package["files"].as_array().unwrap().len(), 2);
    // files are sorted by path: css/style.css before index.html
    assert_eq!(package["files"][0]["path"], "css/style.css");
    assert_eq!(package["files"][0]["content_hex"], hex::encode("body{}".as_bytes()));
    assert_eq!(package["files"][1]["path"], "index.html");
    assert_eq!(package["files"][1]["content_hex"], hex::encode("<html></html>".as_bytes()));

    // tampered manifest: wrong hash
    let mut bad = manifest.clone();
    bad[0]["sha256"] = json!("00".repeat(32));
    assert!(package_workspace_files(&bad, workspace, &work()).is_err());

    // missing file on disk
    let mut missing = manifest.clone();
    missing[0]["path"] = json!("gone.html");
    assert!(package_workspace_files(&missing, workspace, &work()).is_err());

    // extra file on disk not in manifest is refused
    std::fs::write(workspace.join("extra.js"), b"y").unwrap();
    assert!(package_workspace_files(&manifest, workspace, &work()).is_err());
}

/// The manager materializes the admitted artifact package (content_hex from
/// the signed terminal report) into state_dir/artifacts/<run_id>/ and
/// re-verifies every file hash against the package before writing.
#[test]
fn manager_materializes_admitted_artifact_package() {
    use agentranch::core::owner_project::materialize_artifact_package;
    let root = tempfile::tempdir().unwrap();
    let html = "<html id=\"top\"></html>".as_bytes().to_vec();
    let css = "body{margin:0}".as_bytes().to_vec();
    let package = json!({"schema":"project-artifact-package/1","project_id":"doc-project","action_id":"a1",
        "document_version":1,"document_sha256":work().document.sha256,
        "files":[
            {"path":"css/style.css","size":css.len(),"sha256":hex::encode(Sha256::digest(&css)),"content_hex":hex::encode(&css)},
            {"path":"index.html","size":html.len(),"sha256":hex::encode(Sha256::digest(&html)),"content_hex":hex::encode(&html)}
        ]});
    let target = root.path().join("artifacts").join("run-1");
    let written = materialize_artifact_package(&package, &target).unwrap();
    assert_eq!(written.len(), 2);
    assert_eq!(std::fs::read(target.join("index.html")).unwrap(), html);
    assert_eq!(std::fs::read(target.join("css/style.css")).unwrap(), css);

    // tampered content_hex fails the hash check and writes nothing further
    let mut bad = package.clone();
    bad["files"][1]["content_hex"] = json!(hex::encode(b"tampered".to_vec()));
    let target2 = root.path().join("artifacts").join("run-2");
    assert!(materialize_artifact_package(&bad, &target2).is_err());
    assert!(!target2.exists(), "failed package must not leave partial artifacts");

    // escape attempt inside the package
    let mut escape = package.clone();
    escape["files"][0]["path"] = json!("../../escape.css");
    let target3 = root.path().join("artifacts").join("run-3");
    assert!(materialize_artifact_package(&escape, &target3).is_err());
    assert!(!target3.exists());
}
