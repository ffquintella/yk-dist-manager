//! Holders: the people who receive keys.

use crate::app::YkDistApp;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Holders",
        "Minimum personal data: name, corporate e-mail, unit. The e-mail is what \
         binds the signing certificate to the person.",
    );

    egui::Grid::new("holder-form")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Full name");
            ui.add(
                egui::TextEdit::singleline(&mut app.holder_form.full_name)
                    .char_limit(crate::domain::MAX_TEXT)
                    .desired_width(320.0),
            );
            ui.end_row();

            ui.label("Corporate e-mail");
            ui.add(
                egui::TextEdit::singleline(&mut app.holder_form.email)
                    .char_limit(crate::domain::MAX_TEXT)
                    .desired_width(320.0),
            );
            ui.end_row();

            ui.label("Unit");
            ui.add(
                egui::TextEdit::singleline(&mut app.holder_form.unit)
                    .char_limit(crate::domain::MAX_TEXT)
                    .desired_width(320.0),
            );
            ui.end_row();

            ui.label("Registration (optional)");
            ui.add(
                egui::TextEdit::singleline(&mut app.holder_form.registration)
                    .char_limit(crate::domain::MAX_TEXT)
                    .desired_width(320.0),
            );
            ui.end_row();
        });

    ui.add_space(8.0);
    if ui.button("Register holder").clicked() {
        app.submit_holder();
    }

    if let Some(error) = app.holder_form.error.clone() {
        ui.add_space(6.0);
        super::error_label(ui, &error);
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(6.0);

    if app.holders.is_empty() {
        ui.label("No holders registered yet.");
        return;
    }

    egui::Grid::new("holders")
        .striped(true)
        .num_columns(5)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for header in ["Name", "E-mail", "Unit", "Registration", "Keys held"] {
                ui.strong(header);
            }
            ui.end_row();

            for holder in &app.holders {
                let held = app
                    .distributions
                    .iter()
                    .filter(|d| d.holder_id == holder.id && d.is_open())
                    .count();
                ui.label(&holder.full_name);
                ui.monospace(&holder.email);
                ui.label(&holder.unit);
                ui.label(if holder.registration.is_empty() {
                    "—"
                } else {
                    &holder.registration
                });
                ui.label(held.to_string());
                ui.end_row();
            }
        });
}
