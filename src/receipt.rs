//! Whether the responsibility term for a hand-over has come back signed
//! (`features/receipts-and-terms.md` phase 4).
//!
//! The term is where the holder acknowledges that the key is a credential, that
//! the PIN is theirs alone, and that a loss must be reported immediately. Without
//! that acknowledgement the loss procedure has no basis — so an unsigned term is
//! not paperwork outstanding, it is a hand-over whose obligations nobody has
//! agreed to.
//!
//! ## Why this needs a state machine at all
//!
//! Because "signed" is not observable from one field. A term is signed when the
//! scan is filed **or** when the unit recorded its own document reference, and it
//! is *pending* from the moment the key was handed over — which for a posted key
//! is legitimately days before the signature exists. The interesting state is the
//! one in between, and the only way to see it is to compute it: how long has this
//! hand-over been waiting, and is that longer than this unit accepts?
//!
//! That gap is exactly what `features/distribution-records.md` phase 4 exists to
//! make visible, and it is the reason this is derived rather than stored. A
//! `signature_state` column would be a second truth: it would have to be updated
//! when a document is filed, when a reference is typed, and — impossibly — when a
//! day passes.
//!
//! ## A returned key with no signed term stays on the list
//!
//! Deliberately. The term is evidence of custody *while the key was held*, so a
//! key that came back without one leaves a permanent gap in the record. Hiding it
//! once the key is returned would be the tool tidying up its own history, which is
//! the failure this register exists to prevent. The state says `returned`, so
//! nobody wastes an afternoon chasing a signature for a key that is back in the
//! drawer — but the gap is still counted.

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

use crate::domain::{DistributionRecord, DocumentKind};

/// How long a term may sit unsigned before it is called overdue, and whether this
/// deployment uses terms at all.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SignaturePolicy {
    /// Does this unit hand over a responsibility term? A unit that does not is a
    /// real case — an internal pilot, a batch of test keys — and it should not be
    /// nagged about documents it never intended to produce.
    pub required: bool,
    /// Days from the hand-over before an unsigned term is overdue.
    ///
    /// **One threshold, not one per delivery method.** A posted key takes longer to
    /// come back signed than one handed across a desk, so the honest way to set
    /// this is to whatever the slowest channel a unit actually uses takes; two
    /// thresholds would mean an operator working out which one applies to the row
    /// in front of them, which is how a warning stops being read.
    pub overdue_after_days: u32,
}

impl Default for SignaturePolicy {
    fn default() -> Self {
        Self {
            // On by default: the built-in templates exist, the loss procedure
            // depends on the acknowledgement, and a deployment that does not want
            // terms turns this off deliberately rather than by never noticing.
            required: true,
            // Two weeks. Long enough that an internal post round or a holder on
            // leave does not raise a warning, short enough that a term nobody
            // chased is noticed inside the month it was handed over.
            overdue_after_days: 14,
        }
    }
}

impl SignaturePolicy {
    pub const MIN_DAYS: u32 = 1;
    pub const MAX_DAYS: u32 = 365;

    /// Refuse a threshold that would make the warning meaningless.
    pub fn check(&self) -> Result<(), String> {
        if !self.required {
            return Ok(());
        }
        if self.overdue_after_days < Self::MIN_DAYS {
            return Err(
                "a term cannot be overdue before the day it was handed over — use at least 1 day"
                    .into(),
            );
        }
        if self.overdue_after_days > Self::MAX_DAYS {
            return Err(format!(
                "{} days is longer than the register is likely to keep the hand-over on the \
                 outstanding list; use at most {}",
                self.overdue_after_days,
                Self::MAX_DAYS
            ));
        }
        Ok(())
    }

    pub fn describe(&self) -> String {
        if !self.required {
            return "responsibility terms are not used by this unit".into();
        }
        format!(
            "a term unsigned {} day(s) after the hand-over is overdue",
            self.overdue_after_days
        )
    }
}

/// What documents are on file against one hand-over.
///
/// Counted per kind, because "a document is filed" is not the question: a
/// generated term filed against a hand-over is not a *signed* one, and treating
/// any attachment as evidence of a signature would be the tool marking its own
/// homework.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Filed {
    pub signed_terms: usize,
    pub return_receipts: usize,
    pub total: usize,
}

