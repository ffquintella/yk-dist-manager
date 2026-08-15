//! The (holder, key) list an assigned batch runs against
//! (`features/bulk-enrollment.md` phase 4).
//!
//! # The whole file, or none of it
//!
//! Every other import in this tool is a *preview*: `store::import` plans each row
//! separately, refuses the ones it cannot read, and imports the rest — which is
//! right for a spreadsheet somebody has been keeping by hand for three years.
//!
//! This one is the opposite, deliberately. A pairing list is validated **before
//! the first key is touched**, and one malformed address rejects the file. The
//! reason is what a half-import would mean here: eleven keys written with
//! certificates naming eleven people, and then a stop, with the operator holding
//! a box that is now part-configured and a list they have to reconcile by hand.
//! A file refused at the desk costs a minute; a batch abandoned at row 12 costs
//! an afternoon and leaves the register describing keys nobody has checked.
//!
//! So the errors are collected — *all* of them, not the first — and reported
//! together with their line numbers, because an operator fixing a spreadsheet
//! wants every bad row at once.

use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::domain::{ValidationError, validate_email};

/// One row of the list: which key goes to which person.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Pair {
    /// The key this holder is to get, when the list says which.
    ///
    /// Optional because both ways of working are real: a unit that has already
    /// allocated serials to people writes them down, and one that has a box and a
    /// list of names does not care which key goes to whom.
    pub serial: Option<u32>,
    /// The holder's address, normalised and validated.
    pub email: String,
    /// The holder on the register, when this address is already one.
    pub holder_id: Option<Uuid>,
    /// The line it came from, so a refusal can point at the spreadsheet.
    pub line: usize,
}

/// A row this file cannot be run from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub line: usize,
    pub reason: String,
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PairingError {
    #[error("the file is empty")]
    Empty,
    #[error(
        "no e-mail column: an assigned batch writes each holder's address into their \
         certificate, so the list must say whose key each one is. Add a column named one of: \
         {expected}. Found: {found}"
    )]
    NoEmailColumn { expected: String, found: String },
    #[error("nothing to enrol: the file has a header and no rows")]
    NoRows,
    /// The file was read and **nothing** was imported. The list is every problem
    /// in it, so one pass at the spreadsheet can fix all of them.
    #[error("{}", describe(.0))]
    Refused(Vec<Problem>),
}

fn describe(problems: &[Problem]) -> String {
    let mut out = format!(
        "{} row(s) cannot be run, so none of the file was loaded — a batch abandoned part-way \
         leaves keys configured for people the register cannot reconcile:",
        problems.len()
    );
    for problem in problems.iter().take(MAX_REPORTED) {
        out.push_str(&format!("\n  line {}: {}", problem.line, problem.reason));
    }
    if problems.len() > MAX_REPORTED {
        out.push_str(&format!("\n  … and {} more", problems.len() - MAX_REPORTED));
    }
    out
}

/// How many bad rows are quoted before the message stops listing them.
///
/// A file with 200 malformed rows is a file with the wrong columns, and printing
/// 200 lines into a refusal helps nobody.
const MAX_REPORTED: usize = 12;

/// Column headers that mean "the holder's address".
const EMAIL_COLUMNS: [&str; 6] = ["email", "e-mail", "mail", "endereco", "endereço", "correio"];
/// Column headers that mean "the key".
const SERIAL_COLUMNS: [&str; 5] = [
    "serial",
    "serial number",
    "serie",
    "série",
    "numero de serie",
];

