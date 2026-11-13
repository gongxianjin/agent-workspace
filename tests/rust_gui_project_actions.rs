#![cfg(feature = "gui")]
//! W2 GUI selector regression draft.
//!
//! These tests drive actual egui pointer events through Workbench::show and the
//! real CoreService/Projects/Store boundary. The AO/Reason endpoint is an
//! intentionally absent fixture endpoint: no external service, native worker,
//! model, or product acceptance is started. Synthetic persisted project rows,
//! signed-run ledgers, and fixture artifacts are not native execution proof.
//!
//! Install only after the Worker W1 compiler window is released:
//!   copy this file to tests/rust_gui_project_actions.rs
//! Then run in the single Cargo lane with the default gui feature:
//!   cargo test --test rust_gui_project_actions -- --test-threads=1

#[path = "../src/desktop/mod.rs"]
mod desktop;
#[path = "support/lifecycle_reason.rs"]
mod lifecycle_reason;

use agentranch::{
    config::Mode,
    core::{
        projects::Projects,
        service::{ServiceOptions, UiCommand},
    },
};
use eframe::egui;
use rusqlite::Connection;
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use std::{path::{Path, PathBuf}, time::{Duration, Instant}};

const NODE_KEY: &str = "07";
const DOCUMENT: &str = "# 已定稿设计\n库存扣减、退款补偿与审计记录。\n";
const REASON_ENDPOINT: &str = r"\\.\pipe\absent-gui-actions-w2";

fn sha(text: &str) -> String {
    hex::encode(Sha256::digest(text.as_bytes()))
}

fn config_fixture(root: &Path, endpoint: &str, deployment: &str) -> PathBuf {
    std::fs::write(root.join("node.key"), NODE_KEY.repeat(32)).unwrap();
    let config = json!({
        "schema": "ao-reason-config/1",
        "mode": "real",
        "role": "controller",
        "deployment": deployment,
        "state_dir": root.join("host"),
        "nodes": [{
            "id": "B",
            "address": "owned-B/runner",
            "key_file": "node.key",
            "slots": 1,
            "templates": ["project-work-v1"],
            "admitted": true,
            "healthy": true
        }],
        "ao": {"base_url": "http://127.0.0.1:1", "project_id": "fixture", "model": "fixture-only"},
        "reason_endpoint": endpoint,
        "endpoint": "owned-A/controller"
    });
    let path = root.join("controller.json");
    std::fs::write(&path, serde_json::to_vec(&config).unwrap()).unwrap();
    path
}

fn project_row(state: &Path, id: &str) -> Value {
    let db = Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    let body: String = db.query_row("SELECT body FROM projects WHERE id=?1", [id], |row| row.get(0)).unwrap();
    serde_json::from_str(&body).unwrap()
}

fn put_setting(state: &Path, key: &str, value: Value) {
    let db = Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    db.execute(
        "INSERT INTO project_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        rusqlite::params![key, value.to_string()],
    ).unwrap();
}

fn put_raw_setting(state: &Path, key: &str, value: &str) {
    let db = Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    db.execute(
        "INSERT INTO project_settings(key,value) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET value=excluded.value",
        rusqlite::params![key, value],
    ).unwrap();
}

fn put_project(state: &Path, mut row: Value, config: &Path, settings: &[(&str, Value)]) -> String {
    let id = row["id"].as_str().unwrap().to_owned();
    let db = Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    row["phase"] = row["phase"].clone();
    db.execute("UPDATE projects SET body=?1 WHERE id=?2", rusqlite::params![row.to_string(), id]).unwrap();
    put_raw_setting(state, "config", &config.to_string_lossy());
    for (key, value) in settings { put_setting(state, key, value.clone()); }
    id
}

fn action(id: &str, kind: &str, state: &str, detail: &str) -> Value {
    json!({"id": id, "kind": kind, "state": state, "session": null, "detail": detail})
}

