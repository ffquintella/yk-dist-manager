//! The power-cycle handshake in front of a FIDO2 reset
//! (`features/key-lifecycle-and-revocation.md` phase 5a).
//!
//! CTAP only accepts `authenticatorReset` within a few seconds of the
//! authenticator being powered up — five, on the firmware this unit hands out —
//! and it wants a touch on top. A key that has been sitting in the port while the
//! operator read the preview is therefore *always* out of time, and `ykman` says
//! so in words that read like a broken tool:
//!
//! > ERROR: Reset failed. Reset must be triggered within 5 seconds after the
//! > YubiKey is inserted.
//!
//! Until this module existed the tool's answer was a sentence in the preview
//! asking the operator to unplug the key and plug it back in *before* confirming —
//! which asks them to win a race they cannot see the start of. So the tool now
//! runs the race itself: it takes the confirmation first, then asks for the key to
//! be pulled out, watches for it to come back, and sends the reset in the same
//! frame it reappears.
//!
//! ## What this module is, and is not
//!
//! It is a state machine and a presence poll. **It writes nothing to a key** and
//! holds no transport of its own beyond the read-only [`YubiKeyBackend`] the
//! [`PresenceWatch`] enumerates with; firing the reset stays
//! [`crate::device::reset::perform`]'s job, called by the application when
//! [`Handshake::observe`] says [`Reaction::Fire`].
//!
//! The confirmation is taken **before** the handshake starts and the selection is
//! frozen into it ([`Handshake::applets`]). That is deliberate: the operator
//! agreed to a set of applets on a key, and the several seconds they then spend
//! with their hands on the hardware must not be a window in which that agreement
//! can drift.
//!
//! ## Why it can still fail, and why that is not hidden
//!
//! The whole handshake is a bet against a five-second window with two costs
//! inside it: the key has to enumerate (PC/SC brings the CCID interface up around
//! a second after insertion; `ykman list` forks a Python process to ask), and then
//! the reset itself has to be sent (another fork, in the `ykman` fallback that is
//! the only way to send it today). Usually that fits. When it does not, the
//! outcome table says which applet refused and in whose words, and the operator
//! can arm it again — the retry is one click, not a trip to a command line.
//!
//! The lasting fix is a native `authenticatorReset`
//! (`features/native-device-transport.md`), which would take one of the two forks
//! out of the window. This module is what makes the operation reliable enough to
//! use in the meantime, and it stays useful afterwards: the power cycle itself is
//! CTAP's requirement, not `ykman`'s.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::YubiKeyBackend;
use super::reset::{Applet, describe};

/// How long after power-up CTAP still accepts a reset.
///
/// Yubico documents five seconds, and `ykman` quotes the same number back when it
/// refuses. Used as the deadline this tool holds *itself* to: past it, the command
/// is not sent at all, because a refusal the operator has to read and interpret is
/// worse than a sentence saying the window closed.
pub const WINDOW: Duration = Duration::from_secs(5);

/// How long to wait for the operator's half before giving up.
///
/// Bounded on purpose. The presence poll below is a `ykman` subprocess in the
/// fallback build, and a handshake nobody finishes — the operator was called away
/// mid-reset — must not fork a process every half second for the rest of the
/// afternoon.
pub const PATIENCE: Duration = Duration::from_secs(60);

/// Presence poll interval with a transport that talks to the hardware directly.
///
/// Much faster than [`crate::device::watch::POLL_INTERVAL`], and for a different
/// reason: the watch is a background convenience, this is inside a five-second
/// window where every 100ms of latency is spent from the same budget as the reset.
pub const POLL_NATIVE: Duration = Duration::from_millis(200);

/// Presence poll interval when every poll is a subprocess.
///
/// `ykman list --serials` forks, so the loop is self-limiting whatever this says —
/// the fork costs several times the sleep. Half a second keeps the arming
/// responsive without spinning up processes back to back.
pub const POLL_SUBPROCESS: Duration = Duration::from_millis(500);

/// The tick used to notice a stop request, so dropping a watch is prompt.
const STOP_CHECK: Duration = Duration::from_millis(25);

