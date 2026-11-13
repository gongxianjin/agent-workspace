#![cfg(feature = "gui")]
#[path = "../src/desktop/mod.rs"]
mod desktop;
use agentranch::{config::Mode, core::service::{ServiceOptions, Readiness, UiCommand}};
use eframe::egui;
use std::{path::Path, time::{Duration, Instant}};

fn app(state: &Path, mode: Mode) -> (desktop::Workbench, egui::Context) {
    let ctx = egui::Context::default();
    let app = desktop::Workbench::new(ServiceOptions { mode, state_dir: state.into(), worker_exe: env!("CARGO_BIN_EXE_aw-cli").into(), gateway_key: None, gateway_model_token: None }, &ctx).unwrap();
    (app, ctx)
}
fn wait(app: &mut desktop::Workbench, predicate: impl Fn(&agentranch::core::service::AppSnapshot) -> bool) {
    let until = Instant::now() + Duration::from_secs(12);
    while Instant::now() < until {
        app.poll();
        if predicate(&app.snapshot()) && app.pending_count() == 0 { return; }
        std::thread::sleep(Duration::from_millis(15));
    }
    panic!("workbench did not converge: {:?}", app.snapshot());
}
fn render(app: &mut desktop::Workbench, ctx: &egui::Context) -> egui::FullOutput {
    // 1100x1200 keeps the document preview inside the detail scroll's visible
    // region; at smaller heights the taller productized labels push it below
    // the fold and egui culls the etched text from the headless output.
    let mut output = ctx.run_ui(egui::RawInput { screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100., 1200.))), ..Default::default() }, |ui| {
        egui::CentralPanel::default().show(ui, |ui| app.show(ui));
    });
    // A headless renderer intentionally does not upload GPU textures.
    output.textures_delta.clear();
    output
}
fn rendered_text(output: &egui::FullOutput) -> String {
    fn gather(shape: &egui::epaint::Shape, text: &mut String) {
        match shape {
            egui::epaint::Shape::Text(s) => { text.push_str(&s.galley.job.text); text.push('\n'); }
            egui::epaint::Shape::Vec(shapes) => for s in shapes { gather(s, text); },
            _ => {}
        }
    }
    let mut text = String::new();
    for shape in &output.shapes { gather(&shape.shape, &mut text); }
    text
}
// A default UI that still presents fixed sample tasks instead of the design-document
// entry must fail this test, even when the underlying sample runner works.
#[test]
fn default_workbench_starts_from_project_and_final_design_not_sample_dispatch() {
    let t = tempfile::tempdir().unwrap();
    let (mut app, ctx) = app(t.path(), Mode::Local);
    let text = rendered_text(&render(&mut app, &ctx));
    assert!(text.contains("新建项目"), "project creation entry missing: {text}");
    assert!(text.contains("定稿设计文档"), "document-led entry missing: {text}");
    assert!(!text.contains("sum-native-v1"), "fixed diagnostic template must not be the primary workflow");
}
fn expand_project_directories(app: &mut desktop::Workbench, ctx: &egui::Context) {
    // Settle disclosure geometry deterministically; pointer events still open it.
    ctx.style_mut_of(egui::Theme::Light, |style| style.animation_time = 0.0);
    render(app, ctx);
    let output = render(app, ctx);
    fn directory_center(shape: &egui::epaint::Shape) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(s) if s.galley.job.text == "项目与产物目录" => Some(s.pos + s.galley.size() / 2.),
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(directory_center),
            _ => None,
        }
    }
    let pos = output.shapes.iter().find_map(|s| directory_center(&s.shape)).expect("project directory disclosure must render");
    for pressed in [true, false] {
        let mut output = ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024., 768.))),
            events: vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }],
            ..Default::default()
        }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.show(ui)); });
        output.textures_delta.clear();
    }
}
// Omitting either core path, or substituting a guessed UI path, must fail.
// The actual empty directory and idle project must not become execution evidence.
#[test]
fn project_managed_paths_render_actual_core_locations_without_starting_work() {
    let t = tempfile::tempdir().unwrap();
    let (mut app, ctx) = app(&t.path().join("state"), Mode::Local);
    app.send("create".into(), UiCommand::ProjectCreate { name: "目录展示".into() });
    wait(&mut app, |s| s.projects.len() == 1);
    let snapshot = app.snapshot();
    let project = &snapshot.projects[0];
    assert!(Path::new(&project.project_path).is_dir());
    assert!(Path::new(&project.product_path).is_dir());
    assert_eq!(std::fs::read_dir(&project.product_path).unwrap().count(), 0);
    expand_project_directories(&mut app, &ctx);
    let text = rendered_text(&render(&mut app, &ctx));
    assert!(text.contains(&format!("项目目录：{}", project.project_path)), "actual project path missing: {text}");
    assert!(text.contains(&format!("产物目录：{}", project.product_path)), "actual product path missing: {text}");
    assert_eq!(app.snapshot().projects[0].phase, "needs_document");
    assert!(app.snapshot().projects[0].actions.is_empty());
    assert!(app.snapshot().runs.is_empty());
}
// A persisted pre-path record must stay unprepared during rendering; only the
// actual refresh widget may ask core to prepare and return managed locations.
#[test]
fn project_legacy_paths_stay_unprepared_until_explicit_refresh_click() {
    use agentranch::core::projects::Projects;
    let t = tempfile::tempdir().unwrap();
    let state = t.path().join("state");
    drop(Projects::open(&state).unwrap());
    let id = "760827c4-4d4d-4826-b06d-5b3e7d7a9292";
    let legacy = serde_json::json!({
        "id": id, "name": "旧项目", "phase": "needs_document",
        "document": null, "tasks": [], "actions": [], "blockers": [],
        "can_confirm": false, "can_split": false, "can_start": false,
        "can_test": false, "can_repair": false, "can_accept": false
    });
    let db = rusqlite::Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    db.execute("INSERT INTO projects(id,body) VALUES(?1,?2)", rusqlite::params![id, legacy.to_string()]).unwrap();
    drop(db);
    let (mut app, ctx) = app(&state, Mode::Real);
    wait(&mut app, |s| s.projects.len() == 1);
    assert!(app.snapshot().projects[0].project_path.is_empty());
    assert!(app.snapshot().projects[0].product_path.is_empty());
    expand_project_directories(&mut app, &ctx);
    let output = render(&mut app, &ctx);
    let text = rendered_text(&output);
    assert!(!state.join("projects").exists(), "snapshot/render must not prepare directories");
    assert!(text.contains("项目目录：尚未准备目录"), "legacy project placeholder missing: {text}");
    assert!(text.contains("产物目录：尚未准备目录"), "legacy product placeholder missing: {text}");
    fn refresh_center(shape: &egui::epaint::Shape) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(s) if s.galley.job.text == "刷新项目" => Some(s.pos + s.galley.size() / 2.),
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(refresh_center),
            _ => None,
        }
    }
    let pos = output.shapes.iter().find_map(|s| refresh_center(&s.shape)).expect("explicit refresh action must render");
    for pressed in [true, false] {
        let mut output = ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024., 768.))),
            events: vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }],
            ..Default::default()
        }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.show(ui)); });
        output.textures_delta.clear();
    }
    wait(&mut app, |s| !s.projects[0].project_path.is_empty() && !s.projects[0].product_path.is_empty());
    let snapshot = app.snapshot();
    let project = &snapshot.projects[0];
    assert!(Path::new(&project.project_path).is_dir());
    assert!(Path::new(&project.product_path).is_dir());
    let text = rendered_text(&render(&mut app, &ctx));
    assert!(text.contains(&format!("项目目录：{}", project.project_path)));
    assert!(text.contains(&format!("产物目录：{}", project.product_path)));
    assert!(project.actions.is_empty());
    assert!(snapshot.runs.is_empty());
}
// Import and confirmation must operate on the actual persisted project, not a UI
// copy or a successful local sample; absent external AO must never invent tasks.
#[test]
fn project_document_import_and_confirmation_render_real_version_and_blocked_split() {
    let t = tempfile::tempdir().unwrap();
    let document = t.path().join("final-design.md");
    std::fs::write(&document, "# 已定稿设计\n\n实现库存查询。\n").unwrap();
    let (mut app, ctx) = app(&t.path().join("state"), Mode::Local);
    app.send("project-create".into(), UiCommand::ProjectCreate { name: "库存工作台".into() });
    wait(&mut app, |s| s.projects.len() == 1);
    let id = app.snapshot().projects[0].id.clone();
    app.send("import".into(), UiCommand::ProjectImport { project_id: id.clone(), path: document });
    wait(&mut app, |s| s.projects[0].document.is_some());
    let snapshot = app.snapshot();
    let doc = snapshot.projects[0].document.as_ref().unwrap();
    assert_eq!(doc.version, 1);
    assert!(!doc.confirmed);
    let text = rendered_text(&render(&mut app, &ctx));
    assert!(text.contains("实现库存查询。"), "document preview missing: {text}");
    assert!(text.contains("U1"));
    assert!(text.contains(&doc.sha256));
    app.send("confirm".into(), UiCommand::ProjectConfirm { project_id: id.clone(), version: doc.version, sha256: doc.sha256.clone() });
    wait(&mut app, |s| s.projects[0].document.as_ref().is_some_and(|d| d.confirmed));
    app.send("split".into(), UiCommand::ProjectSplit { project_id: id });
    wait(&mut app, |s| !s.projects[0].blockers.is_empty());
    let snapshot = app.snapshot();
    assert!(snapshot.projects[0].tasks.is_empty(), "missing external AO must not manufacture tasks");
    assert!(!snapshot.projects[0].can_start);
    assert!(!render(&mut app, &ctx).shapes.is_empty());
}
// Exercise egui pointer events against the actual widget, not Workbench::send.
// Removing the create-button command must leave the persisted projects empty.
#[test]
fn clicking_new_project_reaches_real_persisted_core() {
    let t = tempfile::tempdir().unwrap();
    let (mut app, ctx) = app(t.path(), Mode::Local);
    let output = render(&mut app, &ctx);
    fn label_center(shape: &egui::epaint::Shape) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(s) if s.galley.job.text == "新建项目" => Some(s.pos + s.galley.size() / 2.),
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(label_center),
            _ => None,
        }
    }
    let pos = output.shapes.iter().find_map(|s| label_center(&s.shape)).expect("new project button must render");
    for pressed in [true, false] {
        let mut output = ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024., 768.))),
            events: vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }],
            ..Default::default()
        }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.show(ui)); });
        output.textures_delta.clear();
    }
    wait(&mut app, |s| s.projects.len() == 1);
    assert!(!app.snapshot().projects[0].id.is_empty());
    assert!(app.snapshot().projects[0].document.is_none());
}
// A dropped file must become an explicit import action, never an automatic
// import/confirmation or a sample dispatch. File content still comes from core.
#[test]
fn dropping_document_waits_for_explicit_import_and_keeps_original_text() {
    #[derive(Debug)]
    struct NativeDrop(std::path::PathBuf);
    impl egui::DroppedFile for NativeDrop {
        fn path(&self) -> &std::path::Path { &self.0 }
        fn bytes(&self) -> Result<Vec<u8>, String> { panic!("GUI must not read dropped-file bytes") }
    }
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("original.md");
    std::fs::write(&path, "# 原始设计\n保留原文，不执行固定样例。\n").unwrap();
    let (mut app, ctx) = app(&t.path().join("state"), Mode::Local);
    app.send("create".into(), UiCommand::ProjectCreate { name: "文件拖放".into() });
    wait(&mut app, |s| s.projects.len() == 1);
    let mut output = ctx.run_ui(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100., 900.))),
        dropped_files: vec![std::sync::Arc::new(NativeDrop(path))],
        ..Default::default()
    }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.show(ui)); });
    output.textures_delta.clear();
    assert!(app.snapshot().projects[0].document.is_none(), "drop must not auto-import");
    fn import_center(shape: &egui::epaint::Shape) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(s) if s.galley.job.text == "导入设计文档" => Some(s.pos + s.galley.size() / 2.),
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(import_center),
            _ => None,
        }
    }
    let pos = output.shapes.iter().find_map(|s| import_center(&s.shape)).expect("import action must render");
    for pressed in [true, false] {
        let mut output = ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100., 900.))),
            events: vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }],
            ..Default::default()
        }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.show(ui)); });
        output.textures_delta.clear();
    }
    wait(&mut app, |s| s.projects[0].document.is_some());
    let snapshot = app.snapshot();
    let doc = snapshot.projects[0].document.as_ref().unwrap();
    assert_eq!(doc.text, "# 原始设计\n保留原文，不执行固定样例。\n");
    assert!(!doc.confirmed);
    assert!(snapshot.runs.is_empty());
}
#[test]
fn real_service_ui_actions_retain_unknown_sum_until_owner_admission() {
    let t = tempfile::tempdir().unwrap();
    let (mut app, ctx) = app(t.path(), Mode::Local);
    assert_eq!(app.snapshot().readiness, Readiness::NotStarted);
    let output = render(&mut app, &ctx);
    assert!(rendered_text(&output).contains("本地体验"), "local mode must be visible before startup");
    app.send("start".into(), UiCommand::StartLocal);
    assert_eq!(app.pending_count(), 1);
    wait(&mut app, |s| s.readiness == Readiness::Ready);
    app.send("dispatch:sum-native-v1".into(), UiCommand::Dispatch { template: "sum-native-v1".into() });
    wait(&mut app, |s| s.runs.iter().any(|r|
        r.run.status == "unknown"
            && r.run.detail["verification"]["state"] == "owner_evidence_required"
    ));
    let snapshot = app.snapshot();
    assert_eq!(snapshot.runs.len(), 1);
    let run = &snapshot.runs[0].run;
    assert_eq!(run.detail["report"]["sum"], 6);
    assert!(run.detail["verification"]["passed"].is_null());
    assert_eq!(snapshot.nodes[0].used, 1, "observer evidence must retain the assignment");
    assert_eq!(app.receipt_count(), 2);
    assert!(!render(&mut app, &ctx).shapes.is_empty());
}
#[test]
fn control_fixture_cancel_retains_pending_until_owner_evidence() {
    let t = tempfile::tempdir().unwrap();
    let (mut app, ctx) = app(t.path(), Mode::Local);
    app.send("start".into(), UiCommand::StartLocal);
    wait(&mut app, |s| s.readiness == Readiness::Ready);
    app.send("dispatch:control-native-v1".into(), UiCommand::Dispatch { template: "control-native-v1".into() });
    wait(&mut app, |s| !s.approvals.is_empty());
    let snapshot = app.snapshot();
    let approval = &snapshot.approvals[0];
    let command = desktop::approval::decision_command(approval, "allow_once", snapshot.observed_at).unwrap();
    let id = approval.run_id.clone();
    app.send(format!("decide:{id}"), command);
    wait(&mut app, |s| s.approvals.is_empty());
    app.send(format!("cancel:{id}"), UiCommand::Cancel { run_id: id.clone() });
    wait(&mut app, |s| s.runs.iter().any(|r|
        r.run.id == id
            && r.run.status == "cancel_pending"
            && r.run.detail["verification"]["state"] == "owner_evidence_required"
    ));
    let snapshot = app.snapshot();
    let run = snapshot.runs.iter().find(|r| r.run.id == id).unwrap();
    assert_eq!(run.run.detail["verification"]["child_exit_code"], 2);
    assert_eq!(run.run.detail["verification"]["passed"], false);
    assert_eq!(snapshot.nodes[0].used, 1, "cancelled observer evidence must retain the assignment");
    assert_eq!(app.receipt_count(), 4);
    assert!(!render(&mut app, &ctx).shapes.is_empty());
}
#[test]
fn failed_command_clears_only_matching_pending_and_real_mode_never_starts_fixture() {
    let t = tempfile::tempdir().unwrap();
    let path = t.path().join("must-not-exist");
    let (mut app, ctx) = app(&path, Mode::Real);
    assert_eq!(app.snapshot().readiness, Readiness::NotReady);
    app.send("dispatch:sum-v1".into(), UiCommand::Dispatch { template: "sum-v1".into() });
    wait(&mut app, |s| s.readiness == Readiness::NotReady);
    assert_eq!(app.receipt_count(), 1);
    assert!(!path.exists());
    assert!(!render(&mut app, &ctx).shapes.is_empty());
}