fn base_row(state: &Path, name: &str) -> (String, Value) {
    let mut projects = Projects::open(state).unwrap();
    let id = projects.command(UiCommand::ProjectCreate { name: name.into() }).unwrap()["id"].as_str().unwrap().to_owned();
    drop(projects);
    let row = project_row(state, &id);
    (id, row)
}

fn confirmed_document() -> Value {
    json!({"version": 1, "filename": "design.md", "sha256": sha(DOCUMENT), "text": DOCUMENT, "confirmed": true})
}

fn task_inputs() -> Value {
    json!([
        {"id":"inventory","title":"库存扣减","description":"订单原子扣减库存","acceptance":"库存不为负","depends_on":[]},
        {"id":"refund","title":"退款补偿","description":"退款仅恢复一次库存","acceptance":"重复退款幂等","depends_on":["inventory"]}
    ])
}

fn developed_fixture_with_endpoint(root: &Path, endpoint: &str, deployment: &str) -> (PathBuf, String) {
    let state = root.join("workflow");
    let (_id, mut row) = base_row(&state, "GUI Test fixture");
    let config = config_fixture(root, endpoint, deployment);
    let product = PathBuf::from(row["product_path"].as_str().unwrap());
    std::fs::write(product.join("inventory.json"), br#"{"items":[]}"#).unwrap();
    row["phase"] = json!("developed");
    row["document"] = confirmed_document();
    row["tasks"] = json!([
        {"id":"inventory","title":"库存扣减","description":"订单原子扣减库存","acceptance":"库存不为负","review":"approved","reason":null},
        {"id":"refund","title":"退款补偿","description":"退款仅恢复一次库存","acceptance":"重复退款幂等","review":"approved","reason":null}
    ]);
    row["actions"] = json!([
        action("develop-action", "develop", "succeeded", "synthetic fixture artifact"),
    ]);
    row["blockers"] = json!([]);
    let id = put_project(&state, row, &config, &[]);
    put_setting(&state, &format!("tasks:{id}"), task_inputs());
    (state, id)
}

fn developed_fixture(root: &Path) -> (PathBuf, String) {
    developed_fixture_with_endpoint(root, REASON_ENDPOINT, "gui-actions-w2")
}

fn test_failed_fixture_with_endpoint(root: &Path, endpoint: &str, deployment: &str) -> (PathBuf, String) {
    let state = root.join("workflow");
    let (id, mut row) = base_row(&state, "GUI Repair fixture");
    let config = config_fixture(root, endpoint, deployment);
    row["phase"] = json!("test_failed");
    row["document"] = confirmed_document();
    row["tasks"] = json!([
        {"id":"inventory","title":"库存扣减","description":"订单原子扣减库存","acceptance":"库存不为负","review":"approved","reason":null},
        {"id":"refund","title":"退款补偿","description":"退款仅恢复一次库存","acceptance":"重复退款幂等","review":"approved","reason":null}
    ]);
    row["actions"] = json!([
        action("develop-action", "develop", "succeeded", "synthetic fixture artifact"),
        action("test-action", "test", "succeeded", "synthetic failed test report"),
    ]);
    row["blockers"] = json!(["synthetic test failure"]);
    let spec = "# Project checks\n- inventory: Inventory remains nonnegative\n- refund: Duplicate refunds are idempotent\n";
    let artifact = r#"{"schema":"project-artifact-manifest/1","files":[{"path":"inventory.json","size":12,"sha256":"fixture"}]}"#;
    let settings = [
        ("test-spec", json!({"document_version":1,"document_sha256":sha(DOCUMENT),"input":{"sha256":sha(spec),"text":spec}})),
        ("test-artifact", json!({"document_version":1,"document_sha256":sha(DOCUMENT),"input":{"sha256":sha(artifact),"text":artifact}})),
    ];
    let keys: Vec<(String, Value)> = settings.into_iter().map(|(key, value)| (format!("{key}:{id}"), value)).collect();
    let refs: Vec<(&str, Value)> = keys.iter().map(|(key, value)| (key.as_str(), value.clone())).collect();
    let id = put_project(&state, row, &config, &refs);
    put_setting(&state, &format!("tasks:{id}"), task_inputs());
    (state, id)
}

fn acceptance_fixture(root: &Path) -> (PathBuf, String) {
    let state = root.join("workflow");
    let (id, mut row) = base_row(&state, "GUI Accept fixture");
    let config = config_fixture(root, REASON_ENDPOINT, "gui-actions-w2");
    row["phase"] = json!("awaiting_acceptance");
    row["document"] = confirmed_document();
    row["tasks"] = json!([
        {"id":"inventory","title":"库存扣减","description":"订单原子扣减库存","acceptance":"库存不为负","review":"approved","reason":null},
        {"id":"refund","title":"退款补偿","description":"退款仅恢复一次库存","acceptance":"重复退款幂等","review":"approved","reason":null}
    ]);
    row["actions"] = json!([
        action("develop-action", "develop", "succeeded", "synthetic fixture artifact"),
        action("test-action", "test", "succeeded", "synthetic passing test report"),
    ]);
    row["blockers"] = json!([]);
    let spec = "# Project checks\n- inventory: Inventory remains nonnegative\n- refund: Duplicate refunds are idempotent\n";
    let artifact = "{\"schema\":\"project-artifact-manifest/1\",\"files\":[]}";
    let settings = [
        (format!("test-spec:{id}"), json!({"document_version":1,"document_sha256":sha(DOCUMENT),"input":{"sha256":sha(spec),"text":spec}})),
        (format!("test-artifact:{id}"), json!({"document_version":1,"document_sha256":sha(DOCUMENT),"input":{"sha256":sha(artifact),"text":artifact}})),
    ];
    let refs: Vec<(&str, Value)> = settings.iter().map(|(key, value)| (key.as_str(), value.clone())).collect();
    let id = put_project(&state, row, &config, &refs);
    (state, id)
}

fn redo_fixture(root: &Path) -> (PathBuf, String) {
    let state = root.join("workflow");
    let (_id, mut row) = base_row(&state, "GUI Redo fixture");
    let config = config_fixture(root, REASON_ENDPOINT, "gui-actions-w2");
    row["phase"] = json!("task_review");
    row["document"] = confirmed_document();
    row["tasks"] = json!([
        {"id":"inventory","title":"库存扣减","description":"订单原子扣减库存","acceptance":"库存不为负","review":"rejected","reason":"库存扣减不安全"},
        {"id":"refund","title":"退款补偿","description":"订单退款恢复库存","acceptance":"重复退款幂等","review":"approved","reason":null},
        {"id":"audit","title":"审计记录","description":"记录审计事件","acceptance":"审计记录只追加","review":"approved","reason":null}
    ]);
    row["actions"] = json!([
        action("split-action", "split", "succeeded", "synthetic split fixture"),
    ]);
    row["blockers"] = json!([]);
    let tasks = json!([
        {"id":"inventory","title":"库存扣减","description":"订单原子扣减库存","acceptance":"库存不为负","depends_on":[]},
        {"id":"refund","title":"退款补偿","description":"订单退款恢复库存","acceptance":"重复退款幂等","depends_on":["inventory"]},
        {"id":"audit","title":"审计记录","description":"记录审计事件","acceptance":"审计记录只追加","depends_on":[]}
    ]);
    let id = put_project(&state, row, &config, &[("tasks", tasks.clone())]);
    // The command looks up the project-scoped task input, not a global key.
    put_setting(&state, &format!("tasks:{id}"), tasks);
    (state, id)
}

fn app(state: &Path) -> (desktop::Workbench, egui::Context) {
    let ctx = egui::Context::default();
    let app = desktop::Workbench::new(
        ServiceOptions { mode: Mode::Real, state_dir: state.into(), worker_exe: env!("CARGO_BIN_EXE_aw-cli").into(), gateway_key: None, gateway_model_token: None },
        &ctx,
    ).unwrap();
    ctx.style_mut_of(egui::Theme::Light, |style| style.animation_time = 0.0);
    (app, ctx)
}

fn wait(app: &mut desktop::Workbench, predicate: impl Fn(&agentranch::core::service::AppSnapshot) -> bool) {
    let until = Instant::now() + Duration::from_secs(8);
    while Instant::now() < until {
        app.poll();
        if predicate(&app.snapshot()) && app.pending_count() == 0 { return; }
        std::thread::sleep(Duration::from_millis(10));
    }
    panic!("GUI fixture did not converge: {:?}", app.snapshot());
}

fn frame(app: &mut desktop::Workbench, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    let mut output = ctx.run_ui(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100., 1200.))),
        events,
        ..Default::default()
    }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.show(ui)); });
    output.textures_delta.clear();
    output
}