/// Audit event: the operator was asked to power-cycle the key.
pub const EVENT_REQUESTED: &str = "key.reset.power_cycle.requested";
/// Audit event: the key came back and the reset is going out now.
pub const EVENT_ARMED: &str = "key.reset.power_cycle.armed";
/// Audit event: the handshake ended without a reset being sent.
pub const EVENT_ABANDONED: &str = "key.reset.power_cycle.abandoned";

/// Which interval suits the transport in use.
pub fn poll_for(native_transport: bool) -> Duration {
    if native_transport {
        POLL_NATIVE
    } else {
        POLL_SUBPROCESS
    }
}

/// Does this selection of applets need the handshake?
///
/// FIDO2 and only FIDO2. PIV and OTP accept a reset from a key that has been in
/// the port all morning, and asking for a power cycle they do not need would
/// teach the operator that this tool's instructions are decorative.
pub fn needed(applets: &[Applet]) -> bool {
    applets.contains(&Applet::Fido2)
}

/// Where a handshake has got to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    /// Confirmed, and waiting for the key to leave the port.
    AwaitingRemoval { since: Instant },
    /// Gone. Waiting for it to come back.
    AwaitingInsertion { since: Instant },
    /// Back, and inside the window: the reset goes out now.
    Armed { since: Instant },
    /// It was back, and the window closed before the reset could be sent.
    Expired,
    /// Nobody touched the key for [`PATIENCE`]. Nothing was written.
    GaveUp,
}

/// What the caller should do about the observation it just handed in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reaction {
    /// Keep polling; nothing has changed that the caller must act on.
    Wait,
    /// Send the reset **now**, in this frame.
    Fire,
    /// The window closed with the key in the port. Nothing was sent.
    Expired,
    /// The operator did not do their half in time. Nothing was sent.
    GaveUp,
}

/// One key's power cycle, from a confirmation to the moment the reset fires.
///
/// Clock-injected: every transition takes the `now` the caller observed at, so
/// the whole machine is unit tested against synthetic instants rather than against
/// a key and a stopwatch.
#[derive(Debug, Clone)]
pub struct Handshake {
    serial: u32,
    /// The applets the operator confirmed, frozen at the click.
    applets: Vec<Applet>,
    stage: Stage,
    window: Duration,
    patience: Duration,
    poll: Duration,
    /// How many times the operator has been asked, this panel.
    attempts: u32,
}

impl Handshake {
    /// Start asking, for a reset that has already been confirmed.
    pub fn start(serial: u32, applets: &[Applet], poll: Duration, now: Instant) -> Self {
        Self {
            serial,
            applets: applets.to_vec(),
            stage: Stage::AwaitingRemoval { since: now },
            window: WINDOW,
            patience: PATIENCE,
            poll,
            attempts: 1,
        }
    }

