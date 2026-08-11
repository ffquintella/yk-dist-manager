//! Audit screen: the hash-chained trail, newest first, plus chain verification.

use elegance::{Button, CalloutTone};

use crate::app::YkDistApp;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Audit",
        "Append-only trail. The table rejects UPDATE and DELETE at the database level; \
         each entry carries the hash of the previous one.",
    );

    ui.horizontal_wrapped(|ui| {
        if ui.add(Button::new("Verify chain")).clicked() {
            app.verify_audit();
        }
        ui.add_space(4.0);
        super::faint(ui, &format!("{} entries shown", app.audit_view.len()));
    });

    ui.add_space(16.0);

    if app.audit_view.is_empty() {
        super::notice(ui, CalloutTone::Neutral, "No audit entries yet.");
        return;
    }

    super::card(ui, |ui| {
        super::table(
            ui,
            "audit",
            &["#", "When", "Actor", "Event", "Target", "Details"],
            |ui| {
                for entry in &app.audit_view {
                    super::mono(ui, &entry.seq.to_string());
                    ui.label(entry.at.format("%d/%m/%Y %H:%M:%S").to_string());
                    ui.label(&entry.actor);
                    ui.label(&entry.event);
                    super::mono(ui, &entry.target);
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(&entry.details)
                                .size(elegance::Theme::current(ui.ctx()).typography.small),
                        )
                        .selectable(true),
                    );
                    ui.end_row();
                }
            },
        );
    });
}