impl Filed {
    /// Fold one filed document in.
    pub fn add(&mut self, kind: DocumentKind, count: usize) {
        self.total += count;
        match kind {
            DocumentKind::SignedTerm => self.signed_terms += count,
            DocumentKind::ReturnReceipt => self.return_receipts += count,
            DocumentKind::GeneratedTerm | DocumentKind::Other => {}
        }
    }
}

/// Where the signature of one hand-over's term stands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureState {
    /// This unit does not use terms.
    NotRequired,
    /// The scan is filed, or the unit recorded its own reference.
    Signed { reference: String },
    /// Handed over, not signed, and not yet past the threshold.
    Pending { days: i64 },
    /// Unsigned for longer than this unit accepts.
    Overdue { days: i64, threshold: u32 },
    /// The key came back and the term was never signed. A permanent gap in the
    /// record rather than something still to chase.
    MissingOnReturn { days: i64 },
}

impl SignatureState {
    /// Words for the badge, distinct per state — the `gui-shell` phase 10 rule, and
    /// here also the useful presentation: "overdue" and "never signed" ask for
    /// different things from the operator.
    pub fn label(&self) -> &'static str {
        match self {
            SignatureState::NotRequired => "term not used",
            SignatureState::Signed { .. } => "signed",
            SignatureState::Pending { .. } => "awaiting signature",
            SignatureState::Overdue { .. } => "overdue",
            SignatureState::MissingOnReturn { .. } => "returned unsigned",
        }
    }

    /// Is somebody expected to do something about this one?
    pub fn needs_chasing(&self) -> bool {
        matches!(
            self,
            SignatureState::Pending { .. } | SignatureState::Overdue { .. }
        )
    }

    pub fn is_overdue(&self) -> bool {
        matches!(self, SignatureState::Overdue { .. })
    }

    /// Is this hand-over's paperwork complete, or deliberately not wanted?
    pub fn is_settled(&self) -> bool {
        matches!(
            self,
            SignatureState::Signed { .. } | SignatureState::NotRequired
        )
    }

    /// The sentence an operator reads, which has to say what to do.
    pub fn describe(&self) -> String {
        match self {
            SignatureState::NotRequired => {
                "no responsibility term is expected — this unit has terms turned off".into()
            }
            SignatureState::Signed { reference } if reference.is_empty() => {
                "the signed term is filed against this hand-over".into()
            }
            SignatureState::Signed { reference } => {
                format!("signed — {reference}")
            }
            SignatureState::Pending { days } => format!(
                "handed over {days} day(s) ago and the signed term is not filed yet. File the scan \
                 against this hand-over, or record the unit's own reference in Receipt"
            ),
            SignatureState::Overdue { days, threshold } => format!(
                "handed over {days} day(s) ago with no signed term, past this unit's {threshold}-day \
                 limit. Until it is signed, the holder has not acknowledged the obligations the \
                 loss procedure depends on"
            ),
            SignatureState::MissingOnReturn { days } => format!(
                "the key was returned and no signed term was ever filed — a permanent gap for the \
                 {days} day(s) it was held. Nothing to chase; recorded because the register does \
                 not tidy away its own history"
            ),
        }
    }

    /// The audit detail for `receipt.pending_overdue`.
    pub fn audit_detail(&self) -> String {
        match self {
            SignatureState::Overdue { days, threshold } => {
                format!("unsigned_days={days} threshold={threshold}")
            }
            other => format!("state={}", other.label()),
        }
    }
}