fn texts(output: &egui::FullOutput) -> Vec<(String, egui::Rect)> {
    fn gather(shape: &egui::epaint::Shape, clip: egui::Rect, found: &mut Vec<(String, egui::Rect)>) {
        match shape {
            egui::epaint::Shape::Text(s) => {
                let rect = egui::Rect::from_min_size(s.pos, s.galley.size());
                if clip.contains(rect.center()) { found.push((s.galley.job.text.clone(), rect)); }
            }
            egui::epaint::Shape::Vec(shapes) => for shape in shapes { gather(shape, clip, found); },
            _ => {}
        }
    }
    let mut found = Vec::new();
    for shape in &output.shapes { gather(&shape.shape, shape.clip_rect, &mut found); }
    found
}

fn scroll(app: &mut desktop::Workbench, ctx: &egui::Context, delta: f32) {
    frame(app, ctx, vec![
        egui::Event::PointerMoved(egui::pos2(1040., 420.)),
        egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0., delta),
            phase: egui::TouchPhase::Move,
            source: egui::MouseWheelSource::Wheel,
            modifiers: Default::default(),
        },
    ]);
    for _ in 0..12 { frame(app, ctx, vec![]); }
}

fn seek(app: &mut desktop::Workbench, ctx: &egui::Context, label: &str) -> egui::Rect {
    scroll(app, ctx, 10000.);
    for _ in 0..90 {
        let output = frame(app, ctx, vec![]);
        if let Some((_, rect)) = texts(&output).into_iter().find(|(text, _)| text == label) { return rect; }
        scroll(app, ctx, -80.);
    }
    panic!("GUI selector not reachable: {label}");
}

