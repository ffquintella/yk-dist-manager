//! Terms: the wording of the consignment term, per language.
//!
//! The term is the artefact that survives an audit, and its wording is
//! institutional text somebody else owns — which is exactly why it is data and
//! not a constant in the source. This screen is where that data is edited.
//!
//! Two rules the screen makes visible rather than assuming:
//!
//! * **Saving stores a new version.** The version already on record is left
//!   alone, because a holder may have signed it. Generating a term takes the
//!   newest version of the language ([`crate::term::choose_template`]).
//! * **A line whose variable is empty disappears.** That is the whole
//!   conditional logic, so a template carries `Telefone: {{holder.phone}}`
//!   unconditionally, and the preview shows what the holder will read.

use elegance::{Accent, Badge, BadgeTone, Button, CalloutTone, Select};

use crate::app::YkDistApp;
use crate::term::{self, MAX_BODY, TermContext, TermTemplate};

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Terms",
        "The consignment term the holder signs, in each language it is offered in. Saving \
         stores a new version — the wording already signed stays readable.",
    );

    if app.store.is_none() {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "No database open — open one to edit the term wording.",
        );
        return;
    }

    // First paint after the screen opens: fill the buffers from the cached
    // templates. No database read, so this is safe inside the paint pass.
    if !app.term_editor.loaded {
        let language = app.term_editor.language.clone();
        app.load_term_template(&language);
    }

    document_bar(app, ui);
    ui.add_space(14.0);
    language_bar(app, ui);
    ui.add_space(14.0);
    editor(app, ui);
    ui.add_space(14.0);
    variables(ui);

    if app.term_editor.preview.is_some() {
        ui.add_space(14.0);
        preview(app, ui);
    }

    ui.add_space(14.0);
    versions(app, ui);
}

/// Which **document** is being edited: the consignment term or the return receipt.
///
/// A picker rather than two screens, because everything below it is identical —
/// the wording is versioned `(id, language, version)` either way, rendered by the
/// same code, exported by the same PDF writer. The return receipt
/// (`features/receipts-and-terms.md` phase 6) is a second template id and nothing
/// more, which is why it cost a picker rather than a feature.
fn document_bar(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let ids: Vec<String> = {
        // Everything on record, plus what this build ships, so a unit that added
        // its own document type still finds it here.
        let mut ids: Vec<String> = app
            .term_templates
            .iter()
            .map(|template| template.id.clone())
            .collect();
        for builtin in term::BUILTIN_IDS {
            if !ids.iter().any(|id| id == builtin) {
                ids.push(builtin.to_owned());
            }
        }
        ids.sort();
        ids.dedup();
        ids
    };

    let mut chosen = app.term_editor.id.clone();
    let mut switch_to: Option<String> = None;

    super::card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(
                    Select::strings("term-editor-document", &mut chosen, ids)
                        .label("Document")
                        .width(200.0),
                )
                .changed()
                && chosen != app.term_editor.id
            {
                switch_to = Some(chosen.clone());
            }
            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(16.0);
                super::faint(
                    ui,
                    match app.term_editor.id.as_str() {
                        term::RETURN_ID => {
                            "the receipt that closes the custody loop when a key comes back"
                        }
                        term::CONSIGNMENT_ID => {
                            "what the holder signs on receiving a key — the obligations the loss                              procedure rests on"
                        }
                        _ => "a document type this unit added",
                    },
                );
            });
        });
    });

    if let Some(id) = switch_to {
        app.load_term_document(&id);
    }
}

