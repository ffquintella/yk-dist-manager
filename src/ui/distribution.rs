//! Distribution: record a hand-over, see who holds what, close a return.

use elegance::{Accent, Button, CalloutTone, Checkbox, Select};

use crate::app::{Tab, YkDistApp};
use crate::domain::{DeliveryMethod, DocumentKind, MAX_NOTE, MAX_TEXT};
use crate::term::BUILTIN_LANGUAGES;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Distribution",
        "Who holds which key, since when, handed over by whom, and what was applied to it.",
    );

    if app.keys.is_empty() || app.holders.is_empty() {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "Register at least one key and one holder first.",
        );
    } else {
        record_form(app, ui);
    }

    ui.add_space(18.0);
    history(app, ui);

    if app.term_panel.open {
        ui.add_space(18.0);
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

    let heading = match serial {
        Some(serial) => format!("Consignment term — serial {serial}"),
        None => "Consignment term".to_owned(),
    };

    super::titled_card(ui, heading, |ui| {
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
        ui.horizontal_top(|ui| {
            let before = app.term_panel.language.clone();
            ui.add(
                Select::strings("term-language", &mut app.term_panel.language, languages)
                    .label("Language")
                    .width(180.0),
            );
            if app.term_panel.language != before {
                regenerate = true;
            }

            ui.add_space(10.0);
            // Nudge the buttons down to sit on the select's baseline, which
            // carries a label above it.
            ui.vertical(|ui| {
                ui.add_space(20.0);
                ui.horizontal(|ui| {
                    if ui.add(Button::new("Generate")).clicked() {
                        regenerate = true;
                    }
                    if ui
                        .add(Button::new("Edit wording…").outline())
                        .on_hover_text(
                            "open the Terms screen on this language — an edit is stored as a \
                             new version and applies to the next term generated",
                        )
                        .clicked()
                    {
                        let language = app.term_panel.language.clone();
                        app.load_term_template(&language);
                        app.tab = Tab::Terms;
                    }
                    if ui.add(Button::new("Close").outline()).clicked() {
                        app.term_panel.open = false;
                    }
                });
            });
        });

        if let Some(used) = app.term_panel.language_used.clone() {
            ui.add_space(8.0);
            super::notice(
                ui,
                CalloutTone::Warning,
                &format!(
                    "no template in {} — rendered in {used}",
                    app.term_panel.language
                ),
            );
        }

        if let Some(used) = app.term_panel.template_used.clone() {
            ui.add_space(6.0);
            // Which wording produced this document: an edit bumps the version, so
            // the number is how the operator sees that it took effect.
            super::faint(ui, &format!("rendered from template {used}"));
        }

        if let Some(error) = app.term_panel.error.clone() {
            ui.add_space(8.0);
            super::error_label(ui, &error);
        }

        if let Some(text) = app.term_panel.rendered.clone() {
            ui.add_space(10.0);
            let theme = elegance::Theme::current(ui.ctx());
            egui::Frame::new()
                .fill(theme.palette.input_bg)
                .stroke(egui::Stroke::new(1.0, theme.palette.border))
                .corner_radius(egui::CornerRadius::same(8))
                .inner_margin(egui::Margin::same(12))
                .show(ui, |ui| {
                    // The document reads as a page: full card width, and the
                    // long lines scroll here rather than widening the screen.
                    ui.set_min_width(ui.available_width());
                    egui::ScrollArea::both()
                        .max_height(300.0)
                        .auto_shrink([false, true])
                        .id_salt("term-preview")
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(&text)
                                        .monospace()
                                        .size(theme.typography.monospace)
                                        .color(theme.palette.text),
                                )
                                .selectable(true),
                            );
                        });
                });

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui.add(Button::new("Save as text…")).clicked() {
                    app.save_term();
                }
                if ui
                    .add(Button::new("Upload signed term…").accent(Accent::Green))
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
            ui.add_space(10.0);
            super::error_label(ui, &format!("could not list documents: {e}"));
            return;
        }
    };

    ui.add_space(14.0);
    let theme = elegance::Theme::current(ui.ctx());
    ui.add(egui::Label::new(
        egui::RichText::new("Filed documents")
            .size(theme.typography.label)
            .color(theme.palette.text_muted)
            .strong(),
    ));
    ui.add_space(6.0);

    if filed.is_empty() {
        super::hint(
            ui,
            "Nothing filed yet — upload the signed term once it comes back.",
        );
        return;
    }

    let mut export: Option<uuid::Uuid> = None;

    super::table(
        ui,
        "documents",
        &["Kind", "File", "Size", "SHA-256", "Filed"],
        |ui| {
            for document in &filed {
                ui.label(document.kind.label());
                super::mono(ui, &document.filename);
                ui.label(document.size_label());
                super::mono(ui, &document.short_digest()).on_hover_text(&document.sha256);
                ui.horizontal(|ui| {
                    super::faint(
                        ui,
                        &format!(
                            "{} by {}",
                            document.uploaded_at.format("%d/%m/%Y"),
                            document.uploaded_by
                        ),
                    );
                    if super::row_button(ui, "export").clicked() {
                        export = Some(document.id);
                    }
                });
                ui.end_row();
            }
        },
    );

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

    super::titled_card(ui, "Record a hand-over", |ui| {
        super::form_columns(ui, |left, right, width| {
            left.add(
                Select::new("dist-key", &mut app.dist_form.key_index)
                    .label("Key")
                    .options(key_labels.iter().cloned().enumerate())
                    .width(width),
            );
            left.add_space(8.0);
            left.add(
                Select::new("dist-holder", &mut app.dist_form.holder_index)
                    .label("Holder")
                    .options(holder_labels.iter().cloned().enumerate())
                    .width(width),
            );
            left.add_space(8.0);
            left.add(
                Select::new("dist-method", &mut app.dist_form.method)
                    .label("Delivery")
                    .options(DeliveryMethod::ALL.map(|m| (m, m.label())))
                    .width(width),
            );

            super::capped_input(right, &mut app.dist_form.receipt_ref, MAX_TEXT, |input| {
                input
                    .label("Receipt / term reference")
                    .id_salt("dist-receipt")
            });
            right.add_space(8.0);
            super::capped_area(right, &mut app.dist_form.notes, MAX_NOTE, |area| {
                area.label("Notes").rows(3).id_salt("dist-notes")
            });
        });

        ui.add_space(12.0);
        ui.add(Checkbox::new(
            &mut app.dist_form.link_last_run,
            "Attach the most recent bootstrap run for this key",
        ));
        ui.add_space(12.0);
        if ui.add(Button::new("Record distribution")).clicked() {
            app.submit_distribution();
        }
        if let Some(error) = app.dist_form.error.clone() {
            ui.add_space(10.0);
            super::error_label(ui, &error);
        }
    });
}

