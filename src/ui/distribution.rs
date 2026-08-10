//! Distribution: record a hand-over, see who holds what, close a return.

use crate::app::YkDistApp;
use crate::domain::{DeliveryMethod, DocumentKind};
use crate::term::BUILTIN_LANGUAGES;

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

    if app.term_panel.open {
        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        term(app, ui);
    }
}

/// The consignment term: generate it in a language, review it, save it, and file
/// the signed copy back against the hand-over.
fn term(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(distribution_id) = app.term_panel.distribution else {
        return;
    };
    let serial = app
        .distributions
        .iter()
        .find(|d| d.id == distribution_id)
        .map(|d| d.key_serial);

    egui::Frame::group(ui.style()).show(ui, |ui| {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(match serial {
                    Some(serial) => format!("Consignment term — serial {serial}"),
                    None => "Consignment term".to_owned(),
                })
                .strong(),
            );
            if ui.small_button("close").clicked() {
                app.term_panel.open = false;
            }
        });

        ui.add_space(6.0);

        // Languages: the ones shipped, plus anything the database carries.
        let mut languages: Vec<String> = app
            .term_templates
            .iter()
            .filter(|t| t.id == "consignment")
            .map(|t| t.language.clone())
            .collect();
        for builtin in BUILTIN_LANGUAGES {
            if !languages.iter().any(|l| l == builtin) {
                languages.push(builtin.to_owned());
            }
        }
        languages.sort();
        languages.dedup();

        let mut regenerate = false;
        ui.horizontal(|ui| {
            ui.label("Language");
            egui::ComboBox::from_id_salt("term-language")
                .selected_text(&app.term_panel.language)
                .show_ui(ui, |ui| {
                    for language in &languages {
                        if ui
                            .selectable_label(&app.term_panel.language == language, language)
                            .clicked()
                        {
                            app.term_panel.language = language.clone();
                            regenerate = true;
                        }
                    }
                });
            if ui.button("Generate").clicked() {
                regenerate = true;
            }
        });

        if let Some(used) = app.term_panel.language_used.clone() {
            ui.label(
                egui::RichText::new(format!(
                    "no template in {} — rendered in {used}",
                    app.term_panel.language
                ))
                .small()
                .color(egui::Color32::from_rgb(190, 140, 40)),
            );
        }

        if let Some(error) = app.term_panel.error.clone() {
            super::error_label(ui, &error);
        }

        if let Some(text) = app.term_panel.rendered.clone() {
            ui.add_space(6.0);
            egui::ScrollArea::vertical()
                .max_height(280.0)
                .id_salt("term-preview")
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&text).monospace().small())
                            .selectable(true),
                    );
                });

            ui.add_space(6.0);
            ui.horizontal_wrapped(|ui| {
                if ui.button("Save as text…").clicked() {
                    app.save_term();
                }
                if ui
                    .button("Upload signed term…")
                    .on_hover_text("file the scanned, signed document against this hand-over")
                    .clicked()
                {
                    app.attach_document(distribution_id, DocumentKind::SignedTerm);
                }
            });
        }

        documents(app, ui, distribution_id);

        if regenerate {
            app.generate_term(distribution_id);
        }
    });
}

/// Documents already filed against this hand-over.
fn documents(app: &mut YkDistApp, ui: &mut egui::Ui, distribution_id: uuid::Uuid) {
    let Some(store) = &app.store else { return };
    let filed = match store.documents_for(distribution_id) {
        Ok(filed) => filed,
        Err(e) => {
            ui.label(
                egui::RichText::new(format!("could not list documents: {e}"))
                    .small()
                    .color(egui::Color32::from_rgb(200, 60, 60)),
            );
            return;
        }
    };

    ui.add_space(10.0);
    ui.label(egui::RichText::new("Filed documents").strong());
    if filed.is_empty() {
        ui.label(
            egui::RichText::new("Nothing filed yet — upload the signed term once it comes back.")
                .small()
                .weak(),
        );
        return;
    }

    let mut export: Option<uuid::Uuid> = None;

    egui::Grid::new("documents")
        .num_columns(5)
        .spacing([12.0, 4.0])
        .show(ui, |ui| {
            for header in ["Kind", "File", "Size", "SHA-256", "Filed"] {
                ui.strong(header);
            }
            ui.end_row();

            for document in &filed {
                ui.label(document.kind.label());
                ui.monospace(&document.filename);
                ui.label(document.size_label());
                ui.label(
                    egui::RichText::new(document.short_digest())
                        .monospace()
                        .small(),
                )
                .on_hover_text(&document.sha256);
                ui.horizontal(|ui| {
                    ui.label(
                        egui::RichText::new(format!(
                            "{} by {}",
                            document.uploaded_at.format("%d/%m/%Y"),
                            document.uploaded_by
                        ))
                        .small(),
                    );
                    if ui.small_button("export").clicked() {
                        export = Some(document.id);
                    }
                });
                ui.end_row();
            }
        });

    if let Some(id) = export {
        app.export_document(id);
    }
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
    let mut show_term: Option<uuid::Uuid> = None;
    let mut upload_for: Option<uuid::Uuid> = None;

    egui::Grid::new("distributions")
        .striped(true)
        .num_columns(8)
        .spacing([16.0, 6.0])
        .show(ui, |ui| {
            for header in [
                "Serial",
                "Holder",
                "Handed over",
                "By",
                "Applied",
                "Status",
                "Term",
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

                // Term column: how many documents are filed, and the actions.
                let filed = app.document_counts.get(&record.id).copied().unwrap_or(0);
                ui.horizontal(|ui| {
                    if ui.small_button("term").clicked() {
                        show_term = Some(record.id);
                    }
                    if ui.small_button("upload").clicked() {
                        upload_for = Some(record.id);
                    }
                    ui.label(
                        egui::RichText::new(if filed == 0 {
                            "none filed".to_owned()
                        } else {
                            format!("{filed} filed")
                        })
                        .small()
                        .color(if filed == 0 {
                            egui::Color32::from_rgb(190, 140, 40)
                        } else {
                            egui::Color32::from_rgb(60, 140, 70)
                        }),
                    );
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
    if let Some(id) = show_term {
        app.generate_term(id);
    }
    if let Some(id) = upload_for {
        app.attach_document(id, DocumentKind::SignedTerm);
    }
}