    /// Same, with the two deadlines shortened — for tests, which must not sleep.
    pub fn with_deadlines(mut self, window: Duration, patience: Duration) -> Self {
        self.window = window;
        self.patience = patience;
        self
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    /// The applets confirmed for this run, in reset order.
    pub fn applets(&self) -> &[Applet] {
        &self.applets
    }

    pub fn stage(&self) -> Stage {
        self.stage
    }

    pub fn attempts(&self) -> u32 {
        self.attempts
    }

    /// Is this handshake over without having fired?
    pub fn is_finished(&self) -> bool {
        matches!(self.stage, Stage::Expired | Stage::GaveUp)
    }

    /// Feed one presence observation in, and learn what to do.
    ///
    /// `present` is `None` until the poll has an answer — which is not the same as
    /// "the key is not there", and must not arm or advance anything.
    pub fn observe(&mut self, present: Option<bool>, now: Instant) -> Reaction {
        match self.stage {
            Stage::AwaitingRemoval { since } => {
                if present == Some(false) {
                    self.stage = Stage::AwaitingInsertion { since: now };
                    Reaction::Wait
                } else if now.duration_since(since) >= self.patience {
                    self.stage = Stage::GaveUp;
                    Reaction::GaveUp
                } else {
                    Reaction::Wait
                }
            }
            Stage::AwaitingInsertion { since } => {
                if present == Some(true) {
                    self.stage = Stage::Armed { since: now };
                    Reaction::Fire
                } else if now.duration_since(since) >= self.patience {
                    self.stage = Stage::GaveUp;
                    Reaction::GaveUp
                } else {
                    Reaction::Wait
                }
            }
            // Reached only when the caller was handed `Fire` and did not act on it
            // — a database that closed under the panel, or a frame that stalled.
            // The command is not sent late: it would be refused, and the refusal
            // reads like a hardware fault.
            Stage::Armed { since } => {
                if now.duration_since(since) > self.window {
                    self.stage = Stage::Expired;
                    Reaction::Expired
                } else {
                    Reaction::Wait
                }
            }
            Stage::Expired | Stage::GaveUp => Reaction::Wait,
        }
    }

    /// Arm on the operator's word rather than on a poll.
    ///
    /// The escape hatch for a workstation whose enumeration is slower than the
    /// window: the operator has the key in their hand and knows it is back. It is
    /// their assertion, so it is theirs to make — the panel offers it while
    /// waiting, never instead of waiting.
    pub fn arm_now(&mut self, now: Instant) -> Reaction {
        self.stage = Stage::Armed { since: now };
        Reaction::Fire
    }

    /// Ask again, after a window that closed or an operator who stepped away.
    pub fn restart(&mut self, now: Instant) {
        self.stage = Stage::AwaitingRemoval { since: now };
        self.attempts += 1;
    }

    /// What is left of the power-up window, while armed.
    pub fn remaining(&self, now: Instant) -> Option<Duration> {
        match self.stage {
            Stage::Armed { since } => Some(self.window.saturating_sub(now.duration_since(since))),
            _ => None,
        }
    }

    /// The step the operator is on, as a heading.
    pub fn title(&self) -> &'static str {
        match self.stage {
            Stage::AwaitingRemoval { .. } => "Step 1 of 2 — pull the key out of the port",
            Stage::AwaitingInsertion { .. } => "Step 2 of 2 — plug it straight back in",
            Stage::Armed { .. } => "Sending the reset — touch the key when it blinks",
            Stage::Expired => "The power-up window closed before the reset was sent",
            Stage::GaveUp => "Nothing happened, so nothing was written",
        }
    }

    /// The same step, with the reason it is being asked for.
    pub fn detail(&self) -> &'static str {
        match self.stage {
            Stage::AwaitingRemoval { .. } => {
                "Nothing has been written yet, and nothing will be until the key is back in \
                 the port. The FIDO2 applet only accepts a reset in the first few seconds \
                 after it powers up, so the reset has to follow the key in — this screen is \
                 watching for it and sends the reset itself."
            }
            Stage::AwaitingInsertion { .. } => {
                "As soon as this workstation sees the key again, the reset goes out — you do \
                 not have to click anything. Then touch the key while it blinks: the \
                 authenticator asks for that too, and it will not reset without it."
            }
            Stage::Armed { .. } => {
                "The reset is on its way to the key. Touch the contact when it blinks."
            }
            Stage::Expired => {
                "The key was back in the port for longer than the applet's window before the \
                 reset could be sent, so it was not sent at all — a command that arrives late \
                 is refused, and the refusal reads like a broken key. Nothing was written. \
                 Try again: pull the key out and plug it back in."
            }
            Stage::GaveUp => {
                "The key was neither pulled out nor plugged back in, so the reset was \
                 abandoned. Nothing was written and the confirmation still stands — start \
                 again when the key is in your hand."
            }
        }
    }

    /// The trail entry for asking in the first place.
    pub fn requested(&self) -> (&'static str, String) {
        (
            EVENT_REQUESTED,
            format!(
                "applets={} attempt={} reason=FIDO2 only accepts a reset within {}ms of \
                 power-up, so the operator was asked to re-insert the key",
                describe(&self.applets),
                self.attempts,
                self.window.as_millis()
            ),
        )
    }

    /// The trail entry a reaction deserves, if any.
    ///
    /// Here rather than in the application so that the events, and the words in
    /// them, are covered by the tests below instead of by the paint code.
    pub fn audit_for(&self, reaction: Reaction) -> Option<(&'static str, String)> {
        match reaction {
            Reaction::Wait => None,
            Reaction::Fire => Some((
                EVENT_ARMED,
                format!(
                    "applets={} attempt={} detected_within={}ms window={}ms",
                    describe(&self.applets),
                    self.attempts,
                    self.poll.as_millis(),
                    self.window.as_millis()
                ),
            )),
            Reaction::Expired => Some((
                EVENT_ABANDONED,
                format!(
                    "applets={} attempt={} reason=the key was back in the port for more than \
                     {}ms before the reset could be sent, so it was not sent",
                    describe(&self.applets),
                    self.attempts,
                    self.window.as_millis()
                ),
            )),
            Reaction::GaveUp => Some((
                EVENT_ABANDONED,
                format!(
                    "applets={} attempt={} reason=the key was not re-inserted within {}s",
                    describe(&self.applets),
                    self.attempts,
                    self.patience.as_secs()
                ),
            )),
        }
    }

    /// The trail entry for an operator who changed their mind mid-handshake.
    pub fn cancelled(&self) -> (&'static str, String) {
        (
            EVENT_ABANDONED,
            format!(
                "applets={} attempt={} reason=the operator cancelled before the key was back \
                 in the port",
                describe(&self.applets),
                self.attempts
            ),
        )
    }
}

