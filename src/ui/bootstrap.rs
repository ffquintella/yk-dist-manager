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
        WizardStage::Selecting => {
            batch_panel(app, ui);
            unfinished_runs(app, ui);
            plan_table(app, ui);
        }
        WizardStage::Confirming => confirmation(app, ui),
        WizardStage::Running => {
            batch_panel(app, ui);
            run_view(app, ui);
        }
    }
}

/// Bulk enrolment (`features/bulk-enrollment.md`, `gui-bootstrap-wizard` phase 8).
///
/// On this screen rather than one of its own, and that is the design rather than
/// a saving: a batch **drives the wizard**. The plan, the pre-flight, the
/// confirmation and the run view are the same ones a single key gets, and a batch
/// mode with its own quieter run path would be a second way of writing to a key —
/// the second way always being the one that skips a check.
fn batch_panel(app: &mut YkDistApp, ui: &mut egui::Ui) {
    use crate::batch::Shape;

    let running = app.batch.current.is_some();
    super::titled_card(ui, "Batch", |ui| {
        if !running {
            super::form_columns(ui, |left, right, width| {
                left.add(
                    Select::new("batch-shape", &mut app.batch.shape)
                        .label("Batch")
                        .options(Shape::ALL.map(|shape| (shape, shape.label())))
                        .width(width),
                );
                super::hint(left, app.batch.shape.describe());

                if app.batch.shape == Shape::StockPreparation {
                    right.label("Keys in the box");
                    right.add(egui::DragValue::new(&mut app.batch.planned).range(1..=500));
                    super::hint(
                        right,
                        "A target, not a limit: one more key in the box than you counted is not \
                         an error worth stopping for.",
                    );
                } else {
                    right.label("Pairing list (CSV: serial, email — serial optional)");
                    super::capped_area(right, &mut app.batch.pairing_text, 64 * 1024, |area| {
                        area.rows(4).id_salt("batch-pairs")
                    });
                    if right
                        .add(elegance::Button::new("Check the list").outline())
                        .clicked()
                    {
                        app.load_pairing_list();
                    }
                }
            });

            ui.add_space(10.0);
            ui.horizontal_wrapped(|ui| {
                let ready =
                    app.batch.shape == Shape::StockPreparation || !app.batch.pairs.is_empty();
                if ui
                    .add(elegance::Button::new("Start batch").enabled(ready))
                    .on_hover_text("the procedure selected above is the one the whole box will get")
                    .clicked()
                {
                    app.start_batch();
                }
                if !app.batch.resumable.is_empty() {
                    ui.add_space(8.0);
                    super::faint(ui, &format!("{} unfinished", app.batch.resumable.len()));
                }
            });

            // Unfinished batches, so an interrupted box is picked up rather than
            // started again.
            let resumable: Vec<(uuid::Uuid, String)> = app
                .batch
                .resumable
                .iter()
                .map(|batch| {
                    (
                        batch.id,
                        format!(
                            "{} · {} v{} · started {} · {}",
                            batch.shape.label(),
                            batch.template_id,
                            batch.template_version,
                            batch.started_at.format("%Y-%m-%d %H:%M"),
                            batch.tally().describe(),
                        ),
                    )
                })
                .collect();
            for (id, label) in resumable {
                ui.add_space(6.0);
                ui.horizontal_wrapped(|ui| {
                    if super::row_button(ui, "Resume").clicked() {
                        app.resume_batch(id);
                    }
                    super::faint(ui, &label);
                });
            }
        } else {
            batch_progress(app, ui);
        }

        if let Some(notice) = app.batch.notice.clone() {
            ui.add_space(10.0);
            super::notice(ui, CalloutTone::Info, &notice);
        }
        if let Some(error) = app.batch.error.clone() {
            ui.add_space(10.0);
            super::error_label(ui, &error);
        }
    });
    ui.add_space(18.0);
}

fn batch_progress(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(batch) = app.batch.current.clone() else {
        return;
    };
    let tally = batch.tally();

    ui.horizontal_wrapped(|ui| {
        ui.add(Badge::new(batch.shape.label(), BadgeTone::Info));
        ui.add_space(8.0);
        super::faint(
            ui,
            &format!(
                "{} v{} · started {}",
                batch.template_id,
                batch.template_version,
                batch.started_at.format("%Y-%m-%d %H:%M")
            ),
        );
    });
    ui.add_space(6.0);
    ui.add(
        elegance::ProgressBar::new(if tally.total() == 0 {
            0.0
        } else {
            tally.settled() as f32 / tally.total() as f32
        })
        .text(tally.describe()),
    );

    ui.add_space(10.0);
    ui.horizontal_wrapped(|ui| {
        // The key in the reader is offered to the batch, which is where the
        // duplicate check lives. Nothing is written until the operator confirms
        // the run, exactly as for one key.
        let attached: Vec<u32> = app.attached.serials();
        for serial in attached {
            if ui
                .add(elegance::Button::new(format!("Use serial {serial}")))
                .clicked()
            {
                app.present_batch_key(serial);
            }
        }
        if app.batch.position.is_some() && super::row_button(ui, "Skip this key").clicked() {
            app.skip_batch_key("not in the box");
        }
        // Offered on an assigned batch only, and offered *while* it runs rather
        // than at the end: half a box done at five o'clock is the ordinary case,
        // and the terms for the keys that are ready are wanted then. What each
        // position produced — or did not — comes back in the notice.
        if batch.shape == crate::batch::Shape::AssignedEnrolment
            && super::row_button(ui, "Hand-over documents…").clicked()
        {
            app.generate_batch_terms_interactive();
        }
        if super::row_button(ui, "Put the batch down").clicked() {
            app.close_batch();
        }
    });
    super::hint(
        ui,
        "The batch keeps the counting; the run is the same one a single key gets — same plan, \
         same pre-flight, same confirmation.",
    );

    let attention = batch.needs_attention();
    if !attention.is_empty() {
        ui.add_space(12.0);
        super::table(ui, "batch-attention", &["Key", "What happened"], |ui| {
            for entry in attention {
                super::mono(ui, &entry.describe());
                super::faint(ui, &entry.detail);
                ui.end_row();
            }
        });
    }
}

