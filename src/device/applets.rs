//! Read what state a key's applets are in, without writing anything
//! (`features/device-detection.md` phase 4).
//!
//! ## Why this is its own module rather than a method on the write backend
//!
//! The three state reads already existed — `Fido2Writer::fido2_state`,
//! `PivWriter::piv_state`, `OtpWriter::otp_state` — but they sat on the *write*
//! traits, reachable only with a `WriteBackend` in hand. So the pre-flight, which is
//! the one place that most needs them, said:
//!
//! ```text
//! // No applet snapshot yet: reading one needs a write-capable transport open,
//! // and this build has none by default.
//! let applets = AppletSnapshot::default();
//! ```
//!
//! and every check that depended on applet state produced no finding. The checks were
//! written, and never fired. This module is the read side, callable from a screen.
//!
//! ## Partial answers are the normal case, not a failure
//!
//! Three applets, three transports, three ways to be unavailable: PIV needs PC/SC,
//! FIDO2 needs HID, OTP needs `ykman` until its status frame is implemented. A key
//! with FIDO2 disabled over USB is a *normal* key, not a broken read.
//!
//! So each applet is read independently and a failure is recorded as a **reason**
//! rather than dropped. That distinction is the whole point: "PIV slot 9c is empty"
//! and "PIV was not read" lead to opposite decisions about whether it is safe to
//! generate a key there, and a snapshot that cannot tell them apart would make the
//! phase-5 refusal unsound.
//!
//! ## Nothing here writes
//!
//! Every call below is a read: `get_info`, `piv::Key::list`, `get_pin_retries`,
//! `ykman otp info`. That is what makes this safe to call from a screen the operator
//! merely opened — AGENTS.md forbids a hardware write as a side effect of opening one.

use super::write::{Fido2State, OtpState, PivState};
use super::{Transport, TransportChoice, write::WriteError};

/// What was read, and what was not.
///
/// Deliberately not `Result<_>` for the whole thing: a snapshot in which two applets
/// answered and one did not is useful, and collapsing it to an error would throw away
/// the two answers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Snapshot {
    pub fido2: Option<Fido2State>,
    pub piv: Option<PivState>,
    pub otp: Option<OtpState>,
    /// One line per applet that could not be read, naming the applet and why.
    ///
    /// Shown to the operator rather than logged: a refusal that depends on a read is
    /// only as trustworthy as the read, so the gaps travel with the answer.
    pub unread: Vec<String>,
}

impl Snapshot {
    /// Did anything answer at all?
    pub fn is_empty(&self) -> bool {
        self.fido2.is_none() && self.piv.is_none() && self.otp.is_none()
    }

    /// Does this key already carry a configuration this tool would have applied?
    ///
    /// The question phase 5 turns into a refusal. Three signals, any one of which
    /// means somebody has been here before:
    ///
    /// * a **certificate in a PIV slot** — the signing identity the procedure creates;
    /// * a **FIDO2 PIN set** — the applet is no longer at factory default;
    /// * a **programmed OTP slot** — likewise.
    ///
    /// A changed PIV management key is deliberately *not* one of them. It is the one
    /// piece of state a fleet-management tool may legitimately have set without ever
    /// bootstrapping the key, and treating it as evidence would refuse keys that are
    /// merely under management.
    pub fn already_configured(&self) -> Vec<String> {
        let mut evidence = Vec::new();
        if let Some(piv) = &self.piv
            && !piv.occupied_slots.is_empty()
        {
            evidence.push(format!(
                "PIV slot(s) {} already hold a certificate",
                piv.occupied_slots.join(", ")
            ));
        }
        if let Some(fido2) = &self.fido2
            && fido2.pin_set
        {
            evidence.push("a FIDO2 PIN is already set".to_owned());
        }
        if let Some(otp) = &self.otp {
            let slots: Vec<&str> = [
                otp.slot_one_programmed.then_some("1"),
                otp.slot_two_programmed.then_some("2"),
            ]
            .into_iter()
            .flatten()
            .collect();
            if !slots.is_empty() {
                evidence.push(format!("OTP slot {} is programmed", slots.join(" and ")));
            }
        }
        evidence
    }

