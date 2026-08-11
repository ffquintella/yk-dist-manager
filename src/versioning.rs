//! Version numbering shared by the two things in this application that are
//! **edited data with a history**: the consignment term ([`crate::term`]) and the
//! bootstrap template ([`crate::template`]).
//!
//! Both follow the same rule, for the same reason: an edit is stored as a *new
//! version* and the previous one is left where it is, because something already
//! refers to it — a term a holder signed, a bootstrap run that recorded which
//! procedure was applied. So both need one answer to "what number does the next
//! edit get?", and it lives here rather than in either of them.
//!
//! The number comes from **what the database already holds**, never from the
//! version on the operator's screen: two workstations editing the same template
//! must not both produce "version 2".

/// Sort key for a version: the leading digits numerically, so `10` sorts after
/// `9`, then the whole string so the order is total and stable.
pub fn version_order(version: &str) -> (u64, &str) {
    let digits: String = version
        .trim()
        .chars()
        .take_while(char::is_ascii_digit)
        .collect();
    (digits.parse().unwrap_or(0), version)
}

/// The version to give the next edit: one more than the highest numeric version
/// already present. A version that is not a number counts as 0, so the first
/// numbered edit of a hand-named version becomes `1`.
pub fn next_version(existing: &[String]) -> String {
    let highest = existing
        .iter()
        .map(|version| version_order(version).0)
        .max()
        .unwrap_or(0);
    (highest + 1).to_string()
}