fn click(app: &mut desktop::Workbench, ctx: &egui::Context, pos: egui::Pos2) {
    for pressed in [true, false] {
        frame(app, ctx, vec![
            egui::Event::PointerMoved(pos),
            egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() },
        ]);
    }
}

fn project_action_scope_id(selector: &str) -> egui::Id {
    egui::Id::unique(("agentranch", "project-action", selector))
}

fn project_action_widget_id(selector: &str) -> egui::Id {
    let scope_id = project_action_scope_id(selector);
    egui::Id::unique(scope_id.value().wrapping_add(1))
}

fn project_action_response(ctx: &egui::Context, selector: &str) -> egui::Response {
    ctx.read_response(project_action_widget_id(selector))
        .unwrap_or_else(|| panic!("GUI action selector not registered: {selector}"))
}

fn selector_center(ctx: &egui::Context, selector: &str) -> egui::Pos2 {
    project_action_response(ctx, selector).rect.center()
}

fn host_project_runs(state: &Path, stage: &str) -> Vec<Value> {
    // The fixture config owns the host ledger beside the workflow state.
    let host = state.parent().unwrap().join("host/controller.sqlite");
    if !host.is_file() { return Vec::new(); }
    let db = Connection::open(host).unwrap();
    let mut statement = db.prepare("SELECT body FROM runs").unwrap();
    statement.query_map([], |row| row.get::<_, String>(0)).unwrap()
        .map(|result| serde_json::from_str::<Value>(&result.unwrap()).unwrap())
        .filter(|run| run["detail"]["project_work"]["stage"] == stage)
        .collect()
}

