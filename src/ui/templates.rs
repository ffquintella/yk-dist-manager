//! Templates: the bootstrap procedure itself, as data an operator can change.
//!
//! The wizard runs a template; this screen decides what a template *is*. It
//! exists as its own screen for the same reason the Terms screen does — the
//! procedure is institutional content that outlives any single hand-over, and
//! editing it is a different job from running it.
//!
//! Three rules the screen makes visible rather than assuming:
//!
//! * **Saving stores a new version.** The version on record is left alone,
//!   because a bootstrap run may have recorded that it applied it. New runs get
//!   the newest version.
//! * **A draft is planned before it is stored.** The verdict next to the buttons
//!   is a real plan against sample data, so an unknown `{{variable}}` or a step
//!   missing a parameter is refused here rather than in front of a key.
//! * **Nothing that ran is deleted.** A version a run refers to can be *retired*
//!   — withdrawn from the wizard, kept in the database. Only a version nothing
//!   refers to can be removed outright, and removal asks first.

use elegance::{Accent, Badge, BadgeTone, Button, CalloutTone, Checkbox, Select};

use crate::app::YkDistApp;
use crate::domain::StepKind;
use crate::template::{MAX_STEPS, RenderContext};

/// What a click in the catalogue asked for. Applied after the table has been
/// painted — a row action cannot mutate the catalogue the table is iterating.
enum Action {
    Edit(String, String),
    Duplicate(String, String),
    Retire(String, String),
    Reinstate(String, String),
    Remove(String, String),
}

/// One catalogue row, snapshotted so the table can be painted while the actions
/// it produces are still pending.
struct Row {
    id: String,
    name: String,
    version: String,
    steps: usize,
    enabled: usize,
    retired: bool,
    builtin: bool,
    offered: bool,
    runs: usize,
    updated: String,
    refusal: Option<String>,
}

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Templates",
        "The bootstrap procedure as data: add a template, change its steps, or withdraw one. \
         Saving stores a new version — the procedure a run recorded stays readable.",
    );

    if app.store.is_none() {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "No database open — open one to manage the bootstrap templates.",
        );
        return;
    }

    // First paint after the screen opens: fill the editor from the cached
    // catalogue. No database read, so this is safe inside the paint pass.
    if !app.template_editor.loaded {
        match app.templates.first().map(|t| t.id.clone()) {
            Some(id) => app.load_template(&id, None),
            None => app.start_template(),
        }
        app.template_editor.loaded = true;
    }

    catalogue(app, ui);
    ui.add_space(14.0);
    if app.template_editor.pending_removal.is_some() {
        removal(app, ui);
        ui.add_space(14.0);
    }
    editor(app, ui);
    ui.add_space(14.0);
    variables(ui);
}

