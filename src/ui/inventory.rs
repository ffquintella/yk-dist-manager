//! Inventory: every key the unit owns, and what the hardware reports.

use elegance::{Accent, Button, CalloutTone, Card};

use crate::app::YkDistApp;
use crate::store::key_status_str;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Inventory",
        "Keys held by the unit, identified from the hardware itself.",
    );

    ui.horizontal_wrapped(|ui| {
        if ui.add(Button::new("Read attached key")).clicked() {
            app.detect_keys();
        }
        let scan_label = if app.scan.open {
            "Hide serial scanner"
        } else {
            "Add by serial / scan…"
        };
        if ui.add(Button::new(scan_label).outline()).clicked() {
            app.scan.open = !app.scan.open;
            #[cfg(feature = "camera")]
            if !app.scan.open {
                app.stop_camera();
            }
        }
        ui.add_space(4.0);
        super::faint(ui, &format!("transport: {}", app.backend.describe()));
    });

    if app.scan.open {
        ui.add_space(12.0);
        scanner(app, ui);
    }

    if !app.detected.is_empty() {
        ui.add_space(12.0);
        for info in &app.detected {
            Card::new().show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.add(elegance::Badge::new("detected", elegance::BadgeTone::Ok));
                    ui.add_space(6.0);
                    super::mono(ui, &info.serial.to_string());
                    ui.label(&info.model);
                    super::faint(ui, &format!("firmware {}", info.firmware));
                });
                if !info.usb_applications.is_empty() {
                    super::faint(ui, &info.usb_applications.join(", "));
                }
            });
        }
    }

    ui.add_space(18.0);

    if app.keys.is_empty() {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "No keys yet. Plug one in and use “Read attached key”, or record a shipment by \
             serial with “Add by serial / scan…”.",
        );
        return;
    }

    let mut status_change: Option<(u32, crate::domain::KeyStatus)> = None;

    Card::new()
        .heading(format!("{} key(s)", app.keys.len()))
        .show(ui, |ui| {
            egui::Grid::new("keys")
                .striped(true)
                .num_columns(7)
                .spacing([18.0, 8.0])
                .show(ui, |ui| {
                    super::table_header(
                        ui,
                        &[
                            "Serial",
                            "Model",
                            "Firmware",
                            "Form factor",
                            "Status",
                            "Applications",
                            "Actions",
                        ],
                    );

                    for key in &app.keys {
                        super::mono(ui, &key.serial.to_string());
                        ui.label(&key.model);
                        ui.label(&key.firmware);
                        ui.label(if key.form_factor.is_empty() {
                            "—"
                        } else {
                            &key.form_factor
                        });
                        super::status_badge(ui, key.status);
                        super::faint(ui, &key.applications.join(", "));
                        ui.horizontal(|ui| {
                            if key.status == crate::domain::KeyStatus::InStock
                                && super::row_button(ui, "mark bootstrapped").clicked()
                            {
                                status_change =
                                    Some((key.serial, crate::domain::KeyStatus::Bootstrapped));
                            }
                            if super::row_button_danger(ui, "mark lost").clicked() {
                                status_change = Some((key.serial, crate::domain::KeyStatus::Lost));
                            }
                        });
                        ui.end_row();
                    }
                });
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
    Card::new()
        .heading("Record a key by serial")
        .show(ui, |ui| {
            super::hint(
                ui,
                "For receiving a shipment: the serial goes in now and the key is verified \
                 later, when it is plugged in. A USB barcode scanner types straight into \
                 the field.",
            );
            ui.add_space(10.0);

            ui.horizontal(|ui| {
                let response = super::capped_input(ui, &mut app.scan.typed, 24, |input| {
                    input
                        .hint("serial, or scan a barcode")
                        .id_salt("scan-typed")
                        .desired_width(220.0)
                });
                let submitted =
                    response.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter));
                if ui.add(Button::new("Add")).clicked() || submitted {
                    app.accept_typed_serial();
                }
            });

            camera_controls(app, ui);

            if let Some(serial) = app.scan.candidate {
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add(elegance::Badge::new(
                        format!("scanned: {serial}"),
                        elegance::BadgeTone::Ok,
                    ));
                    ui.add_space(6.0);
                    if ui
                        .add(Button::new("Add to inventory").accent(Accent::Green))
                        .clicked()
                    {
                        app.accept_scanned_serial();
                    }
                    if ui.add(Button::new("Discard").outline()).clicked() {
                        app.scan.candidate = None;
                        #[cfg(feature = "camera")]
                        if let Some(scanner) = &app.scan.scanner {
                            scanner.clear_serial();
                        }
                    }
                });
            }

            if let Some(error) = app.scan.error.clone() {
                ui.add_space(8.0);
                super::error_label(ui, &error);
            }
        });
}

#[cfg(feature = "camera")]
fn camera_controls(app: &mut YkDistApp, ui: &mut egui::Ui) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        if app.scan.scanner.is_none() {
            if ui.add(Button::new("Start camera").outline()).clicked() {
                app.start_camera();
            }
        } else if ui
            .add(Button::new("Stop camera").accent(Accent::Red))
            .clicked()
        {
            app.stop_camera();
        }
        if let Some(scanner) = &app.scan.scanner {
            ui.add_space(4.0);
            super::faint(ui, &format!("camera: {}", scanner.describe()));
        }
    });

    if let Some(texture) = &app.scan.preview {
        ui.add_space(8.0);
        // Keep the preview modest: it is an aiming aid, not a viewfinder.
        let size = texture.size_vec2();
        let scale = (360.0 / size.x).min(1.0);
        ui.add(
            egui::Image::new(texture)
                .fit_to_exact_size(size * scale)
                .corner_radius(egui::CornerRadius::same(8)),
        );
        ui.add_space(4.0);
        super::hint(
            ui,
            "Fill the frame width with the barcode, ~20cm away. A laptop camera is \
             fixed-focus and struggles closer than that.",
        );
    }
}

#[cfg(not(feature = "camera"))]
fn camera_controls(_app: &mut YkDistApp, ui: &mut egui::Ui) {
    ui.add_space(8.0);
    super::notice(
        ui,
        CalloutTone::Neutral,
        "Camera scanning needs a build with `--features camera`. A USB barcode scanner \
         works in any build — it types into the field above.",
    );
}
