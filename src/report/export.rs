//! Writing a [`Report`](super::Report) to a file
//! (`features/reports-and-export.md` phases 2 and 7).
//!
//! Two formats, one shape. CSV is for the spreadsheet the data will be
//! cross-checked in; JSON is for anything programmatic. Both are produced from
//! the same [`Report`](super::Report), which is what stops them disagreeing
//! about what a report contains — the failure mode of a per-format writer is
//! that one of them quotes a comma and the other does not, and nobody finds out
//! until a row lands in the wrong column of a reconciliation.
//!
//! # The file says what it is
//!
//! Both formats carry the scope, the generation time and the operator. A
//! spreadsheet found on a share six months later has to be datable without the
//! person who exported it, and a file name is not evidence — it can be renamed,
//! and usually is.
//!
//! In CSV that goes in `#`-prefixed preamble lines above the header row. That is
//! the convention every spreadsheet import dialog already copes with, and the
//! alternative — a first row with one filled cell — makes the file ragged for
//! anything parsing it strictly. In JSON it is simply fields on the object.

use std::fmt::Write as _;

use serde::Serialize;

use super::Report;

/// The export formats.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Csv,
    Json,
    /// A printable document, for the two reports that are *handed* to somebody.
    Pdf,
}

impl Format {
    pub const ALL: [Format; 3] = [Format::Csv, Format::Json, Format::Pdf];

    pub fn label(&self) -> &'static str {
        match self {
            Format::Csv => "CSV (spreadsheet)",
            Format::Json => "JSON",
            Format::Pdf => "PDF (to hand over)",
        }
    }

    /// Stable name for the audit detail.
    pub fn slug(&self) -> &'static str {
        match self {
            Format::Csv => "csv",
            Format::Json => "json",
            Format::Pdf => "pdf",
        }
    }

    pub fn extension(&self) -> &'static str {
        self.slug()
    }

    /// Which formats this report may leave in.
    ///
    /// PDF is deliberately **not** offered for the other five. A PDF is what you
    /// produce when the artefact is handed to a person — the ESI's audit extract,
    /// the compliance report that goes with a review — and it is the worst of the
    /// three for the thing the other five are for, which is being opened in a
    /// spreadsheet beside a procurement list. Offering it everywhere would invite
    /// somebody to export a custody list as a document nobody can sort.
    pub fn available_for(kind: super::ReportKind) -> &'static [Format] {
        use super::ReportKind;
        match kind {
            ReportKind::AuditExtract | ReportKind::BootstrapCompliance => {
                &[Format::Csv, Format::Json, Format::Pdf]
            }
            _ => &[Format::Csv, Format::Json],
        }
    }
}

/// Render a report in one format.
///
/// Bytes rather than a string because one of the three is a PDF. The two text
/// formats are still available as [`to_csv`] and [`to_json`] for anything that
/// wants to read them.
pub fn render(report: &Report, format: Format) -> Vec<u8> {
    match format {
        Format::Csv => to_csv(report).into_bytes(),
        Format::Json => to_json(report).into_bytes(),
        Format::Pdf => to_pdf(report),
    }
}

/// RFC 4180 CSV, with a preamble naming the file.
pub fn to_csv(report: &Report) -> String {
    let mut out = String::new();

    let _ = writeln!(out, "# {}", report.provenance());
    for note in &report.notes {
        // Folded to one line: a note carrying a newline would otherwise produce a
        // second, unmarked comment line, and a reader stripping `#` would keep
        // half a sentence as data.
        let _ = writeln!(out, "# {}", fold(note));
    }
    let _ = writeln!(out);

    let _ = writeln!(out, "{}", csv_row(report.columns.iter()));
    for row in &report.rows {
        let _ = writeln!(out, "{}", csv_row(row.iter()));
    }
    out
}

fn csv_row<'a>(cells: impl Iterator<Item = &'a String>) -> String {
    cells.map(|cell| quote(cell)).collect::<Vec<_>>().join(",")
}

/// Quote a cell the way RFC 4180 says, and only when it needs it.
///
/// The leading-separator case is the one worth naming: a cell that starts with
/// `=`, `+`, `-` or `@` is a **formula** to a spreadsheet, so a value pasted out
/// of a certificate subject could execute when the file is opened. Prefixing it
/// with an apostrophe would corrupt the value; quoting alone does not stop the
/// formula, so the cell is prefixed with a tab inside the quotes — which every
/// spreadsheet treats as text and every CSV parser reads back as a leading tab
/// rather than as a changed value.
fn quote(cell: &str) -> String {
    let dangerous_lead = cell
        .chars()
        .next()
        .is_some_and(|c| matches!(c, '=' | '+' | '-' | '@'));
    let body = if dangerous_lead {
        format!("\t{cell}")
    } else {
        cell.to_owned()
    };

    if body.contains([',', '"', '\n', '\r']) || dangerous_lead {
        format!("\"{}\"", body.replace('"', "\"\""))
    } else {
        body
    }
}