/// Every template version on record, what state it is in, and what can be done
/// with it.
fn catalogue(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let rows: Vec<Row> = app
        .template_catalogue
        .iter()
        .map(|stored| Row {
            id: stored.template.id.clone(),
            name: stored.template.name.clone(),
            version: stored.template.version.clone(),
            steps: stored.template.steps.len(),
            enabled: stored.template.enabled_steps().count(),
            retired: stored.is_retired(),
            builtin: stored.template.is_builtin(),
            offered: app
                .templates
                .iter()
                .any(|t| t.id == stored.template.id && t.version == stored.template.version),
            runs: stored.runs,
            updated: stored.updated_at.chars().take(10).collect(),
            refusal: stored.removal_refusal(),
        })
        .collect();

    if rows.is_empty() {
        super::notice(
            ui,
            CalloutTone::Warning,
            "No template on record. Build one below — until then the wizard offers the ones this \
             build ships.",
        );
        return;
    }

    let mut action: Option<Action> = None;
    let mut new_template = false;

    super::titled_card(ui, format!("On record ({})", rows.len()), |ui| {
        super::table(
            ui,
            "template-catalogue",
            &[
                "Template", "Version", "Steps", "Runs", "State", "Updated", "",
            ],
            |ui| {
                for row in &rows {
                    ui.vertical(|ui| {
                        name_and_id(ui, &row.name, &row.id);
                    });
                    super::mono(ui, &row.version);
                    super::faint(ui, &format!("{} of {}", row.enabled, row.steps));
                    super::faint(ui, &row.runs.to_string());
                    // Not `horizontal_wrapped`: a grid cell that is not the last
                    // column is offered the width the column had *last* frame, so
                    // wrapping here locks the column at one badge per line for
                    // good — and the row it doubles in height drags every other
                    // cell a line below the name, because a cell is centred in
                    // its row. Extending is what the table's own horizontal
                    // scroll is for, as with `name_and_id`.
                    ui.horizontal(|ui| {
                        if row.retired {
                            ui.add(Badge::new("retired", BadgeTone::Warning));
                        } else if row.offered {
                            ui.add(Badge::new("offered", BadgeTone::Ok));
                        } else {
                            ui.add(Badge::new("superseded", BadgeTone::Neutral));
                        }
                        if row.builtin {
                            ui.add(Badge::new("built-in", BadgeTone::Neutral));
                        }
                    });
                    super::faint(ui, &row.updated);
                    // One line, like every other row-action cell in the app: a
                    // wrapped second line of buttons would misalign this row
                    // against the ones above it.
                    ui.horizontal(|ui| {
                        if super::row_button(ui, "Edit")
                            .on_hover_text("open this version in the editor below")
                            .clicked()
                        {
                            action = Some(Action::Edit(row.id.clone(), row.version.clone()));
                        }
                        if super::row_button(ui, "Duplicate")
                            .on_hover_text("start a new template from these steps")
                            .clicked()
                        {
                            action = Some(Action::Duplicate(row.id.clone(), row.version.clone()));
                        }
                        if row.retired {
                            if super::row_button(ui, "Reinstate")
                                .on_hover_text("offer this version in the wizard again")
                                .clicked()
                            {
                                action =
                                    Some(Action::Reinstate(row.id.clone(), row.version.clone()));
                            }
                        } else if super::row_button(ui, "Retire")
                            .on_hover_text(
                                "withdraw it from the wizard and keep it on record — what a run \
                                 applied stays readable",
                            )
                            .clicked()
                        {
                            action = Some(Action::Retire(row.id.clone(), row.version.clone()));
                        }
                        match &row.refusal {
                            // Removal is not offered where it cannot be granted;
                            // the reason is on the disabled button rather than in
                            // a refusal after the click.
                            Some(reason) => {
                                ui.add(
                                    Button::new("Remove")
                                        .outline()
                                        .size(elegance::ButtonSize::Small)
                                        .enabled(false),
                                )
                                .on_disabled_hover_text(reason.clone());
                            }
                            None => {
                                if super::row_button_danger(ui, "Remove")
                                    .on_hover_text("delete this version — it asks first")
                                    .clicked()
                                {
                                    action =
                                        Some(Action::Remove(row.id.clone(), row.version.clone()));
                                }
                            }
                        }
                    });
                    ui.end_row();
                }
            },
        );

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("New template").outline())
                .on_hover_text("start from nothing; “Duplicate” starts from an existing procedure")
                .clicked()
            {
                new_template = true;
            }
            ui.add_space(6.0);
            super::faint(
                ui,
                "The wizard offers the newest version of each template that is not retired.",
            );
        });
    });

    // Deferred: every mutation happens after the table closure has finished.
    match action {
        Some(Action::Edit(id, version)) => app.load_template(&id, Some(&version)),
        Some(Action::Duplicate(id, version)) => app.duplicate_template(&id, &version),
        Some(Action::Retire(id, version)) => app.retire_template(&id, &version),
        Some(Action::Reinstate(id, version)) => app.reinstate_template(&id, &version),
        Some(Action::Remove(id, version)) => app.request_template_removal(&id, &version),
        None => {}
    }
    if new_template {
        app.start_template();
    }
}