fn close_frame(app: &mut desktop::Workbench, ctx: &egui::Context) -> egui::FullOutput {
    let mut output = ctx.run_ui(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024., 768.))),
        viewports: std::iter::once((
            egui::ViewportId::ROOT,
            egui::ViewportInfo { events: vec![egui::ViewportEvent::Close], ..Default::default() },
        )).collect(),
        ..Default::default()
    }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.show(ui)); });
    output.textures_delta.clear();
    output
}

fn root_commands(output: &egui::FullOutput) -> &[egui::ViewportCommand] {
    output.viewport_output.get(&egui::ViewportId::ROOT).map_or(&[], |viewport| viewport.commands.as_slice())
}

#[test]
fn normal_close_stops_and_drains_service_before_emitting_close() {
    let t = tempfile::tempdir().unwrap();
    let (mut app, ctx) = app(t.path(), Mode::Real);

    let first = close_frame(&mut app, &ctx);
    assert!(!root_commands(&first).contains(&egui::ViewportCommand::Close), "close must wait for stopped snapshot");
    assert!(root_commands(&first).contains(&egui::ViewportCommand::CancelClose), "normal close must hold the viewport open while service stops");

    let until = Instant::now() + Duration::from_secs(2);
    let mut emitted = false;
    while Instant::now() < until {
        let output = render(&mut app, &ctx);
        if root_commands(&output).contains(&egui::ViewportCommand::Close) {
            emitted = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }
    assert!(emitted, "normal close did not emit Close after service stop");
    assert_eq!(app.snapshot().readiness, Readiness::Stopped);
    assert!(!app.snapshot().running);
    assert_eq!(app.pending_count(), 0);
}

fn click_close_dialog(app: &mut desktop::Workbench, ctx: &egui::Context, label: &str) -> egui::FullOutput {
    fn center(shape: &egui::epaint::Shape, label: &str) -> Option<egui::Pos2> {
        match shape {
            egui::epaint::Shape::Text(s) if s.galley.job.text == label => Some(s.pos + s.galley.size() / 2.),
            egui::epaint::Shape::Vec(shapes) => shapes.iter().find_map(|s| center(s, label)),
            _ => None,
        }
    }
    render(app, ctx);
    let output = render(app, ctx);
    let pos = output.shapes.iter().find_map(|s| center(&s.shape, label)).unwrap_or_else(|| panic!("missing close-dialog action: {label}"));
    let mut last = output;
    for pressed in [true, false] {
        last = ctx.run_ui(egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1024., 768.))),
            events: vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }],
            ..Default::default()
        }, |ui| { egui::CentralPanel::default().show(ui, |ui| app.show(ui)); });
        last.textures_delta.clear();
    }
    last
}

