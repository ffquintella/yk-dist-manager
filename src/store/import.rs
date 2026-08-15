//! Importing the spreadsheet this tool replaces.
//!
//! Every unit that hands out security tokens already has a register: a
//! spreadsheet with a serial column, a name column and an e-mail column. Asking
//! an operator to retype it is asking them to keep the spreadsheet — so the
//! first thing the tool has to be able to do is read it.
//!
//! ## Two passes, always
//!
//! An import is [`plan`] then [`Store::apply_import`], never one call. The plan
//! is a *preview*: it reads the file, decides what each row would do, and
//! returns that as data with nothing written. The operator sees "12 new keys, 3
//! already known, 1 refused: serial 'ABC123' is not a number" and decides.
//!
//! This is not politeness. A spreadsheet is the least trustworthy input this
//! tool takes — it has been edited by hand for years, by people who were not
//! thinking about a parser — and the failure mode of importing it blind is a
//! register that looks populated and is wrong.
//!
//! ## What is imported, and what is deliberately not
//!
//! **Keys** (by serial) and **holders** (by e-mail). Both have a natural key, so
//! a re-import updates rather than duplicates, and a row that names a key that
//! is already registered is a refresh and not a conflict.
//!
//! **Distributions are not imported.** A hand-over record needs a date, a
//! delivery method and the operator who performed it, and a spreadsheet almost
//! never has all three in a form that survives parsing. Importing one anyway
//! would mean inventing custody evidence — a record saying Ana received a key on
//! a date nobody wrote down, from an operator the file does not name. The
//! import brings in the inventory and the people; who holds what is then
//! recorded through the Distribution screen, which asks for the facts a
//! hand-over needs.
//!
//! ## Provenance
//!
//! A serial read out of a spreadsheet is [`SerialSource::ManualEntry`]: nobody
//! has touched that key. It is the weakest provenance the domain has, and it is
//! the honest one — the whole point of `features/serial-scanning.md` is that a
//! serial nobody read from hardware is a claim, not a fact. A later device read
//! upgrades it.

use std::collections::BTreeMap;
use std::path::Path;

use crate::domain::{Holder, SerialSource, YubiKeyRecord};

/// The largest file we will read into memory.
///
/// A unit's register is measured in kilobytes; anything past this is a wrong
/// file (a database, a disk image, a video) and reading it would be the only
/// thing the application did for the next minute.
pub const MAX_IMPORT_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    #[error("could not read {path}: {reason}")]
    Unreadable { path: String, reason: String },
    #[error("the file is {size} bytes, larger than the {max}-byte limit for an import")]
    TooLarge { size: u64, max: u64 },
    #[error("the file is empty")]
    Empty,
    #[error(
        "no column that could be a serial number — expected one of: {expected}. Found: {found}"
    )]
    NoSerialColumn { expected: String, found: String },
    #[error("the file is not text: {0}")]
    NotText(String),
}

/// What one row of the file would do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RowPlan {
    /// A serial that is not in the register yet.
    NewKey { serial: u32 },
    /// A serial already registered; the import would refresh what it names.
    KnownKey { serial: u32 },
    /// A person not in the register yet.
    NewHolder { email: String },
    /// A person already registered.
    KnownHolder { email: String },
    /// The row names both, and both are new.
    NewKeyAndHolder { serial: u32, email: String },
    /// Nothing usable, with the reason to show the operator.
    Refused { reason: String },
}

impl RowPlan {
    pub fn is_refusal(&self) -> bool {
        matches!(self, RowPlan::Refused { .. })
    }
}

/// One row of the file, with its line number so a refusal can be found.
#[derive(Debug, Clone)]
pub struct PlannedRow {
    /// 1-based line in the file, counting the header — what a spreadsheet shows.
    pub line: usize,
    pub plan: RowPlan,
    pub key: Option<YubiKeyRecord>,
    pub holder: Option<Holder>,
}

