//! Bootstrapping a box of keys in one sitting (`features/bulk-enrollment.md`).
//!
//! # Two shapes, and the distinction is the whole design
//!
//! **Stock preparation** writes what a key needs before anybody owns it: PIN
//! policies, a management key, OTP protection. No holder, no certificate, no
//! personal binding — so it can be a fast loop, and the only bookkeeping that
//! matters is *which key am I holding and has it been done*.
//!
//! **Assigned enrolment** binds each key to a person: the certificate carries
//! that holder's e-mail, so the list has to exist before the first write and
//! every address has to be valid before the first key is touched. It is paced by
//! people, and it cannot be unattended when the holder sets their own PIN.
//!
//! Conflating them produces a batch mode that is wrong for both — one that asks
//! for a holder list to prepare stock, or one that lets an operator get eleven
//! keys into a run before discovering that row 12's address is malformed.
//!
//! # What a batch is for
//!
//! Not speed. Fifty runs through the wizard would work; what they would not do is
//! keep count. The two failure modes of repetitive work are losing track of which
//! key was already done and ceasing to read the confirmation — and the first is
//! the one a tool can fix, by holding the counting itself. So the batch persists
//! as it goes rather than at the end, refuses a serial it has already seen, and
//! carries on past a failure with the key marked for attention.
//!
//! # What it deliberately does not do
//!
//! It does not weaken a single run. Every key still goes through the executor
//! with its own plan, its own confirmation and its own audit entries: a batch is
//! not an excuse for a coarser trail, and `batch.key.done` sits *beside*
//! `bootstrap.finished` rather than instead of it.

pub mod documents;
pub mod pairing;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// What kind of batch this is.
///
/// Stock preparation is the default because it is the one that is safe to start
/// without a list: nothing personal is written, so an operator who opens the
/// screen and presses on has prepared stock rather than bound a key to whoever
/// happened to be first in a stale spreadsheet.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Shape {
    /// Keys with no holder yet, prepared for the drawer.
    #[default]
    StockPreparation,
    /// A list of (holder, key) pairs, each key carrying that holder's address.
    AssignedEnrolment,
}

impl Shape {
    pub const ALL: [Shape; 2] = [Shape::StockPreparation, Shape::AssignedEnrolment];

    pub fn label(&self) -> &'static str {
        match self {
            Shape::StockPreparation => "Stock preparation",
            Shape::AssignedEnrolment => "Assigned enrolment",
        }
    }

    /// Stable name for the database column and the audit detail.
    pub fn as_str(&self) -> &'static str {
        match self {
            Shape::StockPreparation => "stock",
            Shape::AssignedEnrolment => "assigned",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|shape| shape.as_str() == raw.trim())
    }

    /// Does this shape need the holder list before the first key is touched?
    pub fn needs_pairing_list(&self) -> bool {
        matches!(self, Shape::AssignedEnrolment)
    }

    /// The sentence that says what this batch will do.
    pub fn describe(&self) -> &'static str {
        match self {
            Shape::StockPreparation => {
                "Keys are prepared with no holder and no certificate, and end as Bootstrapped, \
                 ready to be assigned. Nothing personal is written, so the loop can move as fast \
                 as the keys can be swapped."
            }
            Shape::AssignedEnrolment => {
                "Each key is bound to one person: the certificate carries their e-mail. The whole \
                 pairing list is validated before the first key is touched, and the pace is set \
                 by the people, not the tool."
            }
        }
    }
}

/// Where one key in the batch stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum EntryState {
    /// Not attempted yet.
    Pending,
    /// A run completed against this key.
    Done,
    /// A run failed. The batch carried on.
    Failed,
    /// Deliberately passed over — the key was not available, or the operator
    /// skipped it.
    Skipped,
}