    /// One line per applet, for a screen. Never a secret — states, counts and slots.
    pub fn describe(&self) -> Vec<String> {
        let mut lines = Vec::new();
        match &self.piv {
            Some(piv) => lines.push(format!(
                "PIV: slots [{}], management key {}, PIN {}{}",
                piv.occupied_slots.join(", "),
                if piv.management_key_changed {
                    "changed"
                } else {
                    "at default (or unreadable)"
                },
                if piv.pin_changed_from_default {
                    "changed"
                } else {
                    "at default (or unreadable)"
                },
                match piv.pin_retries {
                    Some(left) => format!(", {left} attempt(s) left"),
                    None => String::new(),
                }
            )),
            None => lines.push("PIV: not read".to_owned()),
        }
        match &self.fido2 {
            Some(fido2) => lines.push(format!(
                "FIDO2: PIN {}, forced change {}{}",
                if fido2.pin_set { "set" } else { "not set" },
                if fido2.force_pin_change_set {
                    "pending"
                } else {
                    "not pending"
                },
                match fido2.min_pin_length {
                    Some(length) => format!(", minimum PIN length {length}"),
                    None => ", no minimum-length policy (firmware predates it)".to_owned(),
                }
            )),
            None => lines.push("FIDO2: not read".to_owned()),
        }
        match &self.otp {
            Some(otp) => lines.push(format!(
                "OTP: slot 1 {}, slot 2 {}",
                if otp.slot_one_programmed {
                    "programmed"
                } else {
                    "empty"
                },
                if otp.slot_two_programmed {
                    "programmed"
                } else {
                    "empty"
                }
            )),
            None => lines.push("OTP: not read".to_owned()),
        }
        lines.extend(self.unread.iter().cloned());
        lines
    }
}

/// Read all three applets of one key.
///
/// `choice` is the session's transport (`device::select`), so a build that decided on
/// `ykman` does not open a PC/SC context it has already been told will not answer.
///
/// Read-only, and every failure is a line rather than an early return.
pub fn read(serial: u32, choice: &TransportChoice) -> Snapshot {
    let mut snapshot = Snapshot::default();

    if choice.transport == Transport::Native {
        read_native(serial, &mut snapshot);
    } else {
        snapshot.unread.push(format!(
            "PIV and FIDO2 were not read: this session reads through {}, which has no applet \
             state read — the native transport does (features/native-device-transport.md)",
            choice.transport.label()
        ));
    }

    // OTP goes through `ykman` whatever the session transport is, because the native
    // status frame is not implemented (`native-device-transport.md` phase 4). Labelled
    // as the fallback it is, rather than presented as a native read.
    let ykman = super::YkmanBackend::default();
    match super::ykman::otp_state(&ykman, serial) {
        Ok(state) => snapshot.otp = Some(state),
        Err(e) => snapshot
            .unread
            .push(format!("OTP was not read (via `ykman otp info`): {e}")),
    }

    snapshot
}

#[cfg(feature = "native-piv")]
fn read_native(serial: u32, snapshot: &mut Snapshot) {
    use super::write::{Fido2Writer, PivWriter};

    let mut backend = super::composite::NativeBackend::for_key(serial);
    match backend.piv_state(serial) {
        Ok(state) => snapshot.piv = Some(state),
        Err(e) => snapshot.unread.push(describe_gap("PIV", &e)),
    }
    match backend.fido2_state(serial) {
        Ok(state) => snapshot.fido2 = Some(state),
        Err(e) => snapshot.unread.push(describe_gap("FIDO2", &e)),
    }
}

#[cfg(not(feature = "native-piv"))]
fn read_native(serial: u32, snapshot: &mut Snapshot) {
    let _ = serial;
    snapshot.unread.push(
        "PIV and FIDO2 were not read: this build has no native transport — rebuild with \
         `--features native-device`"
            .to_owned(),
    );
}

