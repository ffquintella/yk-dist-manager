//! Holders: the people who receive keys.

use elegance::{Button, CalloutTone};

use crate::app::YkDistApp;
use crate::domain::{MAX_NOTE, MAX_TEXT};

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
    super::titled_card(ui, "Register a holder", |ui| {
        // Two columns: what the certificate needs, and what only the term uses.
        // Each takes half the card, so the fields grow with the window.
        super::form_columns(ui, |left, right, _width| {
            super::capped_input(left, &mut app.holder_form.full_name, MAX_TEXT, |input| {
                input.label("Full name").id_salt("holder-name")
            });
            left.add_space(8.0);
            super::capped_input(left, &mut app.holder_form.email, MAX_TEXT, |input| {
                input
                    .label("Corporate e-mail")
                    .hint("name@example.org")
                    .id_salt("holder-email")
            });
            left.add_space(8.0);
            super::capped_input(left, &mut app.holder_form.unit, MAX_TEXT, |input| {
                input.label("Unit").id_salt("holder-unit")
            });
            left.add_space(8.0);
            super::capped_input(left, &mut app.holder_form.registration, MAX_TEXT, |input| {
                input
                    .label("Registration (optional)")
                    .id_salt("holder-registration")
            });

            super::capped_input(
                right,
                &mut app.holder_form.identification_number,
                MAX_TEXT,
                |input| {
                    input
                        .label("Identification number (optional)")
                        .hint("CPF or the local equivalent")
                        .id_salt("holder-identification")
                },
            );
            right.add_space(8.0);
            super::capped_input(right, &mut app.holder_form.phone, MAX_TEXT, |input| {
                input.label("Phone (optional)").id_salt("holder-phone")
            });
            right.add_space(8.0);
            super::capped_area(right, &mut app.holder_form.address, MAX_NOTE, |area| {
                area.label("Address (optional)")
                    .rows(2)
                    .id_salt("holder-address")
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
    let page = crate::browse::holders(
        &app.holders,
        &app.browse_holders.query(),
        app.browse_holders.sort,
        app.browse_holders.direction,
        app.browse_holders.page,
    );
    let rows: Vec<crate::domain::Holder> = page.rows.iter().map(|h| (*h).clone()).collect();
    let summary = page.describe("holders");
    let (pages, current) = (page.pages, page.page);
    drop(page);

    super::titled_card(ui, summary.clone(), |ui| {
        super::table_controls(ui, &mut app.browse_holders, pages, current, &summary);
        ui.horizontal(|ui| {
            ui.label("Sort:");
            super::sort_header(
                ui,
                &mut app.browse_holders,
                "Name",
                crate::browse::HolderSort::Name,
            );
            super::sort_header(
                ui,
                &mut app.browse_holders,
                "E-mail",
                crate::browse::HolderSort::Email,
            );
            super::sort_header(
                ui,
                &mut app.browse_holders,
                "Unit",
                crate::browse::HolderSort::Unit,
            );
        });
        ui.add_space(6.0);

        super::table(
            ui,
            "holders",
            &[
                "Name",
                "E-mail",
                "Unit",
                "Identification",
                "Contact",
                "Keys held",
            ],
            |ui| {
                for holder in &rows {
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
            },
        );
    });
}