impl EntryState {
    pub fn as_str(&self) -> &'static str {
        match self {
            EntryState::Pending => "pending",
            EntryState::Done => "done",
            EntryState::Failed => "failed",
            EntryState::Skipped => "skipped",
        }
    }

    pub fn parse(raw: &str) -> Option<Self> {
        [
            EntryState::Pending,
            EntryState::Done,
            EntryState::Failed,
            EntryState::Skipped,
        ]
        .into_iter()
        .find(|state| state.as_str() == raw.trim())
    }

    pub fn label(&self) -> &'static str {
        match self {
            EntryState::Pending => "waiting",
            EntryState::Done => "done",
            EntryState::Failed => "needs attention",
            EntryState::Skipped => "skipped",
        }
    }

    /// Has this entry been dealt with, one way or another?
    pub fn is_settled(&self) -> bool {
        !matches!(self, EntryState::Pending)
    }
}

/// One key's place in the batch.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Entry {
    /// Position in the batch, from 0. Stable: it is how a resumed batch lines up
    /// with what was written.
    pub position: usize,
    /// The key, once one has been presented for this position.
    pub serial: Option<u32>,
    /// The person this key is for. Always `None` for stock preparation.
    pub holder_id: Option<Uuid>,
    /// The holder as the operator reads them, so a list means something before
    /// the holders are loaded.
    pub holder_display: String,
    /// The run this position produced, when it produced one.
    pub run_id: Option<Uuid>,
    pub state: EntryState,
    /// Why it failed, or why it was skipped. Secret-free: it is a transport
    /// error or an operator's reason, never a value.
    pub detail: String,
}

impl Entry {
    fn waiting(position: usize) -> Self {
        Self {
            position,
            serial: None,
            holder_id: None,
            holder_display: String::new(),
            run_id: None,
            state: EntryState::Pending,
            detail: String::new(),
        }
    }

    /// Is somebody expected to do something about this one?
    pub fn needs_attention(&self) -> bool {
        self.state == EntryState::Failed
    }

    /// `serial 20423633` or `position 4`, for a message about an entry that may
    /// not have a key yet.
    pub fn describe(&self) -> String {
        match self.serial {
            Some(serial) => format!("serial {serial}"),
            None => format!("position {}", self.position + 1),
        }
    }
}

/// What happened when a key was presented.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Presented {
    /// Bound to this position; the run can start.
    Ready { position: usize },
    /// This serial is already in the batch. The commonest real mistake — the
    /// same key inserted twice — and the one a fast loop cannot notice.
    Duplicate { position: usize, state: EntryState },
    /// An assigned batch with nothing left to pair this key with.
    Full,
}

impl Presented {
    /// The sentence the operator reads. A refusal has to say which position the
    /// key is already at, or "already done" is a dead end.
    pub fn describe(&self, serial: u32) -> String {
        match self {
            Presented::Ready { position } => {
                format!("serial {serial} is key {} of this batch", position + 1)
            }
            Presented::Duplicate { position, state } => format!(
                "serial {serial} is already key {} of this batch ({}). Take it out and insert the \
                 next one — running it twice would apply the procedure to a key that has already \
                 been through it",
                position + 1,
                state.label()
            ),
            Presented::Full => format!(
                "every position in this batch already has a key, so there is nowhere to put \
                 serial {serial}. Finish the batch, or start another one"
            ),
        }
    }

    pub fn is_ready(&self) -> bool {
        matches!(self, Presented::Ready { .. })
    }
}

/// How a batch stands.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Tally {
    pub done: usize,
    pub failed: usize,
    pub skipped: usize,
    pub pending: usize,
}

impl Tally {
    pub fn total(&self) -> usize {
        self.done + self.failed + self.skipped + self.pending
    }

    pub fn settled(&self) -> usize {
        self.done + self.failed + self.skipped
    }

    /// `succeeded=8 failed=1 skipped=0` — the `batch.finished` detail.
    pub fn audit_detail(&self) -> String {
        format!(
            "succeeded={} failed={} skipped={}",
            self.done, self.failed, self.skipped
        )
    }

