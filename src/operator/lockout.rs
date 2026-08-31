//! How slowly a wrong sign-in may be retried
//! (`features/operator-auth-and-roles.md` phase 2).
//!
//! ## The timings are the norm's, not this module's
//!
//! Three failures buy a minute, two more buy a quarter of an hour, two more buy
//! an hour. They are written down in the development norm and repeated in the
//! feature file, so they are constants here rather than a curve somebody tuned:
//! a lockout policy that does not match the one an auditor reads is a finding
//! whether or not it is stricter.
//!
//! ## And whether they belong here at all is an open question
//!
//! The norm's progressive lockout is written for a **web login**, where the
//! primary control is the source IP and a lockout costs an attacker a botnet. On
//! a desktop application there is no IP — there is one workstation, physically in
//! the unit, and the person being slowed down is very often the operator who
//! mistyped. `AGENTS.md` §8 makes that mapping an **ESI** question rather than an
//! implementer's, so it is implemented exactly as specified and recorded as open
//! in `features/operator-auth-and-roles.md`.
//!
//! Two consequences are worth stating rather than discovering:
//!
//! * A locked account **can** be unlocked, unlike the database password, whose
//!   throttle deliberately never locks because there is no administrator to lift
//!   it. Here there is one — that is what the administrator role is for — and an
//!   unlock is audited.
//! * The counter is keyed on the **username that was typed**, not on an operator
//!   id, and it counts failures against a username that does not exist. Counting
//!   only real accounts would turn the lockout into an oracle: type a name, see
//!   whether it can be locked out, and learn who is on the register.
//!
//! ## Why the state is in the database
//!
//! A counter in memory is reset by closing the window, which makes the lockout a
//! suggestion. It lives in `operator_sign_ins`, which is one of the three tables
//! a session that has **not** signed in may write — it must be, or the mechanism
//! cannot count the failures it exists to count.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::time::Duration;

/// Failures allowed before the first lockout. The third failure is the one that
/// locks.
pub const FREE_ATTEMPTS: u32 = 2;

/// After 3 failures.
pub const FIRST: Duration = Duration::from_secs(60);

/// After 5.
pub const SECOND: Duration = Duration::from_secs(15 * 60);

/// After 7, and after every failure beyond.
pub const THIRD: Duration = Duration::from_secs(60 * 60);

/// How long a lockout lasts after `failures` consecutive failures.
///
/// `None` for the first two. The steps are `3, 4 → 1 min`, `5, 6 → 15 min`,
/// `7+ → 1 h`: the spec says "3 failures → 1 min, +2 → 15 min, +2 → 1 h", so the
/// increase happens *at* the third, fifth and seventh, and the failures between
/// them repeat the step they are in rather than escalating on every attempt.
pub fn after(failures: u32) -> Option<Duration> {
    match failures {
        0..=FREE_ATTEMPTS => None,
        3 | 4 => Some(FIRST),
        5 | 6 => Some(SECOND),
        _ => Some(THIRD),
    }
}

/// The failure history for one typed username.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Lockout {
    /// Consecutive failures. A success clears it.
    pub failures: u32,
    /// When the current lockout expires, if one is running.
    pub locked_until: Option<DateTime<Utc>>,
    pub last_failure_at: Option<DateTime<Utc>>,
}

impl Lockout {
    /// Record a failure, and say how long the account is now locked for.
    pub fn record_failure(&mut self, now: DateTime<Utc>) -> Option<Duration> {
        self.failures = self.failures.saturating_add(1);
        self.last_failure_at = Some(now);
        let penalty = after(self.failures);
        self.locked_until = penalty.and_then(|penalty| {
            ChronoDuration::from_std(penalty)
                .ok()
                .map(|penalty| now + penalty)
        });
        penalty
    }

    /// A successful sign-in clears the history entirely.
    ///
    /// Not decremented: the policy counts *consecutive* failures, and half-
    /// forgetting them would leave an operator who signs in successfully still
    /// two mistakes from a fifteen-minute wait.
    pub fn clear(&mut self) {
        self.failures = 0;
        self.locked_until = None;
        self.last_failure_at = None;
    }

    pub fn is_locked_at(&self, now: DateTime<Utc>) -> bool {
        !self.remaining_at(now).is_zero()
    }

    /// How much of the lockout is left. Zero when none is running.
    ///
    /// A countdown rather than the fixed penalty, for the reason the database
    /// unlock throttle gives: a wait that sits at its initial value until it
    /// expires reads as a hung application.
    pub fn remaining_at(&self, now: DateTime<Utc>) -> Duration {
        let Some(until) = self.locked_until else {
            return Duration::ZERO;
        };
        (until - now).to_std().unwrap_or(Duration::ZERO)
    }