fn fold(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// The JSON document a report becomes.
///
/// Rows are objects keyed by column name rather than positional arrays: a
/// consumer written against `serial` keeps working when a column is inserted,
/// and one written against `row[0]` does not.
#[derive(Debug, Serialize)]
struct JsonReport<'a> {
    report: &'a str,
    generated_at: String,
    generated_by: &'a str,
    scope: &'a str,
    notes: &'a [String],
    columns: &'a [String],
    row_count: usize,
    rows: Vec<serde_json::Map<String, serde_json::Value>>,
}

pub fn to_json(report: &Report) -> String {
    let rows = report
        .rows
        .iter()
        .map(|row| {
            report
                .columns
                .iter()
                .zip(row.iter())
                .map(|(column, cell)| (column.clone(), serde_json::Value::String(cell.clone())))
                .collect()
        })
        .collect();

    let document = JsonReport {
        report: report.kind.slug(),
        generated_at: report.generated_at.to_rfc3339(),
        generated_by: &report.generated_by,
        scope: &report.scope,
        notes: &report.notes,
        columns: &report.columns,
        row_count: report.rows.len(),
        rows,
    };

    // Pretty, because a report is read by a person at least as often as it is
    // parsed, and `expect` because the document is strings and numbers only.
    serde_json::to_string_pretty(&document).expect("a report serialises")
}

/// The report as a printable document.
///
/// Set in Courier by [`crate::pdf`], which is the same writer the consignment
/// term uses — no PDF crate, no TeX, nothing for an operator to install. That
/// constrains the layout in a way worth stating: the page is a fixed number of
/// monospaced columns, so a table only becomes a table when it fits.
///
/// When it does not, each row is printed as a block of `Column: value` lines
/// instead. That is not a fallback so much as the correct rendering for the two
/// reports allowed here — an audit entry carries a 64-character hash, and a
/// "table" of those is a column of truncated evidence.
pub fn to_pdf(report: &Report) -> Vec<u8> {
    use crate::pdf::{self, TextDocument};

    let width = pdf::columns();
    let mut lines: Vec<String> = Vec::new();

    lines.extend(pdf::wrap(&report.provenance(), width));
    for note in &report.notes {
        lines.push(String::new());
        lines.extend(pdf::wrap(&fold(note), width));
    }
    lines.push(String::new());

    if report.rows.is_empty() {
        lines.push("Nothing to report.".to_owned());
    } else if let Some(widths) = table_widths(report, width) {
        lines.push(pad_row(&report.columns, &widths));
        lines.push("-".repeat(widths.iter().sum::<usize>() + widths.len().saturating_sub(1)));
        for row in &report.rows {
            lines.push(pad_row(row, &widths));
        }
    } else {
        for (index, row) in report.rows.iter().enumerate() {
            if index > 0 {
                lines.push(String::new());
            }
            for (column, cell) in report.columns.iter().zip(row.iter()) {
                let label = format!("{column}: ");
                let indent = " ".repeat(label.chars().count().min(width / 2));
                let wrapped = pdf::wrap(cell, width.saturating_sub(label.chars().count()));
                for (line, text) in wrapped.into_iter().enumerate() {
                    if line == 0 {
                        lines.push(format!("{label}{text}"));
                    } else {
                        lines.push(format!("{indent}{text}"));
                    }
                }
            }
        }
    }

    pdf::render(&TextDocument {
        heading: report.kind.label().to_owned(),
        lines,
        author: String::new(),
        // File metadata travels further than the body, so it names the report and
        // the date and nothing about a person.
        subject: format!(
            "{} — {}",
            report.kind.label(),
            report.generated_at.format("%Y-%m-%d")
        ),
        footer: format!(
            "{} — generated {}",
            report.kind.label(),
            report.generated_at.format("%Y-%m-%d %H:%M:%SZ")
        ),
        created: pdf::pdf_date(&report.generated_at),
    })
}

/// Column widths, or `None` when the table cannot fit the page.
fn table_widths(report: &Report, available: usize) -> Option<Vec<usize>> {
    let mut widths: Vec<usize> = report.columns.iter().map(|c| c.chars().count()).collect();
    for row in &report.rows {
        for (index, cell) in row.iter().enumerate() {
            if let Some(width) = widths.get_mut(index) {
                *width = (*width).max(cell.chars().count());
            }
        }
    }
    let total = widths.iter().sum::<usize>() + widths.len().saturating_sub(1);
    (total <= available).then_some(widths)
}

