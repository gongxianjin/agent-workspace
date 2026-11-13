use agentranch::core::service::UiCommand;
use eframe::egui;

use super::pending::Pending;

const ACTION_ID_NAMESPACE: &str = "agentranch";
const ACTION_ID_KIND: &str = "project-action";

pub(crate) fn widget_scope_id(selector: &str) -> egui::Id {
    egui::Id::unique((ACTION_ID_NAMESPACE, ACTION_ID_KIND, selector))
}

pub(crate) fn action(
    ui: &mut egui::Ui,
    pending: &Pending,
    commands: &mut Vec<(String, UiCommand)>,
    label: &str,
    enabled: bool,
    selector: &str,
    key: String,
    command: UiCommand,
) {
    let waiting = pending.contains(&key);
    let label = if waiting {
        format!("{label}（等待回执）")
    } else {
        label.into()
    };
    let response = ui
        .scope_builder(
            egui::UiBuilder::new().scope_id(widget_scope_id(selector)),
            |ui| ui.add_enabled(enabled && !waiting, egui::Button::new(label)),
        )
        .inner;
    if response.clicked() {
        commands.push((key, command));
    }
}
