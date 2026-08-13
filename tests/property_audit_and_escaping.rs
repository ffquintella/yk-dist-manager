//! Property tests for the two pieces of pure logic whose failure modes are shaped
//! like inputs nobody thinks to write down
//! (`features/testing-strategy.md` phase 9).
//!
//! ## Why these two, and not everything
//!
//! A property test earns its keep where the *space of inputs* is the risk, not the
//! logic. Both of these qualify, and for opposite reasons:
//!
//! * **The audit chain** is the thing this register's credibility rests on. Its
//!   example tests all use plausible entries — `felipe`, `key.added`,
//!   `serial:20423633` — and a hash chain does not fail on plausible input. It fails
//!   on an actor containing a newline, on details that happen to look like another
//!   entry's, on an empty string where the concatenation loses a boundary. Those are
//!   generated, not imagined.
//! * **The RFC 4514 escaper** takes the one field this application has no control
//!   over: a person's name, as they spell it. `Ana Silva` proves nothing. What
//!   matters is `#Ana`, `Ana,` , `\\`, a name that is one space, a name ending in a
//!   space — each of which has a rule, and the rules interact at the ends of the
//!   string.
//!
//! Everything else in this crate is either driven by the operator through a screen
//! (where a behaviour test is the honest shape) or bounded by an enum.
//!
//! ## What a failure here means
//!
//! `proptest` prints the minimal shrunken input and writes it to
//! `tests/property-regressions/…`. **Commit that file.** It turns the counterexample
//! into a permanent example test, which is the whole point: the property found it
//! once, the regression file makes sure it stays found.

use proptest::prelude::*;
use yk_dist_manager::audit::{AuditEntry, verify};
use yk_dist_manager::domain::escape_rfc4514;

/// Text that goes into an audit entry or a certificate subject.
///
/// Deliberately nasty, and deliberately *not* only nasty: the separators a naive
/// concatenation would confuse (`|`, `:`, newline, tab), the characters RFC 4514
/// gives rules to, an empty string, and ordinary words — because a property that
/// only ever sees line noise stops testing the common path.
fn text() -> impl Strategy<Value = String> {
    prop_oneof![
        2 => "[a-zA-Z0-9 .@_-]{0,40}",
        1 => r#"[|:;,+<>="\\#\n\t ]{0,12}"#,
        1 => any::<String>(),
        1 => Just(String::new()),
    ]
}

/// A chain built the way [`yk_dist_manager::audit::AuditLog`] builds one: each
/// entry's `prev_hash` is the previous entry's `hash`, and each `hash` is computed
/// from its own content.
///
/// Built here rather than through `AuditLog` so the properties are about the
/// *chain*, with no filesystem in the way — the file is `AuditLog`'s own tested
/// concern.
fn chain(fields: Vec<(String, String, String, String)>) -> Vec<AuditEntry> {
    const GENESIS: &str = "0000000000000000000000000000000000000000000000000000000000000000";
    let mut previous = GENESIS.to_owned();
    let mut entries = Vec::with_capacity(fields.len());

    for (index, (actor, event, target, details)) in fields.into_iter().enumerate() {
        let mut entry = AuditEntry {
            seq: index as u64 + 1,
            // Fixed rather than `now()`: two entries written inside the same
            // millisecond would otherwise make a "reordering is detected" property
            // pass for the wrong reason.
            at: chrono::DateTime::parse_from_rfc3339("2026-08-12T10:00:00Z")
                .unwrap()
                .with_timezone(&chrono::Utc)
                + chrono::Duration::seconds(index as i64),
            actor,
            event,
            target,
            details,
            prev_hash: previous.clone(),
            hash: String::new(),
        };
        entry.hash = entry.compute_hash();
        previous = entry.hash.clone();
        entries.push(entry);
    }
    entries
}

fn fields() -> impl Strategy<Value = (String, String, String, String)> {
    (text(), text(), text(), text())
}