    /// One line for the screen.
    pub fn describe(&self) -> String {
        let mut parts = vec![format!("{} of {} done", self.done, self.total())];
        if self.failed > 0 {
            parts.push(format!("{} needing attention", self.failed));
        }
        if self.skipped > 0 {
            parts.push(format!("{} skipped", self.skipped));
        }
        parts.join(", ")
    }
}

/// A batch of keys against one template.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Batch {
    pub id: Uuid,
    pub shape: Shape,
    pub template_id: String,
    pub template_version: String,
    pub operator: String,
    pub started_at: DateTime<Utc>,
    pub finished_at: Option<DateTime<Utc>>,
    pub entries: Vec<Entry>,
    pub notes: String,
}

impl Batch {
    /// A stock batch of `planned` keys.
    ///
    /// The count is what the operator says is in the box, and it is a *target*
    /// rather than a limit: positions are created up front so progress can be
    /// read as "8 of 50", and [`Batch::present`] appends beyond it rather than
    /// refusing, because a box with an extra key in it is not an error worth
    /// stopping for.
    pub fn stock(
        template_id: &str,
        template_version: &str,
        operator: &str,
        planned: usize,
    ) -> Self {
        Self {
            id: Uuid::new_v4(),
            shape: Shape::StockPreparation,
            template_id: template_id.to_owned(),
            template_version: template_version.to_owned(),
            operator: operator.trim().to_owned(),
            started_at: Utc::now(),
            finished_at: None,
            entries: (0..planned.max(1)).map(Entry::waiting).collect(),
            notes: String::new(),
        }
    }

    /// An assigned batch, one position per pairing, in the order the list gave
    /// them.
    pub fn assigned(
        template_id: &str,
        template_version: &str,
        operator: &str,
        pairs: &[pairing::Pair],
    ) -> Self {
        let entries = pairs
            .iter()
            .enumerate()
            .map(|(position, pair)| Entry {
                position,
                serial: pair.serial,
                holder_id: pair.holder_id,
                holder_display: pair.email.clone(),
                run_id: None,
                state: EntryState::Pending,
                detail: String::new(),
            })
            .collect();

        Self {
            id: Uuid::new_v4(),
            shape: Shape::AssignedEnrolment,
            template_id: template_id.to_owned(),
            template_version: template_version.to_owned(),
            operator: operator.trim().to_owned(),
            started_at: Utc::now(),
            finished_at: None,
            entries,
            notes: String::new(),
        }
    }

    /// `batch=<id> template=<id>@<version> shape=stock count=<n>`
    pub fn audit_detail(&self) -> String {
        format!(
            "batch={} template={}@{} shape={} count={}",
            self.id,
            self.template_id,
            self.template_version,
            self.shape.as_str(),
            self.entries.len()
        )
    }

    pub fn tally(&self) -> Tally {
        let mut tally = Tally::default();
        for entry in &self.entries {
            match entry.state {
                EntryState::Done => tally.done += 1,
                EntryState::Failed => tally.failed += 1,
                EntryState::Skipped => tally.skipped += 1,
                EntryState::Pending => tally.pending += 1,
            }
        }
        tally
    }

