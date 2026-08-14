//! Reports screen: the questions the register answers, and the file they leave
//! in (`features/reports-and-export.md`).
//!
//! Paint only. Every rule — what a report contains, how a cell is quoted, what
//! the audit detail says — lives in [`crate::report`], where it is covered by
//! tests; this module chooses a report, shows the table, and asks the app to
//! generate or export it.

use elegance::{Button, CalloutTone, Select};

use crate::app::YkDistApp;
use crate::report::{PERSONAL_DATA_WARNING, ReportKind, export::Format};

pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::screen_header(
        ui,
        "Reports",
        "What the register knows, answered on screen and exportable. Every export is audited, \
         and none of them can contain a secret because none is stored.",
    );

    super::titled_card(ui, "Report", |ui| {
        let chosen = app.reports.kind;
        super::form_columns(ui, |left, right, width| {
            left.add(
                Select::new("report-kind", &mut app.reports.kind)
                    .label("Report")
                    .options(ReportKind::ALL.map(|kind| (kind, kind.label())))
                    .width(width),
            );
            super::hint(left, app.reports.kind.question());

            // Only the formats this report may leave in: a PDF is for the two
            // that are handed to somebody, and offering it for a custody list
            // would invite exporting a spreadsheet's worth of data as a document
            // nobody can sort.
            let available = Format::available_for(app.reports.kind);
            if !available.contains(&app.reports.format) {
                app.reports.format = Format::Csv;
            }
            right.add(
                Select::new("report-format", &mut app.reports.format)
                    .label("Export as")
                    .options(available.iter().map(|format| (*format, format.label())))
                    .width(width),
            );
            super::hint(
                right,
                "CSV for a spreadsheet, JSON for anything programmatic, PDF for what is handed \
                 over.",
            );
        });

        // The old table belongs to the old question: keeping it on screen under a
        // new heading is how somebody exports the wrong report.
        if app.reports.kind != chosen {
            app.reports.current = None;
        }

        ui.add_space(10.0);
        match app.reports.kind {
            ReportKind::CertificateExpiry => expiry_window(app, ui),
            ReportKind::AuditExtract => extract_range(app, ui),
            _ => {}
        }

        ui.add_space(10.0);
        ui.horizontal_wrapped(|ui| {
            if ui.add(Button::new("Generate")).clicked() {
                app.generate_report();
            }
            let ready = app.reports.current.is_some();
            if ui
                .add_enabled(ready, egui::Button::new("Export…"))
                .clicked()
            {
                app.export_report();
            }
            ui.add_space(8.0);
            if ui.add(egui::Button::new("Export all…")).clicked() {
                app.export_bundle_interactive();
            }
        });
        super::hint(
            ui,
            "Export all writes every report into one dated folder, from one moment in time, with \
             a manifest saying what is in it.",
        );

        if app.reports.kind.carries_personal_data() {
            ui.add_space(10.0);
            super::notice(ui, CalloutTone::Warning, PERSONAL_DATA_WARNING);
        }

        if let Some(error) = &app.reports.error {
            ui.add_space(10.0);
            super::error_label(ui, error);
        }
    });

    let Some(report) = &app.reports.current else {
        ui.add_space(16.0);
        super::notice(
            ui,
            CalloutTone::Neutral,
            "Choose a report and press Generate. Nothing is generated in the background: a report \
             carries the moment it was made.",
        );
        return;
    };

    ui.add_space(16.0);
    super::titled_card(ui, report.kind.label(), |ui| {
        super::faint(ui, &report.provenance());
        for note in &report.notes {
            ui.add_space(6.0);
            super::notice(ui, CalloutTone::Neutral, note);
        }
        ui.add_space(12.0);

        if report.rows.is_empty() {
            super::notice(ui, CalloutTone::Neutral, "Nothing to show for this report.");
            return;
        }

        let headers: Vec<&str> = report.columns.iter().map(String::as_str).collect();
        super::table(ui, "report", &headers, |ui| {
            for row in &report.rows {
                for cell in row {
                    ui.add(
                        egui::Label::new(
                            egui::RichText::new(cell)
                                .size(elegance::Theme::current(ui.ctx()).typography.small),
                        )
                        .selectable(true),
                    );
                }
                ui.end_row();
            }
        });
    });
}

fn expiry_window(app: &mut YkDistApp, ui: &mut egui::Ui) {
    ui.horizontal(|ui| {
        ui.label("Look ahead");
        ui.add(
            egui::DragValue::new(&mut app.reports.expiry_days)
                .range(1..=3650)
                .suffix(" days"),
        );
    });
    super::hint(
        ui,
        "Certificates outside the window are still listed — the window decides what is flagged, \
         not what is left out.",
    );
}

fn extract_range(app: &mut YkDistApp, ui: &mut egui::Ui) {
    super::form_columns(ui, |left, right, _| {
        super::capped_input(left, &mut app.reports.audit_filter.event, 64, |input| {
            input
                .label("Event")
                .id_salt("extract-event")
                .hint("bootstrap.")
        });
        left.add_space(8.0);
        super::capped_input(left, &mut app.reports.audit_filter.actor, 64, |input| {
            input.label("Actor").id_salt("extract-actor").hint("felipe")
        });

        super::capped_input(right, &mut app.reports.audit_filter.target, 64, |input| {
            input
                .label("Target")
                .id_salt("extract-target")
                .hint("serial:20423633")
        });
        right.add_space(8.0);
        right.horizontal(|ui| {
            super::capped_input(ui, &mut app.reports.from, 10, |input| {
                input
                    .label("From")
                    .id_salt("extract-from")
                    .hint("2026-01-01")
            });
            super::capped_input(ui, &mut app.reports.until, 10, |input| {
                input
                    .label("Until")
                    .id_salt("extract-until")
                    .hint("2026-12-31")
            });
        });
    });

    // Parsed into the filter as it is typed, so the report the operator generates
    // is the range they can see. A half-typed date narrows nothing rather than
    // silently excluding the whole trail.
    app.reports.audit_filter.from = crate::report::parse_day(&app.reports.from, false);
    app.reports.audit_filter.until = crate::report::parse_day(&app.reports.until, true);

    super::hint(
        ui,
        "The statement in the file covers the whole chain, not only the rows printed: an extract \
         that verified just its own range would pass while the entries around it were rewritten.",
    );
}
