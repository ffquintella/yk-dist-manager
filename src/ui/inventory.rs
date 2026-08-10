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
        let scan_label = if app.scan.open {
            "Hide serial scanner"
        } else {
            "Add by serial / scan…"
        };
        if ui.button(scan_label).clicked() {
            app.scan.open = !app.scan.open;
            #[cfg(feature = "camera")]
            if !app.scan.open {
                app.stop_camera();
            }
        }
        ui.label(
            egui::RichText::new(format!("transport: {}", app.backend.describe()))
                .weak()
                .small(),
        );
    });

    if app.scan.open {
        ui.add_space(8.0);
        scanner(app, ui);
    }

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

/// Panel for recording keys by serial: a typed number, a USB barcode wedge, or
/// the camera.
fn scanner(app: &mut YkDistApp, ui: &mut egui::Ui) {
    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.label(egui::RichText::new("Record a key by serial").strong());
        ui.label(
            egui::RichText::new(
                "For receiving a shipment: the serial goes in now and the key is verified \
                 later, when it is plugged in. A USB barcode scanner types straight into \
                 the field.",
            )
            .small()
            .weak(),
        );
        ui.add_space(6.0);

        ui.horizontal(|ui| {
            let response = ui.add(
                egui::TextEdit::singleline(&mut app.scan.typed)
                    .hint_text("serial, or scan a barcode")
                    .char_limit(24)
                    .desired_width(200.0),
            );
            let submitted = response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
            if ui.button("Add").clicked() || submitted {
                app.accept_typed_serial();
            }
        });

        camera_controls(app, ui);

        if let Some(serial) = app.scan.candidate {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.label(
                    egui::RichText::new(format!("scanned: {serial}"))
                        .strong()
                        .color(egui::Color32::from_rgb(60, 140, 70)),
                );
                if ui.button("Add to inventory").clicked() {
                    app.accept_scanned_serial();
                }
                if ui.button("Discard").clicked() {
                    app.scan.candidate = None;
                    #[cfg(feature = "camera")]
                    if let Some(scanner) = &app.scan.scanner {
                        scanner.clear_serial();
                    }
                }
            });
        }

        if let Some(error) = app.scan.error.clone() {
            ui.add_space(4.0);
            super::error_label(ui, &error);
        }
    });
}

#[cfg(feature = "camera")]
fn camera_controls(app: &mut YkDistApp, ui: &mut egui::Ui) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        if app.scan.scanner.is_none() {
            if ui.button("Start camera").clicked() {
                app.start_camera();
            }
        } else if ui.button("Stop camera").clicked() {
            app.stop_camera();
        }
        if let Some(scanner) = &app.scan.scanner {
            ui.label(
                egui::RichText::new(format!("camera: {}", scanner.describe()))
                    .small()
                    .weak(),
            );
        }
    });

    if let Some(texture) = &app.scan.preview {
        ui.add_space(6.0);
        // Keep the preview modest: it is an aiming aid, not a viewfinder.
        let size = texture.size_vec2();
        let scale = (360.0 / size.x).min(1.0);
        ui.add(egui::Image::new(texture).fit_to_exact_size(size * scale));
        ui.label(
            egui::RichText::new(
                "Fill the frame width with the barcode, ~20cm away. A laptop camera is \
                 fixed-focus and struggles closer than that.",
            )
            .small()
            .weak(),
        );
    }
}

#[cfg(not(feature = "camera"))]
fn camera_controls(_app: &mut YkDistApp, ui: &mut egui::Ui) {
    ui.add_space(4.0);
    ui.label(
        egui::RichText::new(
            "Camera scanning needs a build with `--features camera`. A USB barcode scanner \
             works in any build — it types into the field above.",
        )
        .small()
        .weak(),
    );
}