/// Runs on the register that still have something left to do
/// (`features/gui-bootstrap-wizard.md` phase 5).
///
/// Shown on the selection stage because that is where an operator arrives with a
/// certificate in their hand and a key from last week. Until this existed a resume
/// could only continue a run still held in memory from the same session — which is
/// the short half of a wait the manual issuer makes routine.
fn unfinished_runs(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let open: Vec<(uuid::Uuid, String)> = crate::bootstrap::resumable(&app.runs)
        .into_iter()
        .map(|run| {
            let (done, failed, skipped, pending) = run.tally();
            (
                run.id,
                format!(
                    "serial {} · {} v{} · started {} · {done} done, {failed} failed, {skipped} \
                     skipped, {pending} pending",
                    run.key_serial,
                    run.template_id,
                    run.template_version,
                    run.started_at.format("%Y-%m-%d %H:%M"),
                ),
            )
        })
        .collect();

    if open.is_empty() {
        return;
    }

    super::titled_card(ui, "Unfinished runs on this register", |ui| {
        ui.label(
            "A run whose certificate had not come back yet, or that stopped part-way. Picking one \
             up rebuilds its plan from the version it recorded and leaves every completed step \
             alone.",
        );
        ui.add_space(8.0);
        for (id, label) in open {
            ui.horizontal_wrapped(|ui| {
                if ui.add(Button::new("Pick up")).clicked() {
                    app.adopt_run(id);
                }
                ui.label(label);
            });
        }
    });
    ui.add_space(18.0);
}

