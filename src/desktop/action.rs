use agentranch::core::service::UiCommand;
use eframe::egui;

use super::{pending::Pending, theme};

const ACTION_ID_NAMESPACE: &str = "agentranch";
const ACTION_ID_KIND: &str = "project-action";

pub(crate) fn widget_scope_id(selector: &str) -> egui::Id {
    egui::Id::unique((ACTION_ID_NAMESPACE, ACTION_ID_KIND, selector))
}

fn render(ui: &mut egui::Ui, label: &str, enabled: bool, selector: &str, primary: bool) -> egui::Response {
    let text = egui::RichText::new(label).size(if primary { 17. } else { 15. });
    let button = if primary {
        egui::Button::new(text).fill(theme::BRAND).min_size(egui::vec2(150., 34.))
    } else {
        egui::Button::new(text)
    };
    ui.scope_builder(
        egui::UiBuilder::new().scope_id(widget_scope_id(selector)),
        |ui| ui.add_enabled(enabled, button),
    )
    .inner
}

/// Secondary (outline) action button. `enabled` is the authoritative core gate;
/// this layer never widens it.
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
    let label = if waiting { format!("{label}（等待回执）") } else { label.into() };
    let response = render(ui, &label, enabled && !waiting, selector, false);
    if response.clicked() {
        commands.push((key, command));
    }
}

/// Primary (brand-filled) action button for the current step's main control.
/// Same gate semantics as [`action`]; only the visual treatment differs.
pub(crate) fn action_primary(
    ui: &mut egui::Ui,
    pending: &Pending,
    commands: &mut Vec<(String, UiCommand)>,
    label: &str,
    enabled: bool,
    selector: &str,
    key: String,
    command: UiCommand,
) {
    action_step(ui, pending, commands, label, enabled, true, selector, key, command);
}

/// Step-scoped action: renders as the primary button when `primary` (the step
/// is the current one), as an ordinary button otherwise. The gate is the
/// authoritative core state either way.
pub(crate) fn action_step(
    ui: &mut egui::Ui,
    pending: &Pending,
    commands: &mut Vec<(String, UiCommand)>,
    label: &str,
    enabled: bool,
    primary: bool,
    selector: &str,
    key: String,
    command: UiCommand,
) {
    let waiting = pending.contains(&key);
    let label = if waiting { format!("{label}（等待回执）") } else { label.into() };
    let response = render(ui, &label, enabled && !waiting, selector, primary);
    if response.clicked() {
        commands.push((key, command));
    }
}