/// The preview: what an import would do, with nothing written.
#[derive(Debug, Clone, Default)]
pub struct ImportPlan {
    pub rows: Vec<PlannedRow>,
    /// Header names that were not recognised, so the operator can see that the
    /// "Notes" column they care about is being dropped.
    pub ignored_columns: Vec<String>,
    /// Why no holder was imported, when the file could not support one.
    ///
    /// A file-level fact reported once, rather than the same refusal repeated
    /// against every row: "your spreadsheet has no unit column" is one thing to
    /// fix, and seeing it two hundred times buries the rows that are genuinely
    /// wrong.
    pub holders_skipped: Option<String>,
}

impl ImportPlan {
    pub fn new_keys(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| {
                matches!(
                    r.plan,
                    RowPlan::NewKey { .. } | RowPlan::NewKeyAndHolder { .. }
                )
            })
            .count()
    }

    pub fn new_holders(&self) -> usize {
        self.rows
            .iter()
            .filter(|r| {
                matches!(
                    r.plan,
                    RowPlan::NewHolder { .. } | RowPlan::NewKeyAndHolder { .. }
                )
            })
            .count()
    }

    pub fn refusals(&self) -> impl Iterator<Item = &PlannedRow> {
        self.rows.iter().filter(|r| r.plan.is_refusal())
    }

    pub fn refusal_count(&self) -> usize {
        self.refusals().count()
    }

    /// One line for the operator, before they commit to anything.
    pub fn summary(&self) -> String {
        format!(
            "{} rows: {} new keys, {} new holders, {} refused",
            self.rows.len(),
            self.new_keys(),
            self.new_holders(),
            self.refusal_count()
        )
    }
}

/// The column names this importer understands, in the languages a register in
/// this deployment is actually written in.
///
/// Matched case-insensitively with surrounding whitespace and punctuation
/// stripped, because a spreadsheet header is `Serial Number ` as often as
/// `serial`.
const SERIAL_COLUMNS: [&str; 8] = [
    "serial",
    "serialnumber",
    "serialno",
    "sn",
    "numeroserie",
    "numerodeserie",
    "serie",
    "numserie",
];
const NAME_COLUMNS: [&str; 7] = [
    "name",
    "fullname",
    "holder",
    "nome",
    "nomecompleto",
    "portador",
    "responsavel",
];
const EMAIL_COLUMNS: [&str; 5] = ["email", "mail", "emailaddress", "correio", "endereco"];
const UNIT_COLUMNS: [&str; 6] = [
    "unit",
    "department",
    "unidade",
    "departamento",
    "setor",
    "lotacao",
];
const MODEL_COLUMNS: [&str; 4] = ["model", "modelo", "type", "tipo"];
const NOTES_COLUMNS: [&str; 4] = ["notes", "note", "observacao", "observacoes"];

/// Fold a header cell to its comparison form: lower case, no accents, no
/// separators. `Número de Série` and `serial_number` both reduce to something
/// the tables above can match.
fn fold(header: &str) -> String {
    header
        .chars()
        .filter_map(|c| {
            let c = match c {
                'á' | 'à' | 'ã' | 'â' | 'ä' | 'Á' | 'À' | 'Ã' | 'Â' | 'Ä' => 'a',
                'é' | 'è' | 'ê' | 'ë' | 'É' | 'È' | 'Ê' | 'Ë' => 'e',
                'í' | 'ì' | 'î' | 'ï' | 'Í' | 'Ì' | 'Î' | 'Ï' => 'i',
                'ó' | 'ò' | 'õ' | 'ô' | 'ö' | 'Ó' | 'Ò' | 'Õ' | 'Ô' | 'Ö' => 'o',
                'ú' | 'ù' | 'û' | 'ü' | 'Ú' | 'Ù' | 'Û' | 'Ü' => 'u',
                'ç' | 'Ç' => 'c',
                other => other,
            };
            c.is_alphanumeric().then(|| c.to_ascii_lowercase())
        })
        .collect()
}