/// Which language is being edited, what state it is in, and how to add another.
fn language_bar(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let id = app.term_editor.id.clone();
    let dirty = app.term_editor.is_dirty(&app.term_templates);

    // Everything on record, plus the languages this build ships, plus whatever
    // language is being drafted right now.
    let mut languages: Vec<String> = term::languages_of(&app.term_templates, &id)
        .into_iter()
        .map(|t| t.language.clone())
        .collect();
    for builtin in term::BUILTIN_LANGUAGES {
        if !languages.iter().any(|l| l == builtin) {
            languages.push(builtin.to_owned());
        }
    }
    if !languages.iter().any(|l| l == &app.term_editor.language)
        && !app.term_editor.language.trim().is_empty()
    {
        languages.push(app.term_editor.language.clone());
    }
    languages.sort();
    languages.dedup();

    let mut chosen = app.term_editor.language.clone();
    let mut switch_to: Option<String> = None;
    let mut add_language = false;

    super::card(ui, |ui| {
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(
                    Select::strings("term-editor-language", &mut chosen, languages.clone())
                        .label("Language")
                        .width(140.0),
                )
                .changed()
                && chosen != app.term_editor.language
            {
                switch_to = Some(chosen.clone());
            }

            ui.add_space(12.0);
            ui.vertical(|ui| {
                ui.add_space(16.0);
                ui.horizontal_wrapped(|ui| {
                    match &app.term_editor.loaded_version {
                        Some(version) => {
                            ui.add(Badge::new(format!("version {version}"), BadgeTone::Info))
                        }
                        None => ui.add(Badge::new("not stored yet", BadgeTone::Warning)),
                    };
                    if dirty {
                        ui.add(Badge::new("unsaved changes", BadgeTone::Warning));
                    }
                    if TermTemplate::builtin_for(&id, &app.term_editor.language).is_some() {
                        ui.add(Badge::new("built-in language", BadgeTone::Neutral));
                    }
                });
            });
        });

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            super::capped_input(
                ui,
                &mut app.term_editor.new_language,
                crate::domain::MAX_TEXT,
                |input| {
                    input
                        .label("Add a language")
                        .hint("BCP 47 tag, e.g. es or fr-FR")
                        .id_salt("term-editor-new-language")
                        .desired_width(200.0)
                },
            );
            ui.vertical(|ui| {
                ui.add_space(16.0);
                if ui.add(Button::new("Start it").outline()).clicked() {
                    add_language = true;
                }
            });
        });
        super::hint(
            ui,
            "A holder signs in the language they read. A language starts from the built-in \
             wording when this build ships one, and blank otherwise.",
        );
    });

    // Deferred: mutations happen after the paint closure.
    if let Some(language) = switch_to {
        if dirty {
            app.term_editor.error = Some(format!(
                "there are unsaved changes to `{}` — save them, or discard them with “Reload \
                 stored version”, before switching to `{language}`",
                app.term_editor.language
            ));
        } else {
            app.load_term_template(&language);
        }
    }
    if add_language {
        app.start_term_language();
    }
}

/// Title, body, and what can be done with them.
fn editor(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let heading = format!("{} — {}", app.term_editor.id, app.term_editor.language);
    let dirty = app.term_editor.is_dirty(&app.term_templates);
    let has_builtin =
        TermTemplate::builtin_for(&app.term_editor.id, &app.term_editor.language).is_some();

    let mut preview = false;
    let mut save = false;
    let mut restore = false;
    let mut reload = false;

    super::titled_card(ui, heading, |ui| {
        super::capped_input(
            ui,
            &mut app.term_editor.title,
            crate::domain::MAX_TEXT,
            |input| {
                input
                    .label("Title")
                    .hint("the heading printed at the top of the document")
                    .id_salt("term-editor-title")
                    .dirty(dirty)
            },
        );

        ui.add_space(10.0);
        // No `desired_width`: the editor takes the card, which now takes the
        // window. An infinite width would have made the card infinite with it.
        super::capped_area(ui, &mut app.term_editor.body, MAX_BODY, |area| {
            area.label("Body")
                .id_salt("term-editor-body")
                .rows(24)
                .monospace(true)
                .dirty(dirty)
        });
        super::hint(
            ui,
            "Use {{variables}} from the list below. A line whose variable is empty is dropped \
             from the document, so optional details need no conditional — and a line with no \
             variable is always printed. Two or more spaces make a column: whatever follows \
             them stays at that position however long the value before it turns out to be, \
             which is what keeps a signature block aligned.",
        );

        ui.add_space(10.0);

        // Live verdict on the draft: an unknown variable is refused here, at the
        // desk, and not at the counter with the holder waiting.
        let draft = app.term_editor.draft();
        match draft.check() {
            Ok(()) => {
                ui.horizontal_wrapped(|ui| {
                    ui.add(Badge::new("renders", BadgeTone::Ok));
                    ui.add_space(6.0);
                    super::faint(
                        ui,
                        &format!("uses: {}", {
                            let used = draft.referenced_variables();
                            if used.is_empty() {
                                "no variables — this term would say the same thing to everybody"
                                    .to_owned()
                            } else {
                                used.join(", ")
                            }
                        }),
                    );
                });
            }
            Err(e) => super::error_label(ui, &e.to_string()),
        }

        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Preview").outline()).clicked() {
                preview = true;
            }
            if ui
                .add(Button::new("Save as new version").accent(Accent::Green))
                .clicked()
            {
                save = true;
            }
            if ui
                .add(Button::new("Reload stored version").outline())
                .clicked()
            {
                reload = true;
            }
            if has_builtin
                && ui
                    .add(Button::new("Restore built-in wording").outline())
                    .clicked()
            {
                restore = true;
            }
        });

        if let Some(notice) = app.term_editor.notice.clone() {
            ui.add_space(10.0);
            super::notice(ui, CalloutTone::Success, &notice);
        }
        if let Some(error) = app.term_editor.error.clone() {
            ui.add_space(10.0);
            super::error_label(ui, &error);
        }
    });

    if preview {
        app.preview_term_template();
    }
    if save {
        app.save_term_template();
    }
    if reload {
        let language = app.term_editor.language.clone();
        app.load_term_template(&language);
    }
    if restore {
        app.restore_builtin_term_template();
    }
}

