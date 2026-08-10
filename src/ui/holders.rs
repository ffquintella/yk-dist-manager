//! Holders: the people who receive keys.

use elegance::{Button, CalloutTone, Card};

use crate::app::YkDistApp;
use crate::domain::{MAX_NOTE, MAX_TEXT};

/// Width of a form field. Wide enough for a corporate e-mail, narrow enough
/// that the label stays in the same glance.
const FIELD: f32 = 340.0;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Holders",
        "Minimum personal data: name, corporate e-mail, unit. The e-mail is what \
         binds the signing certificate to the person.",
    );

    register_form(app, ui);

    ui.add_space(18.0);

    if app.holders.is_empty() {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "No holders registered yet. A hand-over needs one.",
        );
        return;
    }

    register(app, ui);
}

fn register_form(app: &mut YkDistApp, ui: &mut egui::Ui) {
    Card::new().heading("Register a holder").show(ui, |ui| {
        // Two columns: what the certificate needs, and what only the term uses.
        ui.horizontal_top(|ui| {
            ui.vertical(|ui| {
                ui.set_width(FIELD + 16.0);
                super::capped_input(ui, &mut app.holder_form.full_name, MAX_TEXT, |input| {
                    input
                        .label("Full name")
                        .id_salt("holder-name")
                        .desired_width(FIELD)
                });
                ui.add_space(8.0);
                super::capped_input(ui, &mut app.holder_form.email, MAX_TEXT, |input| {
                    input
                        .label("Corporate e-mail")
                        .hint("name@fgv.br")
                        .id_salt("holder-email")
                        .desired_width(FIELD)
                });
                ui.add_space(8.0);
                super::capped_input(ui, &mut app.holder_form.unit, MAX_TEXT, |input| {
                    input
                        .label("Unit")
                        .id_salt("holder-unit")
                        .desired_width(FIELD)
                });
                ui.add_space(8.0);
                super::capped_input(ui, &mut app.holder_form.registration, MAX_TEXT, |input| {
                    input
                        .label("Registration (optional)")
                        .id_salt("holder-registration")
                        .desired_width(FIELD)
                });
            });

            ui.add_space(20.0);

            ui.vertical(|ui| {
                ui.set_width(FIELD + 16.0);
                super::capped_input(
                    ui,
                    &mut app.holder_form.identification_number,
                    MAX_TEXT,
                    |input| {
                        input
                            .label("Identification number (optional)")
                            .hint("CPF or the local equivalent")
                            .id_salt("holder-identification")
                            .desired_width(FIELD)
                    },
                );
                ui.add_space(8.0);
                super::capped_input(ui, &mut app.holder_form.phone, MAX_TEXT, |input| {
                    input
                        .label("Phone (optional)")
                        .id_salt("holder-phone")
                        .desired_width(FIELD)
                });
                ui.add_space(8.0);
                super::capped_area(ui, &mut app.holder_form.address, MAX_NOTE, |area| {
                    area.label("Address (optional)")
                        .rows(2)
                        .id_salt("holder-address")
                        .desired_width(FIELD)
                });
            });
        });

        ui.add_space(10.0);
        super::hint(
            ui,
            "The optional fields appear on the consignment term when they are filled in, \
             and the corresponding line is omitted when they are not.",
        );

        ui.add_space(12.0);
        if ui.add(Button::new("Register holder")).clicked() {
            app.submit_holder();
        }

        if let Some(error) = app.holder_form.error.clone() {
            ui.add_space(10.0);
            super::error_label(ui, &error);
        }
    });
}

fn register(app: &mut YkDistApp, ui: &mut egui::Ui) {
    Card::new()
        .heading(format!("{} holder(s)", app.holders.len()))
        .show(ui, |ui| {
            egui::Grid::new("holders")
                .striped(true)
                .num_columns(6)
                .spacing([18.0, 8.0])
                .show(ui, |ui| {
                    super::table_header(
                        ui,
                        &[
                            "Name",
                            "E-mail",
                            "Unit",
                            "Identification",
                            "Contact",
                            "Keys held",
                        ],
                    );

                    for holder in &app.holders {
                        let held = app
                            .distributions
                            .iter()
                            .filter(|d| d.holder_id == holder.id && d.is_open())
                            .count();
                        ui.label(&holder.full_name);
                        super::mono(ui, &holder.email);
                        ui.label(&holder.unit);
                        ui.label(if holder.has_identification() {
                            &holder.identification_number
                        } else {
                            "—"
                        });
                        ui.label(if holder.phone.is_empty() {
                            "—"
                        } else {
                            &holder.phone
                        });
                        // Zero is the common case and should not shout.
                        if held == 0 {
                            super::faint(ui, "—");
                        } else {
                            ui.add(elegance::Badge::new(
                                held.to_string(),
                                elegance::BadgeTone::Info,
                            ));
                        }
                        ui.end_row();
                    }
                });
        });
}