fn column_of(headers: &[String], candidates: &[&str]) -> Option<usize> {
    headers
        .iter()
        .position(|header| candidates.contains(&fold(header).as_str()))
}

/// Split one CSV line into cells (RFC 4180: quotes, doubled quotes, embedded
/// separators).
///
/// Hand-written rather than pulled from a crate for the same reason the PDF
/// writer is: the grammar is four rules, the input is one line, and a
/// dependency here would be carried by every build for the sake of a function
/// that fits on a screen.
pub(crate) fn split_row(line: &str, separator: char) -> Vec<String> {
    let mut cells = Vec::new();
    let mut cell = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();

    while let Some(c) = chars.next() {
        match c {
            '"' if quoted => {
                // A doubled quote inside a quoted cell is one literal quote.
                if chars.peek() == Some(&'"') {
                    chars.next();
                    cell.push('"');
                } else {
                    quoted = false;
                }
            }
            '"' => quoted = true,
            c if c == separator && !quoted => cells.push(std::mem::take(&mut cell)),
            c => cell.push(c),
        }
    }
    cells.push(cell);
    cells.into_iter().map(|c| c.trim().to_owned()).collect()
}

/// Guess the separator from the header line.
///
/// A spreadsheet exported in a locale that uses a decimal comma writes
/// semicolon-separated CSV, which is most of Europe and Brazil — so guessing
/// "comma" would fail on exactly the files this deployment produces.
pub(crate) fn detect_separator(header: &str) -> char {
    let semicolons = header.matches(';').count();
    let commas = header.matches(',').count();
    let tabs = header.matches('\t').count();
    if tabs > semicolons && tabs > commas {
        '\t'
    } else if semicolons >= commas {
        ';'
    } else {
        ','
    }
}

/// Read a serial that a human typed into a spreadsheet.
///
/// Tolerates the decorations a spreadsheet adds — thousands separators, a
/// leading apostrophe from a cell forced to text, surrounding whitespace — and
/// refuses anything else rather than guessing. A serial is the primary key of
/// every record in this tool; a wrong one attributes a key to the wrong person.
fn parse_serial(raw: &str) -> Result<u32, String> {
    let cleaned: String = raw
        .trim()
        .trim_start_matches('\'')
        .chars()
        .filter(|c| !matches!(c, '.' | ',' | ' ' | '\u{a0}' | '_'))
        .collect();

    if cleaned.is_empty() {
        return Err("no serial number in the row".to_owned());
    }
    cleaned
        .parse::<u32>()
        .map_err(|_| format!("`{raw}` is not a serial number"))
}

/// What is already in the register, so the plan can say "new" or "known"
/// without the planner needing a [`crate::store::Store`].
#[derive(Debug, Default, Clone)]
pub struct Existing {
    pub serials: std::collections::HashSet<u32>,
    pub emails: std::collections::HashSet<String>,
}

