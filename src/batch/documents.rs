//! Which hand-over documents a batch owes, and what each one is called
//! (`features/bulk-enrollment.md` phase 7, `features/receipts-and-terms.md` phase 7).
//!
//! A batch of fifty keys ends with fifty terms to print, and generating them one
//! screen at a time is the same repetitive-work failure the batch exists to fix —
//! the fiftieth term is the one nobody checks was generated at all. So the set is
//! produced in one action, and this module is the part of it that can be reasoned
//! about without a filesystem: *which* positions owe a document, which do not and
//! why, and what each file is called.
//!
//! Writing them is [`crate::app::YkDistApp::generate_batch_terms`], which renders
//! each one through exactly the same `term::render_term_pdf` as the single
//! hand-over. A batch is not a second way of producing a term, for the same reason
//! it is not a second way of writing to a key: the second way is the one that
//! drifts.

use chrono::{DateTime, Utc};
use uuid::Uuid;

use super::{Batch, EntryState, Shape};

/// One document the batch can produce.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Planned {
    pub position: usize,
    pub serial: u32,
    pub holder_id: Uuid,
    /// The holder as the operator reads them, for the summary on screen.
    pub holder_display: String,
}

impl Planned {
    /// `termo-20423633.pdf`.
    ///
    /// The serial and nothing else, which is the same name the single hand-over
    /// suggests. Deliberately **not** the holder's name: the term inside carries
    /// it because the document is a consignment to a person, but a directory
    /// listing is read by whoever can see the folder, and a folder of fifty file
    /// names is a staff list nobody decided to publish. The serial identifies the
    /// file, and the register says whose it is.
    pub fn file_name(&self, extension: &str) -> String {
        format!("termo-{}.{extension}", self.serial)
    }
}

/// A position that gets no document, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Skipped {
    pub position: usize,
    pub reason: Reason,
    /// `serial 20423633` or `position 4` — a position with no key still has to be
    /// nameable in the summary.
    pub describe: String,
}

/// Why a position produced no document.
///
/// Each is a state a real batch reaches, and none of them is an error: a term for
/// a key that was never written would say a procedure was applied that was not.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reason {
    /// The key was never bootstrapped: pending, failed, or passed over.
    NotDone,
    /// An assigned position with no holder, which the pairing list should have
    /// made impossible — carried rather than assumed away, because a term with no
    /// name on it is worse than a position reported as needing attention.
    NoHolder,
}

impl Reason {
    pub fn label(&self) -> &'static str {
        match self {
            Reason::NotDone => "no run finished for this key",
            Reason::NoHolder => "no holder on this position",
        }
    }
}

/// What a batch owes, and what it does not.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Plan {
    pub planned: Vec<Planned>,
    pub skipped: Vec<Skipped>,
}

impl Plan {
    pub fn is_empty(&self) -> bool {
        self.planned.is_empty()
    }

    /// `12 document(s); 3 position(s) skipped`, for the status line.
    pub fn describe(&self) -> String {
        let documents = format!("{} document(s)", self.planned.len());
        if self.skipped.is_empty() {
            documents
        } else {
            format!("{documents}; {} position(s) skipped", self.skipped.len())
        }
    }
}

/// Why a whole batch can produce nothing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refusal {
    /// Stock preparation has no holders by definition, and a consignment term is
    /// a document one *person* signs. Refused rather than rendered with the name
    /// left blank: a term with an empty holder is a form, and a form that comes
    /// out of the register looks like a record.
    StockBatch,
}

impl Refusal {
    pub fn message(&self) -> &'static str {
        match self {
            Refusal::StockBatch => {
                "a stock-preparation batch has no holders, so it has no consignment terms — \
                 hand the keys out first, then generate each term from the Distribution screen"
            }
        }
    }
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message())
    }
}

/// The documents this batch owes.
///
/// An **unfinished** batch is planned rather than refused. Half a box done at the
/// end of an afternoon is the ordinary case, and the terms for the keys that are
/// ready are wanted then rather than after the rest — the skipped list is what
/// keeps that honest, by naming every position that got nothing.
pub fn plan(batch: &Batch) -> Result<Plan, Refusal> {
    if batch.shape == Shape::StockPreparation {
        return Err(Refusal::StockBatch);
    }

    let mut plan = Plan::default();
    for entry in &batch.entries {
        let skip = |reason| Skipped {
            position: entry.position,
            reason,
            describe: entry.describe(),
        };

        if entry.state != EntryState::Done {
            plan.skipped.push(skip(Reason::NotDone));
            continue;
        }
        // A Done entry always has a serial — `present` is what sets one, and
        // nothing reaches Done without it — but the type says `Option`, and
        // reading it as "no run finished" is the safe reading of a shape that
        // should not occur.
        let Some(serial) = entry.serial else {
            plan.skipped.push(skip(Reason::NotDone));
            continue;
        };
        let Some(holder_id) = entry.holder_id else {
            plan.skipped.push(skip(Reason::NoHolder));
            continue;
        };

        plan.planned.push(Planned {
            position: entry.position,
            serial,
            holder_id,
            holder_display: entry.holder_display.clone(),
        });
    }

    Ok(plan)
}

