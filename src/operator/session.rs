//! Being signed in: how long it lasts, when it locks, and what a sensitive
//! operation needs on top of it
//! (`features/operator-auth-and-roles.md` phases 5 and 6).
//!
//! ## The workstation is shared
//!
//! That is the fact the whole module follows from. A hand-over desk has one
//! machine and several people, and the failure it produces is not an attacker —
//! it is Bruno recording a return under Ana's session because Ana walked away
//! without closing it. A session timeout is therefore an *accuracy* control
//! before it is a security one: the audit trail's actor has to be who did the
//! thing.
//!
//! Two thresholds rather than one, because "gone for a coffee" and "gone home"
//! deserve different answers:
//!
//! * [`LOCK_AFTER`] — the session **locks**. Nothing is lost; the same person
//!   signs back in and carries on. Idle work in progress stays on screen.
//! * [`TIMEOUT_AFTER`] — the session **ends**, and is audited as
//!   `operator.logout` with `reason=timeout`, because a session that quietly
//!   stopped existing is indistinguishable from one nobody closed.
//!
//! ## Re-verification is not the session
//!
//! Phase 5. A session answers *how long ago did somebody authenticate*; a
//! destructive or irreversible operation needs *is that person still here*, and
//! those are different questions. So editing a procedure, resetting an applet,
//! changing the database password, managing operators or taking an export asks
//! for the credential again — touch the key, or type the password — and the
//! answer is good for [`REVERIFICATION_WINDOW`] and no longer.
//!
//! The window exists so that a batch of related sensitive actions is not a batch
//! of prompts, which is how a confirmation stops being read. Two minutes is long
//! enough to retire three template versions and short enough that walking away
//! ends it.

use chrono::{DateTime, Duration as ChronoDuration, Utc};
use std::time::Duration;
use uuid::Uuid;

use super::{AuthMethod, Authority, Role};

/// Idle time after which the session locks and has to be re-opened.
pub const LOCK_AFTER: Duration = Duration::from_secs(5 * 60);

/// Idle time after which the session ends outright.
pub const TIMEOUT_AFTER: Duration = Duration::from_secs(30 * 60);

/// How long a re-verification is good for.
pub const REVERIFICATION_WINDOW: Duration = Duration::from_secs(2 * 60);

/// What the idle clock says about a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Idle {
    /// Still in use.
    Active,
    /// Idle past [`LOCK_AFTER`]: lock it.
    Lock,
    /// Idle past [`TIMEOUT_AFTER`]: end it.
    Expire,
}

/// Somebody is signed in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub operator_id: Uuid,
    /// What goes in `audit.actor`. Lower-cased and unique in the register, so
    /// "everything Ana did" is one query and not a guess.
    pub username: String,
    pub display_name: String,
    pub role: Role,
    pub method: AuthMethod,
    pub opened_at: DateTime<Utc>,
    /// Last time the operator did anything. Advanced by [`Session::touch`].
    pub last_activity: DateTime<Utc>,
    /// When the credential was last presented again for a sensitive operation.
    pub reverified_at: Option<DateTime<Utc>>,
    /// Locked by the idle clock or by the operator pressing Lock.
    pub locked: bool,
}

impl Session {
    pub fn open(
        operator_id: Uuid,
        username: impl Into<String>,
        display_name: impl Into<String>,
        role: Role,
        method: AuthMethod,
        now: DateTime<Utc>,
    ) -> Self {
        Self {
            operator_id,
            username: username.into(),
            display_name: display_name.into(),
            role,
            method,
            opened_at: now,
            last_activity: now,
            // Signing in **is** a verification, so the first sensitive operation
            // in the first two minutes does not ask twice. Anything later does.
            reverified_at: Some(now),
            locked: false,
        }
    }

    /// The authority this session carries, which is what the store enforces on.
    ///
    /// A **locked** session carries none: it is signed out for every purpose
    /// except knowing whose name to put on the unlock prompt. That is the point
    /// of locking rather than merely dimming the screen.
    pub fn authority(&self) -> Authority {
        if self.locked {
            Authority::SignedOut
        } else {
            Authority::SignedIn(self.role)
        }
    }

    /// Record that the operator did something.
    pub fn touch(&mut self, now: DateTime<Utc>) {
        self.last_activity = now;
    }

    /// How long since the last activity.
    pub fn idle_for(&self, now: DateTime<Utc>) -> Duration {
        (now - self.last_activity)
            .to_std()
            .unwrap_or(Duration::ZERO)
    }

    /// What should happen to this session, given the clock.
    ///
    /// Checked in order: expiry beats locking, because a session idle for an hour
    /// crossed both thresholds and only the stricter answer is correct.
    pub fn idle_state_at(&self, now: DateTime<Utc>) -> Idle {
        let idle = self.idle_for(now);
        if idle >= TIMEOUT_AFTER {
            Idle::Expire
        } else if idle >= LOCK_AFTER {
            Idle::Lock
        } else {
            Idle::Active
        }
    }