/// Read `path` and decide what importing it would do. Writes nothing.
pub fn plan(path: &Path, existing: &Existing) -> Result<ImportPlan, ImportError> {
    let metadata = std::fs::metadata(path).map_err(|e| ImportError::Unreadable {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    if metadata.len() > MAX_IMPORT_BYTES {
        return Err(ImportError::TooLarge {
            size: metadata.len(),
            max: MAX_IMPORT_BYTES,
        });
    }

    let bytes = std::fs::read(path).map_err(|e| ImportError::Unreadable {
        path: path.display().to_string(),
        reason: e.to_string(),
    })?;
    // A spreadsheet saved as "CSV UTF-8" starts with a byte-order mark, which
    // would otherwise become part of the first header name and stop it matching.
    let text =
        String::from_utf8(strip_bom(&bytes)).map_err(|e| ImportError::NotText(e.to_string()))?;

    plan_text(&text, existing)
}

fn strip_bom(bytes: &[u8]) -> Vec<u8> {
    match bytes.strip_prefix(&[0xEF, 0xBB, 0xBF]) {
        Some(rest) => rest.to_vec(),
        None => bytes.to_vec(),
    }
}

/// The half of [`plan`] that has no file system in it, so the mapping rules are
/// testable from a string literal.
pub fn plan_text(text: &str, existing: &Existing) -> Result<ImportPlan, ImportError> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header_line = lines.next().ok_or(ImportError::Empty)?;
    let separator = detect_separator(header_line);
    let headers = split_row(header_line, separator);

    let serial_at = column_of(&headers, &SERIAL_COLUMNS);
    let email_at = column_of(&headers, &EMAIL_COLUMNS);

    // A file with neither a serial nor an e-mail column is not this register.
    if serial_at.is_none() && email_at.is_none() {
        return Err(ImportError::NoSerialColumn {
            expected: SERIAL_COLUMNS.join(", "),
            found: headers.join(", "),
        });
    }

    let name_at = column_of(&headers, &NAME_COLUMNS);
    let unit_at = column_of(&headers, &UNIT_COLUMNS);
    let model_at = column_of(&headers, &MODEL_COLUMNS);
    let notes_at = column_of(&headers, &NOTES_COLUMNS);

    // A holder needs a unit: it reaches the `OU=` of the signing certificate
    // this tool puts on the key. Inventing one would mean issuing a certificate
    // that names a department the person is not in, so a file without the column
    // imports its keys and none of its people — said once, here.
    let (email_at, holders_skipped) = match (email_at, unit_at) {
        (Some(_), None) => (
            None,
            Some(format!(
                "no unit column, so no holder was imported — a holder's unit reaches the `OU=` of \
                 the signing certificate and cannot be guessed. Add a column named one of: {}",
                UNIT_COLUMNS.join(", ")
            )),
        ),
        (email_at, _) => (email_at, None),
    };

    let recognised: Vec<usize> = [serial_at, email_at, name_at, unit_at, model_at, notes_at]
        .into_iter()
        .flatten()
        .collect();
    let ignored_columns = headers
        .iter()
        .enumerate()
        .filter(|(i, name)| !recognised.contains(i) && !name.is_empty())
        .map(|(_, name)| name.clone())
        .collect();

    // Seen within this file, so a spreadsheet listing the same key twice reports
    // the second one as known rather than planning two inserts.
    let mut seen_serials = existing.serials.clone();
    let mut seen_emails = existing.emails.clone();

    let mut rows = Vec::new();
    for (index, line) in lines.enumerate() {
        let cells = split_row(line, separator);
        let cell = |at: Option<usize>| -> String {
            at.and_then(|i| cells.get(i)).cloned().unwrap_or_default()
        };

        let line_number = index + 2; // 1-based, and the header was line 1.
        rows.push(plan_row(
            line_number,
            PlannedCells {
                serial: serial_at.map(|_| cell(serial_at)),
                email: cell(email_at),
                name: cell(name_at),
                unit: cell(unit_at),
                model: cell(model_at),
                notes: cell(notes_at),
            },
            &mut seen_serials,
            &mut seen_emails,
        ));
    }

    Ok(ImportPlan {
        rows,
        ignored_columns,
        holders_skipped,
    })
}

struct PlannedCells {
    serial: Option<String>,
    email: String,
    name: String,
    unit: String,
    model: String,
    notes: String,
}

fn plan_row(
    line: usize,
    cells: PlannedCells,
    seen_serials: &mut std::collections::HashSet<u32>,
    seen_emails: &mut std::collections::HashSet<String>,
) -> PlannedRow {
    let refuse = |reason: String| PlannedRow {
        line,
        plan: RowPlan::Refused { reason },
        key: None,
        holder: None,
    };

    // The key half.
    let mut key = None;
    let mut key_plan = None;
    match cells.serial.as_deref() {
        Some(raw) if !raw.trim().is_empty() => match parse_serial(raw) {
            Ok(serial) => {
                let known = !seen_serials.insert(serial);
                let mut record = YubiKeyRecord::imported(serial, &cells.model);
                record.notes = cells.notes.clone();
                key = Some(record);
                key_plan = Some(if known {
                    RowPlan::KnownKey { serial }
                } else {
                    RowPlan::NewKey { serial }
                });
            }
            Err(reason) => return refuse(reason),
        },
        _ => {}
    }

    // The holder half. An e-mail is what identifies a person here, so a name
    // with no e-mail cannot become a holder — and saying so is more useful than
    // inventing `ana@` from `Ana`.
    let mut holder = None;
    let mut holder_plan = None;
    if !cells.email.trim().is_empty() {
        let name = if cells.name.trim().is_empty() {
            cells.email.clone()
        } else {
            cells.name.clone()
        };
        match Holder::new(&name, &cells.email, &cells.unit, "") {
            Ok(record) => {
                let email = record.email.clone();
                let known = !seen_emails.insert(email.clone());
                holder = Some(record);
                holder_plan = Some(if known {
                    RowPlan::KnownHolder { email }
                } else {
                    RowPlan::NewHolder { email }
                });
            }
            Err(e) => return refuse(format!("line {line}: {e}")),
        }
    } else if !cells.name.trim().is_empty() && key_plan.is_none() {
        return refuse(format!(
            "`{}` has no e-mail address, and no serial number either",
            cells.name
        ));
    }

    let plan = match (key_plan, holder_plan) {
        (Some(RowPlan::NewKey { serial }), Some(RowPlan::NewHolder { email })) => {
            RowPlan::NewKeyAndHolder { serial, email }
        }
        (Some(key), _) => key,
        (None, Some(holder)) => holder,
        (None, None) => return refuse("the row is empty".to_owned()),
    };

    PlannedRow {
        line,
        plan,
        key,
        holder,
    }
}

impl YubiKeyRecord {
    /// A key known only from a spreadsheet.
    ///
    /// Provenance is [`SerialSource::ManualEntry`] — the weakest the domain has,
    /// and the truthful one: nobody has read this key. Firmware and form factor
    /// are left empty rather than guessed, because a report that shows a
    /// firmware nobody verified is worse than one that shows a blank.
    pub fn imported(serial: u32, model: &str) -> Self {
        let mut record = Self::from_serial(serial, SerialSource::ManualEntry);
        if !model.trim().is_empty() {
            record.model = model.trim().to_owned();
        }
        record
    }
}

/// The counts an applied import reports, for the status line and the audit entry.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportOutcome {
    pub keys_added: usize,
    pub keys_refreshed: usize,
    pub holders_added: usize,
    pub holders_refreshed: usize,
    pub refused: usize,
}

