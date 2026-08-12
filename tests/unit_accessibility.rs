//! The accessibility invariant, expressed as a test.
//!
//! `features/gui-shell.md` phase 10 asks for "no colour-only meaning". The paint
//! code that *applies* the colour is outside the coverage gate, so the rule
//! cannot be tested where it is used — but the thing that makes it satisfiable
//! can be: **every state the UI gives a colour to also has distinct, non-empty
//! text.**
//!
//! That is the half worth pinning. A screen can be reviewed for whether it
//! displays the label; it cannot be reviewed into existence if two states share
//! a label, or one returns an empty string, because then no amount of careful
//! painting distinguishes them without hue.
//!
//! Who this is actually for: an operator with a monochrome or failing display, a
//! colour-blind operator (about one man in twelve), and anyone reading a
//! screenshot pasted into a ticket after the colours were flattened.

use std::collections::HashSet;

/// Every label in a set must be non-empty and unique.
fn distinct(what: &str, labels: &[&str]) {
    for label in labels {
        assert!(
            !label.trim().is_empty(),
            "{what}: a state with no text can only be told apart by colour"
        );
    }
    let unique: HashSet<&&str> = labels.iter().collect();
    assert_eq!(
        unique.len(),
        labels.len(),
        "{what}: two states share a label, so colour is the only thing separating them: {labels:?}"
    );
}

#[test]
fn every_key_status_has_its_own_words() {
    use yk_dist_manager::domain::KeyStatus;

    let labels: Vec<&str> = KeyStatus::ALL.iter().map(|s| s.label()).collect();
    distinct("KeyStatus", &labels);
    assert_eq!(
        KeyStatus::ALL.len(),
        6,
        "a new lifecycle state needs a label before it needs a colour"
    );
}

#[test]
fn every_transport_has_its_own_words() {
    // The plan table colours this column, and it is the one an operator most
    // needs to read correctly: whether a step goes native, through `ykman`, or
    // has to be done by hand.
    use yk_dist_manager::template::Transport;

    let labels: Vec<&str> = [Transport::Native, Transport::Ykman, Transport::Manual]
        .iter()
        .map(|t| t.label())
        .collect();
    distinct("Transport", &labels);
}

#[test]
fn every_preflight_severity_has_its_own_words() {
    use yk_dist_manager::bootstrap::Severity;

    let labels: Vec<&str> = [Severity::Skip, Severity::Warning, Severity::Blocking]
        .iter()
        .map(|s| s.label())
        .collect();
    distinct("Severity", &labels);
}

#[test]
fn the_sort_direction_is_an_arrow_rather_than_a_highlight() {
    use yk_dist_manager::browse::Direction;

    let labels: Vec<&str> = [Direction::Ascending, Direction::Descending]
        .iter()
        .map(|d| d.arrow())
        .collect();
    distinct("Direction", &labels);
}

#[test]
fn every_log_level_has_its_own_words() {
    use yk_dist_manager::logbuf::Level;

    let labels: Vec<&str> = [Level::Debug, Level::Info, Level::Warn, Level::Error]
        .iter()
        .map(|l| l.label())
        .collect();
    distinct("Level", &labels);
    assert_eq!(
        Level::Error.label(),
        "ERROR",
        "an error must not read like the rest of the list"
    );
}

#[test]
fn every_delivery_method_has_its_own_words() {
    use yk_dist_manager::domain::DeliveryMethod;

    let labels: Vec<&str> = DeliveryMethod::ALL.iter().map(|m| m.label()).collect();
    distinct("DeliveryMethod", &labels);
}

#[test]
fn every_custody_model_has_its_own_words() {
    // This one reaches a hand-over record, so it has to be readable in a
    // printed report as well as on a themed screen.
    use yk_dist_manager::domain::CustodyModel;

    let labels: Vec<&str> = CustodyModel::ALL.iter().map(|m| m.label()).collect();
    distinct("CustodyModel", &labels);
}

#[test]
fn every_password_strength_has_its_own_words() {
    // The meter is a coloured bar, which is the shape most likely to be read by
    // hue alone — so every step it can paint has to say what it is, and "too
    // weak" has to be distinguishable from "weak" in words rather than in shade.
    use yk_dist_manager::password::Strength;

    let labels: Vec<&str> = Strength::ALL.iter().map(|s| s.label()).collect();
    distinct("Strength", &labels);
    assert!(
        Strength::TooWeak.label().contains("refused"),
        "the refused step must say it is refused, not merely look redder: {}",
        Strength::TooWeak.label()
    );
}

#[test]
fn the_status_line_severity_is_derived_from_the_text_not_from_the_caller() {
    // `status::classify` reads the message, so the words and the colour cannot
    // disagree — the colour is a function of the text rather than a second,
    // independently-set signal that could contradict it.
    use yk_dist_manager::status::{Severity, classify};

    assert_eq!(classify("AUDIT FAILURE: chain broken"), Severity::Alarm);
    assert_ne!(classify("backup written to /tmp/x"), Severity::Alarm);
}
