//! Audit screen: the hash-chained trail, newest first, plus chain verification.

use crate::app::YkDistApp;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Audit",
        "Append-only trail. The table rejects UPDATE and DELETE at the database level; \
         each entry carries the hash of the previous one.",
    );

    ui.horizontal_wrapped(|ui| {
        if ui.button("Verify chain").clicked() {
            app.verify_audit();
        }
        ui.label(
            egui::RichText::new(format!("{} entries shown", app.audit_view.len()))
                .weak()
                .small(),
        );
    });

    ui.add_space(10.0);

    if app.audit_view.is_empty() {
        ui.label("No audit entries yet.");
        return;
    }

    egui::Grid::new("audit")
        .striped(true)
        .num_columns(6)
        .spacing([14.0, 6.0])
        .show(ui, |ui| {
            for header in ["#", "When", "Actor", "Event", "Target", "Details"] {
                ui.strong(header);
            }
            ui.end_row();

            for entry in &app.audit_view {
                ui.monospace(entry.seq.to_string());
                ui.label(entry.at.format("%d/%m/%Y %H:%M:%S").to_string());
                ui.label(&entry.actor);
                ui.label(&entry.event);
                ui.monospace(&entry.target);
                ui.add(
                    egui::Label::new(egui::RichText::new(&entry.details).small()).selectable(true),
                );
                ui.end_row();
            }
        });
}