/// Turn a failed read into a line an operator can act on.
///
/// A disabled applet is the most common reason and is not a fault, so it is worded as
/// a state. Anything else keeps the transport's own message, which already names the
/// operation.
#[cfg_attr(not(feature = "native-piv"), allow(dead_code))]
fn describe_gap(applet: &str, error: &WriteError) -> String {
    format!("{applet} was not read: {error}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn piv(slots: &[&str]) -> PivState {
        PivState {
            occupied_slots: slots.iter().map(|s| s.to_string()).collect(),
            ..PivState::default()
        }
    }

    #[test]
    fn a_factory_fresh_key_shows_no_evidence_of_a_previous_bootstrap() {
        let snapshot = Snapshot {
            fido2: Some(Fido2State::default()),
            piv: Some(piv(&[])),
            otp: Some(OtpState::default()),
            unread: Vec::new(),
        };
        assert!(snapshot.already_configured().is_empty());
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn each_applet_can_be_the_one_that_gives_it_away() {
        // Any single signal is enough. A run that overwrote one applet because the
        // other two looked untouched is exactly the failure phase 5 prevents.
        let cert = Snapshot {
            piv: Some(piv(&["9c"])),
            ..Snapshot::default()
        };
        assert_eq!(cert.already_configured().len(), 1);
        assert!(cert.already_configured()[0].contains("9c"));

        let pin = Snapshot {
            fido2: Some(Fido2State {
                pin_set: true,
                ..Fido2State::default()
            }),
            ..Snapshot::default()
        };
        assert_eq!(pin.already_configured().len(), 1);

        let otp = Snapshot {
            otp: Some(OtpState {
                slot_two_programmed: true,
                ..OtpState::default()
            }),
            ..Snapshot::default()
        };
        assert_eq!(otp.already_configured().len(), 1);
        assert!(otp.already_configured()[0].contains('2'));
    }

    #[test]
    fn a_managed_key_whose_management_key_was_changed_is_not_treated_as_bootstrapped() {
        // The one signal deliberately excluded: a fleet-management tool may have set
        // it without ever bootstrapping the key, and refusing on it would refuse keys
        // that are merely under management.
        let snapshot = Snapshot {
            piv: Some(PivState {
                management_key_changed: true,
                pin_changed_from_default: true,
                ..PivState::default()
            }),
            fido2: Some(Fido2State::default()),
            otp: Some(OtpState::default()),
            unread: Vec::new(),
        };
        assert!(snapshot.already_configured().is_empty());
    }

    #[test]
    fn an_applet_that_was_not_read_is_never_reported_as_empty() {
        // The distinction the refusal rests on. `None` must not read as "clean", or a
        // key with a certificate in 9c and PC/SC held by another process would be
        // reported as safe to overwrite.
        let nothing = Snapshot::default();
        assert!(nothing.is_empty());
        assert!(
            nothing.already_configured().is_empty(),
            "an unread applet yields no evidence — which is why `is_empty` has to be \
             consulted before trusting that"
        );

        let described = nothing.describe();
        assert!(described.iter().any(|l| l.contains("PIV: not read")));
        assert!(described.iter().any(|l| l.contains("FIDO2: not read")));
        assert!(described.iter().any(|l| l.contains("OTP: not read")));
    }

    #[test]
    fn the_description_says_the_retry_counter_when_the_applet_reported_one() {
        // A key one wrong PIN from needing its PUK is context an operator wants
        // before starting a run, not after.
        let snapshot = Snapshot {
            piv: Some(PivState {
                pin_retries: Some(1),
                ..PivState::default()
            }),
            ..Snapshot::default()
        };
        let line = snapshot
            .describe()
            .into_iter()
            .find(|l| l.starts_with("PIV:"))
            .unwrap();
        assert!(line.contains("1 attempt(s) left"), "{line}");
    }

    #[test]
    fn a_gap_names_the_applet_and_keeps_the_transports_own_words() {
        let error = WriteError::TransportUnavailable {
            operation: "piv.metadata",
            feature: "native-piv",
        };
        let line = describe_gap("PIV", &error);
        assert!(line.starts_with("PIV was not read:"), "{line}");
        assert!(line.contains("native-piv"), "{line}");
    }

    #[test]
    fn a_session_on_the_subprocess_transport_says_why_two_applets_are_missing() {
        // Not silence: an operator who overrode the transport to `ykman` and then
        // wonders why the pre-flight stopped flagging a configured key deserves the
        // sentence that explains it.
        let choice = super::super::select::decide(
            Transport::Ykman,
            super::super::Availability {
                native_compiled: true,
                native_probe: None,
                ykman_present: true,
            },
        );
        // No hardware in a test run, so the OTP read fails too — what is asserted is
        // the *reason*, which is pure.
        let snapshot = read(20_423_633, &choice);
        assert!(
            snapshot.unread.iter().any(|l| l.contains("ykman")),
            "{:?}",
            snapshot.unread
        );
        assert!(snapshot.piv.is_none() && snapshot.fido2.is_none());
    }
}