// ------------------------------------------------------------- the presence poll

/// Whether one serial is in a port, as last seen.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Presence {
    /// `None` until the first answer — which is not "the key is gone".
    pub present: Option<bool>,
    pub polls: u64,
    /// The last failure, kept until a poll succeeds.
    pub last_error: Option<String>,
    /// Set when the thread has given up: looking again will not help.
    pub stopped: Option<String>,
}

impl Presence {
    /// One line for the panel, next to the step.
    pub fn describe(&self, serial: u32) -> String {
        if let Some(reason) = &self.stopped {
            return format!("not watching the port: {reason}");
        }
        match self.present {
            None => "asking the port…".to_owned(),
            Some(true) => format!("serial {serial} is in a port"),
            Some(false) => format!("serial {serial} is not in a port"),
        }
    }
}

/// A thread watching one port for one serial, fast, for as long as it is held.
///
/// Separate from [`crate::device::DeviceWatch`] rather than a faster setting of
/// it, because the two want different things. The watch identifies every key it
/// finds — a second subprocess per key in the fallback build — so that a screen
/// can name them; this asks one question, `list_serials`, and compares. Inside a
/// five-second window that difference is the whole point.
pub struct PresenceWatch {
    state: Arc<Mutex<Presence>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
    interval: Duration,
}

impl PresenceWatch {
    /// Start watching for `serial`, with `backend` moved onto the thread.
    pub fn start(backend: Box<dyn YubiKeyBackend>, serial: u32, interval: Duration) -> Self {
        let state = Arc::new(Mutex::new(Presence::default()));
        let stop = Arc::new(AtomicBool::new(false));

        let thread_state = Arc::clone(&state);
        let thread_stop = Arc::clone(&stop);
        let handle = std::thread::Builder::new()
            .name("reset-presence".into())
            .spawn(move || poll_loop(backend, serial, interval, thread_state, thread_stop))
            .ok();

        if handle.is_none() {
            tracing::error!(
                event = "device.presence.spawn_failed",
                detail = "the power cycle has to be confirmed by hand"
            );
            state.lock().expect("presence state").stopped =
                Some("this workstation could not start the watch thread".into());
        }

        Self {
            state,
            stop,
            handle,
            interval,
        }
    }

    /// What the port last said, cheap enough to call every frame.
    pub fn snapshot(&self) -> Presence {
        self.state.lock().expect("presence state").clone()
    }

    pub fn interval(&self) -> Duration {
        self.interval
    }
}

impl Drop for PresenceWatch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(handle) = self.handle.take() {
            // Joined, for the reason the device watch is: the thread holds a
            // transport handle, and the reset that runs the moment this is dropped
            // must not race an enumeration still in flight.
            let _ = handle.join();
        }
    }
}