/// Parse and validate a pairing list.
///
/// `known` maps a normalised address to the holder already on the register, so a
/// list of people who are already registered comes back linked. An address that
/// is *not* known is not an error: registering the holder is part of the batch.
pub fn parse(
    text: &str,
    known: &std::collections::BTreeMap<String, Uuid>,
) -> Result<Vec<Pair>, PairingError> {
    let mut lines = text.lines().filter(|line| !line.trim().is_empty());
    let header_line = lines.next().ok_or(PairingError::Empty)?;
    let separator = crate::store::import::detect_separator(header_line);
    let headers = crate::store::import::split_row(header_line, separator);

    let email_at =
        column_of(&headers, &EMAIL_COLUMNS).ok_or_else(|| PairingError::NoEmailColumn {
            expected: EMAIL_COLUMNS.join(", "),
            found: headers.join(", "),
        })?;
    let serial_at = column_of(&headers, &SERIAL_COLUMNS);

    let mut pairs = Vec::new();
    let mut problems = Vec::new();
    // Within the file, because the same person twice in one batch is a mistake
    // the spreadsheet made, and the same key twice is the mistake that would
    // write two certificates to one device.
    let mut seen_emails: std::collections::BTreeSet<String> = std::collections::BTreeSet::new();
    let mut seen_serials: std::collections::BTreeSet<u32> = std::collections::BTreeSet::new();

    for (index, line) in lines.enumerate() {
        let number = index + 2; // 1-based, and the header was line 1.
        let cells = crate::store::import::split_row(line, separator);
        let cell = |at: usize| cells.get(at).cloned().unwrap_or_default();

        let email = match validate_email(&cell(email_at)) {
            Ok(email) => email,
            Err(ValidationError::Missing(_)) => {
                problems.push(Problem {
                    line: number,
                    reason: "no e-mail address, and a certificate cannot be issued without one"
                        .into(),
                });
                continue;
            }
            Err(e) => {
                problems.push(Problem {
                    line: number,
                    reason: format!(
                        "{e} — this address reaches the certificate's rfc822Name, so it is \
                         checked here rather than at issuance"
                    ),
                });
                continue;
            }
        };
        if !seen_emails.insert(email.clone()) {
            problems.push(Problem {
                line: number,
                reason: format!("{email} appears more than once in this file"),
            });
            continue;
        }

        // An empty serial cell means "any key from the box", not zero: a unit
        // that has not allocated serials to people leaves the column blank.
        let raw_serial = serial_at
            .map(cell)
            .map(|raw| raw.trim().to_owned())
            .filter(|raw| !raw.is_empty());

        let serial = match raw_serial {
            None => None,
            Some(raw) => match raw.parse::<u32>() {
                Ok(serial) if serial > 0 => {
                    if !seen_serials.insert(serial) {
                        problems.push(Problem {
                            line: number,
                            reason: format!("serial {serial} appears more than once in this file"),
                        });
                        continue;
                    }
                    Some(serial)
                }
                _ => {
                    problems.push(Problem {
                        line: number,
                        reason: format!("`{raw}` is not a serial number"),
                    });
                    continue;
                }
            },
        };

        pairs.push(Pair {
            serial,
            holder_id: known.get(&email).copied(),
            email,
            line: number,
        });
    }

    if !problems.is_empty() {
        return Err(PairingError::Refused(problems));
    }
    if pairs.is_empty() {
        return Err(PairingError::NoRows);
    }
    Ok(pairs)
}

fn column_of(headers: &[String], names: &[&str]) -> Option<usize> {
    headers.iter().position(|header| {
        let header = header.trim().to_lowercase();
        names.iter().any(|name| header == *name)
    })
}