fn history(app: &mut YkDistApp, ui: &mut egui::Ui) {
    if app.distributions.is_empty() {
        super::notice(ui, CalloutTone::Neutral, "Nothing distributed yet.");
        return;
    }

    let now = chrono::Utc::now();
    let mut to_return: Option<(uuid::Uuid, u32)> = None;
    let mut show_term: Option<uuid::Uuid> = None;
    let mut upload_for: Option<uuid::Uuid> = None;

    super::titled_card(
        ui,
        format!("{} hand-over(s)", app.distributions.len()),
        |ui| {
            super::table(
                ui,
                "distributions",
                &[
                    "Serial",
                    "Holder",
                    "Handed over",
                    "By",
                    "Applied",
                    "Status",
                    "Term",
                    "Actions",
                ],
                |ui| {
                    for record in &app.distributions {
                        super::mono(ui, &record.key_serial.to_string());
                        ui.label(&record.holder_display);
                        ui.label(record.distributed_at.format("%d/%m/%Y %H:%M").to_string());
                        ui.label(&record.distributed_by);

                        let applied = record
                            .bootstrap_run_id
                            .and_then(|id| app.runs.iter().find(|r| r.id == id))
                            .map(|run| run.summary())
                            .unwrap_or_else(|| "—".into());
                        super::faint(ui, &applied);

                        if record.is_open() {
                            ui.add(elegance::Badge::new(
                                format!("held {}d", record.days_held(now)),
                                elegance::BadgeTone::Info,
                            ));
                        } else {
                            ui.add(elegance::Badge::new(
                                format!(
                                    "returned {}",
                                    record
                                        .returned_at
                                        .map(|at| at.format("%d/%m/%Y").to_string())
                                        .unwrap_or_default()
                                ),
                                elegance::BadgeTone::Neutral,
                            ));
                        }

                        // Term column: how many documents are filed, and the actions.
                        let filed = app.document_counts.get(&record.id).copied().unwrap_or(0);
                        ui.horizontal(|ui| {
                            if super::row_button(ui, "term").clicked() {
                                show_term = Some(record.id);
                            }
                            if super::row_button(ui, "upload").clicked() {
                                upload_for = Some(record.id);
                            }
                            if filed == 0 {
                                ui.add(elegance::Badge::new(
                                    "none filed",
                                    elegance::BadgeTone::Warning,
                                ));
                            } else {
                                ui.add(elegance::Badge::new(
                                    format!("{filed} filed"),
                                    elegance::BadgeTone::Ok,
                                ));
                            }
                        });

                        ui.horizontal(|ui| {
                            if record.is_open() && super::row_button(ui, "record return").clicked()
                            {
                                to_return = Some((record.id, record.key_serial));
                            }
                        });
                        ui.end_row();
                    }
                },
            );
        },
    );

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
