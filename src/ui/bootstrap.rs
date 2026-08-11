//! Bootstrap wizard: pick a key, a holder and a template, review the plan,
//! then run it.
//!
//! Everything on this screen is currently **dry run only** — the plan is shown
//! and can be recorded as evidence of intent, but no step touches the key. The
//! executor is Wave 2 in `roadmap.md`.

use elegance::{Accent, Badge, BadgeTone, Button, CalloutTone, Checkbox, Select};

use crate::app::YkDistApp;
use crate::template::Transport;

/// Widest a serial field needs to be. A serial is eight digits; the column it
/// sits in is free to be wider, the field is not.
const SERIAL_FIELD: f32 = 180.0;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Bootstrap",
        "Templated procedure: FIDO2 PIN, OTP access code, on-key FIDO2 credential, \
         and a PIV signing certificate carrying the holder's e-mail.",
    );

    super::titled_card(ui, "Selection", |ui| {
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

    let mut detect = false;
    super::form_columns(ui, |left, right, width| {
        super::capped_input(left, &mut app.wizard.serial, 12, |input| {
            input
                .label("Key serial")
                .id_salt("wizard-serial")
                .desired_width(SERIAL_FIELD.min(width))
        });
        left.add_space(6.0);
        if left
            .add(
                Button::new("read attached key")
                    .outline()
                    .size(elegance::ButtonSize::Small),
            )
            .clicked()
        {
            detect = true;
        }

        if holder_labels.is_empty() {
            super::notice(right, CalloutTone::Warning, "Register a holder first.");
        } else {
            app.wizard.holder_index = app.wizard.holder_index.min(holder_labels.len() - 1);
            right.add(
                Select::new("wizard-holder", &mut app.wizard.holder_index)
                    .label("Holder")
                    .options(holder_labels.iter().cloned().enumerate())
                    .width(width),
            );
        }

        right.add_space(8.0);

        if template_labels.is_empty() {
            super::notice(right, CalloutTone::Warning, "No template available.");
        } else {
            app.wizard.template_index = app.wizard.template_index.min(template_labels.len() - 1);
            let before = app.wizard.template_index;
            right.add(
                Select::new("wizard-template", &mut app.wizard.template_index)
                    .label("Template")
                    .options(template_labels.iter().cloned().enumerate())
                    .width(width),
            );
            if before != app.wizard.template_index {
                app.wizard.step_enabled.clear();
                app.wizard.plan.clear();
            }
        }
    });
    // Deferred, as everywhere else: reading the key happens outside the layout
    // closure that is holding the form's fields.
    if detect {
        app.detect_keys();
    }

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

    super::titled_card(
        ui,
        format!("Planned steps ({})", app.wizard.plan.len()),
        |ui| {
            super::notice(
                ui,
                CalloutTone::Info,
                "Secrets appear as placeholders — a PIN is never rendered, logged or stored.",
            );
            ui.add_space(10.0);

            super::table(
                ui,
                "plan",
                &["Step", "Transport", "Operation", "Note"],
                |ui| {
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
                },
            );
        },
    );
}