/// What will be written, and the one confirmation that authorises it.
///
/// What is plugged in, above the serial field
/// (`features/device-detection.md` phases 2 and 3).
///
/// The wizard is the screen where getting the key wrong is most expensive — the
/// next thing it does is write to one — so the serial field is not left as a number
/// somebody typed while a key sits in a port. With one key attached the field fills
/// itself; with several, the choice is explicit and this says so where the operator
/// is looking.
fn attached_keys(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let snapshot = app.attached.clone();
    if app.watch.is_none() || snapshot.stopped.is_some() {
        return;
    }

    let mut choose: Option<u32> = None;
    if snapshot.is_ambiguous() {
        super::notice(
            ui,
            CalloutTone::Warning,
            "More than one key is attached, so nothing has been assumed. Choose the one this run \
             is for — the serial below is what gets written to.",
        );
        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            for key in &snapshot.keys {
                let chosen = app.target_serial() == Some(key.serial);
                let label = format!("{} · {}", key.serial, key.model);
                if chosen {
                    ui.add(elegance::Badge::new(
                        format!("{label} — in use"),
                        elegance::BadgeTone::Ok,
                    ));
                } else if ui
                    .add(
                        Button::new(label)
                            .outline()
                            .size(elegance::ButtonSize::Small),
                    )
                    .clicked()
                {
                    choose = Some(key.serial);
                }
            }
        });
        ui.add_space(12.0);
    } else if let Some(only) = snapshot.only_key() {
        super::faint(
            ui,
            &format!(
                "attached: {} {} (firmware {})",
                only.model, only.serial, only.firmware
            ),
        );
        ui.add_space(8.0);
    } else if snapshot.polls > 0 && snapshot.keys.is_empty() {
        super::faint(ui, "no key attached — plug one in, or type a serial");
        ui.add_space(8.0);
    }

    if let Some(serial) = choose {
        app.select_key(serial);
    }
}

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

    certificate_exchange(app, ui);

    // One click from the evidence to the hand-over
    // (`features/gui-bootstrap-wizard.md` phase 6). Offered only on a completed
    // run: a key whose procedure did not finish is not one to hand over, and the
    // lifecycle refuses it anyway — refusing here says so before the click.
    if app
        .wizard
        .run
        .as_ref()
        .is_some_and(|run| run.status == crate::domain::RunStatus::Completed)
    {
        ui.add_space(14.0);
        super::titled_card(ui, "Hand this key over", |ui| {
            ui.label(
                "Opens the hand-over form with this key, its holder and this run already \
                 attached. Nothing is recorded until you confirm it there.",
            );
            ui.add_space(8.0);
            if ui
                .add(Button::new("Attach to a hand-over…"))
                .on_hover_text("Fills the Distribution form from this run")
                .clicked()
            {
                app.attach_run_to_handover();
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

/// The round trip to the CA: take the request out, bring the certificate back.
///
/// Shown only when this run actually produced a request, because that is the only
/// state in which there is anything to do here. The issuer is the operator
/// (`features/ca-integration.md` phase 1, decided 2026-08-13), so this is where a
/// run that ended with the import still pending gets finished.
fn certificate_exchange(app: &mut YkDistApp, ui: &mut egui::Ui) {
    use crate::domain::{StepKind, StepStatus};

    let Some(run) = app.wizard.run.clone() else {
        return;
    };
    if crate::bootstrap::certificate_request(&run).is_none() {
        return;
    }
    let import_pending = run
        .steps
        .iter()
        .any(|step| step.kind == StepKind::PivCertImport && step.status != StepStatus::Done);

    ui.add_space(14.0);
    super::titled_card(ui, "Signing certificate — the CA round trip", |ui| {
        if import_pending {
            super::notice(
                ui,
                CalloutTone::Warning,
                "This key has its signing key but no certificate, so it is not ready to hand \
                 over. Save the request below, have it signed by the CA this deployment uses, \
                 then bring the certificate back here.",
            );
        } else {
            super::notice(
                ui,
                CalloutTone::Success,
                "The certificate is on the key. The request is kept below as evidence of what \
                 was asked for.",
            );
        }

        ui.add_space(10.0);
        if ui
            .add(Button::new("Save the certification request…"))
            .on_hover_text("a PKCS#10 request — a public key and a name, no secret")
            .clicked()
        {
            app.save_certificate_request();
        }

        if !import_pending {
            return;
        }

        ui.add_space(14.0);
        ui.label("The issued certificate (PEM), pasted or loaded from a file:");
        let box_ = ui.add(
            egui::TextEdit::multiline(&mut app.wizard.certificate_pem)
                .desired_rows(6)
                .code_editor()
                .hint_text("-----BEGIN CERTIFICATE-----"),
        );
        if box_.changed() {
            app.preview_certificate();
        }

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Load from a file…")).clicked() {
                app.load_certificate();
            }
            if ui.add(Button::new("Check it")).clicked() {
                app.preview_certificate();
            }
        });

        // What the certificate says, before it is written rather than after. A
        // certificate for the wrong holder is the failure this display exists for.
        if let Some(preview) = &app.wizard.certificate_preview {
            ui.add_space(10.0);
            match preview {
                Ok(summary) => {
                    super::table(ui, "certificate-preview", &["Field", "Value"], |ui| {
                        for (field, value) in [
                            ("Subject", summary.subject.clone()),
                            ("Issuer", summary.issuer.clone()),
                            ("Serial", summary.serial.clone()),
                            (
                                "Valid",
                                format!("{} .. {}", summary.not_before, summary.not_after),
                            ),
                            ("rfc822Name", summary.email_sans.join(", ")),
                        ] {
                            ui.label(field);
                            super::mono(ui, &value);
                            ui.end_row();
                        }
                    });
                    ui.add_space(8.0);
                    match app.certificate_matches_holder() {
                        Some(true) => super::notice(
                            ui,
                            CalloutTone::Success,
                            "The address in this certificate is the one this run was built for.",
                        ),
                        Some(false) => super::notice(
                            ui,
                            CalloutTone::Warning,
                            "This certificate does not carry the address this run was built for. \
                             The import will refuse it — check it is the right holder's \
                             certificate.",
                        ),
                        None => {}
                    }
                }
                Err(message) => super::error_label(ui, message),
            }
        }

        // The PIN has to be typed because nothing was retained: the run that set it
        // kept no copy, and the applet will not accept a certificate without it.
        // While the key waits for its certificate it is still on the operator's
        // desk, with the transport PIN written down beside it.
        ui.add_space(14.0);
        ui.label("The PIV PIN this key was given (needed to authenticate the import):");
        ui.add(
            egui::TextEdit::singleline(&mut app.wizard.resume_pin)
                .password(true)
                .desired_width(140.0),
        );

        ui.add_space(14.0);
        let loaded = matches!(app.wizard.certificate_preview, Some(Ok(_)));
        let has_pin = !app.wizard.resume_pin.trim().is_empty();
        if ui
            .add(
                Button::new("Import the certificate and finish the run")
                    .accent(Accent::Red)
                    .enabled(loaded && has_pin),
            )
            .on_hover_text(match (loaded, has_pin) {
                (false, _) => "load a certificate this tool can read first",
                (true, false) => "the import needs this key's PIV PIN",
                (true, true) => "this writes the certificate to the key",
            })
            .clicked()
        {
            app.resume_run();
        }
    });
}

fn selection(app: &mut YkDistApp, ui: &mut egui::Ui) {
    attached_keys(app, ui);

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
