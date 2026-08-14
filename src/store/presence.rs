//! Who else has this register open right now
//! (`features/database-selection.md` phase 6).
//!
//! # Why this is a warning and not a lock
//!
//! On a share, two operators writing at once is *safe*: SQLite's own locking
//! serialises them, the busy timeout waits for the other one, and
//! [`crate::store::StoreError::Busy`] is what an operator sees if it waits too
//! long. What is not safe is two operators editing **the same record** from two
//! screens that were painted minutes apart — and that is answered by the
//! optimistic check ([`crate::store::StoreError::Conflict`]), which refuses the
//! second save rather than losing the first.
//!
//! So what is left for this module is the thing neither of those covers: an
//! operator about to start a hand-over has no way of knowing that somebody else
//! is standing at another desk doing the same thing with the same box of keys.
//! That is not a data-integrity problem, it is a coordination one, and the fix is
//! a name on the screen rather than a refusal.
//!
//! The cloud-sync lock ([`crate::store::cloud`]) is the opposite case and stays
//! as it is: there, the second workstation is refused outright, because the two
//! are not sharing a lock manager at all and the failure mode is two divergent
//! registers rather than a wasted afternoon.
//!
//! # A row is a claim, not a fact
//!
//! A session that crashes or has its laptop closed leaves its row behind, so a
//! row means "this session said it was here at *this* time" and nothing more.
//! Anything silent for [`SILENT_AFTER`] is not shown and is pruned the next time
//! somebody opens the register — the same reasoning, and the same window, as the
//! cloud lease's staleness.

use chrono::{DateTime, Duration, Utc};
use uuid::Uuid;

/// A session silent for this long is treated as gone.
///
/// The cloud lease's [`crate::store::cloud::STALE_AFTER`], deliberately: a
/// sleeping laptop stops writing without saying goodbye, and one vocabulary for
/// "how long before we stop believing a session is alive" is worth more than a
/// number tuned per feature.
pub fn silent_after() -> Duration {
    Duration::seconds(15 * 60)
}

/// How often a live session rewrites its row.
pub fn renew_every() -> Duration {
    Duration::seconds(60)
}

/// The window, as the constant the docs quote.
pub const SILENT_AFTER: &str = "15 minutes";

/// One session that has the register open.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// The run, not the workstation: a pid is reused and a host may run two.
    pub id: Uuid,
    pub host: String,
    pub operator: String,
    pub opened_at: DateTime<Utc>,
    pub last_seen_at: DateTime<Utc>,
}

impl Session {
    /// Silent for longer than [`silent_after`] — a row nobody should be shown.
    pub fn is_stale(&self, now: DateTime<Utc>) -> bool {
        now - self.last_seen_at >= silent_after()
    }

    /// `Ana (WORKSTATION-3)`, or just the host when no operator was recorded.
    pub fn who(&self) -> String {
        let operator = self.operator.trim();
        if operator.is_empty() {
            self.host.clone()
        } else {
            format!("{operator} ({})", self.host)
        }
    }

    /// How long ago this session was last heard from, in words.
    pub fn last_seen(&self, now: DateTime<Utc>) -> String {
        let minutes = (now - self.last_seen_at).num_minutes();
        match minutes {
            m if m <= 0 => "just now".to_owned(),
            1 => "1 minute ago".to_owned(),
            m => format!("{m} minutes ago"),
        }
    }
}

/// Everybody except this session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presence {
    pub others: Vec<Session>,
}

impl Presence {
    pub fn is_empty(&self) -> bool {
        self.others.is_empty()
    }

    /// The line an operator reads, or `None` when nobody else is here.
    ///
    /// `None` rather than "0 other operators": a banner that is always on screen
    /// is a banner nobody reads, which is the same rule the outstanding-paperwork
    /// line follows.
    pub fn describe(&self, now: DateTime<Utc>) -> Option<String> {
        match self.others.as_slice() {
            [] => None,
            [one] => Some(format!(
                "{} also has this register open (last seen {}). Two operators editing the same \
                 key will find out at the second save, not at the first.",
                one.who(),
                one.last_seen(now)
            )),
            many => Some(format!(
                "{} other operators have this register open: {}. Two operators editing the same \
                 key will find out at the second save, not at the first.",
                many.len(),
                many.iter().map(Session::who).collect::<Vec<_>>().join(", ")
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(minutes_ago: i64) -> DateTime<Utc> {
        Utc::now() - Duration::minutes(minutes_ago)
    }

    fn session(operator: &str, host: &str, minutes_ago: i64) -> Session {
        Session {
            id: Uuid::new_v4(),
            host: host.into(),
            operator: operator.into(),
            opened_at: at(minutes_ago + 30),
            last_seen_at: at(minutes_ago),
        }
    }

    #[test]
    fn a_session_that_has_gone_quiet_for_the_window_is_stale() {
        // One `now` for the whole test, taken *after* the sessions are built:
        // reading the clock twice would make "exactly 15 minutes ago" land a few
        // microseconds short of the window, which is the off-by-one that would
        // leave a dead session on somebody's banner for another minute.
        let mut fresh = session("ana", "WS-1", 0);
        let mut old = session("ana", "WS-1", 0);
        let now = Utc::now();
        fresh.last_seen_at = now - Duration::minutes(14);
        old.last_seen_at = now - silent_after();

        assert!(!fresh.is_stale(now));
        assert!(old.is_stale(now));
    }

    #[test]
    fn nobody_else_says_nothing_at_all() {
        assert_eq!(Presence::default().describe(Utc::now()), None);
        assert!(Presence::default().is_empty());
    }

    #[test]
    fn one_other_operator_is_named_with_when_they_were_last_heard_from() {
        let presence = Presence {
            others: vec![session("Bruno", "WORKSTATION-3", 2)],
        };
        let line = presence.describe(Utc::now()).expect("something to say");
        assert!(line.contains("Bruno (WORKSTATION-3)"), "{line}");
        assert!(line.contains("2 minutes ago"), "{line}");
        // And it says what the consequence is, rather than only that somebody is
        // there: a warning nobody can act on is a warning that gets dismissed.
        assert!(line.contains("second save"), "{line}");
    }

    #[test]
    fn several_are_counted_and_listed() {
        let presence = Presence {
            others: vec![
                session("Bruno", "WORKSTATION-3", 1),
                session("", "NAS-KIOSK", 0),
            ],
        };
        let line = presence.describe(Utc::now()).expect("something to say");
        assert!(line.starts_with("2 other operators"), "{line}");
        // A session with no operator name is still a workstation worth naming.
        assert!(line.contains("NAS-KIOSK"), "{line}");
    }

    #[test]
    fn the_freshest_session_reads_as_just_now() {
        assert_eq!(session("ana", "WS-1", 0).last_seen(Utc::now()), "just now");
        assert_eq!(
            session("ana", "WS-1", 1).last_seen(Utc::now()),
            "1 minute ago"
        );
    }
}