    /// What the operator is told while they are locked out.
    ///
    /// Names the wait and the number of failures, and nothing about what was
    /// typed. It also does not say whether the username exists, for the reason in
    /// this module's header.
    pub fn message_at(&self, now: DateTime<Utc>) -> Option<String> {
        let remaining = self.remaining_at(now);
        if remaining.is_zero() {
            return None;
        }
        let seconds = remaining.as_secs().max(1);
        let wait = if seconds >= 60 {
            format!("{} minute(s)", seconds.div_ceil(60))
        } else {
            format!("{seconds} second(s)")
        };
        Some(format!(
            "{} consecutive failed sign-ins — locked for another {wait}. An administrator can \
             clear it.",
            self.failures
        ))
    }

    /// The audit detail for a failure.
    ///
    /// No password material, not even a length: the attempt count and the
    /// lockout, which is what the spec's `operator.login.failed` row asks for.
    pub fn audit_detail(&self, now: DateTime<Utc>) -> String {
        format!(
            "attempt={} lockout={}",
            self.failures,
            self.remaining_at(now).as_secs()
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("a valid instant")
    }

    /// The spec's timings, exactly: 3 → 1 min, +2 → 15 min, +2 → 1 h.
    #[test]
    fn the_lockout_steps_are_the_ones_the_norm_specifies() {
        assert_eq!(after(0), None);
        assert_eq!(after(1), None);
        assert_eq!(after(2), None);
        assert_eq!(after(3), Some(FIRST));
        assert_eq!(after(4), Some(FIRST));
        assert_eq!(after(5), Some(SECOND));
        assert_eq!(after(6), Some(SECOND));
        assert_eq!(after(7), Some(THIRD));
        assert_eq!(after(8), Some(THIRD));
        assert_eq!(after(4_000), Some(THIRD));

        assert_eq!(FIRST.as_secs(), 60);
        assert_eq!(SECOND.as_secs(), 15 * 60);
        assert_eq!(THIRD.as_secs(), 60 * 60);
    }

    #[test]
    fn two_failures_lock_nothing_and_the_third_locks_for_a_minute() {
        let mut lockout = Lockout::default();
        assert_eq!(lockout.record_failure(at(0)), None);
        assert_eq!(lockout.record_failure(at(1)), None);
        assert!(!lockout.is_locked_at(at(1)));

        assert_eq!(lockout.record_failure(at(2)), Some(FIRST));
        assert!(lockout.is_locked_at(at(2)));
        assert_eq!(lockout.remaining_at(at(2)).as_secs(), 60);
        // It counts down rather than sitting at its initial value.
        assert_eq!(lockout.remaining_at(at(32)).as_secs(), 30);
        // And it expires on its own.
        assert!(!lockout.is_locked_at(at(62)));
    }

    #[test]
    fn the_fifth_failure_buys_a_quarter_of_an_hour_and_the_seventh_an_hour() {
        let mut lockout = Lockout::default();
        for second in 0..4 {
            lockout.record_failure(at(second));
        }
        assert_eq!(lockout.record_failure(at(4)), Some(SECOND));
        assert_eq!(lockout.remaining_at(at(4)).as_secs(), 15 * 60);

        lockout.record_failure(at(5));
        assert_eq!(lockout.record_failure(at(6)), Some(THIRD));
        assert_eq!(lockout.remaining_at(at(6)).as_secs(), 60 * 60);
    }

    #[test]
    fn a_success_clears_the_history_rather_than_decrementing_it() {
        let mut lockout = Lockout::default();
        for second in 0..6 {
            lockout.record_failure(at(second));
        }
        assert!(lockout.is_locked_at(at(6)));

        lockout.clear();
        assert_eq!(lockout.failures, 0);
        assert!(!lockout.is_locked_at(at(6)));
        // The next failure starts again from the beginning.
        assert_eq!(lockout.record_failure(at(7)), None);
    }

    #[test]
    fn neither_the_message_nor_the_audit_detail_can_carry_a_password() {
        let mut lockout = Lockout::default();
        for second in 0..3 {
            lockout.record_failure(at(second));
        }
        let message = lockout.message_at(at(2)).expect("a lockout is running");
        assert!(message.contains('3'));
        assert!(message.contains("administrator"));

        let detail = lockout.audit_detail(at(2));
        assert_eq!(detail, "attempt=3 lockout=60");
        // The two fields the spec's `operator.login.failed` row names, and no
        // third one that could ever hold what was typed.
        assert_eq!(detail.split(' ').count(), 2);
    }

    #[test]
    fn nothing_is_said_while_nothing_is_locked() {
        let lockout = Lockout::default();
        assert_eq!(lockout.message_at(at(0)), None);
        assert_eq!(lockout.remaining_at(at(0)), Duration::ZERO);
    }
}