/// Where one hand-over's term stands, from the record and what is filed against it.
pub fn state_of(
    record: &DistributionRecord,
    filed: Filed,
    policy: &SignaturePolicy,
    now: DateTime<Utc>,
) -> SignatureState {
    if !policy.required {
        return SignatureState::NotRequired;
    }

    // Either kind of evidence counts, and the reference is named so the badge can
    // say *which* — a filed scan and "our file 2026/114" are both an answer to
    // "where is the signed term", and a unit that uses one should not be asked for
    // the other.
    if filed.signed_terms > 0 {
        return SignatureState::Signed {
            reference: String::new(),
        };
    }
    let reference = record.receipt_ref.trim();
    if !reference.is_empty() {
        return SignatureState::Signed {
            reference: reference.to_owned(),
        };
    }

    let days = record.days_held(now).max(0);
    if record.returned_at.is_some() {
        return SignatureState::MissingOnReturn { days };
    }
    if days > policy.overdue_after_days as i64 {
        return SignatureState::Overdue {
            days,
            threshold: policy.overdue_after_days,
        };
    }
    SignatureState::Pending { days }
}

/// Has the return been documented? Only meaningful once a key is back.
///
/// The mirror of the signature state, and the other half of the custody loop
/// (`features/receipts-and-terms.md` phase 6): a hand-over is only fully
/// accounted for when there is a document saying it began and one saying it ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReturnState {
    /// Still with the holder.
    Held,
    /// Back, with the receipt filed.
    Documented,
    /// Back, and nothing filed to say so.
    Undocumented,
}

impl ReturnState {
    pub fn label(&self) -> &'static str {
        match self {
            ReturnState::Held => "with the holder",
            ReturnState::Documented => "return receipt filed",
            ReturnState::Undocumented => "no return receipt",
        }
    }
}

pub fn return_state_of(record: &DistributionRecord, filed: Filed) -> ReturnState {
    match (record.returned_at.is_some(), filed.return_receipts > 0) {
        (false, _) => ReturnState::Held,
        (true, true) => ReturnState::Documented,
        (true, false) => ReturnState::Undocumented,
    }
}

/// How the whole register's paperwork stands, for one line at the top of a screen.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Outstanding {
    pub signed: usize,
    pub pending: usize,
    pub overdue: usize,
    pub returned_unsigned: usize,
    pub returns_undocumented: usize,
}

impl Outstanding {
    /// Is there anything for the operator to act on?
    pub fn needs_attention(&self) -> bool {
        self.overdue > 0 || self.returns_undocumented > 0
    }

    /// One line, or `None` when there is nothing worth saying.
    pub fn describe(&self) -> Option<String> {
        let mut parts = Vec::new();
        if self.overdue > 0 {
            parts.push(format!("{} term(s) overdue", self.overdue));
        }
        if self.pending > 0 {
            parts.push(format!("{} awaiting signature", self.pending));
        }
        if self.returned_unsigned > 0 {
            parts.push(format!(
                "{} returned without a signed term",
                self.returned_unsigned
            ));
        }
        if self.returns_undocumented > 0 {
            parts.push(format!(
                "{} return(s) with no receipt filed",
                self.returns_undocumented
            ));
        }
        (!parts.is_empty()).then(|| parts.join(", "))
    }
}

