//! Bootstrap wizard: pick a key, a holder and a template, review the plan,
//! then run it.
//!
//! Everything on this screen is currently **dry run only** — the plan is shown
//! and can be recorded as evidence of intent, but no step touches the key. The
//! executor is Wave 2 in `roadmap.md`.

use elegance::{Accent, Badge, BadgeTone, Button, CalloutTone, Card, Checkbox, Select};

use crate::app::YkDistApp;
use crate::template::Transport;

/// Width of the wider selects on this screen.
const FIELD: f32 = 360.0;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Bootstrap",
        "Templated procedure: FIDO2 PIN, OTP access code, on-key FIDO2 credential, \
         and a PIV signing certificate carrying the holder's e-mail.",
    );

    Card::new().heading("Selection").show(ui, |ui| {
        selection(app, ui);
        ui.add_space(14.0);
        steps(app, ui);

        ui.add_space(16.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Build plan")).clicked() {
                app.build_plan();
            }
            if ui
                .add(
                    Button::new("Record dry run")
                        .accent(Accent::Green)
                        .enabled(!app.wizard.plan.is_empty()),
                )
                .on_hover_text("save the plan as evidence of intent; no step touches the key")
                .clicked()
            {
                app.record_dry_run();
            }
            ui.add(Button::new("Execute on key").outline().enabled(false))
                .on_hover_text("the executor lands in Wave 2 — see roadmap.md");
        });

        if let Some(error) = app.wizard.error.clone() {
            ui.add_space(10.0);
            super::error_label(ui, &error);
        }
    });

    ui.add_space(18.0);
    plan_table(app, ui);
}

fn selection(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let holder_labels: Vec<String> = app.holders.iter().map(|h| h.display()).collect();
    let template_labels: Vec<String> = app
        .templates
        .iter()
        .map(|t| format!("{} (v{})", t.name, t.version))
        .collect();

    ui.horizontal_top(|ui| {
        ui.vertical(|ui| {
            ui.set_width(200.0);
            super::capped_input(ui, &mut app.wizard.serial, 12, |input| {
                input
                    .label("Key serial")
                    .id_salt("wizard-serial")
                    .desired_width(140.0)
            });
            ui.add_space(6.0);
            if ui
                .add(
                    Button::new("read attached key")
                        .outline()
                        .size(elegance::ButtonSize::Small),
                )
                .clicked()
            {
                app.detect_keys();
            }
        });

        ui.add_space(20.0);

        ui.vertical(|ui| {
            ui.set_width(FIELD + 16.0);

            if holder_labels.is_empty() {
                super::notice(ui, CalloutTone::Warning, "Register a holder first.");
            } else {
                app.wizard.holder_index = app.wizard.holder_index.min(holder_labels.len() - 1);
                ui.add(
                    Select::new("wizard-holder", &mut app.wizard.holder_index)
                        .label("Holder")
                        .options(holder_labels.iter().cloned().enumerate())
                        .width(FIELD),
                );
            }

            ui.add_space(8.0);

            if template_labels.is_empty() {
                super::notice(ui, CalloutTone::Warning, "No template available.");
            } else {
                app.wizard.template_index =
                    app.wizard.template_index.min(template_labels.len() - 1);
                let before = app.wizard.template_index;
                ui.add(
                    Select::new("wizard-template", &mut app.wizard.template_index)
                        .label("Template")
                        .options(template_labels.iter().cloned().enumerate())
                        .width(FIELD),
                );
                if before != app.wizard.template_index {
                    app.wizard.step_enabled.clear();
                    app.wizard.plan.clear();
                }
            }
        });
    });

    if let Some(template) = app.selected_template() {
        ui.add_space(8.0);
        let description = template.description.clone();
        super::hint(ui, &description);
    }
}

fn steps(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(template) = app.selected_template().cloned() else {
        return;
    };
    if app.wizard.step_enabled.len() != template.steps.len() {
        app.wizard.step_enabled = template.steps.iter().map(|s| s.enabled).collect();
    }

    let theme = elegance::Theme::current(ui.ctx());
    ui.add(egui::Label::new(
        egui::RichText::new("Steps to apply")
            .size(theme.typography.label)
            .color(theme.palette.text_muted)
            .strong(),
    ));
    ui.add_space(6.0);

    for (index, step) in template.steps.iter().enumerate() {
        ui.horizontal(|ui| {
            // A required step cannot be opted out of; showing it disabled is
            // more honest than hiding the choice.
            ui.add_enabled(
                !step.required,
                Checkbox::new(&mut app.wizard.step_enabled[index], step.kind.label()),
            );
            if step.required {
                ui.add(Badge::new("required", BadgeTone::Neutral));
            }
        });
    }
}

fn plan_table(app: &mut YkDistApp, ui: &mut egui::Ui) {
    if app.wizard.plan.is_empty() {
        super::notice(
            ui,
            CalloutTone::Neutral,
            "No plan built yet. Nothing has been sent to the key.",
        );
        return;
    }

    Card::new()
        .heading(format!("Planned steps ({})", app.wizard.plan.len()))
        .show(ui, |ui| {
            super::notice(
                ui,
                CalloutTone::Info,
                "Secrets appear as placeholders — a PIN is never rendered, logged or stored.",
            );
            ui.add_space(10.0);

            egui::Grid::new("plan")
                .striped(true)
                .num_columns(4)
                .spacing([14.0, 8.0])
                .show(ui, |ui| {
                    super::table_header(ui, &["Step", "Transport", "Operation", "Note"]);

                    for command in &app.wizard.plan {
                        ui.label(command.kind.label());
                        // The transport is the thing to notice: `ykman` is a
                        // labelled fallback, `manual` means a human does it.
                        let (text, tone) = match command.transport() {
                            Transport::Native => ("native", BadgeTone::Ok),
                            Transport::Ykman => ("ykman", BadgeTone::Warning),
                            Transport::Manual => ("manual", BadgeTone::Info),
                        };
                        ui.add(Badge::new(text, tone));
                        super::mono(ui, &command.transport_detail());
                        super::faint(ui, &command.note.clone().unwrap_or_default());
                        ui.end_row();
                    }
                });
        });
}
