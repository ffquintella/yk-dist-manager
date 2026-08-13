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
    paperwork(app, ui);
    history(app, ui);

    if app.term_panel.open {
        ui.add_space(18.0);
        term(app, ui);
    }
}

/// What paperwork is outstanding across the whole register
/// (`features/receipts-and-terms.md` phase 4).
///
/// One line, and **only when there is something to say**: a banner that is always
/// on screen is one nobody reads. It is at the top of this screen rather than in
/// Settings because this is where the hand-overs are, and the action it asks for —
/// file the scan, or record the unit's own reference — is a row away.
fn paperwork(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let tally = app.outstanding_paperwork();
    let Some(line) = tally.describe() else {
        return;
    };

    super::notice(
        ui,
        if tally.needs_attention() {
            CalloutTone::Warning
        } else {
            CalloutTone::Neutral
        },
        &format!(
            "{line}. {}. An unsigned term means the holder has not acknowledged the obligations \
             the loss procedure depends on, so it is a gap in the record rather than a missing \
             attachment.",
            app.settings.signatures.describe()
        ),
    );
    ui.add_space(12.0);
}

/// The consignment term or the return receipt: generate it in a language, review
/// it, save it, and file the signed copy back against the hand-over.
fn term(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(distribution_id) = app.term_panel.distribution else {
        return;
    };
    let serial = app
        .distributions
        .iter()
        .find(|d| d.id == distribution_id)
        .map(|d| d.key_serial);

    // One panel, two documents. The heading says which, because the two are
    // legally different things and a reviewer must never be unsure which one is on
    // screen.
    let is_return = app.term_panel.document == crate::term::RETURN_ID;
    let what = if is_return {
        "Return receipt"
    } else {
        "Consignment term"
    };
    let heading = match serial {
        Some(serial) => format!("{what} — serial {serial}"),
        None => what.to_owned(),
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

            if let Some(note) = app.term_panel.pdf_note.clone() {
                ui.add_space(8.0);
                super::notice(ui, CalloutTone::Warning, &note);
            }

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add(Button::new("Export as PDF…").accent(Accent::Blue))
                    .on_hover_text("the sheet to print, sign and file")
                    .clicked()
                {
                    app.save_term_pdf();
                }
                if ui
                    .add(Button::new("Save as text…").outline())
                    .on_hover_text("the same document as plain text, for a ticket")
                    .clicked()
                {
                    app.save_term();
                }
                if ui
                    .add(
                        Button::new(if is_return {
                            "Upload signed receipt…"
                        } else {
                            "Upload signed term…"
                        })
                        .accent(Accent::Green),
                    )
                    .on_hover_text("file the scanned, signed document against this hand-over")
                    .clicked()
                {
                    // The kind follows the document on screen: a signed return
                    // receipt filed as a signed *term* would settle the signature
                    // state of a hand-over nobody signed for.
                    app.attach_document(
                        distribution_id,
                        if is_return {
                            DocumentKind::ReturnReceipt
                        } else {
                            DocumentKind::SignedTerm
                        },
                    );
                }
            });

            // The other way a term gets settled, and the one a unit that files
            // paper elsewhere needs: record *their* reference. It lives here
            // because this is the moment it exists — a posted key's term comes back
            // days after the hand-over was recorded, and until now there was
            // nowhere to put the number.
            if !is_return {
                ui.add_space(10.0);
                let mut record_reference = false;
                super::form_columns(ui, |left, right, _width| {
                    let response = super::capped_input(
                        left,
                        &mut app.term_panel.reference,
                        MAX_TEXT,
                        |input| {
                            input
                                .label("Or record your own reference")
                                .hint("processo 2026/114 — where your unit filed the signed term")
                                .id_salt("term-reference")
                        },
                    );
                    if response.lost_focus() && left.input(|i| i.key_pressed(egui::Key::Enter)) {
                        record_reference = true;
                    }
                    right.add_space(16.0);
                    if right
                        .add(
                            Button::new("Record reference")
                                .outline()
                                .enabled(!app.term_panel.reference.trim().is_empty()),
                        )
                        .on_hover_text(
                            "marks this hand-over's term as signed, and records the reference in \
                             the audit trail",
                        )
                        .clicked()
                    {
                        record_reference = true;
                    }
                });
                if record_reference {
                    let reference = app.term_panel.reference.clone();
                    app.record_receipt_reference(distribution_id, &reference);
                }
            }
        }

        documents(app, ui, distribution_id);

        if regenerate {
            // Whichever document is open — switching language must not silently
            // swap a return receipt for a consignment term.
            if is_return {
                app.generate_return_receipt(distribution_id);
            } else {
                app.generate_term(distribution_id);
            }
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
    let mut show_receipt: Option<uuid::Uuid> = None;
    let mut upload_for: Option<uuid::Uuid> = None;

    let page = crate::browse::distributions(
        &app.distributions,
        &app.browse_distributions.query(),
        app.outstanding_only,
        app.browse_distributions.sort,
        app.browse_distributions.direction,
        app.browse_distributions.page,
    );
    let rows: Vec<crate::domain::DistributionRecord> =
        page.rows.iter().map(|d| (*d).clone()).collect();
    let summary = page.describe("hand-overs");
    let (pages, current) = (page.pages, page.page);
    drop(page);

    super::titled_card(ui, summary.clone(), |ui| {
        super::table_controls(ui, &mut app.browse_distributions, pages, current, &summary);
        ui.horizontal(|ui| {
            // The question this screen is usually opened to answer: who
            // still has a key?
            if ui
                .checkbox(&mut app.outstanding_only, "Outstanding only")
                .changed()
            {
                app.browse_distributions.page = 0;
            }
            ui.add_space(12.0);
            ui.label("Sort:");
            super::sort_header(
                ui,
                &mut app.browse_distributions,
                "Date",
                crate::browse::DistributionSort::Date,
            );
            super::sort_header(
                ui,
                &mut app.browse_distributions,
                "Serial",
                crate::browse::DistributionSort::Serial,
            );
            super::sort_header(
                ui,
                &mut app.browse_distributions,
                "Holder",
                crate::browse::DistributionSort::Holder,
            );
            super::sort_header(
                ui,
                &mut app.browse_distributions,
                "Returned",
                crate::browse::DistributionSort::Returned,
            );
        });
        ui.add_space(6.0);

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
                for record in &rows {
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

                    // Term column: where the signature stands, then the actions.
                    // The state is derived (`crate::receipt`) rather than a count of
                    // attachments — a generated term filed against a hand-over is
                    // not evidence that anybody signed it.
                    let signature = app.signature_state(record);
                    let filed = app.document_counts.get(&record.id).copied().unwrap_or(0);
                    ui.horizontal(|ui| {
                        use crate::receipt::SignatureState;
                        let tone = match &signature {
                            SignatureState::Signed { .. } => elegance::BadgeTone::Ok,
                            SignatureState::NotRequired => elegance::BadgeTone::Neutral,
                            SignatureState::Pending { .. } => elegance::BadgeTone::Info,
                            SignatureState::Overdue { .. }
                            | SignatureState::MissingOnReturn { .. } => {
                                elegance::BadgeTone::Warning
                            }
                        };
                        // Days on the badge, because "overdue" without a number is
                        // not something an operator can prioritise.
                        let label = match &signature {
                            SignatureState::Pending { days }
                            | SignatureState::Overdue { days, .. }
                            | SignatureState::MissingOnReturn { days } => {
                                format!("{} · {days}d", signature.label())
                            }
                            other => other.label().to_owned(),
                        };
                        ui.add(elegance::Badge::new(label, tone))
                            .on_hover_text(signature.describe());
                        if filed > 0 {
                            ui.add(elegance::Badge::new(
                                format!("{filed} filed"),
                                elegance::BadgeTone::Neutral,
                            ));
                        }
                    });

                    ui.horizontal(|ui| {
                        if super::row_button(ui, "term").clicked() {
                            show_term = Some(record.id);
                        }
                        if super::row_button(ui, "upload").clicked() {
                            upload_for = Some(record.id);
                        }
                        if record.is_open() {
                            if super::row_button(ui, "record return").clicked() {
                                to_return = Some((record.id, record.key_serial));
                            }
                        } else {
                            // The other half of the custody loop: a returned key with
                            // no receipt filed is a return only the unit is
                            // asserting. Offered on returned rows only — a receipt
                            // for a key still in somebody's pocket would be a
                            // document contradicting the register.
                            if super::row_button(ui, "return receipt")
                                .on_hover_text(
                                    "the document that closes the custody loop: both ends of the \
                                     hand-over, and what happens to the credentials",
                                )
                                .clicked()
                            {
                                show_receipt = Some(record.id);
                            }
                            if app.return_state(record) == crate::receipt::ReturnState::Undocumented
                            {
                                ui.add(elegance::Badge::new(
                                    "no receipt",
                                    elegance::BadgeTone::Warning,
                                ))
                                .on_hover_text("the key is back and nothing is filed to say so");
                            }
                        }
                    });
                    ui.end_row();
                }
            },
        );
    });

    if let Some((id, serial)) = to_return {
        app.return_key(id, serial);
    }
    if let Some(id) = show_term {
        app.generate_term(id);
    }
    if let Some(id) = show_receipt {
        app.generate_return_receipt(id);
    }
    if let Some(id) = upload_for {
        app.attach_document(id, DocumentKind::SignedTerm);
    }
}
