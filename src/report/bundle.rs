//! The one-click bundle: every report at once, in a dated folder, with a
//! manifest (`features/reports-and-export.md` phase 8).
//!
//! # Why one click and not a schedule
//!
//! The phase is written as "scheduled / one-click", and only the second half is
//! built — deliberately, and this is the reason rather than an omission. A
//! schedule inside a desktop application can only fire while the application is
//! open, so a "monthly report" would be produced on whichever day somebody
//! happened to launch the tool, and *not at all* in a month where nobody did.
//! That is worse than no schedule: it looks like a control and is not one, and
//! the gap is invisible until an audit asks for the month that is missing.
//!
//! The backup schedule is the exception that shows the rule — a backup is a copy
//! of a file that only changes while the tool is open, so "when it is open" is
//! exactly when it needs taking. A report is asked for by a person on a date
//! somebody else chose. So the bundle is one button, and a deployment that wants
//! it monthly puts a reminder where its other monthly obligations live.

use chrono::{DateTime, Utc};

use super::{PERSONAL_DATA_WARNING, Report, ReportKind, export::Format};

/// What the bundle contains, in the order the manifest lists it.
///
/// Every report as CSV, because the bundle is what somebody cross-checks against
/// a procurement list — plus the two that are *handed* to a person as PDF as
/// well. The duplication is on purpose: the ESI gets a document, and whoever
/// reconciles gets a spreadsheet, from one action and one moment in time.
pub const CONTENTS: [(ReportKind, Format); 9] = [
    (ReportKind::InventorySummary, Format::Csv),
    (ReportKind::Custody, Format::Csv),
    (ReportKind::Unaccounted, Format::Csv),
    (ReportKind::BootstrapCompliance, Format::Csv),
    (ReportKind::BootstrapCompliance, Format::Pdf),
    (ReportKind::CertificateExpiry, Format::Csv),
    (ReportKind::CustodyModel, Format::Csv),
    (ReportKind::AuditExtract, Format::Csv),
    (ReportKind::AuditExtract, Format::Pdf),
];

/// The folder the bundle is written into: `reports-2026-08-14`.
pub fn directory_name(now: DateTime<Utc>) -> String {
    format!("reports-{}", now.format("%Y-%m-%d"))
}

/// The file that says what the folder holds.
pub const MANIFEST: &str = "MANIFEST.txt";

/// One line per file, plus what each report is and what the whole set means.
///
/// The manifest exists because a folder of nine files is not self-describing
/// even when each file is: the question somebody asks six months later is "is
/// this everything, and from when", and that is a property of the set.
pub fn manifest(entries: &[(String, &Report)], generated_by: &str, now: DateTime<Utc>) -> String {
    let mut out = String::new();
    out.push_str("yk-dist-manager — report bundle\n");
    out.push_str(&format!(
        "Generated {} by {}\n",
        now.format("%Y-%m-%d %H:%M:%SZ"),
        if generated_by.trim().is_empty() {
            "(operator not recorded)"
        } else {
            generated_by.trim()
        }
    ));
    out.push_str(&format!("Built by {}\n\n", crate::build_id()));

    out.push_str("Contents\n--------\n");
    for (name, report) in entries {
        out.push_str(&format!("{name}\n"));
        out.push_str(&format!("    {}\n", report.kind.question()));
        out.push_str(&format!("    {} row(s) — {}\n", report.rows.len(), report.scope));
        for note in &report.notes {
            out.push_str(&format!("    {}\n", note.split_whitespace().collect::<Vec<_>>().join(" ")));
        }
        out.push('\n');
    }

    out.push_str("Handling\n--------\n");
    out.push_str(PERSONAL_DATA_WARNING);
    out.push('\n');
    out.push_str(
        "Every file in this folder was recorded in the register's audit trail as it was written \
         (`export.taken`).\n",
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn report(kind: ReportKind) -> Report {
        Report {
            kind,
            generated_at: at(),
            generated_by: "felipe".into(),
            scope: "3 open hand-over(s)".into(),
            columns: vec!["Serial".into()],
            rows: vec![vec!["1".into()], vec!["2".into()]],
            notes: vec!["a note".into()],
        }
    }

    #[test]
    fn the_bundle_covers_every_report_and_names_its_folder_by_date() {
        for kind in ReportKind::ALL {
            assert!(
                CONTENTS.iter().any(|(k, _)| *k == kind),
                "{kind:?} is missing from the bundle"
            );
        }
        // And nothing is offered in a format that report may not leave in.
        for (kind, format) in CONTENTS {
            assert!(
                Format::available_for(kind).contains(&format),
                "{kind:?} may not be exported as {format:?}"
            );
        }
        assert_eq!(directory_name(at()), "reports-2026-08-14");
    }

    #[test]
    fn the_manifest_says_what_the_set_is_and_how_to_handle_it() {
        let custody = report(ReportKind::Custody);
        let extract = report(ReportKind::AuditExtract);
        let entries = vec![
            ("custody-2026-08-14.csv".to_owned(), &custody),
            ("audit-extract-2026-08-14.csv".to_owned(), &extract),
        ];
        let text = manifest(&entries, "felipe", at());

        assert!(text.contains("custody-2026-08-14.csv"), "{text}");
        assert!(text.contains("2 row(s)"), "{text}");
        assert!(text.contains("a note"), "{text}");
        assert!(text.contains("felipe"), "{text}");
        // The handling section is the point of the file, not decoration.
        assert!(text.contains("outside"), "{text}");
        assert!(text.contains("export.taken"), "{text}");
    }
}
