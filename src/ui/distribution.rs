//! Distribution: record a hand-over, see who holds what, close a return.

use crate::app::YkDistApp;
use crate::domain::DeliveryMethod;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Distribution",
        "Who holds which key, since when, handed over by whom, and what was applied to it.",
    );

    if app.keys.is_empty() || app.holders.is_empty() {
        ui.label("Register at least one key and one holder first.");
    } else {
        record_form(app, ui);
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(6.0);
    history(app, ui);
}

fn record_form(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let key_labels: Vec<String> = app
        .keys
        .iter()
        .map(|k| format!("{} — {} ({})", k.serial, k.model, k.status.label()))
        .collect();
    let holder_labels: Vec<String> = app.holders.iter().map(|h| h.display()).collect();

    app.dist_form.key_index = app.dist_form.key_index.min(key_labels.len() - 1);
    app.dist_form.holder_index = app.dist_form.holder_index.min(holder_labels.len() - 1);

    egui::Grid::new("dist-form")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Key");
            egui::ComboBox::from_id_salt("dist-key")
                .selected_text(&key_labels[app.dist_form.key_index])
                .width(340.0)
                .show_ui(ui, |ui| {
                    for (index, label) in key_labels.iter().enumerate() {
                        ui.selectable_value(&mut app.dist_form.key_index, index, label);
                    }
                });
            ui.end_row();

            ui.label("Holder");
            egui::ComboBox::from_id_salt("dist-holder")
                .selected_text(&holder_labels[app.dist_form.holder_index])
                .width(340.0)
                .show_ui(ui, |ui| {
                    for (index, label) in holder_labels.iter().enumerate() {
                        ui.selectable_value(&mut app.dist_form.holder_index, index, label);
                    }
                });
            ui.end_row();

            ui.label("Delivery");
            egui::ComboBox::from_id_salt("dist-method")
                .selected_text(app.dist_form.method.label())
                .show_ui(ui, |ui| {
                    for method in DeliveryMethod::ALL {
                        ui.selectable_value(&mut app.dist_form.method, method, method.label());
                    }
                });
            ui.end_row();

            ui.label("Receipt / term reference");
            ui.add(
                egui::TextEdit::singleline(&mut app.dist_form.receipt_ref)
                    .char_limit(crate::domain::MAX_TEXT)
                    .desired_width(340.0),
            );
            ui.end_row();

            ui.label("Notes");
            ui.add(
                egui::TextEdit::multiline(&mut app.dist_form.notes)
                    .char_limit(crate::domain::MAX_NOTE)
                    .desired_rows(2)
                    .desired_width(340.0),
            );
            ui.end_row();
        });

    ui.checkbox(
        &mut app.dist_form.link_last_run,
        "Attach the most recent bootstrap run for this key",
    );
    ui.add_space(6.0);
    if ui.button("Record distribution").clicked() {
        app.submit_distribution();
    }
    if let Some(error) = app.dist_form.error.clone() {
        ui.add_space(6.0);
        super::error_label(ui, &error);
    }
}

fn history(app: &mut YkDistApp, ui: &mut egui::Ui) {
    if app.distributions.is_empty() {
        ui.label("Nothing distributed yet.");
        return;
    }

    let now = chrono::Utc::now();
    let mut to_return: Option<(uuid::Uuid, u32)> = None;

    egui::Grid::new("distributions")
        .striped(true)
        .num_columns(7)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for header in [
                "Serial",
                "Holder",
                "Handed over",
                "By",
                "Applied",
                "Status",
                "Actions",
            ] {
                ui.strong(header);
            }
            ui.end_row();

            for record in &app.distributions {
                ui.monospace(record.key_serial.to_string());
                ui.label(&record.holder_display);
                ui.label(record.distributed_at.format("%d/%m/%Y %H:%M").to_string());
                ui.label(&record.distributed_by);

                let applied = record
                    .bootstrap_run_id
                    .and_then(|id| app.runs.iter().find(|r| r.id == id))
                    .map(|run| run.summary())
                    .unwrap_or_else(|| "—".into());
                ui.label(egui::RichText::new(applied).small());

                ui.label(if record.is_open() {
                    format!("held for {} day(s)", record.days_held(now))
                } else {
                    format!(
                        "returned {}",
                        record
                            .returned_at
                            .map(|at| at.format("%d/%m/%Y").to_string())
                            .unwrap_or_default()
                    )
                });

                ui.horizontal(|ui| {
                    if record.is_open() && ui.small_button("record return").clicked() {
                        to_return = Some((record.id, record.key_serial));
                    }
                });
                ui.end_row();
            }
        });

    if let Some((id, serial)) = to_return {
        app.return_key(id, serial);
    }
}
