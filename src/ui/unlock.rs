//! Unlock screen: shown when the database could not be opened, which is either
//! a password-protected file or a real error.

use crate::app::YkDistApp;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    egui::CentralPanel::default().show(ui, |ui| {
        ui.add_space(60.0);
        ui.vertical_centered(|ui| {
            ui.heading("YubiKey Distribution Manager");
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(app.config.path.display().to_string())
                    .weak()
                    .monospace(),
            );
            ui.add_space(16.0);

            if let Some(error) = &app.open_error {
                super::error_label(ui, error);
                ui.add_space(12.0);
            }

            ui.label("Database password (leave empty if the file is not protected)");
            let response = ui.add(
                egui::TextEdit::singleline(&mut app.password_input)
                    .password(true)
                    .desired_width(280.0),
            );
            ui.add_space(8.0);

            let submit = ui.button("Open").clicked()
                || (response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)));

            if submit {
                let password = if app.password_input.is_empty() {
                    None
                } else {
                    Some(app.password_input.clone())
                };
                app.try_open(password);
                // Never keep the password in the UI state after use.
                app.password_input.clear();
            }

            ui.add_space(24.0);
            ui.label(
                egui::RichText::new(
                    "Password protection requires a build with `--features encrypted-db`.",
                )
                .weak()
                .small(),
            );
        });
    });
}