    /// Every position still waiting.
    pub fn pending(&self) -> impl Iterator<Item = &Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.state == EntryState::Pending)
    }

    /// The next position to work on, which is where a resumed batch continues.
    pub fn next_pending(&self) -> Option<&Entry> {
        self.pending().next()
    }

    /// The list somebody has to work through afterwards.
    pub fn needs_attention(&self) -> Vec<&Entry> {
        self.entries
            .iter()
            .filter(|entry| entry.needs_attention())
            .collect()
    }

    /// Nothing left waiting.
    pub fn is_complete(&self) -> bool {
        self.tally().pending == 0
    }

    /// Present a key to the batch.
    ///
    /// The duplicate check is against **every** position, settled or not: a key
    /// already done is the case worth catching, and one that failed is worth
    /// catching too, because re-running it blindly is how a half-configured key
    /// gets a second, conflicting attempt.
    pub fn present(&mut self, serial: u32) -> Presented {
        if let Some(existing) = self
            .entries
            .iter()
            .find(|entry| entry.serial == Some(serial))
        {
            // An assigned batch may legitimately have the serial *listed* against
            // a position that has not run yet: the list said which key goes to
            // whom. That is not a duplicate, it is the plan.
            if existing.state == EntryState::Pending {
                return Presented::Ready {
                    position: existing.position,
                };
            }
            return Presented::Duplicate {
                position: existing.position,
                state: existing.state,
            };
        }

        // The first position with no key on it.
        let free = self
            .entries
            .iter()
            .position(|entry| entry.state == EntryState::Pending && entry.serial.is_none());

        match (free, self.shape) {
            (Some(position), _) => {
                self.entries[position].serial = Some(serial);
                Presented::Ready { position }
            }
            // Stock: the box had one more key in it than the operator counted,
            // which is not worth refusing.
            (None, Shape::StockPreparation) => {
                let position = self.entries.len();
                let mut entry = Entry::waiting(position);
                entry.serial = Some(serial);
                self.entries.push(entry);
                Presented::Ready { position }
            }
            // Assigned: there is nobody left for this key to belong to, and
            // inventing a holder is the one thing this shape must never do.
            (None, Shape::AssignedEnrolment) => Presented::Full,
        }
    }

    /// Record what a run did to one position.
    pub fn record(&mut self, position: usize, outcome: Outcome) {
        let Some(entry) = self.entries.get_mut(position) else {
            return;
        };
        match outcome {
            Outcome::Done { run } => {
                entry.state = EntryState::Done;
                entry.run_id = Some(run);
                entry.detail.clear();
            }
            Outcome::Failed { run, reason } => {
                entry.state = EntryState::Failed;
                entry.run_id = run;
                entry.detail = reason;
            }
            Outcome::Skipped { reason } => {
                entry.state = EntryState::Skipped;
                entry.detail = reason;
            }
        }
        if self.is_complete() && self.finished_at.is_none() {
            self.finished_at = Some(Utc::now());
        }
    }

    /// The audit detail for one key's outcome.
    pub fn key_audit_detail(&self, position: usize) -> String {
        let Some(entry) = self.entries.get(position) else {
            return format!("batch={} position={position}", self.id);
        };
        let mut detail = format!("batch={} serial=", self.id);
        match entry.serial {
            Some(serial) => detail.push_str(&serial.to_string()),
            None => detail.push_str("(none)"),
        }
        if let Some(run) = entry.run_id {
            detail.push_str(&format!(" run={run}"));
        }
        if !entry.detail.trim().is_empty() {
            // The reason is folded onto one line and left as the transport wrote
            // it: it is an error message, never a value.
            detail.push_str(&format!(
                " reason={}",
                entry
                    .detail
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ")
            ));
        }
        detail
    }

    /// `batch=<id> from_key=<n>` — what a resumed batch says it is doing.
    pub fn resume_audit_detail(&self) -> String {
        format!(
            "batch={} from_key={}",
            self.id,
            self.next_pending().map(|e| e.position + 1).unwrap_or(0)
        )
    }

    /// Every serial this batch has touched or been told about.
    pub fn serials(&self) -> Vec<u32> {
        self.entries
            .iter()
            .filter_map(|entry| entry.serial)
            .collect()
    }

    /// The runs this batch produced, for the evidence export.
    pub fn runs(&self) -> Vec<Uuid> {
        self.entries
            .iter()
            .filter_map(|entry| entry.run_id)
            .collect()
    }
}