#[test]
fn normal_close_pending_confirmation_can_cancel_then_waits_for_accepted_receipt() {
    use agentranch::core::projects::Projects;
    let t = tempfile::tempdir().unwrap();
    let mut projects = Projects::open(t.path()).unwrap();
    projects.command(UiCommand::ProjectCreate { name: "已有项目".into() }).unwrap();
    drop(projects);
    let (mut app, ctx) = app(t.path(), Mode::Real);
    wait(&mut app, |s| s.projects.len() == 1);

    // Hold this fresh fixture's ledger so the real accepted command cannot finish
    // before the confirmation interaction. No service or GUI internals are mocked.
    let db = rusqlite::Connection::open(t.path().join("project-workflow/controller.sqlite")).unwrap();
    db.execute_batch("BEGIN IMMEDIATE").unwrap();
    app.send("create-pending".into(), UiCommand::ProjectCreate { name: "待回执项目".into() });
    let first = close_frame(&mut app, &ctx);
    assert!(root_commands(&first).contains(&egui::ViewportCommand::CancelClose));
    assert!(!root_commands(&first).contains(&egui::ViewportCommand::Close));
    assert!(rendered_text(&render(&mut app, &ctx)).contains("仍有未完成工作"));
    assert_eq!(app.pending_count(), 1);

    click_close_dialog(&mut app, &ctx, "继续查看");
    let continued = render(&mut app, &ctx);
    assert!(!rendered_text(&continued).contains("仍有未完成工作"));
    assert!(app.snapshot().running, "cancelled close must not stop the service");
    assert_eq!(app.pending_count(), 1);

    close_frame(&mut app, &ctx);
    let confirmed = click_close_dialog(&mut app, &ctx, "仍然关闭");
    assert!(!root_commands(&confirmed).contains(&egui::ViewportCommand::Close), "confirmation cannot bypass pending receipt drain");
    let stopping = render(&mut app, &ctx);
    assert!(rendered_text(&stopping).contains("正在关闭工作台"));
    assert!(!root_commands(&stopping).contains(&egui::ViewportCommand::Close));
    assert_eq!(app.pending_count(), 1);
    app.send("must-not-run".into(), UiCommand::ProjectCreate { name: "关闭后不应接受".into() });
    assert_eq!(app.pending_count(), 1, "closing cannot accept a new action");

    db.execute_batch("ROLLBACK").unwrap();
    let until = Instant::now() + Duration::from_secs(5);
    loop {
        let output = render(&mut app, &ctx);
        if root_commands(&output).contains(&egui::ViewportCommand::Close) { break; }
        assert!(Instant::now() < until, "confirmed close did not converge");
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(app.pending_count(), 0);
    assert_eq!(app.receipt_count(), 1);
    assert_eq!(app.snapshot().readiness, Readiness::Stopped);
    assert!(!app.snapshot().running);
}