/// Tally every hand-over.
pub fn outstanding<'a>(
    records: impl IntoIterator<Item = &'a DistributionRecord>,
    filed: impl Fn(&DistributionRecord) -> Filed,
    policy: &SignaturePolicy,
    now: DateTime<Utc>,
) -> Outstanding {
    let mut out = Outstanding::default();
    for record in records {
        let on_file = filed(record);
        match state_of(record, on_file, policy, now) {
            SignatureState::Signed { .. } => out.signed += 1,
            SignatureState::Pending { .. } => out.pending += 1,
            SignatureState::Overdue { .. } => out.overdue += 1,
            SignatureState::MissingOnReturn { .. } => out.returned_unsigned += 1,
            SignatureState::NotRequired => {}
        }
        if return_state_of(record, on_file) == ReturnState::Undocumented {
            out.returns_undocumented += 1;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::DeliveryMethod;
    use uuid::Uuid;

    fn handed_over(days_ago: i64) -> DistributionRecord {
        DistributionRecord {
            id: Uuid::new_v4(),
            key_id: Uuid::new_v4(),
            key_serial: 20_423_633,
            holder_id: Uuid::new_v4(),
            holder_display: "Ana Silva <ana@example.org>".into(),
            distributed_at: Utc::now() - chrono::Duration::days(days_ago),
            distributed_by: "felipe".into(),
            method: DeliveryMethod::InPerson,
            receipt_ref: String::new(),
            bootstrap_run_id: None,
            returned_at: None,
            returned_to: None,
            notes: String::new(),
        }
    }

    fn now() -> DateTime<Utc> {
        Utc::now()
    }

    fn nothing() -> Filed {
        Filed::default()
    }

    #[test]
    fn a_fresh_handover_is_pending_and_not_yet_a_problem() {
        let state = state_of(
            &handed_over(2),
            nothing(),
            &SignaturePolicy::default(),
            now(),
        );
        assert_eq!(state, SignatureState::Pending { days: 2 });
        assert!(state.needs_chasing());
        assert!(!state.is_overdue());
        assert!(!state.is_settled());
    }

    #[test]
    fn a_term_unsigned_past_the_threshold_is_overdue_and_says_why_it_matters() {
        let policy = SignaturePolicy::default();
        let state = state_of(&handed_over(30), nothing(), &policy, now());
        assert_eq!(
            state,
            SignatureState::Overdue {
                days: 30,
                threshold: 14
            }
        );
        // The sentence has to say what an unsigned term costs, not just that it is
        // late: the acknowledgement is what the loss procedure rests on.
        let said = state.describe();
        assert!(said.contains("loss procedure"), "{said}");
        assert!(said.contains("30"), "{said}");
        assert_eq!(state.audit_detail(), "unsigned_days=30 threshold=14");
    }

    #[test]
    fn the_day_the_threshold_falls_on_is_not_yet_overdue() {
        // An off-by-one here is a warning that fires a day early on every hand-over
        // in the register, which is how a warning gets ignored.
        let policy = SignaturePolicy::default();
        assert!(!state_of(&handed_over(14), nothing(), &policy, now()).is_overdue());
        assert!(state_of(&handed_over(15), nothing(), &policy, now()).is_overdue());
    }

    #[test]
    fn a_filed_scan_settles_it_and_so_does_the_units_own_reference() {
        let policy = SignaturePolicy::default();

        // The scan.
        let filed = Filed {
            signed_terms: 1,
            total: 1,
            ..Filed::default()
        };
        let state = state_of(&handed_over(90), filed, &policy, now());
        assert!(state.is_settled(), "{state:?}");
        assert_eq!(state.label(), "signed");

        // Or the unit's own document reference, for a unit that files paper
        // elsewhere. Both are answers to "where is the signed term"; demanding the
        // scan as well would be this tool insisting on its own filing system.
        let mut with_reference = handed_over(90);
        with_reference.receipt_ref = "processo 2026/114".into();
        let state = state_of(&with_reference, nothing(), &policy, now());
        assert_eq!(
            state,
            SignatureState::Signed {
                reference: "processo 2026/114".into()
            }
        );
        assert!(
            state.describe().contains("2026/114"),
            "{}",
            state.describe()
        );
    }

    #[test]
    fn a_generated_term_on_file_is_not_a_signed_one() {
        // The failure this guards: treating any attachment as evidence. A term the
        // tool generated and filed says nothing about whether anybody signed it.
        let mut filed = Filed::default();
        filed.add(DocumentKind::GeneratedTerm, 1);
        filed.add(DocumentKind::Other, 2);
        assert_eq!(filed.total, 3);
        assert_eq!(filed.signed_terms, 0);

        let state = state_of(&handed_over(30), filed, &SignaturePolicy::default(), now());
        assert!(state.is_overdue(), "{state:?}");
    }

    #[test]
    fn a_returned_key_with_no_signed_term_is_a_permanent_gap_not_a_chase() {
        let mut record = handed_over(60);
        record.returned_at = Some(Utc::now() - chrono::Duration::days(10));

        let state = state_of(&record, nothing(), &SignaturePolicy::default(), now());
        // 50 days held, not 60: the clock stops when the key comes back.
        assert_eq!(state, SignatureState::MissingOnReturn { days: 50 });
        assert!(
            !state.needs_chasing(),
            "nobody should chase a signature for a key that is back in the drawer"
        );
        assert!(!state.is_settled(), "it is still a gap in the record");
        assert!(
            state.describe().contains("permanent"),
            "{}",
            state.describe()
        );
    }

    #[test]
    fn a_unit_that_does_not_use_terms_is_not_nagged() {
        let policy = SignaturePolicy {
            required: false,
            ..SignaturePolicy::default()
        };
        let state = state_of(&handed_over(400), nothing(), &policy, now());
        assert_eq!(state, SignatureState::NotRequired);
        assert!(state.is_settled());
        assert!(!state.needs_chasing());
        assert!(
            policy.describe().contains("not used"),
            "{}",
            policy.describe()
        );
    }

    #[test]
    fn the_return_receipt_is_tracked_separately_from_the_term() {
        let mut record = handed_over(30);
        assert_eq!(return_state_of(&record, nothing()), ReturnState::Held);

        record.returned_at = Some(Utc::now());
        assert_eq!(
            return_state_of(&record, nothing()),
            ReturnState::Undocumented
        );

        let filed = Filed {
            return_receipts: 1,
            total: 1,
            ..Filed::default()
        };
        assert_eq!(return_state_of(&record, filed), ReturnState::Documented);
    }

    #[test]
    fn the_tally_counts_every_kind_of_gap() {
        let policy = SignaturePolicy::default();
        let signed = {
            let mut r = handed_over(40);
            r.receipt_ref = "ref".into();
            r
        };
        let pending = handed_over(3);
        let overdue = handed_over(40);
        let returned_unsigned = {
            let mut r = handed_over(40);
            r.returned_at = Some(Utc::now());
            r
        };
        let records = vec![signed, pending, overdue, returned_unsigned];

        let tally = outstanding(&records, |_| nothing(), &policy, now());
        assert_eq!(tally.signed, 1);
        assert_eq!(tally.pending, 1);
        assert_eq!(tally.overdue, 1);
        assert_eq!(tally.returned_unsigned, 1);
        assert_eq!(tally.returns_undocumented, 1);
        assert!(tally.needs_attention());

        let line = tally.describe().expect("something to say");
        assert!(line.contains("1 term(s) overdue"), "{line}");
        assert!(line.contains("no receipt filed"), "{line}");

        // And a register with nothing outstanding says nothing at all, rather than
        // "0 overdue" — a screen that always shows a warning line trains the
        // operator to skip it.
        assert_eq!(
            outstanding(std::iter::empty(), |_| nothing(), &policy, now()).describe(),
            None
        );
    }

    #[test]
    fn a_threshold_that_would_make_the_warning_meaningless_is_refused() {
        assert!(SignaturePolicy::default().check().is_ok());
        assert!(
            SignaturePolicy {
                required: true,
                overdue_after_days: 0
            }
            .check()
            .is_err()
        );
        assert!(
            SignaturePolicy {
                required: true,
                overdue_after_days: 4_000
            }
            .check()
            .is_err()
        );
        // Turned off, the threshold is not used and is nobody's problem.
        assert!(
            SignaturePolicy {
                required: false,
                overdue_after_days: 0
            }
            .check()
            .is_ok()
        );
    }

    #[test]
    fn every_state_has_its_own_words() {
        let states = [
            SignatureState::NotRequired,
            SignatureState::Signed {
                reference: String::new(),
            },
            SignatureState::Pending { days: 1 },
            SignatureState::Overdue {
                days: 20,
                threshold: 14,
            },
            SignatureState::MissingOnReturn { days: 5 },
        ];
        let labels: std::collections::BTreeSet<&str> = states.iter().map(|s| s.label()).collect();
        assert_eq!(labels.len(), states.len(), "{labels:?}");
        for state in &states {
            assert!(!state.describe().trim().is_empty());
        }

        let returns = [
            ReturnState::Held,
            ReturnState::Documented,
            ReturnState::Undocumented,
        ];
        let labels: std::collections::BTreeSet<&str> = returns.iter().map(|s| s.label()).collect();
        assert_eq!(labels.len(), returns.len(), "{labels:?}");
    }
}
