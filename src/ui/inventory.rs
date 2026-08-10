//! Inventory: every key the unit owns, and what the hardware reports.

use crate::app::YkDistApp;
use crate::store::key_status_str;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Inventory",
        "Keys held by the unit, identified from the hardware itself.",
    );

    ui.horizontal_wrapped(|ui| {
        if ui.button("Read attached key").clicked() {
            app.detect_keys();
        }
        ui.label(
            egui::RichText::new(format!("transport: {}", app.backend.describe()))
                .weak()
                .small(),
        );
    });

    if !app.detected.is_empty() {
        ui.add_space(6.0);
        for info in &app.detected {
            ui.label(format!(
                "detected: {} — serial {} — firmware {} — {}",
                info.model,
                info.serial,
                info.firmware,
                if info.usb_applications.is_empty() {
                    "no application list".to_owned()
                } else {
                    info.usb_applications.join(", ")
                }
            ));
        }
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(6.0);

    if app.keys.is_empty() {
        ui.label("No keys yet. Plug one in and use “Read attached key”.");
        return;
    }

    let mut status_change: Option<(u32, crate::domain::KeyStatus)> = None;

    egui::Grid::new("keys")
        .striped(true)
        .num_columns(7)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for header in [
                "Serial",
                "Model",
                "Firmware",
                "Form factor",
                "Status",
                "Applications",
                "Actions",
            ] {
                ui.strong(header);
            }
            ui.end_row();

            for key in &app.keys {
                ui.monospace(key.serial.to_string());
                ui.label(&key.model);
                ui.label(&key.firmware);
                ui.label(if key.form_factor.is_empty() {
                    "—"
                } else {
                    &key.form_factor
                });
                ui.label(key.status.label());
                ui.label(
                    egui::RichText::new(key.applications.join(", "))
                        .small()
                        .weak(),
                );
                ui.horizontal(|ui| {
                    if key.status == crate::domain::KeyStatus::InStock
                        && ui.small_button("mark bootstrapped").clicked()
                    {
                        status_change = Some((key.serial, crate::domain::KeyStatus::Bootstrapped));
                    }
                    if ui.small_button("mark lost").clicked() {
                        status_change = Some((key.serial, crate::domain::KeyStatus::Lost));
                    }
                });
                ui.end_row();
            }
        });

    if let Some((serial, next)) = status_change {
        let Some(store) = &app.store else { return };
        match store.set_key_status(serial, next) {
            Ok(()) => {
                app.record(
                    "key.status_changed",
                    &format!("serial:{serial}"),
                    &format!("to={}", key_status_str(next)),
                );
                app.status = format!("serial {serial} is now {}", next.label());
                app.refresh();
            }
            Err(e) => app.status = format!("refused: {e}"),
        }
    }
}
