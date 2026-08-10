//! Bootstrap wizard: pick a key, a holder and a template, review the plan,
//! then run it.
//!
//! Everything on this screen is currently **dry run only** — the plan is shown
//! and can be recorded as evidence of intent, but no step touches the key. The
//! executor is Wave 2 in `roadmap.md`.

use crate::app::YkDistApp;
use crate::template::Transport;

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Bootstrap",
        "Templated procedure: FIDO2 PIN, OTP access code, on-key FIDO2 credential, \
         and a PIV signing certificate carrying the holder's e-mail.",
    );

    selection(app, ui);
    ui.add_space(10.0);
    steps(app, ui);
    ui.add_space(10.0);

    ui.horizontal_wrapped(|ui| {
        if ui.button("Build plan").clicked() {
            app.build_plan();
        }
        if ui
            .add_enabled(
                !app.wizard.plan.is_empty(),
                egui::Button::new("Record dry run"),
            )
            .clicked()
        {
            app.record_dry_run();
        }
        ui.add_enabled(false, egui::Button::new("Execute on key (Wave 2)"));
    });

    if let Some(error) = app.wizard.error.clone() {
        ui.add_space(6.0);
        super::error_label(ui, &error);
    }

    ui.add_space(12.0);
    ui.separator();
    ui.add_space(6.0);
    plan_table(app, ui);
}

fn selection(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let holder_labels: Vec<String> = app.holders.iter().map(|h| h.display()).collect();
    let template_labels: Vec<String> = app
        .templates
        .iter()
        .map(|t| format!("{} (v{})", t.name, t.version))
        .collect();

    egui::Grid::new("wizard-selection")
        .num_columns(2)
        .spacing([12.0, 6.0])
        .show(ui, |ui| {
            ui.label("Key serial");
            ui.horizontal(|ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut app.wizard.serial)
                        .char_limit(12)
                        .desired_width(140.0),
                );
                if ui.button("read attached key").clicked() {
                    app.detect_keys();
                }
            });
            ui.end_row();

            ui.label("Holder");
            if holder_labels.is_empty() {
                ui.label("register a holder first");
            } else {
                app.wizard.holder_index = app.wizard.holder_index.min(holder_labels.len() - 1);
                egui::ComboBox::from_id_salt("wizard-holder")
                    .selected_text(&holder_labels[app.wizard.holder_index])
                    .width(340.0)
                    .show_ui(ui, |ui| {
                        for (index, label) in holder_labels.iter().enumerate() {
                            ui.selectable_value(&mut app.wizard.holder_index, index, label);
                        }
                    });
            }
            ui.end_row();

            ui.label("Template");
            if template_labels.is_empty() {
                ui.label("no template available");
            } else {
                app.wizard.template_index =
                    app.wizard.template_index.min(template_labels.len() - 1);
                let before = app.wizard.template_index;
                egui::ComboBox::from_id_salt("wizard-template")
                    .selected_text(&template_labels[app.wizard.template_index])
                    .width(340.0)
                    .show_ui(ui, |ui| {
                        for (index, label) in template_labels.iter().enumerate() {
                            ui.selectable_value(&mut app.wizard.template_index, index, label);
                        }
                    });
                if before != app.wizard.template_index {
                    app.wizard.step_enabled.clear();
                    app.wizard.plan.clear();
                }
            }
            ui.end_row();
        });

    if let Some(template) = app.selected_template() {
        ui.add_space(4.0);
        ui.label(egui::RichText::new(&template.description).weak().small());
    }
}

fn steps(app: &mut YkDistApp, ui: &mut egui::Ui) {
    let Some(template) = app.selected_template().cloned() else {
        return;
    };
    if app.wizard.step_enabled.len() != template.steps.len() {
        app.wizard.step_enabled = template.steps.iter().map(|s| s.enabled).collect();
    }

    ui.label("Steps to apply:");
    for (index, step) in template.steps.iter().enumerate() {
        let required = step.required;
        ui.horizontal(|ui| {
            ui.add_enabled(
                !required,
                egui::Checkbox::new(&mut app.wizard.step_enabled[index], ""),
            );
            ui.label(step.kind.label());
            if required {
                ui.label(egui::RichText::new("required").small().weak());
            }
        });
    }
}

fn plan_table(app: &mut YkDistApp, ui: &mut egui::Ui) {
    if app.wizard.plan.is_empty() {
        ui.label("No plan built yet. Nothing has been sent to the key.");
        return;
    }

    ui.label(
        egui::RichText::new(
            "Secrets appear as placeholders — a PIN is never rendered, logged or stored.",
        )
        .small()
        .weak(),
    );
    ui.add_space(6.0);

    egui::Grid::new("plan")
        .striped(true)
        .num_columns(4)
        .spacing([14.0, 6.0])
        .show(ui, |ui| {
            for header in ["Step", "Transport", "Operation", "Note"] {
                ui.strong(header);
            }
            ui.end_row();

            for command in &app.wizard.plan {
                ui.label(command.kind.label());
                let (text, color) = match command.transport() {
                    Transport::Native => ("native", egui::Color32::from_rgb(60, 140, 70)),
                    Transport::Ykman => ("ykman", egui::Color32::from_rgb(190, 140, 40)),
                    Transport::Manual => ("manual", egui::Color32::from_rgb(150, 90, 170)),
                };
                ui.label(egui::RichText::new(text).color(color).small());
                ui.add(
                    egui::Label::new(
                        egui::RichText::new(command.transport_detail())
                            .monospace()
                            .small(),
                    )
                    .selectable(true),
                );
                ui.label(
                    egui::RichText::new(command.note.clone().unwrap_or_default())
                        .small()
                        .weak(),
                );
                ui.end_row();
            }
        });
}