#[test]
fn project_action_selectors_are_stable_and_review_decisions_are_distinct() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id) = redo_fixture(temp.path());
    let (mut app, ctx) = app(&state);
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "task_review"));
    let _approve_label = seek(&mut app, &ctx, "审核通过");
    let project_selector = format!("project:{id}:task:inventory:approve");
    let reject_selector = format!("project:{id}:task:inventory:reject");
    let approve = project_action_response(&ctx, &project_selector);
    let reject = project_action_response(&ctx, &reject_selector);
    assert_ne!(approve.id, reject.id, "approve and reject must have distinct widget identities");
    assert_eq!(approve.id, project_action_response(&ctx, &project_selector).id);
    assert_eq!(reject.id, project_action_response(&ctx, &reject_selector).id);
}

#[test]
fn project_action_selector_preserves_disabled_gate_without_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id) = developed_fixture(temp.path());
    let (mut app, ctx) = app(&state);
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "developed"));
    let _label = seek(&mut app, &ctx, "人工验收通过");
    let selector = format!("project:{id}:accept");
    let response = project_action_response(&ctx, &selector);
    assert!(!response.enabled(), "the selector must expose the real disabled widget, not bypass the project gate");

    click(&mut app, &ctx, response.rect.center());
    for _ in 0..5 { frame(&mut app, &ctx, vec![]); }
    assert_eq!(app.pending_count(), 0);
    assert!(host_project_runs(&state, "accept").is_empty(), "disabled pointer action must not dispatch");
    assert_eq!(app.snapshot().projects[0].phase, "developed");
}

#[test]
fn project_action_selector_resolves_the_real_accept_button_widget() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id) = acceptance_fixture(temp.path());
    let (mut app, ctx) = app(&state);
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "awaiting_acceptance"));

    let label_rect = seek(&mut app, &ctx, "人工验收通过");
    let selector = format!("project:{id}:accept");
    let response = project_action_response(&ctx, &selector);
    assert!(response.rect.contains(label_rect.center()), "stable selector must identify the rendered accept widget: {:?} vs {:?}", response.rect, label_rect);
    assert_eq!(response.id, project_action_widget_id(&selector));
    let stable_id = response.id;

    click(&mut app, &ctx, response.rect.center());
    assert!(app.pending_count() <= 1, "one click must not duplicate the pending request");
    frame(&mut app, &ctx, vec![]);
    assert_eq!(project_action_response(&ctx, &selector).id, stable_id, "pending-label/layout changes must not change widget identity");
}

#[test]
fn clicking_test_runs_real_project_command_and_does_not_auto_accept() {
    let temp = tempfile::tempdir().unwrap();
    let reason = lifecycle_reason::LocalReason::new(temp.path(), true);
    let (state, id) = developed_fixture_with_endpoint(temp.path(), &reason.endpoint, "lifecycle-test");
    let (mut app, ctx) = app(&state);
    wait(&mut app, |snapshot| snapshot.readiness == agentranch::core::service::Readiness::Ready && snapshot.projects.iter().any(|project| project.id == id && project.phase == "developed"));
    let before = app.snapshot();
    assert_eq!(before.projects[0].actions.len(), 1);
    assert_eq!(before.projects[0].can_test, true);
    assert_eq!(before.projects[0].can_accept, false);
    assert!(host_project_runs(&state, "test").is_empty(), "click must be the dispatch boundary");

    let _button = seek(&mut app, &ctx, "运行独立测试");
    let selector = format!("project:{id}:test");
    click(&mut app, &ctx, selector_center(&ctx, &selector));
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "testing"));

    let after = app.snapshot().projects.iter().find(|project| project.id == id).cloned().unwrap();
    assert_eq!(after.actions.len(), 2);
    assert_eq!(after.actions[1].kind, "test");
    assert!(matches!(after.actions[1].state.as_str(), "requested" | "running"), "Test click must create a live requested/running action: {:?}", after.actions[1]);
    assert_eq!(after.can_accept, false, "Test click must not auto-accept");
    let runs = host_project_runs(&state, "test");
    assert_eq!(runs.len(), 1, "exactly one test work item must be dispatched");
    assert_eq!(runs[0]["detail"]["project_work"]["stage"], "test");
}

