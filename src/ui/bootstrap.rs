//! Bootstrap wizard: pick a key, a holder and a template, review the plan,
//! then run it.
//!
//! Everything on this screen is currently **dry run only** — the plan is shown
//! and can be recorded as evidence of intent, but no step touches the key. The
//! executor is Wave 2 in `roadmap.md`.

use elegance::{Accent, Badge, BadgeTone, Button, CalloutTone, Checkbox, Select};

use crate::app::{WizardStage, YkDistApp};
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
            // The gate. Enabled only when a plan exists *and* this build can
            // reach a key — and even then it opens the confirmation rather than
            // writing. `features/gui-bootstrap-wizard.md` phase 4: no
            // confirmation, no writes.
            let can_write = YkDistApp::can_write_to_a_key();
            let response = ui
                .add(
                    Button::new("Execute on key…")
                        .accent(Accent::Red)
                        .enabled(!app.wizard.plan.is_empty() && can_write),
                )
                .on_hover_text(if can_write {
                    "review what will be written, then confirm"
                } else {
                    "this build has no transport that can write to a key — rebuild with \
                     `--features native-device`"
                });
            if response.clicked() {
                app.preflight();
            }
        });

        if let Some(error) = app.wizard.error.clone() {
            ui.add_space(10.0);
            super::error_label(ui, &error);
        }
    });

    ui.add_space(18.0);

    match app.wizard.stage {
        WizardStage::Selecting => plan_table(app, ui),
        WizardStage::Confirming => confirmation(app, ui),
        WizardStage::Running => run_view(app, ui),
    }
}

/// What will be written, and the one confirmation that authorises it.
///
/// Deliberately **one** confirmation for the whole run rather than one per step:
/// a per-step prompt trains an operator to click through, and the thing they
/// need to read is the list of what cannot be undone.
fn confirmation(app: &mut YkDistApp, ui: &mut egui::Ui) {
    use crate::bootstrap::{self, preflight};

    let serial = app.wizard.serial.trim().to_owned();
    let holder = app
        .holders
        .get(app.wizard.holder_index)
        .map(|h| h.display())
        .unwrap_or_else(|| "(no holder selected)".into());
    let steps = app.wizard.plan.len();
    let blocked = preflight::blocks(&app.wizard.findings);

    // The signature verdict for the procedure about to be applied, and whether it
    // is allowed. Computed here so the confirmation can *refuse* rather than
    // letting the operator click and be told afterwards — and so the sentence
    // beside the button is the same sentence the run would have logged.
    let template = app.selected_template().cloned();
    let permission = template
        .as_ref()
        .map(|template| (template.clone(), app.template_run_permission(template)));

    super::titled_card(ui, "Confirm — nothing has been written yet", |ui| {
        ui.label(format!("Key serial: {serial}"));
        ui.label(format!("Holder: {holder}"));
        ui.label(format!("Steps: {steps}"));

        if let Some((template, permission)) = &permission {
            ui.label(format!(
                "Procedure: {} version {}",
                template.id, template.version
            ));
            ui.add_space(10.0);
            match permission {
                // A verified signature is worth saying out loud: it is the one
                // state in which somebody other than the person at this keyboard
                // has approved what is about to be written.
                Ok(trust) if trust.is_verified() => {
                    super::notice(ui, CalloutTone::Success, &trust.describe())
                }
                Ok(trust) => super::notice(
                    ui,
                    CalloutTone::Warning,
                    &format!(
                        "{} — this run is allowed because unsigned templates are permitted on \
                         this workstation, and it will be recorded as such.",
                        trust.describe()
                    ),
                ),
                Err(refusal) => super::error_label(ui, refusal),
            }
        }
        ui.add_space(10.0);

        // Rule 8 of the engine: no rollback pretence. Name what cannot be undone
        // rather than offering an undo that would silently fail.
        let irreversible = bootstrap::irreversible_steps(&app.wizard.plan);
        if !irreversible.is_empty() {
            super::notice(
                ui,
                CalloutTone::Warning,
                "These steps cannot be undone. A key that has had them applied can only be \
                 returned to its previous state by resetting the applet, which destroys what is \
                 on it:",
            );
            for command in &irreversible {
                ui.label(format!("  • {} — {}", command.step_id, command.description));
            }
            ui.add_space(10.0);
        }

        if app.wizard.findings.is_empty() {
            super::notice(
                ui,
                CalloutTone::Neutral,
                "Pre-flight found nothing to flag.",
            );
        } else {
            ui.label(preflight::summarise(&app.wizard.findings));
            ui.add_space(6.0);
            for finding in &app.wizard.findings {
                // Severity as a word, not only a colour — phase 10.
                let where_ = if finding.step_id.is_empty() {
                    "run".to_owned()
                } else {
                    finding.step_id.clone()
                };
                ui.label(format!(
                    "  [{}] {where_}: {}",
                    finding.severity.label(),
                    finding.message
                ));
            }
        }

        ui.add_space(16.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Cancel")).clicked() {
                app.cancel_confirmation();
            }
            // Two independent gates on the same button: the pre-flight's findings,
            // and whether this deployment accepts this procedure's signature. The
            // app re-checks the signature before the first write regardless — a
            // disabled button is a courtesy, not a control.
            let unsigned_refusal = permission
                .as_ref()
                .and_then(|(_, permission)| permission.as_ref().err().cloned());
            let confirm = ui
                .add(
                    Button::new(format!("Write {steps} step(s) to serial {serial}"))
                        .accent(Accent::Red)
                        .enabled(!blocked && unsigned_refusal.is_none()),
                )
                .on_hover_text(match (&unsigned_refusal, blocked) {
                    (Some(_), _) => "this deployment requires a signed template",
                    (None, true) => "the pre-flight found something that blocks this run",
                    (None, false) => "this writes to the key",
                });
            if confirm.clicked() {
                // The only place a `Confirmation` is constructed. It carries the
                // serial and step count that were on screen, and the executor
                // re-checks both.
                if let Ok(parsed) = serial.parse::<u32>() {
                    app.execute_run(bootstrap::Confirmation::given(parsed, steps));
                }
            }
        });
    });

    ui.add_space(18.0);
    plan_table(app, ui);
}

