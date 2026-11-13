//! Product visual tokens: one brand palette shared by every panel.
//! Display-only; never gates or carries domain state.
use eframe::egui;

/// Primary brand color: main buttons, current-step markers, subtitles.
pub(crate) const BRAND: egui::Color32 = egui::Color32::from_rgb(64, 96, 216);
/// Completed-step green.
pub(crate) const OK_GREEN: egui::Color32 = egui::Color32::from_rgb(38, 142, 96);
/// Blocker/warning amber for ⚠ cards.
pub(crate) const WARN: egui::Color32 = egui::Color32::from_rgb(176, 92, 20);
/// Card border on the light panel background.
pub(crate) const CARD_STROKE: egui::Color32 = egui::Color32::from_rgb(222, 229, 242);

/// White rounded card used for the current-step panel and task groups.
pub(crate) fn card(ui: &mut egui::Ui, content: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::group(ui.style())
        .fill(egui::Color32::WHITE)
        .stroke(egui::Stroke::new(1., CARD_STROKE))
        .inner_margin(14)
        .show(ui, content);
}