#[test]
fn repair_requires_reason_and_click_dispatches_only_after_reason_entry() {
    let temp = tempfile::tempdir().unwrap();
    let reason = lifecycle_reason::LocalReason::new(temp.path(), true);
    let (state, id) = test_failed_fixture_with_endpoint(temp.path(), &reason.endpoint, "lifecycle-test");
    let (mut app, ctx) = app(&state);
    wait(&mut app, |snapshot| snapshot.readiness == agentranch::core::service::Readiness::Ready && snapshot.projects.iter().any(|project| project.id == id && project.phase == "test_failed"));
    assert_eq!(app.snapshot().projects[0].can_repair, true);
    assert!(host_project_runs(&state, "repair").is_empty());

    // Empty repair reason keeps the real rendered button disabled and submits no request.
    let _disabled = seek(&mut app, &ctx, "提交返修");
    let selector = format!("project:{id}:repair");
    click(&mut app, &ctx, selector_center(&ctx, &selector));
    for _ in 0..5 { frame(&mut app, &ctx, vec![]); }
    assert_eq!(app.pending_count(), 0);
    assert!(host_project_runs(&state, "repair").is_empty());
    assert_eq!(app.snapshot().projects[0].phase, "test_failed");

    seek(&mut app, &ctx, "返修原因（测试未通过时必须显式发起返修，之后再复测）");
    // seek() parks the label at the scroll viewport's bottom edge, where the
    // fixed +20 click offset can fall into the status panel below the detail
    // scroll. Scroll the reason editor fully into view first, then re-locate
    // the label in the settled frame before clicking.
    scroll(&mut app, &ctx, -160.);
    let output = frame(&mut app, &ctx, vec![]);
    let reason_label = texts(&output).into_iter()
        .find(|(text, _)| text == "返修原因（测试未通过时必须显式发起返修，之后再复测）")
        .map(|(_, rect)| rect)
        .expect("reason label must stay visible after the settling scroll");
    click(&mut app, &ctx, egui::pos2(reason_label.center().x, reason_label.bottom() + 20.));
    frame(&mut app, &ctx, vec![egui::Event::Text("补齐库存扣减失败修复".into())]);
    let _submit = seek(&mut app, &ctx, "提交返修");
    let selector = format!("project:{id}:repair");
    assert!(project_action_response(&ctx, &selector).enabled(), "entered repair reason must enable the real repair selector");
    click(&mut app, &ctx, selector_center(&ctx, &selector));
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "repairing"));

    let after = app.snapshot().projects.iter().find(|project| project.id == id).cloned().unwrap();
    assert_eq!(after.actions.iter().filter(|action| action.kind == "repair").count(), 1);
    let repair = after.actions.iter().find(|action| action.kind == "repair").unwrap();
    assert!(matches!(repair.state.as_str(), "requested" | "running"), "Repair click must create a live requested/running action: {:?}", repair);
    assert_eq!(host_project_runs(&state, "repair").len(), 1);
    assert_eq!(host_project_runs(&state, "repair")[0]["detail"]["project_work"]["repair_reason"], "补齐库存扣减失败修复");
}