/// The run as it happened, plus the secrets it produced.
fn run_view(app: &mut YkDistApp, ui: &mut egui::Ui) {
    use crate::domain::StepStatus;

    if let Some(run) = app.wizard.run.clone() {
        let (done, failed, skipped, pending) = run.tally();
        super::titled_card(
            ui,
            format!(
                "Run {:?} — {done} done, {failed} failed, {skipped} skipped, {pending} pending",
                run.status
            ),
            |ui| {
                super::table(ui, "run-steps", &["Step", "Status", "Detail"], |ui| {
                    for step in &run.steps {
                        super::mono(ui, &step.step_id);
                        // Status as a word: a screen-reader user and a
                        // monochrome screen both need this without colour.
                        ui.label(match step.status {
                            StepStatus::Done => "done",
                            StepStatus::Failed => "FAILED",
                            StepStatus::Skipped => "skipped",
                            StepStatus::Running => "running…",
                            StepStatus::Pending => "not reached",
                        });
                        ui.add(
                            egui::Label::new(egui::RichText::new(&step.detail)).selectable(true),
                        );
                        ui.end_row();
                    }
                });

                ui.add_space(12.0);
                ui.label(format!("Custody: {}", run.custody));
                if failed > 0 || pending > 0 {
                    ui.add_space(6.0);
                    super::notice(
                        ui,
                        CalloutTone::Warning,
                        "This key is not ready to hand over. The record above says what was and \
                         was not applied; it is the evidence, so do not re-run blindly.",
                    );
                }
            },
        );
    }

    // The show-once panel. Model B hands a transport secret over, and this is the
    // one moment a value is readable by a person.
    let has_secrets = app
        .wizard
        .secrets
        .as_ref()
        .is_some_and(|s| !s.entries().is_empty());
    if has_secrets {
        ui.add_space(14.0);
        super::titled_card(ui, "Write these down now — shown once", |ui| {
            super::notice(
                ui,
                CalloutTone::Warning,
                "These are transport secrets. Nothing keeps a copy: once this panel is \
                 dismissed they are gone, and a key whose PIN is lost has to be reset.",
            );
            ui.add_space(8.0);
            if let Some(panel) = &app.wizard.secrets {
                for secret in panel.for_the_holder() {
                    ui.horizontal(|ui| {
                        ui.label(format!("{}:", secret.kind().label()));
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(secret.expose()).monospace().size(18.0),
                            )
                            .selectable(true),
                        );
                    });
                }
            }
            ui.add_space(12.0);
            if ui
                .add(Button::new("I have written them down — dismiss"))
                .clicked()
            {
                app.dismiss_secrets();
            }
        });
    }

    ui.add_space(14.0);
    if ui.add(Button::new("Start another")).clicked() {
        app.reset_wizard();
    }

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

    // The wizard runs a template; the Templates screen decides what one is. The
    // jump carries the selected template with it, the way the Distribution screen
    // opens the Terms editor on the language it just generated.
    let selected = app.selected_template().map(|t| t.id.clone());
    let mut manage = false;
    ui.add_space(8.0);
    ui.horizontal_wrapped(|ui| {
        if ui
            .add(
                Button::new("Manage templates…")
                    .outline()
                    .size(elegance::ButtonSize::Small),
            )
            .on_hover_text(
                "add, change or withdraw a template — an edit is stored as a new version and \
                 applies to the next run",
            )
            .clicked()
        {
            manage = true;
        }
        ui.add_space(6.0);
        super::faint(
            ui,
            "The list offers the newest version of each template in use.",
        );
    });
    if manage {
        if let Some(id) = selected {
            app.load_template(&id, None);
        }
        app.tab = crate::app::Tab::Templates;
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