/// A template's name with its id underneath, in one table cell.
///
/// Both extend rather than wrap: a name is two or three words, and letting the
/// grid wrap it turns "Organisation standard bootstrap" into a four-line column while
/// row of buttons beside it stays one line. The table brings its own horizontal
/// scroll (`ui::table`), so extending costs nothing the layout has not already
/// accounted for.
fn name_and_id(ui: &mut egui::Ui, name: &str, id: &str) {
    let theme = elegance::Theme::current(ui.ctx());
    ui.add(
        egui::Label::new(
            egui::RichText::new(name)
                .size(theme.typography.body)
                .color(theme.palette.text),
        )
        .wrap_mode(egui::TextWrapMode::Extend),
    );
    ui.add(
        egui::Label::new(
            egui::RichText::new(id)
                .size(theme.typography.small)
                .color(theme.palette.text_faint),
        )
        .wrap_mode(egui::TextWrapMode::Extend),
    );
}

/// The confirmation for a removal: what will go, and what will not.
fn removal(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some((id, version)) = app.template_editor.pending_removal.clone() else {
        return;
    };
    let mut confirm = false;
    let mut cancel = false;

    super::titled_card(ui, format!("Remove {id} version {version}?"), |ui| {
        super::notice(
            ui,
            CalloutTone::Warning,
            "The steps of this version are deleted. No bootstrap run refers to it, so no record \
             loses its procedure — and the audit entry for the removal stays either way.",
        );
        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Remove it").accent(Accent::Red))
                .clicked()
            {
                confirm = true;
            }
            if ui.add(Button::new("Keep it").outline()).clicked() {
                cancel = true;
            }
        });
    });

    if confirm {
        app.remove_template(&id, &version);
    }
    if cancel {
        app.cancel_template_removal();
    }
}

/// The draft: what it is called, what it does, and its steps in order.
fn editor(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let dirty = app.template_editor_dirty();
    let stored_version = app.template_editor.draft.loaded_version.clone();
    let id = app.template_editor.draft.id.trim().to_owned();
    let has_builtin = crate::template::BootstrapTemplate::builtin_for(&id).is_some();
    let heading = if id.is_empty() {
        "New template".to_owned()
    } else {
        format!("Editing {id}")
    };

    let mut save = false;
    let mut reload = false;
    let mut restore = false;

    super::titled_card(ui, heading, |ui| {
        ui.horizontal_wrapped(|ui| {
            match &stored_version {
                Some(version) => ui.add(Badge::new(format!("version {version}"), BadgeTone::Info)),
                None => ui.add(Badge::new("not stored yet", BadgeTone::Warning)),
            };
            if dirty {
                ui.add(Badge::new("unsaved changes", BadgeTone::Warning));
            }
            if has_builtin {
                ui.add(Badge::new("built-in id", BadgeTone::Neutral));
            }
        });

        ui.add_space(10.0);
        let editable_id = stored_version.is_none();
        super::form_columns(ui, |left, right, _width| {
            // The id is the name a bootstrap run records, so it is settled when
            // the template is first stored and immutable afterwards: changing it
            // would orphan every run that referred to it. A new id is a new
            // template, reached with "Duplicate".
            left.add_enabled_ui(editable_id, |left| {
                super::capped_input(
                    left,
                    &mut app.template_editor.draft.id,
                    crate::template::MAX_ID,
                    |input| {
                        input
                            .label("Id")
                            .hint("lower-case, hyphens, e.g. fido-only")
                            .id_salt("template-editor-id")
                            .dirty(dirty)
                    },
                );
            });
            if !editable_id {
                super::hint(
                    left,
                    "The id is fixed once stored — bootstrap runs refer to it. Duplicate the \
                     template to start one under another id.",
                );
            }
            super::capped_input(
                right,
                &mut app.template_editor.draft.name,
                crate::domain::MAX_TEXT,
                |input| {
                    input
                        .label("Name")
                        .hint("what the wizard shows in the template list")
                        .id_salt("template-editor-name")
                        .dirty(dirty)
                },
            );
        });

        ui.add_space(10.0);
        super::capped_area(
            ui,
            &mut app.template_editor.draft.description,
            crate::domain::MAX_NOTE,
            |area| {
                area.label("Description")
                    .id_salt("template-editor-description")
                    .rows(3)
                    .dirty(dirty)
            },
        );
        super::hint(
            ui,
            "Read by the operator under the template list, before a key is touched. It may use \
             {{variables}} too.",
        );

        ui.add_space(14.0);
        steps(app, ui);

        ui.add_space(12.0);
        verdict(app, ui);

        ui.add_space(12.0);
        ui.horizontal_wrapped(|ui| {
            if ui
                .add(Button::new("Save as new version").accent(Accent::Green))
                .on_hover_text(
                    "stores a new version; the version already on record keeps explaining the \
                     runs that used it",
                )
                .clicked()
            {
                save = true;
            }
            if stored_version.is_some()
                && ui
                    .add(Button::new("Reload stored version").outline())
                    .clicked()
            {
                reload = true;
            }
            if has_builtin
                && ui
                    .add(Button::new("Restore built-in steps").outline())
                    .clicked()
            {
                restore = true;
            }
        });

        if let Some(notice) = app.template_editor.notice.clone() {
            ui.add_space(10.0);
            super::notice(ui, CalloutTone::Success, &notice);
        }
        if let Some(error) = app.template_editor.error.clone() {
            ui.add_space(10.0);
            super::error_label(ui, &error);
        }
    });

    if save {
        app.save_template();
    }
    if reload && let Some(version) = stored_version {
        app.load_template(&id, Some(&version));
    }
    if restore {
        app.restore_builtin_template();
    }
}

