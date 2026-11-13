#![cfg(feature="gui")]
#[path="../src/desktop/mod.rs"]
mod desktop;
use agentranch::{config::Mode,core::service::ServiceOptions};
use eframe::egui;

// Removing the real-mode gateway controls makes the actual headless rendered output fail.
#[test]
fn real_manager_renders_explicit_configuration_without_starting_ssh() {
    let root=tempfile::tempdir().unwrap();
    let context=egui::Context::default();
    let mut app=desktop::Workbench::new(ServiceOptions{mode:Mode::Real,state_dir:root.path().join("ui"),worker_exe:"unused.exe".into(),gateway_key:None,gateway_model_token:None},&context).unwrap();
    let mut output=context.run_ui(egui::RawInput{screen_rect:Some(egui::Rect::from_min_size(egui::Pos2::ZERO,egui::vec2(1100.,800.))),..Default::default()},|ui|{
        egui::CentralPanel::default().show(ui,|ui|app.show(ui));
    });
    output.textures_delta.clear();
    fn text(shape:&egui::epaint::Shape,result:&mut String) {
        match shape {
            egui::epaint::Shape::Text(value)=>{result.push_str(&value.galley.job.text);result.push('\n');}
            egui::epaint::Shape::Vec(values)=>for value in values{text(value,result);},
            _=>{},
        }
    }
    let mut rendered=String::new();
    for shape in &output.shapes{text(&shape.shape,&mut rendered);}
    assert!(rendered.contains("直接 SSH Worker"),"manager entry missing: {rendered}");
    assert!(rendered.contains("加载 SSH 配置"),"configuration action missing: {rendered}");
    assert!(rendered.contains("执行 sum-v1"),"explicit start action missing: {rendered}");
    assert!(app.snapshot().gateway.is_none());
    assert!(app.snapshot().runs.is_empty());
}