impl ImportOutcome {
    /// Secret-free, one `key=value` per fact, per AGENTS.md §3.
    pub fn detail(&self) -> String {
        format!(
            "keys_added={} keys_refreshed={} holders_added={} holders_refreshed={} refused={}",
            self.keys_added,
            self.keys_refreshed,
            self.holders_added,
            self.holders_refreshed,
            self.refused
        )
    }
}

/// Column names recognised, for the operator-facing help on the import screen.
pub fn recognised_columns() -> BTreeMap<&'static str, Vec<&'static str>> {
    BTreeMap::from([
        ("serial", SERIAL_COLUMNS.to_vec()),
        ("name", NAME_COLUMNS.to_vec()),
        ("e-mail", EMAIL_COLUMNS.to_vec()),
        ("unit", UNIT_COLUMNS.to_vec()),
        ("model", MODEL_COLUMNS.to_vec()),
        ("notes", NOTES_COLUMNS.to_vec()),
    ])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn nothing() -> Existing {
        Existing::default()
    }

    #[test]
    fn a_comma_separated_export_is_read() {
        let csv = "Serial,Name,Email,Unit\n20423633,Ana Silva,ana@example.org,ESI\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(plan.rows.len(), 1);
        assert_eq!(
            plan.rows[0].plan,
            RowPlan::NewKeyAndHolder {
                serial: 20_423_633,
                email: "ana@example.org".into()
            }
        );
        assert_eq!(plan.new_keys(), 1);
        assert_eq!(plan.new_holders(), 1);
    }

    #[test]
    fn a_semicolon_export_from_a_decimal_comma_locale_is_read_too() {
        // This is what Excel writes in pt-BR, and getting it wrong would fail on
        // exactly the files this deployment produces.
        let csv = "Número de Série;Nome;E-mail;Unidade\n20423633;Ana Silva;ana@example.org;ESI\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(plan.refusal_count(), 0, "{:?}", plan.rows);
        assert_eq!(plan.new_keys(), 1);
        assert_eq!(plan.rows[0].holder.as_ref().unwrap().unit, "ESI");
    }

    #[test]
    fn accented_and_spaced_headers_match() {
        for header in [
            "Número de Série",
            "numero_de_serie",
            "SERIAL NUMBER",
            "Serial-No",
        ] {
            let csv = format!("{header}\n20423633\n");
            let plan = plan_text(&csv, &nothing()).unwrap();
            assert_eq!(
                plan.new_keys(),
                1,
                "header `{header}` should be recognised as the serial column"
            );
        }
    }

    #[test]
    fn a_quoted_cell_may_contain_the_separator() {
        let csv = "Serial,Name,Email,Unit\n20423633,\"Silva, Ana\",ana@example.org,ESI\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(
            plan.rows[0].holder.as_ref().unwrap().full_name,
            "Silva, Ana"
        );
    }

    #[test]
    fn a_file_with_no_unit_column_imports_its_keys_and_none_of_its_people() {
        // The unit reaches the `OU=` of a signing certificate. Guessing it would
        // issue a certificate naming a department the holder is not in, so the
        // people are left out and the operator is told once why.
        let csv = "Serial,Name,Email\n20423633,Ana Silva,ana@example.org\n";
        let plan = plan_text(csv, &nothing()).unwrap();

        assert_eq!(plan.new_keys(), 1);
        assert_eq!(plan.new_holders(), 0);
        assert_eq!(plan.refusal_count(), 0, "not a per-row refusal");
        let reason = plan.holders_skipped.expect("the operator must be told");
        assert!(
            reason.contains("unit"),
            "names the missing column: {reason}"
        );
        assert!(
            reason.contains("OU="),
            "and says why it cannot be guessed: {reason}"
        );
    }

    #[test]
    fn a_doubled_quote_is_one_literal_quote() {
        let csv = "Serial,Notes\n20423633,\"she said \"\"spare\"\"\"\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(
            plan.rows[0].key.as_ref().unwrap().notes,
            "she said \"spare\""
        );
    }

    #[test]
    fn a_serial_a_spreadsheet_decorated_is_still_read() {
        // A cell forced to text keeps a leading apostrophe; a numeric cell picks
        // up the locale's thousands separator.
        for raw in ["'20423633", "20.423.633", "20,423,633", " 20423633 "] {
            assert_eq!(parse_serial(raw), Ok(20_423_633), "raw: {raw}");
        }
    }

    #[test]
    fn a_serial_that_is_not_a_number_is_refused_naming_the_line() {
        let csv = "Serial,Name,Email\nABC123,Ana,ana@example.org\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(plan.refusal_count(), 1);
        let refusal = plan.refusals().next().unwrap();
        assert_eq!(refusal.line, 2, "the line number a spreadsheet shows");
        assert!(
            matches!(&refusal.plan, RowPlan::Refused { reason } if reason.contains("ABC123")),
            "the refusal must quote the cell: {:?}",
            refusal.plan
        );
    }

    #[test]
    fn an_imported_serial_is_manual_entry_provenance() {
        // Nobody has touched this key: it is a claim from a spreadsheet, and the
        // record has to say so or a mis-typed serial outranks a device read.
        let csv = "Serial\n20423633\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(
            plan.rows[0].key.as_ref().unwrap().serial_source,
            SerialSource::ManualEntry
        );
    }

    #[test]
    fn a_serial_already_in_the_register_is_a_refresh_not_a_conflict() {
        let existing = Existing {
            serials: std::collections::HashSet::from([20_423_633]),
            ..Default::default()
        };
        let csv = "Serial\n20423633\n";
        let plan = plan_text(csv, &existing).unwrap();
        assert_eq!(plan.rows[0].plan, RowPlan::KnownKey { serial: 20_423_633 });
        assert_eq!(plan.new_keys(), 0);
    }

    #[test]
    fn the_same_key_listed_twice_in_one_file_is_counted_once() {
        let csv = "Serial\n20423633\n20423633\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(plan.new_keys(), 1);
        assert_eq!(plan.rows[1].plan, RowPlan::KnownKey { serial: 20_423_633 });
    }

    #[test]
    fn a_person_with_no_email_cannot_become_a_holder() {
        // An e-mail is the natural key and reaches a certificate SAN. Inventing
        // one from a name would put the wrong address in a signing certificate.
        let csv = "Name,Unit\nAna Silva,ESI\n";
        let err = plan_text(csv, &nothing()).unwrap_err();
        assert!(matches!(err, ImportError::NoSerialColumn { .. }));

        let csv = "Serial,Name,Unit\n20423633,Ana Silva,ESI\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(
            plan.new_holders(),
            0,
            "the key imports, the person does not"
        );
        assert_eq!(plan.new_keys(), 1);
    }

    #[test]
    fn an_invalid_email_is_refused_rather_than_stored() {
        let csv = "Serial,Name,Email,Unit\n20423633,Ana,not-an-address,ESI\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(plan.refusal_count(), 1);
    }

    #[test]
    fn columns_that_are_not_understood_are_reported_not_silently_dropped() {
        let csv = "Serial,Cost Centre,Warranty\n20423633,C-12,2028\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(plan.ignored_columns, vec!["Cost Centre", "Warranty"]);
    }

    #[test]
    fn a_file_that_is_not_this_register_is_refused_with_the_columns_it_wanted() {
        let csv = "Product,Price\nWidget,9.99\n";
        let err = plan_text(csv, &nothing()).unwrap_err();
        let message = err.to_string();
        assert!(
            message.contains("serial"),
            "names what it wanted: {message}"
        );
        assert!(
            message.contains("Product"),
            "names what it found: {message}"
        );
    }

    #[test]
    fn an_empty_file_is_refused() {
        assert!(matches!(
            plan_text("", &nothing()).unwrap_err(),
            ImportError::Empty
        ));
    }

    #[test]
    fn a_utf8_bom_does_not_break_the_first_column_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("register.csv");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(b"Serial,Email\n20423633,ana@example.org\n");
        std::fs::write(&path, bytes).unwrap();

        let plan = plan(&path, &nothing()).unwrap();
        assert_eq!(plan.new_keys(), 1, "the BOM must not hide the header");
    }

    #[test]
    fn an_oversized_file_is_refused_before_it_is_read() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("huge.csv");
        std::fs::write(&path, vec![b'x'; (MAX_IMPORT_BYTES + 1) as usize]).unwrap();
        assert!(matches!(
            plan(&path, &nothing()).unwrap_err(),
            ImportError::TooLarge { .. }
        ));
    }

    #[test]
    fn the_summary_counts_what_the_operator_is_agreeing_to() {
        let csv = "Serial,Name,Email,Unit\n\
                   20423633,Ana Silva,ana@example.org,ESI\n\
                   20423634,Bruno Costa,bruno@example.org,ESI\n\
                   NOPE,Carla,carla@example.org,ESI\n";
        let plan = plan_text(csv, &nothing()).unwrap();
        assert_eq!(
            plan.summary(),
            "3 rows: 2 new keys, 2 new holders, 1 refused"
        );
    }
}