/// The steps, in the order they will run.
fn steps(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let theme = elegance::Theme::current(ui.ctx());
    ui.add(egui::Label::new(
        egui::RichText::new(format!(
            "Steps ({} of {MAX_STEPS} used, applied top to bottom)",
            app.template_editor.draft.steps.len()
        ))
        .size(theme.typography.label)
        .color(theme.palette.text_muted)
        .strong(),
    ));
    ui.add_space(6.0);

    if app.template_editor.draft.steps.is_empty() {
        super::notice(
            ui,
            CalloutTone::Warning,
            "No steps yet — a template with nothing to apply cannot be saved.",
        );
    }

    let open = app.template_editor.open_step;
    let count = app.template_editor.draft.steps.len();
    let mut move_step: Option<(usize, bool)> = None;
    let mut remove_step: Option<usize> = None;
    let mut toggle_open: Option<usize> = None;

    for index in 0..count {
        let expanded = open == Some(index);
        ui.horizontal_wrapped(|ui| {
            super::faint(ui, &format!("{}.", index + 1));
            {
                let step = &mut app.template_editor.draft.steps[index];
                ui.add(Checkbox::new(&mut step.enabled, step.kind.label()))
                    .on_hover_text("unticked: the wizard starts with this step deselected");
                ui.add(Checkbox::new(&mut step.required, "required"))
                    .on_hover_text(
                        "required: the operator cannot deselect it, and a failure aborts the run",
                    );
            }
            let step_id = app.template_editor.draft.steps[index].id.clone();
            super::mono(ui, &step_id);

            if super::row_button(ui, if expanded { "Hide" } else { "Details" }).clicked() {
                toggle_open = Some(index);
            }
            if ui
                .add_enabled(
                    index > 0,
                    Button::new("↑").outline().size(elegance::ButtonSize::Small),
                )
                .clicked()
            {
                move_step = Some((index, true));
            }
            if ui
                .add_enabled(
                    index + 1 < count,
                    Button::new("↓").outline().size(elegance::ButtonSize::Small),
                )
                .clicked()
            {
                move_step = Some((index, false));
            }
            if super::row_button_danger(ui, "Remove step").clicked() {
                remove_step = Some(index);
            }
        });

        if expanded {
            ui.add_space(4.0);
            let step = &mut app.template_editor.draft.steps[index];
            super::capped_input(ui, &mut step.id, crate::template::MAX_ID, |input| {
                input
                    .label("Step id")
                    .hint("recorded against each step outcome of a run")
                    .id_salt(format!("template-step-id-{index}"))
            });
            ui.add_space(6.0);
            super::capped_input(
                ui,
                &mut step.description,
                crate::domain::MAX_NOTE,
                |input| {
                    input
                        .label("Description")
                        .hint("what the operator reads for this step")
                        .id_salt(format!("template-step-description-{index}"))
                },
            );
            ui.add_space(6.0);
            super::capped_area(ui, &mut step.params_text, crate::domain::MAX_NOTE, |area| {
                area.label("Parameters")
                    .id_salt(format!("template-step-params-{index}"))
                    .rows(5)
                    .monospace(true)
            });
            super::hint(
                ui,
                "One `name = value` per line; values may use {{variables}}. The parameters a step \
                 needs come from its kind — “Add a step” fills in the ones it reads, and a \
                 missing one is refused before the template can be saved.",
            );
            ui.add_space(6.0);
        }
        ui.add_space(2.0);
    }

    ui.add_space(8.0);
    let kinds: Vec<String> = StepKind::ALL
        .iter()
        .map(|kind| kind.label().to_owned())
        .collect();
    let mut add = false;
    ui.horizontal_wrapped(|ui| {
        app.template_editor.new_kind = app.template_editor.new_kind.min(kinds.len() - 1);
        ui.add(
            Select::new("template-new-step", &mut app.template_editor.new_kind)
                .label("Add a step")
                .options(kinds.iter().cloned().enumerate())
                .width(260.0),
        );
        ui.vertical(|ui| {
            ui.add_space(16.0);
            if ui.add(Button::new("Add step").outline()).clicked() {
                add = true;
            }
        });
    });

    // Deferred, as everywhere else: the list is not edited while it is painted.
    if let Some((index, up)) = move_step {
        app.template_editor.draft.move_step(index, up);
        app.template_editor.open_step = None;
    }
    if let Some(index) = remove_step {
        app.template_editor.draft.remove_step(index);
        app.template_editor.open_step = None;
    }
    if let Some(index) = toggle_open {
        app.template_editor.open_step = if open == Some(index) {
            None
        } else {
            Some(index)
        };
    }
    if add {
        app.add_template_step();
    }
}