/// `termos-2026-08-17-1a2b3c4d`.
///
/// Dated and carrying the batch, because a unit that runs two boxes in a week
/// otherwise gets one folder that the second run writes into — and a term
/// overwritten by a term for a different key is not something a directory listing
/// shows.
pub fn directory_name(batch: &Batch, now: DateTime<Utc>) -> String {
    let id = batch.id.simple().to_string();
    format!("termos-{}-{}", now.format("%Y-%m-%d"), &id[..8])
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::batch::{Outcome, pairing::Pair};

    fn assigned(count: usize) -> Batch {
        let pairs: Vec<Pair> = (0..count)
            .map(|i| Pair {
                serial: None,
                email: format!("person.{i}@example.org"),
                holder_id: Some(Uuid::new_v4()),
                line: i + 2,
            })
            .collect();
        Batch::assigned("org-standard", "2", "operator", &pairs)
    }

    fn complete(batch: &mut Batch, position: usize, serial: u32) {
        batch.present(serial);
        batch.record(
            position,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );
    }

    #[test]
    fn a_stock_batch_is_refused_rather_than_rendered_without_a_name() {
        let batch = Batch::stock("org-standard", "2", "operator", 5);

        let refusal = plan(&batch).expect_err("a stock batch has no holders");

        assert_eq!(refusal, Refusal::StockBatch);
        assert!(refusal.message().contains("no holders"));
    }

    #[test]
    fn only_the_keys_that_finished_owe_a_document() {
        let mut batch = assigned(3);
        complete(&mut batch, 0, 20_423_633);
        // Position 1 fails, position 2 is never presented.
        batch.present(20_423_634);
        batch.record(
            1,
            Outcome::Failed {
                run: None,
                reason: "the PIV applet refused".into(),
            },
        );

        let plan = plan(&batch).expect("an assigned batch plans");

        assert_eq!(plan.planned.len(), 1);
        assert_eq!(plan.planned[0].serial, 20_423_633);
        assert_eq!(plan.skipped.len(), 2);
        assert!(plan.skipped.iter().all(|s| s.reason == Reason::NotDone));
    }

    #[test]
    fn a_skipped_position_is_nameable_even_without_a_key() {
        let batch = assigned(2);

        let plan = plan(&batch).expect("an assigned batch plans");

        assert_eq!(plan.skipped[0].describe, "position 1");
        assert_eq!(plan.skipped[1].describe, "position 2");
    }

    #[test]
    fn the_file_name_carries_the_serial_and_not_the_holder() {
        let mut batch = assigned(1);
        complete(&mut batch, 0, 20_423_633);

        let plan = plan(&batch).expect("an assigned batch plans");
        let name = plan.planned[0].file_name("pdf");

        assert_eq!(name, "termo-20423633.pdf");
        assert!(
            !name.contains("person"),
            "a directory listing is not a staff list: {name}"
        );
    }

    #[test]
    fn an_unfinished_batch_still_produces_what_it_can() {
        let mut batch = assigned(4);
        complete(&mut batch, 0, 20_423_633);
        complete(&mut batch, 1, 20_423_634);

        let plan = plan(&batch).expect("an assigned batch plans");

        assert!(!batch.is_complete(), "the batch is deliberately half done");
        assert_eq!(plan.planned.len(), 2);
        assert_eq!(plan.describe(), "2 document(s); 2 position(s) skipped");
    }

    #[test]
    fn a_finished_batch_says_nothing_about_skipping() {
        let mut batch = assigned(2);
        complete(&mut batch, 0, 20_423_633);
        complete(&mut batch, 1, 20_423_634);

        let plan = plan(&batch).expect("an assigned batch plans");

        assert_eq!(plan.describe(), "2 document(s)");
        assert!(!plan.is_empty());
    }

    #[test]
    fn two_batches_on_one_day_do_not_share_a_folder() {
        let first = assigned(1);
        let second = assigned(1);
        let now = chrono::Utc::now();

        assert_ne!(directory_name(&first, now), directory_name(&second, now));
        assert!(directory_name(&first, now).starts_with("termos-"));
    }
}