fn pad_row(cells: &[String], widths: &[usize]) -> String {
    cells
        .iter()
        .zip(widths.iter())
        .map(|(cell, width)| format!("{cell:<width$}"))
        .collect::<Vec<_>>()
        .join(" ")
        .trim_end()
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::report::{Report, ReportKind};
    use chrono::{DateTime, Utc};

    fn at() -> DateTime<Utc> {
        DateTime::parse_from_rfc3339("2026-08-14T12:00:00Z")
            .unwrap()
            .with_timezone(&Utc)
    }

    fn report() -> Report {
        let mut report = Report {
            kind: ReportKind::Custody,
            generated_at: at(),
            generated_by: "felipe".into(),
            scope: "2 open hand-over(s)".into(),
            columns: vec!["Serial".into(), "Holder".into()],
            rows: vec![
                vec!["20423633".into(), "Ana Silva".into()],
                vec!["20423634".into(), "Silva, Bruno \"BB\"".into()],
            ],
            notes: Vec::new(),
        };
        report.notes.push("a note\nover two lines".into());
        report
    }

    #[test]
    fn the_csv_names_itself_above_the_header_row() {
        let csv = to_csv(&report());
        let lines: Vec<&str> = csv.lines().collect();
        assert!(lines[0].starts_with("# Custody"), "{}", lines[0]);
        assert!(lines[0].contains("2026-08-14"), "{}", lines[0]);
        assert!(lines[0].contains("felipe"), "{}", lines[0]);
        // The note is folded onto one comment line, not spread over two.
        assert_eq!(lines[1], "# a note over two lines");
        assert_eq!(lines[2], "");
        assert_eq!(lines[3], "Serial,Holder");
    }

    #[test]
    fn a_comma_and_a_quote_survive_the_round_trip() {
        let csv = to_csv(&report());
        let last = csv.lines().last().expect("a row");
        assert_eq!(last, r#"20423634,"Silva, Bruno ""BB""""#);
    }

    #[test]
    fn a_cell_that_a_spreadsheet_would_run_as_a_formula_is_neutralised() {
        // The failure this guards: a value out of a certificate subject or an
        // operator's note opening as a formula when the file is double-clicked.
        let mut report = report();
        report.rows = vec![vec!["=1+1".into(), "-2".into()]];
        let csv = to_csv(&report);
        let row = csv.lines().last().expect("a row");
        assert_eq!(row, "\"\t=1+1\",\"\t-2\"");
        // And the value itself is unchanged apart from the tab: nothing is
        // rewritten, so the export still says what the register says.
        assert!(row.contains("=1+1"), "{row}");
    }

    #[test]
    fn the_json_keys_rows_by_column_name_and_carries_the_scope() {
        let json = to_json(&report());
        let parsed: serde_json::Value = serde_json::from_str(&json).expect("valid JSON");
        assert_eq!(parsed["report"], "custody");
        assert_eq!(parsed["generated_by"], "felipe");
        assert_eq!(parsed["scope"], "2 open hand-over(s)");
        assert_eq!(parsed["row_count"], 2);
        assert_eq!(parsed["rows"][0]["Serial"], "20423633");
        assert_eq!(parsed["rows"][1]["Holder"], "Silva, Bruno \"BB\"");
    }

    #[test]
    fn the_file_name_carries_the_report_and_the_date() {
        let report = report();
        assert_eq!(report.file_name(Format::Csv), "custody-2026-08-14.csv");
        assert_eq!(report.file_name(Format::Json), "custody-2026-08-14.json");
    }

    #[test]
    fn the_audit_detail_names_what_left_and_where_it_went() {
        let report = report();
        let detail = report.audit_detail(Format::Csv, std::path::Path::new("/tmp/custody.csv"));
        assert_eq!(
            detail,
            "report=custody format=csv rows=2 path=/tmp/custody.csv"
        );
    }

    #[test]
    fn render_produces_every_format() {
        for format in Format::ALL {
            assert!(!render(&report(), format).is_empty());
            assert!(!format.label().is_empty());
        }
    }

    #[test]
    fn a_pdf_is_offered_only_for_the_two_reports_that_are_handed_to_somebody() {
        assert!(Format::available_for(ReportKind::AuditExtract).contains(&Format::Pdf));
        assert!(Format::available_for(ReportKind::BootstrapCompliance).contains(&Format::Pdf));
        assert!(!Format::available_for(ReportKind::Custody).contains(&Format::Pdf));
        // And the two text formats are always available.
        for kind in ReportKind::ALL {
            let available = Format::available_for(kind);
            assert!(available.contains(&Format::Csv), "{kind:?}");
            assert!(available.contains(&Format::Json), "{kind:?}");
        }
    }

    #[test]
    fn a_narrow_report_is_set_as_a_table_and_a_wide_one_as_blocks() {
        // Narrow: two short columns fit the page, so the PDF is a table.
        let narrow = report();
        let widths = table_widths(&narrow, crate::pdf::columns()).expect("it fits");
        assert_eq!(widths.len(), 2);
        assert_eq!(pad_row(&narrow.rows[0], &widths), "20423633 Ana Silva");

        // Wide: an audit entry carries a 64-character hash, and a table of those
        // would be a column of truncated evidence.
        let mut wide = report();
        wide.columns.push("Hash".into());
        for row in &mut wide.rows {
            row.push("a".repeat(64));
        }
        assert!(table_widths(&wide, crate::pdf::columns()).is_none());
        assert!(!to_pdf(&wide).is_empty());
    }

    #[test]
    fn the_pdf_is_a_pdf_and_names_the_report() {
        let bytes = to_pdf(&report());
        assert!(bytes.starts_with(b"%PDF-"), "not a PDF");
        let text = String::from_utf8_lossy(&bytes);
        assert!(text.contains("Custody"), "the heading is in the document");
    }
}