/// What the operator is told before the batch starts.
pub fn summarise(pairs: &[Pair]) -> String {
    let with_serial = pairs.iter().filter(|pair| pair.serial.is_some()).count();
    let known = pairs.iter().filter(|pair| pair.holder_id.is_some()).count();
    let mut parts = vec![format!("{} holder(s)", pairs.len())];
    if with_serial > 0 {
        parts.push(format!("{with_serial} with the key already chosen"));
    }
    parts.push(format!("{known} already on the register"));
    parts.join(", ")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn nobody() -> BTreeMap<String, Uuid> {
        BTreeMap::new()
    }

    #[test]
    fn a_list_of_addresses_is_enough() {
        let pairs = parse("email\nana@example.org\nbruno@example.org\n", &nobody()).unwrap();
        assert_eq!(pairs.len(), 2);
        assert_eq!(pairs[0].email, "ana@example.org");
        assert_eq!(pairs[0].serial, None);
        assert_eq!(pairs[0].line, 2);
        assert!(summarise(&pairs).contains("2 holder(s)"));
    }

    #[test]
    fn a_serial_column_binds_each_key_to_its_person() {
        let pairs = parse(
            "serial;email\n20423633;ana@example.org\n20423634;bruno@example.org\n",
            &nobody(),
        )
        .unwrap();
        assert_eq!(pairs[0].serial, Some(20_423_633));
        assert_eq!(pairs[1].serial, Some(20_423_634));
        assert!(summarise(&pairs).contains("2 with the key already chosen"));
    }

    #[test]
    fn one_malformed_address_rejects_the_whole_file() {
        // The rule this module exists for. Importing the good rows would leave a
        // box part-configured and a list to reconcile by hand.
        let error = parse(
            "email\nana@example.org\nnot-an-address\nbruno@example.org\n",
            &nobody(),
        )
        .unwrap_err();

        let PairingError::Refused(problems) = &error else {
            panic!("expected a refusal, got {error:?}");
        };
        assert_eq!(problems.len(), 1);
        assert_eq!(problems[0].line, 3);

        let said = error.to_string();
        assert!(said.contains("none of the file was loaded"), "{said}");
        assert!(said.contains("line 3"), "{said}");
        // And it says why that rule exists, because the operator is about to ask.
        assert!(said.contains("abandoned part-way"), "{said}");
    }

    #[test]
    fn every_bad_row_is_reported_at_once_not_just_the_first() {
        // An operator fixing a spreadsheet wants the whole list in one pass.
        let error = parse("email\nbad-one\nbad@@two\n\nbad three\n", &nobody()).unwrap_err();
        let PairingError::Refused(problems) = &error else {
            panic!("expected a refusal");
        };
        assert_eq!(problems.len(), 3);
        assert_eq!(
            problems.iter().map(|p| p.line).collect::<Vec<_>>(),
            vec![2, 3, 4],
            "blank lines are skipped without shifting the numbering of what follows"
        );
    }

    #[test]
    fn a_very_broken_file_stops_listing_rather_than_printing_two_hundred_lines() {
        let mut text = String::from("email\n");
        for _ in 0..200 {
            text.push_str("nope\n");
        }
        let said = parse(&text, &nobody()).unwrap_err().to_string();
        assert!(said.contains("200 row(s) cannot be run"), "{said}");
        assert!(said.contains("… and 188 more"), "{said}");
    }

    #[test]
    fn the_same_person_or_the_same_key_twice_is_refused() {
        let error = parse("email\nana@example.org\nAna@Example.org\n", &nobody()).unwrap_err();
        assert!(
            error.to_string().contains("more than once"),
            "{error}, and the comparison has to be case-insensitive"
        );

        let error = parse(
            "serial,email\n20423633,ana@example.org\n20423633,bruno@example.org\n",
            &nobody(),
        )
        .unwrap_err();
        assert!(error.to_string().contains("serial 20423633"), "{error}");
    }

    #[test]
    fn a_file_with_no_email_column_names_what_it_needed_and_what_it_found() {
        let error = parse("serial;name\n1;Ana\n", &nobody()).unwrap_err();
        let said = error.to_string();
        assert!(said.contains("email"), "{said}");
        assert!(said.contains("serial"), "{said}");
        assert!(said.contains("certificate"), "{said}");
    }

    #[test]
    fn an_empty_file_and_a_header_with_no_rows_are_different_refusals() {
        assert_eq!(parse("", &nobody()).unwrap_err(), PairingError::Empty);
        assert_eq!(
            parse("email\n", &nobody()).unwrap_err(),
            PairingError::NoRows
        );
    }

    #[test]
    fn a_holder_already_on_the_register_comes_back_linked() {
        let id = Uuid::new_v4();
        let mut known = BTreeMap::new();
        known.insert("ana@example.org".to_owned(), id);

        let pairs = parse("email\nAna@example.org\nbruno@example.org\n", &known).unwrap();
        assert_eq!(pairs[0].holder_id, Some(id), "matched after normalisation");
        assert_eq!(
            pairs[1].holder_id, None,
            "registering them is part of the batch"
        );
        assert!(summarise(&pairs).contains("1 already on the register"));
    }

    #[test]
    fn a_serial_that_is_not_a_number_is_a_refusal_not_a_guess() {
        let error = parse("serial,email\nTWO,ana@example.org\n", &nobody()).unwrap_err();
        assert!(
            error.to_string().contains("`TWO` is not a serial"),
            "{error}"
        );

        // And an empty serial cell means "any key", not zero.
        let pairs = parse("serial,email\n,ana@example.org\n", &nobody()).unwrap();
        assert_eq!(pairs[0].serial, None);
    }
}