    pub fn lock(&mut self) {
        self.locked = true;
        // A locked session holds no re-verification: coming back to the desk is
        // exactly the moment the tool must not assume the same person returned.
        self.reverified_at = None;
    }

    pub fn is_locked(&self) -> bool {
        self.locked
    }

    /// Re-opened after a lock, by presenting the credential again.
    pub fn unlock(&mut self, now: DateTime<Utc>) {
        self.locked = false;
        self.last_activity = now;
        self.reverified_at = Some(now);
    }

    /// The credential was presented again for a sensitive operation.
    pub fn reverify(&mut self, now: DateTime<Utc>) {
        self.reverified_at = Some(now);
        self.last_activity = now;
    }

    /// Is a re-verification still good?
    pub fn is_reverified_at(&self, now: DateTime<Utc>) -> bool {
        if self.locked {
            return false;
        }
        let Some(at) = self.reverified_at else {
            return false;
        };
        let window = ChronoDuration::from_std(REVERIFICATION_WINDOW).expect("a sane window");
        now >= at && now - at < window
    }

    /// How long the current re-verification has left, for the screen.
    pub fn reverification_remaining_at(&self, now: DateTime<Utc>) -> Duration {
        let Some(at) = self.reverified_at else {
            return Duration::ZERO;
        };
        if self.locked {
            return Duration::ZERO;
        }
        let window = ChronoDuration::from_std(REVERIFICATION_WINDOW).expect("a sane window");
        (at + window - now).to_std().unwrap_or(Duration::ZERO)
    }

    /// `Ana Silva (ana.silva) — Distributor`, for the top bar.
    pub fn describe(&self) -> String {
        format!(
            "{} ({}) — {}, signed in with a {}",
            self.display_name,
            self.username,
            self.role.label(),
            self.method.label()
        )
    }
}

/// What the application knows about who is using it.
///
/// The three states are [`Authority`]'s, carried at the application level with
/// the identity attached — `Authority` is what the store enforces on, and this is
/// what the screen and the audit `actor` come from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionState {
    /// No operator has been enrolled in this register.
    ///
    /// Carries the workstation's signed-in user, which is what the audit `actor`
    /// falls back to and which this tool has always said is a **label**, not
    /// authentication. That sentence has to stay on screen for as long as this
    /// state exists.
    Unenrolled { workstation_user: String },
    /// Operators exist and nobody is signed in.
    SignedOut,
    /// Signed in — possibly locked, which [`Session::authority`] accounts for.
    SignedIn(Box<Session>),
}

impl Default for SessionState {
    fn default() -> Self {
        SessionState::Unenrolled {
            workstation_user: crate::store::cloud::local_operator(),
        }
    }
}

impl SessionState {
    /// What goes in `audit.actor` and on a hand-over record.
    ///
    /// In [`SessionState::Unenrolled`] this is the workstation user, exactly as
    /// before this feature — and that is the honest answer, because in that state
    /// nothing has been authenticated. Under
    /// [`SessionState::SignedOut`] it is a name no person owns, so that an entry
    /// written between sign-ins (there is one: the failed attempt itself) cannot
    /// be mistaken for somebody's work.
    pub fn actor(&self) -> &str {
        match self {
            SessionState::Unenrolled { workstation_user } => workstation_user,
            SessionState::SignedOut => "(not signed in)",
            SessionState::SignedIn(session) => &session.username,
        }
    }

    /// What the screen shows.
    pub fn display(&self) -> String {
        match self {
            SessionState::Unenrolled { workstation_user } => {
                format!("{workstation_user} (workstation user — not authenticated)")
            }
            SessionState::SignedOut => "not signed in".to_owned(),
            SessionState::SignedIn(session) if session.is_locked() => {
                format!("{} — locked", session.display_name)
            }
            SessionState::SignedIn(session) => session.describe(),
        }
    }

    pub fn authority(&self) -> Authority {
        match self {
            SessionState::Unenrolled { .. } => Authority::Unenrolled,
            SessionState::SignedOut => Authority::SignedOut,
            SessionState::SignedIn(session) => session.authority(),
        }
    }

    pub fn session(&self) -> Option<&Session> {
        match self {
            SessionState::SignedIn(session) => Some(session),
            _ => None,
        }
    }

    pub fn session_mut(&mut self) -> Option<&mut Session> {
        match self {
            SessionState::SignedIn(session) => Some(session),
            _ => None,
        }
    }