/// What a run did to one position.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Done {
        run: Uuid,
    },
    /// A run that failed. `run` is `Some` when one was recorded before it did —
    /// which is the usual case, and is what makes the failure investigable.
    Failed {
        run: Option<Uuid>,
        reason: String,
    },
    Skipped {
        reason: String,
    },
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stock(planned: usize) -> Batch {
        Batch::stock("org-standard", "2", "felipe", planned)
    }

    #[test]
    fn a_stock_batch_binds_each_key_to_the_next_free_position() {
        let mut batch = stock(3);
        assert_eq!(batch.tally().pending, 3);

        assert_eq!(batch.present(20_423_633), Presented::Ready { position: 0 });
        assert_eq!(batch.present(20_423_634), Presented::Ready { position: 1 });
        assert_eq!(batch.serials(), vec![20_423_633, 20_423_634]);
        assert_eq!(batch.next_pending().map(|e| e.position), Some(0));
    }

    #[test]
    fn the_same_key_inserted_twice_is_refused_and_says_where_it_already_is() {
        // The mistake a fast loop cannot notice, and the reason a batch keeps the
        // counting rather than the operator.
        let mut batch = stock(3);
        batch.present(20_423_633);
        batch.record(
            0,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );

        let again = batch.present(20_423_633);
        assert_eq!(
            again,
            Presented::Duplicate {
                position: 0,
                state: EntryState::Done
            }
        );
        let said = again.describe(20_423_633);
        assert!(said.contains("key 1"), "{said}");
        assert!(said.contains("already been through it"), "{said}");
        assert!(!again.is_ready());

        // And it was not bound anywhere else.
        assert_eq!(batch.serials(), vec![20_423_633]);
    }

    #[test]
    fn a_key_that_failed_is_also_refused_a_second_time() {
        // Re-running a half-configured key blindly is how it gets a second,
        // conflicting attempt.
        let mut batch = stock(2);
        batch.present(20_423_633);
        batch.record(
            0,
            Outcome::Failed {
                run: None,
                reason: "the key was pulled out mid-run".into(),
            },
        );
        assert!(matches!(
            batch.present(20_423_633),
            Presented::Duplicate {
                state: EntryState::Failed,
                ..
            }
        ));
    }

    #[test]
    fn a_failure_does_not_stop_the_batch_and_leaves_the_key_on_a_list() {
        // "a batch must not stop dead on key 7 of 50"
        let mut batch = stock(3);
        batch.present(1);
        batch.record(
            0,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );
        batch.present(2);
        batch.record(
            1,
            Outcome::Failed {
                run: Some(Uuid::new_v4()),
                reason: "PIV applet did not answer".into(),
            },
        );
        batch.present(3);
        batch.record(
            2,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );

        let tally = batch.tally();
        assert_eq!((tally.done, tally.failed, tally.pending), (2, 1, 0));
        assert_eq!(tally.audit_detail(), "succeeded=2 failed=1 skipped=0");

        let attention = batch.needs_attention();
        assert_eq!(attention.len(), 1);
        assert_eq!(attention[0].serial, Some(2));
        assert!(attention[0].detail.contains("PIV applet"));

        assert!(batch.is_complete());
        assert!(batch.finished_at.is_some());
    }

    #[test]
    fn an_interrupted_batch_resumes_at_the_next_unprocessed_key() {
        let mut batch = stock(5);
        batch.present(1);
        batch.record(
            0,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );
        batch.present(2);
        batch.record(
            1,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );

        // Reopened: positions 0 and 1 are settled, so work continues at 2 and the
        // two done keys are not offered again.
        assert_eq!(batch.next_pending().map(|e| e.position), Some(2));
        assert_eq!(
            batch.resume_audit_detail().split(' ').next_back(),
            Some("from_key=3")
        );
        assert_eq!(batch.tally().done, 2);
        assert!(!batch.is_complete());
    }

    #[test]
    fn a_box_with_one_more_key_than_expected_is_not_an_error() {
        let mut batch = stock(1);
        batch.present(1);
        batch.record(
            0,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );
        // The count was a target, not a limit.
        assert_eq!(batch.present(2), Presented::Ready { position: 1 });
        assert_eq!(batch.entries.len(), 2);
    }

    #[test]
    fn an_assigned_batch_keeps_the_list_and_refuses_a_key_with_nobody_left() {
        let pairs = vec![
            pairing::Pair {
                serial: None,
                email: "ana@example.org".into(),
                holder_id: None,
                line: 2,
            },
            pairing::Pair {
                serial: Some(20_423_634),
                email: "bruno@example.org".into(),
                holder_id: None,
                line: 3,
            },
        ];
        let mut batch = Batch::assigned("org-standard", "2", "felipe", &pairs);
        assert_eq!(batch.entries.len(), 2);
        assert_eq!(batch.entries[1].serial, Some(20_423_634));

        // A serial the list already names is the plan, not a duplicate.
        assert_eq!(batch.present(20_423_634), Presented::Ready { position: 1 });

        // The unpaired position takes the next key presented.
        assert_eq!(batch.present(20_423_633), Presented::Ready { position: 0 });

        batch.record(
            0,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );
        batch.record(
            1,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );

        // And a third key has nobody to belong to. Inventing a holder is the one
        // thing this shape must never do.
        let extra = batch.present(31_000_001);
        assert_eq!(extra, Presented::Full);
        assert!(extra.describe(31_000_001).contains("nowhere to put"));
    }

    #[test]
    fn the_audit_details_name_the_batch_the_key_and_the_run() {
        let mut batch = stock(2);
        batch.present(20_423_633);
        let run = Uuid::new_v4();
        batch.record(0, Outcome::Done { run });

        let started = batch.audit_detail();
        assert!(started.contains("template=org-standard@2"), "{started}");
        assert!(started.contains("shape=stock"), "{started}");
        assert!(started.contains("count=2"), "{started}");

        let key = batch.key_audit_detail(0);
        assert!(key.contains("serial=20423633"), "{key}");
        assert!(key.contains(&format!("run={run}")), "{key}");

        // A failure carries its reason, folded to one line so an audit entry stays
        // one line.
        batch.present(20_423_634);
        batch.record(
            1,
            Outcome::Failed {
                run: None,
                reason: "the key\nwas pulled out".into(),
            },
        );
        let failed = batch.key_audit_detail(1);
        assert!(failed.contains("reason=the key was pulled out"), "{failed}");
    }

    #[test]
    fn a_skipped_key_settles_without_counting_as_done_or_failed() {
        let mut batch = stock(2);
        batch.present(1);
        batch.record(
            0,
            Outcome::Skipped {
                reason: "not in the box".into(),
            },
        );
        let tally = batch.tally();
        assert_eq!((tally.done, tally.failed, tally.skipped), (0, 0, 1));
        assert!(
            batch.needs_attention().is_empty(),
            "skipped is not a failure"
        );
    }

    #[test]
    fn every_shape_and_state_has_its_own_words_and_round_trips() {
        for shape in Shape::ALL {
            assert_eq!(Shape::parse(shape.as_str()), Some(shape));
            assert!(!shape.describe().trim().is_empty());
        }
        assert_eq!(Shape::parse("nonsense"), None);
        assert!(Shape::AssignedEnrolment.needs_pairing_list());
        assert!(!Shape::StockPreparation.needs_pairing_list());

        for state in [
            EntryState::Pending,
            EntryState::Done,
            EntryState::Failed,
            EntryState::Skipped,
        ] {
            assert_eq!(EntryState::parse(state.as_str()), Some(state));
        }
        assert!(!EntryState::Pending.is_settled());
        assert!(EntryState::Skipped.is_settled());
    }

    #[test]
    fn the_tally_reads_as_progress_rather_than_as_four_numbers() {
        let mut batch = stock(3);
        batch.present(1);
        batch.record(
            0,
            Outcome::Done {
                run: Uuid::new_v4(),
            },
        );
        let line = batch.tally().describe();
        assert_eq!(line, "1 of 3 done");

        batch.present(2);
        batch.record(
            1,
            Outcome::Failed {
                run: None,
                reason: "no".into(),
            },
        );
        assert!(batch.tally().describe().contains("1 needing attention"));
    }
}
