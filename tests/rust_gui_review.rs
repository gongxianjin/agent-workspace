#![cfg(feature = "gui")]
#[path = "../src/desktop/mod.rs"]
mod desktop;
use agentranch::{config::Mode, core::{projects::Projects, service::{ServiceOptions, UiCommand}}};
use eframe::egui;
use std::{path::Path, time::{Duration, Instant}};

// This is a persisted recovery-state fixture, not evidence of a live split.
// Real Workbench -> ServiceHandle -> Projects handles every subsequent event.
fn recovered_pending_exit(state: &Path) -> (desktop::Workbench, egui::Context, String) {
    let mut projects = Projects::open(state).unwrap();
    let value = projects.command(UiCommand::ProjectCreate { name: "待退出审核".into() }).unwrap();
    let id = value["id"].as_str().unwrap().to_owned();
    drop(projects);
    let document = "# 定稿设计\n库存扣减与退款补偿。\n";
    use sha2::{Digest, Sha256};
    let mut value = value;
    value["phase"] = serde_json::json!("task_review");
    value["document"] = serde_json::json!({"version":1,"filename":"design.md","sha256":hex::encode(Sha256::digest(document.as_bytes())),"text":document,"confirmed":true});
    value["tasks"] = serde_json::json!([
        {"id":"inventory","title":"库存扣减","description":"订单原子扣减库存","acceptance":"库存不为负","review":"pending","reason":null},
        {"id":"refund","title":"退款补偿","description":"退款仅恢复一次库存","acceptance":"重复退款幂等","review":"pending","reason":null}
    ]);
    value["actions"] = serde_json::json!([{"id":"split-action","kind":"split","state":"report_pending_exit","session":"owned-document-worker","detail":"真实拆分结果已读取；执行退出未获独立证明，保留名额，禁止后续开工"}]);
    value["blockers"] = serde_json::json!(["真实拆分结果已读取；执行退出未获独立证明，保留名额，禁止后续开工"]);
    let db = rusqlite::Connection::open(state.join("project-workflow/controller.sqlite")).unwrap();
    db.execute("UPDATE projects SET body=?1 WHERE id=?2", rusqlite::params![value.to_string(), id]).unwrap();
    drop(db);
    let ctx = egui::Context::default();
    let mut app = desktop::Workbench::new(ServiceOptions { mode: Mode::Real, state_dir: state.into(), worker_exe: env!("CARGO_BIN_EXE_aw-cli").into(), gateway_key: None, gateway_model_token: None }, &ctx).unwrap();
    ctx.style_mut_of(egui::Theme::Light, |style| style.animation_time = 0.0);
    wait(&mut app, |s| s.projects.len() == 1);
    (app, ctx, id)
}
fn wait(app: &mut desktop::Workbench, predicate: impl Fn(&agentranch::core::service::AppSnapshot) -> bool) {
    let end = Instant::now() + Duration::from_secs(6);
    loop {
        app.poll();
        if predicate(&app.snapshot()) && app.pending_count() == 0 { return; }
        assert!(
            Instant::now() < end,
            "focused GUI state did not converge: pending={} snapshot={:?}",
            app.pending_count(),
            app.snapshot()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}
fn frame(app: &mut desktop::Workbench, ctx: &egui::Context, events: Vec<egui::Event>) -> egui::FullOutput {
    let mut output = ctx.run_ui(egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(egui::Pos2::ZERO, egui::vec2(1100., 1200.))),
        events, ..Default::default()
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
            egui::epaint::Shape::Vec(shapes) => for s in shapes { gather(s, clip, found); },
            _ => {}
        }
    }
    let mut found = Vec::new();
    for shape in &output.shapes { gather(&shape.shape, shape.clip_rect, &mut found); }
    found
}
fn scroll(app: &mut desktop::Workbench, ctx: &egui::Context, delta: f32) {
    frame(app, ctx, vec![egui::Event::PointerMoved(egui::pos2(1040., 420.)), egui::Event::MouseWheel {
        unit: egui::MouseWheelUnit::Point, delta: egui::vec2(0., delta), phase: egui::TouchPhase::Move,
        source: egui::MouseWheelSource::Wheel, modifiers: Default::default()
    }]);
    for _ in 0..12 { frame(app, ctx, vec![]); }
}
fn seek(app: &mut desktop::Workbench, ctx: &egui::Context, label: &str, after: Option<&str>) -> egui::Rect {
    scroll(app, ctx, 10000.);
    for _ in 0..70 {
        let output = frame(app, ctx, vec![]);
        let found = texts(&output);
        let anchor = match after {
            Some(anchor) => found.iter().find(|(t, _)| t == anchor).map(|(_, r)| r.bottom()),
            None => Some(f32::NEG_INFINITY),
        };
        if let Some(top) = anchor {
            if let Some((_, rect)) = found.iter().find(|(text, rect)| text == label && rect.top() > top) { return *rect; }
        }
        scroll(app, ctx, -80.);
    }
    panic!("control/text not reachable by scrolling: {label}, after {after:?}");
}
fn click(app: &mut desktop::Workbench, ctx: &egui::Context, pos: egui::Pos2) {
    for pressed in [true, false] {
        frame(app, ctx, vec![egui::Event::PointerMoved(pos), egui::Event::PointerButton { pos, button: egui::PointerButton::Primary, pressed, modifiers: Default::default() }]);
    }
}
fn project_action_widget_id(selector: &str) -> egui::Id {
    let scope = egui::Id::unique(("agentranch", "project-action", selector));
    egui::Id::unique(scope.value().wrapping_add(1))
}
fn action_response(ctx: &egui::Context, selector: &str) -> egui::Response {
    ctx.read_response(project_action_widget_id(selector)).unwrap()
}
// Breaks if review buttons are absent/unreachable, use a wrong task/project,
// conflate report with completion, or auto-start after the final approval.
#[test]
fn pending_exit_reviews_are_reachable_and_all_approval_cannot_start() {
    let t = tempfile::tempdir().unwrap();
    let (mut app, ctx, id) = recovered_pending_exit(t.path());
    let status = seek(&mut app, &ctx, "拆分任务 · 结果已回传，等退出确认", None);
    assert!(status.width() > 0.);
    let detail = seek(&mut app, &ctx, "真实拆分结果已读取；执行退出未获独立证明，保留名额，禁止后续开工", None);
    assert!(detail.width() > 0.);
    for task in ["inventory", "refund"] {
        let title = if task == "inventory" { "库存扣减 · pending" } else { "退款补偿 · pending" };
        let button = seek(&mut app, &ctx, "审核通过", Some(title));
        click(&mut app, &ctx, button.center());
        wait(&mut app, |s| s.projects[0].tasks.iter().find(|t| t.id == task).unwrap().review == "approved");
    }
    let s = app.snapshot();
    let p = s.projects.iter().find(|p| p.id == id).unwrap();
    assert!(p.tasks.iter().all(|t| t.review == "approved"));
    assert_eq!(p.phase, "ready");
    assert!(!p.can_start);
    assert_eq!(p.actions.len(), 1);
    assert_eq!(p.actions[0].state, "report_pending_exit");
    assert_eq!(s.unfinished, 1);
    assert!(s.runs.is_empty());
    let receipts = app.receipt_count();
    let start = seek(&mut app, &ctx, "手动开始开发", None);
    click(&mut app, &ctx, start.center());
    for _ in 0..5 { frame(&mut app, &ctx, vec![]); }
    assert_eq!(app.pending_count(), 0);
    assert_eq!(app.receipt_count(), receipts, "disabled Start must submit no command");
    assert_eq!(app.snapshot().projects[0].actions.len(), 1);
}
// Breaks if the reason field is unreachable or the reject button drops the
// user's input, targets a different task, or submits without an explicit click.
#[test]
fn pending_exit_rejection_click_persists_the_entered_reason() {
    let t = tempfile::tempdir().unwrap();
    let (mut app, ctx, id) = recovered_pending_exit(t.path());
    let task_title = "库存扣减 · pending";
    let label = seek(&mut app, &ctx, "驳回原因", Some(task_title));
    let input_pos = egui::pos2(label.center().x, label.bottom() + 20.);
    click(&mut app, &ctx, input_pos);
    frame(&mut app, &ctx, vec![egui::Event::Text("需要补充重复回调验收".into())]);
    assert!(app.snapshot().projects[0].tasks[0].reason.is_none());
    let _button = seek(&mut app, &ctx, "驳回此项", Some(task_title));
    let selector = format!("project:{id}:task:inventory:reject");
    assert!(action_response(&ctx, &selector).enabled(), "entered review reason must enable the real reject selector");
    click(&mut app, &ctx, action_response(&ctx, &selector).rect.center());
    wait(&mut app, |s| s.projects[0].tasks[0].review == "rejected");
    let s = app.snapshot();
    assert_eq!(s.projects[0].tasks[0].reason.as_deref(), Some("需要补充重复回调验收"));
    assert_eq!(s.projects[0].tasks[1].review, "pending");
    assert_eq!(s.projects[0].actions[0].state, "report_pending_exit");
    assert!(!s.projects[0].can_start);
    let reopened = Projects::open(t.path()).unwrap();
    assert_eq!(reopened.snapshots()[0].tasks[0].reason.as_deref(), Some("需要补充重复回调验收"));
}
