//! How loud the status bar should be about the last outcome.
//!
//! The status line carries every outcome the operator sees between screens —
//! "serial 12345678 distributed" sits in the same place as "AUDIT FAILURE".
//! AGENTS.md requires an audit failure to be loud, so the bar needs to know
//! which of the two it is holding.
//!
//! This is a **display** classification of a message the application itself
//! wrote, not a parser for arbitrary text: the prefixes matched here are the
//! ones `YkDistApp` produces. A message that matches nothing is shown
//! normally, which is the safe direction to be wrong in — a routine outcome
//! never gets dressed up as a failure.

/// How the status bar should render the current message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Severity {
    /// A routine outcome.
    Normal,
    /// Something the operator asked for did not happen.
    Warning,
    /// The audit trail itself is in question.
    Alarm,
}

/// Openings that mean the audit trail is compromised. These are written by
/// `YkDistApp::record` and `YkDistApp::verify_audit`.
const ALARMS: [&str; 2] = ["AUDIT FAILURE", "AUDIT CHAIN BROKEN"];

/// Openings that mean an operation was refused or failed. Every one of these
/// is a literal prefix used somewhere in `YkDistApp`.
const WARNINGS: [&str; 6] = [
    "REFUSED",
    "could not",
    "detection failed",
    "backup failed",
    "integrity check failed",
    "recorded, but",
];

/// Classify a status message.
pub fn classify(message: &str) -> Severity {
    let message = message.trim_start();
    if ALARMS.iter().any(|prefix| message.starts_with(prefix)) {
        return Severity::Alarm;
    }
    if WARNINGS.iter().any(|prefix| message.starts_with(prefix)) {
        return Severity::Warning;
    }
    Severity::Normal
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_audit_failure_is_an_alarm() {
        assert_eq!(
            classify("AUDIT FAILURE: database is locked"),
            Severity::Alarm
        );
        assert_eq!(
            classify("AUDIT CHAIN BROKEN: entry 7 does not match"),
            Severity::Alarm
        );
    }

    #[test]
    fn a_refusal_is_a_warning() {
        assert_eq!(
            classify("could not save the key: disk full"),
            Severity::Warning
        );
        assert_eq!(
            classify("recorded, but status not updated: illegal status transition"),
            Severity::Warning
        );
        assert_eq!(
            classify("REFUSED: digest does not match"),
            Severity::Warning
        );
    }

    #[test]
    fn a_routine_outcome_is_not_dressed_up() {
        assert_eq!(classify("serial 12345678 distributed"), Severity::Normal);
        assert_eq!(classify(""), Severity::Normal);
        // "failed" appearing later in a sentence is not a prefix match: the
        // classifier stays deliberately literal.
        assert_eq!(
            classify("term written to /tmp/termo-failed-attempt.txt"),
            Severity::Normal
        );
    }
}