fn poll_loop(
    backend: Box<dyn YubiKeyBackend>,
    serial: u32,
    interval: Duration,
    state: Arc<Mutex<Presence>>,
    stop: Arc<AtomicBool>,
) {
    while !stop.load(Ordering::Relaxed) {
        match backend.list_serials() {
            Ok(serials) => {
                let mut guard = state.lock().expect("presence state");
                guard.present = Some(serials.contains(&serial));
                guard.last_error = None;
                guard.polls += 1;
            }
            // Nothing attached at all is an answer, and it is the one this
            // handshake is waiting for: the key has left the port.
            Err(super::DeviceError::NoDevice) => {
                let mut guard = state.lock().expect("presence state");
                guard.present = Some(false);
                guard.last_error = None;
                guard.polls += 1;
            }
            Err(e) => {
                // A missing transport will not appear by being asked again, and the
                // operator needs to be told rather than left watching a step that
                // can never complete.
                let fatal = matches!(e, super::DeviceError::ToolMissing { .. });
                let mut guard = state.lock().expect("presence state");
                guard.polls += 1;
                guard.last_error = Some(e.to_string());
                if fatal {
                    guard.stopped = Some(e.to_string());
                    drop(guard);
                    tracing::warn!(event = "device.presence.stopped", reason = %e);
                    return;
                }
            }
        }

        let mut slept = Duration::ZERO;
        while slept < interval && !stop.load(Ordering::Relaxed) {
            let slice = STOP_CHECK.min(interval - slept);
            std::thread::sleep(slice);
            slept += slice;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::device::{DeviceError, DeviceInfo, MockBackend};

    const SERIAL: u32 = 20_423_633;

    fn handshake(now: Instant) -> Handshake {
        Handshake::start(SERIAL, &[Applet::Fido2, Applet::Piv], POLL_NATIVE, now)
            .with_deadlines(Duration::from_millis(500), Duration::from_millis(2_000))
    }

    #[test]
    fn only_a_selection_with_fido2_in_it_needs_a_power_cycle() {
        // Asking for a power cycle the applet does not need would teach the
        // operator that this tool's instructions are decorative.
        assert!(needed(&[Applet::Fido2]));
        assert!(needed(&Applet::ALL));
        assert!(!needed(&[Applet::Piv, Applet::Otp]));
        assert!(!needed(&[]));
    }

    #[test]
    fn the_key_in_the_port_at_the_start_does_not_arm_anything() {
        // The failure this prevents is the whole reason the module exists: the key
        // has been in the port since the operator opened the preview, so "it is
        // there" must never be read as "it has just been plugged in".
        let start = Instant::now();
        let mut handshake = handshake(start);

        for tick in 1..=5 {
            let now = start + Duration::from_millis(tick * 10);
            assert_eq!(handshake.observe(Some(true), now), Reaction::Wait);
            assert!(matches!(handshake.stage(), Stage::AwaitingRemoval { .. }));
        }
    }

    #[test]
    fn a_key_pulled_out_and_put_back_fires_the_reset_the_moment_it_returns() {
        let start = Instant::now();
        let mut handshake = handshake(start);

        // An unanswered poll is not an absent key.
        assert_eq!(handshake.observe(None, start), Reaction::Wait);
        assert!(matches!(handshake.stage(), Stage::AwaitingRemoval { .. }));

        // Out.
        assert_eq!(
            handshake.observe(Some(false), start + Duration::from_millis(20)),
            Reaction::Wait
        );
        assert!(matches!(handshake.stage(), Stage::AwaitingInsertion { .. }));

        // Still out, for a while. Nothing is sent to an empty port.
        assert_eq!(
            handshake.observe(Some(false), start + Duration::from_millis(40)),
            Reaction::Wait
        );

        // Back — and this is the one observation that fires.
        let back = start + Duration::from_millis(60);
        assert_eq!(handshake.observe(Some(true), back), Reaction::Fire);
        assert!(matches!(handshake.stage(), Stage::Armed { .. }));
        assert_eq!(handshake.remaining(back), Some(Duration::from_millis(500)));

        // And it fires once: a caller that keeps polling must not send a second
        // reset to a key that is already being reset.
        assert_eq!(
            handshake.observe(Some(true), back + Duration::from_millis(10)),
            Reaction::Wait
        );
    }

    #[test]
    fn a_window_that_closes_before_the_reset_goes_out_is_reported_not_risked() {
        // Reached when the caller was handed `Fire` and could not act — a database
        // that closed under the panel. Sending the command late would earn the
        // refusal that reads like a hardware fault.
        let start = Instant::now();
        let mut handshake = handshake(start);
        handshake.observe(Some(false), start);
        assert_eq!(
            handshake.observe(Some(true), start + Duration::from_millis(10)),
            Reaction::Fire
        );

        let late = start + Duration::from_millis(10) + Duration::from_millis(501);
        assert_eq!(handshake.observe(Some(true), late), Reaction::Expired);
        assert_eq!(handshake.stage(), Stage::Expired);
        assert!(handshake.is_finished());
        assert_eq!(handshake.remaining(late), None);

        // Terminal, and quiet: the entry is written once.
        assert_eq!(
            handshake.observe(Some(true), late + Duration::from_millis(10)),
            Reaction::Wait
        );

        let (event, detail) = handshake.audit_for(Reaction::Expired).expect("an entry");
        assert_eq!(event, EVENT_ABANDONED);
        assert!(detail.contains("was not sent"), "{detail}");
    }

    #[test]
    fn an_operator_who_walks_away_ends_the_handshake_instead_of_polling_for_ever() {
        // The poll is a subprocess in the fallback build. A handshake nobody
        // finishes must not fork one every half second all afternoon.
        let start = Instant::now();
        let mut handshake = handshake(start);

        assert_eq!(
            handshake.observe(Some(true), start + Duration::from_millis(1_999)),
            Reaction::Wait
        );
        assert_eq!(
            handshake.observe(Some(true), start + Duration::from_millis(2_000)),
            Reaction::GaveUp
        );
        assert_eq!(handshake.stage(), Stage::GaveUp);

        let (event, detail) = handshake.audit_for(Reaction::GaveUp).expect("an entry");
        assert_eq!(event, EVENT_ABANDONED);
        assert!(detail.contains("not re-inserted within 2s"), "{detail}");
    }

    #[test]
    fn giving_up_also_covers_a_key_that_never_comes_back() {
        let start = Instant::now();
        let mut handshake = handshake(start);
        handshake.observe(Some(false), start);
        assert_eq!(
            handshake.observe(Some(false), start + Duration::from_millis(2_000)),
            Reaction::GaveUp
        );
    }

    #[test]
    fn asking_again_keeps_the_confirmed_selection_and_counts_the_attempt() {
        // The retry must not become a way to reset something the operator did not
        // agree to: the applets are frozen at the click and carried, not re-read.
        let start = Instant::now();
        let mut handshake = handshake(start);
        assert_eq!(handshake.applets(), &[Applet::Fido2, Applet::Piv]);
        handshake.observe(Some(true), start + Duration::from_millis(2_000));
        assert!(handshake.is_finished());

        let again = start + Duration::from_secs(5);
        handshake.restart(again);
        assert_eq!(handshake.attempts(), 2);
        assert!(!handshake.is_finished());
        assert!(matches!(handshake.stage(), Stage::AwaitingRemoval { .. }));
        assert_eq!(handshake.applets(), &[Applet::Fido2, Applet::Piv]);
        assert!(handshake.requested().1.contains("attempt=2"));
    }

    #[test]
    fn the_operator_may_arm_it_by_hand_when_the_port_is_slower_than_the_window() {
        let start = Instant::now();
        let mut handshake = handshake(start);
        handshake.observe(Some(false), start);

        assert_eq!(
            handshake.arm_now(start + Duration::from_millis(30)),
            Reaction::Fire
        );
        assert!(matches!(handshake.stage(), Stage::Armed { .. }));
    }

    #[test]
    fn every_step_says_what_to_do_and_that_nothing_has_been_written() {
        // The panel shows exactly these words, and a step with no instruction in it
        // is a step the operator has to guess at.
        let start = Instant::now();
        let mut handshake = handshake(start);
        assert!(handshake.title().contains("pull the key out"));
        assert!(handshake.detail().contains("Nothing has been written"));

        handshake.observe(Some(false), start);
        assert!(handshake.title().contains("plug it straight back in"));
        assert!(handshake.detail().contains("touch the key"));

        handshake.observe(Some(true), start + Duration::from_millis(10));
        assert!(handshake.title().contains("touch the key"));

        for stage in [Stage::Expired, Stage::GaveUp] {
            handshake.stage = stage;
            assert!(!handshake.title().is_empty());
            assert!(
                handshake.detail().contains("Nothing was written")
                    || handshake.detail().contains("nothing was written"),
                "{}",
                handshake.detail()
            );
        }
    }

    #[test]
    fn the_trail_names_the_power_cycle_it_asked_for_and_the_one_it_got() {
        let start = Instant::now();
        let mut handshake = handshake(start);

        let (event, detail) = handshake.requested();
        assert_eq!(event, EVENT_REQUESTED);
        assert!(detail.contains("applets=fido2+piv"), "{detail}");
        assert!(detail.contains("attempt=1"), "{detail}");

        handshake.observe(Some(false), start);
        let fire = handshake.observe(Some(true), start + Duration::from_millis(10));
        let (event, detail) = handshake.audit_for(fire).expect("an entry");
        assert_eq!(event, EVENT_ARMED);
        assert!(detail.contains("applets=fido2+piv"), "{detail}");
        assert!(detail.contains("detected_within=200ms"), "{detail}");

        // Waiting is not an event: one entry per poll would bury the trail.
        assert!(handshake.audit_for(Reaction::Wait).is_none());

        let (event, detail) = handshake.cancelled();
        assert_eq!(event, EVENT_ABANDONED);
        assert!(detail.contains("the operator cancelled"), "{detail}");
    }

    #[test]
    fn no_entry_this_handshake_writes_can_carry_a_secret() {
        // Nothing here is given one — the module holds a serial, a clock and a
        // read-only enumeration — and this is what says so.
        let start = Instant::now();
        let mut handshake = handshake(start);
        handshake.observe(Some(false), start);
        let fire = handshake.observe(Some(true), start + Duration::from_millis(5));

        let mut details = vec![handshake.requested().1, handshake.cancelled().1];
        details.extend(
            [fire, Reaction::Expired, Reaction::GaveUp]
                .into_iter()
                .filter_map(|r| handshake.audit_for(r))
                .map(|(_, detail)| detail),
        );
        for detail in details {
            assert!(!detail.contains("123456"), "{detail}");
            assert!(!detail.contains("12345678"), "{detail}");
        }
    }

    #[test]
    fn the_poll_is_slower_when_every_poll_is_a_subprocess() {
        assert_eq!(poll_for(true), POLL_NATIVE);
        assert_eq!(poll_for(false), POLL_SUBPROCESS);
        assert!(poll_for(false) > poll_for(true));
        assert!(
            poll_for(true) < crate::device::watch::POLL_INTERVAL,
            "arming happens inside a five-second window; the background watch does not"
        );
    }

    // ------------------------------------------------------- the presence thread

    /// Wait for a snapshot to satisfy `done`, or fail the test.
    fn eventually(watch: &PresenceWatch, what: &str, done: impl Fn(&Presence) -> bool) -> Presence {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            let snapshot = watch.snapshot();
            if done(&snapshot) {
                return snapshot;
            }
            assert!(
                Instant::now() < deadline,
                "timed out waiting for {what}; last snapshot: {snapshot:?}"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
    }

    fn device(serial: u32) -> DeviceInfo {
        DeviceInfo {
            serial,
            model: "YubiKey 5 NFC".into(),
            firmware: "5.7.4".into(),
            ..DeviceInfo::default()
        }
    }

    struct Shared(Arc<MockBackend>);

    impl YubiKeyBackend for Shared {
        fn list_serials(&self) -> crate::device::Result<Vec<u32>> {
            self.0.list_serials()
        }
        fn info(&self, serial: Option<u32>) -> crate::device::Result<DeviceInfo> {
            self.0.info(serial)
        }
        fn describe(&self) -> String {
            self.0.describe()
        }
    }

    #[test]
    fn the_watch_reports_the_one_serial_it_was_given_and_ignores_the_rest() {
        // A second key in the next port is not this key coming back. Arming on it
        // would send a reset into a five-second window that belongs to something
        // else — and the applets were confirmed for this serial.
        let backend = Arc::new(MockBackend::new(vec![device(SERIAL)]));
        let watch = PresenceWatch::start(
            Box::new(Shared(Arc::clone(&backend))),
            SERIAL,
            Duration::from_millis(10),
        );

        let there = eventually(&watch, "the first answer", |p| p.present.is_some());
        assert_eq!(there.present, Some(true));
        assert!(there.describe(SERIAL).contains("is in a port"));

        backend.set_devices(vec![device(11_111_111)]);
        let gone = eventually(&watch, "the key to leave", |p| p.present == Some(false));
        assert!(gone.last_error.is_none());
        assert!(gone.describe(SERIAL).contains("is not in a port"));

        backend.set_devices(vec![device(11_111_111), device(SERIAL)]);
        eventually(&watch, "the key to come back", |p| p.present == Some(true));
    }

    #[test]
    fn an_empty_port_is_an_answer_rather_than_a_failure() {
        let backend = Arc::new(MockBackend::new(Vec::new()));
        let watch = PresenceWatch::start(
            Box::new(Shared(Arc::clone(&backend))),
            SERIAL,
            Duration::from_millis(10),
        );
        let empty = eventually(&watch, "the empty port", |p| p.present.is_some());
        assert_eq!(empty.present, Some(false));
        assert!(empty.last_error.is_none());
    }

    #[test]
    fn a_transport_that_is_not_installed_stops_the_watch_and_says_so() {
        // The step could never complete, and an operator staring at "asking the
        // port…" for ever is worse than a sentence naming what is missing.
        let watch = PresenceWatch::start(Box::new(MissingTool), SERIAL, Duration::from_millis(5));
        let snapshot = eventually(&watch, "the watch to give up", |p| p.stopped.is_some());
        assert!(snapshot.stopped.as_ref().unwrap().contains("ykman"));
        assert!(snapshot.describe(SERIAL).starts_with("not watching"));

        let polls = watch.snapshot().polls;
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(watch.snapshot().polls, polls, "it really stopped");
    }

    #[test]
    fn a_busy_reader_keeps_the_watch_running_and_shows_the_reason() {
        let backend = Arc::new(MockBackend::failing("the reader is busy"));
        let watch = PresenceWatch::start(
            Box::new(Shared(Arc::clone(&backend))),
            SERIAL,
            Duration::from_millis(5),
        );
        let snapshot = eventually(&watch, "a failing poll", |p| p.last_error.is_some());
        assert!(snapshot.stopped.is_none(), "a busy reader may free up");
        assert_eq!(
            snapshot.present, None,
            "and it claims nothing about the port"
        );
        eventually(&watch, "it to keep trying", |p| p.polls >= 3);
    }

    #[test]
    fn dropping_the_watch_stops_the_thread_before_the_reset_runs() {
        // Load-bearing: the watch is dropped to get the transport out of the way,
        // and an enumeration still in flight while `ykman fido reset` runs is
        // exactly what that is for.
        let backend = Arc::new(MockBackend::new(vec![device(SERIAL)]));
        let watch = PresenceWatch::start(
            Box::new(Shared(Arc::clone(&backend))),
            SERIAL,
            // Longer than this test will wait: dropping must not sit out an interval.
            Duration::from_secs(30),
        );
        eventually(&watch, "the first poll", |p| p.polls > 0);

        let start = Instant::now();
        drop(watch);
        assert!(
            start.elapsed() < Duration::from_secs(1),
            "dropping took {:?}",
            start.elapsed()
        );
    }

    struct MissingTool;

    impl YubiKeyBackend for MissingTool {
        fn list_serials(&self) -> crate::device::Result<Vec<u32>> {
            Err(DeviceError::ToolMissing {
                binary: "ykman".into(),
            })
        }
        fn info(&self, _serial: Option<u32>) -> crate::device::Result<DeviceInfo> {
            Err(DeviceError::ToolMissing {
                binary: "ykman".into(),
            })
        }
        fn describe(&self) -> String {
            "no transport".into()
        }
    }
}