proptest! {
    /// Whatever anybody records, the chain verifies.
    ///
    /// The baseline: if this can fail, every other property here is noise. It is
    /// also the one that catches a hash function that ignores a field — an entry
    /// whose `details` never reached the digest would still verify, and this
    /// property alone would not notice, which is why the tampering properties below
    /// exist as well.
    #[test]
    fn any_chain_built_by_appending_verifies(fields in prop::collection::vec(fields(), 0..24)) {
        let entries = chain(fields);
        prop_assert!(verify(&entries).is_ok(), "a well-formed chain must verify");
    }

    /// Changing **any** field of **any** entry breaks verification.
    ///
    /// This is the property the register's credibility actually rests on: an entry
    /// cannot be edited after the fact without the chain saying so. It also pins the
    /// digest's coverage — a field left out of `compute_hash` fails here and nowhere
    /// else.
    #[test]
    fn editing_any_field_of_any_entry_is_detected(
        fields in prop::collection::vec(fields(), 1..12),
        victim in 0usize..12,
        which in 0usize..5,
        replacement in text(),
    ) {
        let entries = chain(fields);
        let victim = victim % entries.len();
        let mut tampered = entries.clone();

        // Only a *change* is a test: replacing a field with what it already held
        // proves nothing, so those runs are skipped rather than passing vacuously.
        let target = &mut tampered[victim];
        let changed = match which {
            0 => std::mem::replace(&mut target.actor, replacement.clone()) != replacement,
            1 => std::mem::replace(&mut target.event, replacement.clone()) != replacement,
            2 => std::mem::replace(&mut target.target, replacement.clone()) != replacement,
            3 => std::mem::replace(&mut target.details, replacement.clone()) != replacement,
            _ => {
                target.at += chrono::Duration::seconds(1);
                true
            }
        };
        prop_assume!(changed);

        prop_assert!(
            verify(&tampered).is_err(),
            "an edited entry must break the chain: entry {victim}, field {which}"
        );
    }

    /// Swapping two entries breaks verification.
    ///
    /// Order is part of the record — "the key was marked lost, then handed over" is a
    /// different history from the reverse — and a chain that only covered content
    /// would let somebody reorder it.
    #[test]
    fn reordering_entries_is_detected(
        fields in prop::collection::vec(fields(), 2..12),
        a in 0usize..12,
        b in 0usize..12,
    ) {
        let entries = chain(fields);
        let a = a % entries.len();
        let b = b % entries.len();
        prop_assume!(a != b);

        let mut swapped = entries.clone();
        swapped.swap(a, b);
        prop_assert!(
            verify(&swapped).is_err(),
            "a reordered chain must not verify"
        );
    }

    /// Removing an entry from the middle breaks verification.
    #[test]
    fn removing_an_entry_from_the_middle_is_detected(
        fields in prop::collection::vec(fields(), 2..12),
        victim in 0usize..12,
    ) {
        let entries = chain(fields);
        // The *last* entry is the documented exception — see the example test below.
        let victim = victim % (entries.len() - 1);
        let mut short = entries.clone();
        short.remove(victim);
        prop_assert!(
            verify(&short).is_err(),
            "a missing entry must break the chain"
        );
    }

    /// An escaped value can be read back to exactly what went in.
    ///
    /// The property that matters for a certificate subject: escaping is reversible,
    /// so a name is not silently altered on its way into a `CN`. A holder whose name
    /// reaches a certificate mangled has been issued a credential in somebody else's
    /// name, which is the failure this whole application exists to avoid.
    #[test]
    fn escaping_is_reversible(value in text()) {
        let escaped = escape_rfc4514(&value);
        prop_assert_eq!(unescape_rfc4514(&escaped), value);
    }

    /// No **unescaped** separator survives escaping.
    ///
    /// The other half: a `,` or `+` left bare would end the attribute value early,
    /// and the parser at the CA would read one RDN as two. Checked by walking the
    /// output rather than by counting, because a backslash is itself escaped.
    #[test]
    fn no_bare_separator_survives(value in text()) {
        let escaped = escape_rfc4514(&value);
        let mut chars = escaped.chars();
        while let Some(ch) = chars.next() {
            if ch == '\\' {
                // Whatever follows is escaped by construction, and there is always
                // something: a trailing lone backslash would be the bug.
                prop_assert!(chars.next().is_some(), "a trailing escape in {escaped:?}");
                continue;
            }
            prop_assert!(
                !matches!(ch, '"' | '+' | ',' | ';' | '<' | '>' | '\\'),
                "bare {ch:?} survived escaping of {value:?} -> {escaped:?}"
            );
        }
    }

    /// A leading `#` and an edge space are escaped, wherever the value came from.
    ///
    /// RFC 4514 §2.4 treats those positionally, which is exactly the kind of rule
    /// that works on `Ana Silva` and fails on ` Ana` — and a name with a leading
    /// space is a copy-paste away.
    #[test]
    fn positional_rules_hold_at_both_ends(value in text()) {
        let escaped = escape_rfc4514(&value);
        prop_assume!(!value.is_empty());

        if value.starts_with('#') {
            prop_assert!(escaped.starts_with("\\#"), "{value:?} -> {escaped:?}");
        }
        if value.starts_with(' ') {
            prop_assert!(escaped.starts_with("\\ "), "{value:?} -> {escaped:?}");
        }
        if value.ends_with(' ') {
            prop_assert!(escaped.ends_with("\\ "), "{value:?} -> {escaped:?}");
        }
        // And nothing that was not a rule got one: the escaped form is never shorter.
        prop_assert!(escaped.len() >= value.len());
    }
}