/// Every variable a term may use, with the value the preview substitutes.
fn variables(ui: &mut egui::Ui) {
    super::titled_card(ui, "Variables", |ui| {
        super::hint(
            ui,
            "Written {{name}}. The right-hand column is what the preview substitutes — the \
                 real term takes these from the holder, the key and the hand-over record.",
        );
        ui.add_space(8.0);
        super::table(ui, "term-variables", &["Variable", "Sample value"], |ui| {
            for (name, value) in TermContext::sample().as_map() {
                super::mono(ui, &format!("{{{{{name}}}}}"));
                super::faint(ui, &value);
                ui.end_row();
            }
        });
    });
}

/// The draft rendered against the sample context.
fn preview(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(text) = app.term_editor.preview.clone() else {
        return;
    };
    let mut close = false;
    let mut export = false;

    super::titled_card(ui, "Preview — sample data, nothing recorded", |ui| {
        // The rendered term keeps its own line breaks: it scrolls both ways
        // inside the card rather than being wrapped into something the holder
        // will not read.
        egui::ScrollArea::both()
            .max_height(360.0)
            .auto_shrink([false, true])
            .id_salt("term-editor-preview")
            .show(ui, |ui| {
                ui.add(
                    egui::Label::new(egui::RichText::new(&text).monospace().small())
                        .selectable(true),
                );
            });
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Export as PDF…").accent(Accent::Blue))
                .on_hover_text(
                    "the document as it will be printed — for the review the wording needs",
                )
                .clicked()
            {
                export = true;
            }
            if ui.add(Button::new("Close preview").outline()).clicked() {
                close = true;
            }
            ui.add_space(6.0);
            super::faint(
                ui,
                "The holder's name and identification number here are fictitious.",
            );
        });
    });

    if export {
        app.save_term_preview_pdf();
    }
    if close {
        app.term_editor.preview = None;
    }
}

/// Versions of this language on record. The newest is the one a term uses; the
/// others are kept because something may have been signed against them.
fn versions(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let id = app.term_editor.id.clone();
    let language = app.term_editor.language.clone();
    let mut stored: Vec<&TermTemplate> = app
        .term_templates
        .iter()
        .filter(|t| t.id == id && t.language.eq_ignore_ascii_case(&language))
        .collect();
    if stored.is_empty() {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "Nothing stored in this language yet — saving writes version 1.",
        );
        return;
    }

    let in_use = term::latest_in_language(&app.term_templates, &id, &language)
        .map(|t| t.version.clone())
        .unwrap_or_default();
    stored.sort_by_key(|t| t.version.clone());

    super::titled_card(ui, format!("{language} — versions on record"), |ui| {
        super::table(ui, "term-versions", &["Version", "Title", "State"], |ui| {
            for template in &stored {
                super::mono(ui, &template.version);
                ui.label(&template.title);
                if template.version == in_use {
                    ui.add(Badge::new("used for new terms", BadgeTone::Ok));
                } else {
                    ui.add(Badge::new("kept for terms signed", BadgeTone::Neutral));
                }
                ui.end_row();
            }
        });
    });
}