#[test]
fn clicking_manual_accept_persists_acceptance_and_reopens_as_accepted() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id) = acceptance_fixture(temp.path());
    let (mut app, ctx) = app(&state);
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "awaiting_acceptance"));
    let before = app.snapshot();
    assert_eq!(before.projects[0].can_accept, true);
    assert!(before.projects[0].actions.iter().all(|action| action.kind != "accept"));

    let _button = seek(&mut app, &ctx, "人工验收通过");
    let selector = format!("project:{id}:accept");
    click(&mut app, &ctx, selector_center(&ctx, &selector));
    click(&mut app, &ctx, selector_center(&ctx, &selector));
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "accepted"));
    let accepted = app.snapshot().projects.iter().find(|project| project.id == id).cloned().unwrap();
    assert_eq!(accepted.actions.iter().filter(|action| action.kind == "accept").count(), 1);
    assert_eq!(accepted.can_accept, false);
    assert!(host_project_runs(&state, "accept").is_empty(), "manual accept records acceptance; it does not dispatch a worker run");
    drop(app);

    let reopened = Projects::open(&state).unwrap();
    let reopened = reopened.snapshots().into_iter().find(|project| project.id == id).unwrap();
    assert_eq!(reopened.phase, "accepted");
    assert_eq!(reopened.actions.iter().filter(|action| action.kind == "accept").count(), 1);
    let db = Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    let evidence: String = db.query_row("SELECT value FROM project_settings WHERE key=?1", [format!("acceptance:{id}")], |row| row.get(0)).unwrap();
    let evidence: Value = serde_json::from_str(&evidence).unwrap();
    assert_eq!(evidence["accepted"], true);
    assert_eq!(evidence["project_id"], id);
}

#[test]
fn clicking_redo_dispatches_exact_affected_closure_without_auto_start() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id) = redo_fixture(temp.path());
    let (mut app, ctx) = app(&state);
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "task_review"));
    let before = app.snapshot().projects.iter().find(|project| project.id == id).cloned().unwrap();
    assert_eq!(before.can_start, false);
    assert_eq!(before.tasks.iter().find(|task| task.id == "inventory").unwrap().review, "rejected");
    assert_eq!(before.tasks.iter().find(|task| task.id == "refund").unwrap().review, "approved");
    assert!(host_project_runs(&state, "redo").is_empty());

    let _button = seek(&mut app, &ctx, "仅重做此项");
    let selector = format!("project:{id}:task:inventory:redo");
    click(&mut app, &ctx, selector_center(&ctx, &selector));
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "redoing"));

    let after = app.snapshot().projects.iter().find(|project| project.id == id).cloned().unwrap();
    assert_eq!(after.can_start, false, "Redo click must not auto-start development");
    assert_eq!(after.actions.iter().filter(|action| action.kind == "redo").count(), 1);
    let redo = host_project_runs(&state, "redo");
    assert_eq!(redo.len(), 1);
    let work = &redo[0]["detail"]["project_work"];
    assert_eq!(work["stage"], "redo");
    assert_eq!(work["target_task"], "inventory");
    let ids: Vec<&str> = work["tasks"].as_array().unwrap().iter().map(|task| task["id"].as_str().unwrap()).collect();
    assert_eq!(ids, vec!["inventory", "refund"]);
    assert!(!ids.contains(&"audit"), "Redo must dispatch target plus downstream closure, not unrelated approved work");
}

#[test]
fn not_ready_disables_test_selector_without_dispatch() {
    let temp = tempfile::tempdir().unwrap();
    let (state, id) = developed_fixture(temp.path());
    let (mut app, ctx) = app(&state);
    wait(&mut app, |snapshot| snapshot.projects.iter().any(|project| project.id == id && project.phase == "developed"));
    assert_eq!(app.snapshot().readiness, agentranch::core::service::Readiness::NotReady);

    let _button = seek(&mut app, &ctx, "运行独立测试");
    let selector = format!("project:{id}:test");
    let response = project_action_response(&ctx, &selector);
    assert!(!response.enabled(), "NotReady must disable the real test selector");

    click(&mut app, &ctx, response.rect.center());
    for _ in 0..5 { frame(&mut app, &ctx, vec![]); }
    assert_eq!(app.pending_count(), 0);
    assert!(host_project_runs(&state, "test").is_empty(), "disabled selector must not dispatch a project test");
}