/// The documented limit of `verify` alone: **truncation at the tail**.
///
/// A prefix of a valid chain is itself a valid chain, so deleting the newest entries
/// cannot be detected by verification. That is a property of a hash chain, not a bug
/// to fix here, and it is why the register's protection against deletion is the
/// database trigger that refuses `DELETE` on the audit table
/// (`features/audit-trail.md`) — plus the segregated mirror, whose divergence from
/// the table is the alert.
///
/// An example test rather than a property: the statement is about a specific
/// operation, and pinning it here means a future reader finds the limit written down
/// beside the properties that hold, instead of assuming `verify` covers it.
#[test]
fn truncating_the_tail_is_not_detected_by_verification_and_that_is_documented() {
    let entries = chain(vec![
        (
            "felipe".into(),
            "key.added".into(),
            "serial:1".into(),
            String::new(),
        ),
        (
            "felipe".into(),
            "key.distributed".into(),
            "serial:1".into(),
            String::new(),
        ),
        (
            "felipe".into(),
            "key.returned".into(),
            "serial:1".into(),
            String::new(),
        ),
    ]);
    assert!(verify(&entries).is_ok());

    let truncated = &entries[..2];
    assert!(
        verify(truncated).is_ok(),
        "a prefix of a valid chain verifies — the DELETE trigger and the mirror are \
         what stand between the register and a truncated trail, not this function"
    );
}

/// Reverse [`escape_rfc4514`]: drop one level of backslash escaping.
///
/// Only in the test, and deliberately so — the application has no reason to
/// *un*-escape a subject it built, and a public inverse would be an invitation to
/// round-trip data through a certificate field. It exists here to state the
/// reversibility property, which is the thing worth guaranteeing.
fn unescape_rfc4514(escaped: &str) -> String {
    let mut out = String::with_capacity(escaped.len());
    let mut chars = escaped.chars();
    while let Some(ch) = chars.next() {
        if ch == '\\' {
            if let Some(next) = chars.next() {
                out.push(next);
            }
        } else {
            out.push(ch);
        }
    }
    out
}
