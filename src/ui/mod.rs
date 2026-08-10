//! egui screens. Each module exposes a single `show` entry point and owns no
//! state of its own — everything lives on [`crate::app::YkDistApp`].

pub mod audit;
pub mod bootstrap;
pub mod distribution;
pub mod holders;
pub mod inventory;
pub mod settings;
pub mod unlock;

/// Heading plus explanatory line, used at the top of every screen.
pub fn screen_header(ui: &mut egui::Ui, title: &str, subtitle: &str) {
    ui.heading(title);
    ui.label(egui::RichText::new(subtitle).weak());
    ui.add_space(8.0);
}

/// Red, selectable error text — errors go to the screen *and* the log, never
/// only to a dialog that vanishes.
pub fn error_label(ui: &mut egui::Ui, message: &str) {
    ui.add(
        egui::Label::new(egui::RichText::new(message).color(egui::Color32::from_rgb(200, 60, 60)))
            .selectable(true),
    );
}