    /// Is a sensitive operation currently authorised?
    ///
    /// `true` while the register is unenrolled, because in that state the control
    /// is off and pretending otherwise would refuse work on every register
    /// written before this release.
    pub fn is_reverified_at(&self, now: DateTime<Utc>) -> bool {
        match self {
            SessionState::Unenrolled { .. } => true,
            SessionState::SignedOut => false,
            SessionState::SignedIn(session) => session.is_reverified_at(now),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn at(seconds: i64) -> DateTime<Utc> {
        DateTime::from_timestamp(1_800_000_000 + seconds, 0).expect("a valid instant")
    }

    fn session() -> Session {
        Session::open(
            Uuid::nil(),
            "ana.silva",
            "Ana Silva",
            Role::Distributor,
            AuthMethod::Fido2,
            at(0),
        )
    }

    #[test]
    fn a_session_locks_after_five_minutes_and_ends_after_thirty() {
        let session = session();
        assert_eq!(session.idle_state_at(at(60)), Idle::Active);
        assert_eq!(session.idle_state_at(at(5 * 60 - 1)), Idle::Active);
        assert_eq!(session.idle_state_at(at(5 * 60)), Idle::Lock);
        assert_eq!(session.idle_state_at(at(29 * 60)), Idle::Lock);
        // Expiry beats locking: an hour idle crossed both thresholds and only the
        // stricter answer is correct.
        assert_eq!(session.idle_state_at(at(30 * 60)), Idle::Expire);
        assert_eq!(session.idle_state_at(at(60 * 60)), Idle::Expire);
    }

    #[test]
    fn using_the_application_puts_the_idle_clock_back() {
        let mut session = session();
        session.touch(at(4 * 60));
        assert_eq!(session.idle_state_at(at(8 * 60)), Idle::Active);
        assert_eq!(session.idle_state_at(at(9 * 60)), Idle::Lock);
    }

    /// The whole point of locking rather than dimming: a locked session carries
    /// no authority at all.
    #[test]
    fn a_locked_session_is_signed_out_for_every_purpose_but_its_name() {
        let mut session = session();
        assert_eq!(session.authority(), Authority::SignedIn(Role::Distributor));

        session.lock();
        assert_eq!(session.authority(), Authority::SignedOut);
        assert!(!session.is_reverified_at(at(1)));
        assert_eq!(session.display_name, "Ana Silva");

        session.unlock(at(600));
        assert_eq!(session.authority(), Authority::SignedIn(Role::Distributor));
        assert_eq!(session.idle_state_at(at(600)), Idle::Active);
    }

    #[test]
    fn signing_in_counts_as_a_verification_and_it_lasts_two_minutes() {
        let session = session();
        assert!(session.is_reverified_at(at(0)));
        assert!(session.is_reverified_at(at(119)));
        assert!(!session.is_reverified_at(at(120)));
        assert!(!session.is_reverified_at(at(600)));
    }

    #[test]
    fn presenting_the_credential_again_starts_the_window_over() {
        let mut session = session();
        assert!(!session.is_reverified_at(at(300)));
        session.reverify(at(300));
        assert!(session.is_reverified_at(at(300)));
        assert!(session.is_reverified_at(at(419)));
        assert!(!session.is_reverified_at(at(420)));
        assert_eq!(
            session.reverification_remaining_at(at(360)).as_secs(),
            60,
            "the screen counts it down rather than showing the whole window"
        );
    }

    /// Coming back to the desk is exactly the moment the tool must not assume the
    /// same person returned.
    #[test]
    fn locking_discards_a_live_re_verification() {
        let mut session = session();
        session.reverify(at(10));
        assert!(session.is_reverified_at(at(11)));
        session.lock();
        assert!(!session.is_reverified_at(at(11)));
        assert_eq!(session.reverification_remaining_at(at(11)), Duration::ZERO);
    }

    #[test]
    fn an_unenrolled_register_reports_the_workstation_user_and_says_it_is_a_label() {
        let state = SessionState::Unenrolled {
            workstation_user: "felipe".into(),
        };
        assert_eq!(state.actor(), "felipe");
        assert_eq!(state.authority(), Authority::Unenrolled);
        assert!(
            state.display().contains("not authenticated"),
            "the screen must never let a label be mistaken for identity: {}",
            state.display()
        );
        // The control is off, so nothing is refused for want of a
        // re-verification.
        assert!(state.is_reverified_at(at(0)));
    }

    #[test]
    fn an_entry_written_between_sign_ins_is_owned_by_nobody() {
        let state = SessionState::SignedOut;
        assert_eq!(state.actor(), "(not signed in)");
        assert_eq!(state.authority(), Authority::SignedOut);
        assert!(!state.is_reverified_at(at(0)));
    }

    #[test]
    fn a_signed_in_state_reports_the_username_as_the_actor() {
        let state = SessionState::SignedIn(Box::new(session()));
        assert_eq!(state.actor(), "ana.silva");
        assert_eq!(state.authority(), Authority::SignedIn(Role::Distributor));
        let described = state.display();
        assert!(described.contains("Ana Silva"));
        assert!(described.contains("Distributor"));
        assert!(described.contains("security key"));
    }
}