/// The live verdict on the draft: would it plan, and how.
///
/// This is a real [`crate::template::plan`] against sample data, not a syntax
/// check — the same gate the store applies, run on every keystroke so the refusal
/// arrives while the operator is still looking at the field that caused it.
fn verdict(app: &YkDistApp, ui: &mut egui::Ui) {
    match app.template_editor.draft.check() {
        Ok(()) => {
            let steps = app.template_editor.draft.steps.len();
            let enabled = app
                .template_editor
                .draft
                .steps
                .iter()
                .filter(|s| s.enabled)
                .count();
            ui.horizontal_wrapped(|ui| {
                ui.add(Badge::new("plans", BadgeTone::Ok));
                ui.add_space(6.0);
                super::faint(
                    ui,
                    &format!(
                        "{enabled} of {steps} step(s) selected by default; every step planned \
                         against sample data",
                    ),
                );
            });
        }
        Err(e) => super::error_label(ui, &e.to_string()),
    }
}

/// Every variable a template may use, with the value the check substitutes.
fn variables(ui: &mut egui::Ui) {
    let sample = RenderContext::sample();
    super::titled_card(ui, "Variables", |ui| {
        super::hint(
            ui,
            "Written {{name}} in a description or a parameter value. The right-hand column is \
             what the draft check substitutes — a real run takes these from the holder, the key \
             and the operator. An unknown name is refused, never left blank.",
        );
        ui.add_space(8.0);
        super::table(
            ui,
            "template-variables",
            &["Variable", "Sample value"],
            |ui| {
                for name in RenderContext::VARIABLES {
                    super::mono(ui, &format!("{{{{{name}}}}}"));
                    super::faint(
                        ui,
                        &crate::template::render(&format!("{{{{{name}}}}}"), &sample)
                            .unwrap_or_default(),
                    );
                    ui.end_row();
                }
            },
        );
    });
}
