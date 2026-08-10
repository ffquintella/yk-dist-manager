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

use elegance::{Accent, Badge, BadgeTone, Button, CalloutTone, Card, Select};

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

    Card::new().show(ui, |ui| {
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

    Card::new().heading(heading).show(ui, |ui| {
        super::capped_input(
            ui,
            &mut app.term_editor.title,
            crate::domain::MAX_TEXT,
            |input| {
                input
                    .label("Title")
                    .hint("the heading printed at the top of the document")
                    .id_salt("term-editor-title")
                    .desired_width(520.0)
                    .dirty(dirty)
            },
        );

        ui.add_space(10.0);
        super::capped_area(ui, &mut app.term_editor.body, MAX_BODY, |area| {
            area.label("Body")
                .id_salt("term-editor-body")
                .rows(24)
                .monospace(true)
                .desired_width(f32::INFINITY)
                .dirty(dirty)
        });
        super::hint(
            ui,
            "Use {{variables}} from the list below. A line whose variable is empty is dropped \
             from the document, so optional details need no conditional — and a line with no \
             variable is always printed.",
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
    Card::new().heading("Variables").show(ui, |ui| {
        super::hint(
            ui,
            "Written {{name}}. The right-hand column is what the preview substitutes — the \
                 real term takes these from the holder, the key and the hand-over record.",
        );
        ui.add_space(8.0);
        egui::Grid::new("term-variables")
            .striped(true)
            .num_columns(2)
            .spacing([18.0, 6.0])
            .show(ui, |ui| {
                super::table_header(ui, &["Variable", "Sample value"]);
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

    Card::new()
        .heading("Preview — sample data, nothing recorded")
        .show(ui, |ui| {
            egui::ScrollArea::vertical()
                .max_height(360.0)
                .id_salt("term-editor-preview")
                .show(ui, |ui| {
                    ui.add(
                        egui::Label::new(egui::RichText::new(&text).monospace().small())
                            .selectable(true),
                    );
                });
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
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

    Card::new()
        .heading(format!("{} — versions on record", language))
        .show(ui, |ui| {
            egui::Grid::new("term-versions")
                .striped(true)
                .num_columns(3)
                .spacing([18.0, 6.0])
                .show(ui, |ui| {
                    super::table_header(ui, &["Version", "Title", "State"]);
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
