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
    /// What the **management** applet says: form factor, which applications are
    /// enabled, FIPS state (`features/native-device-transport.md` phase 5).
    ///
    /// The one applet that answers about the *device* rather than about itself, and
    /// therefore the only authoritative answer to "is FIDO2 even switched on".
    /// Before it was read, the pre-flight had to warn on every applet-dependent
    /// step that it could not check.
    pub management: Option<super::mgmt::DeviceConfig>,
    /// One line per applet that could not be read, naming the applet and why.
    ///
    /// Shown to the operator rather than logged: a refusal that depends on a read is
    /// only as trustworthy as the read, so the gaps travel with the answer.
    pub unread: Vec<String>,
}

impl Snapshot {
    /// Did anything answer at all?
    ///
    /// The management applet is deliberately **not** counted: it says which
    /// applications are enabled, and nothing about whether any of them has been
    /// configured. A snapshot holding only a management read has learned nothing
    /// about the state the phase-5 refusal rests on, and reporting it as non-empty
    /// would let a caller trust `already_configured()`'s silence.
    pub fn is_empty(&self) -> bool {
        self.fido2.is_none() && self.piv.is_none() && self.otp.is_none()
    }

    /// Is this application enabled on the key, as the management applet reports it?
    ///
    /// `None` means the applet was not read, which is not `false` — the whole point
    /// of [`super::mgmt`]'s existence. `applet` is the pre-flight's own vocabulary
    /// (`FIDO2`, `PIV`, `OTP`), mapped here to the name the applet uses.
    pub fn application_enabled(&self, applet: &str) -> Option<bool> {
        let name = match applet {
            "FIDO2" => "FIDO2",
            "PIV" => "PIV",
            "OTP" => "Yubico OTP",
            _ => return None,
        };
        self.management.as_ref()?.usb_has(name)
    }

    /// The factory defaults this key is **known** to still carry, as sentences for
    /// the Inventory badge (`features/step-piv-pin-puk-management-key.md` phase 6).
    ///
    /// The wizard has warned about these since the pre-flight existed, and that is
    /// the wrong place for the only warning: it is seen once, by the operator who is
    /// about to fix it. Somebody auditing the fleet a year later needs to see it on
    /// the key's own row.
    ///
    /// A FIDO2 applet with **no PIN** is included, and it is not a "default" in the
    /// same sense — there is no value to change. It belongs here anyway: the state a
    /// badge is for is *this key was never configured*, and an unprotected FIDO2
    /// applet is that state as much as a factory PIV PIN is.
    ///
    /// Silence when nothing was read, always.
    pub fn factory_defaults(&self) -> Vec<String> {
        let mut lines: Vec<String> = self
            .piv
            .as_ref()
            .map(|piv| {
                piv.factory_defaults()
                    .into_iter()
                    .map(|name| format!("the {name} is still the factory default"))
                    .collect()
            })
            .unwrap_or_default();
        if let Some(fido2) = &self.fido2
            && !fido2.pin_set
        {
            lines.push("no FIDO2 PIN is set, so the applet is unprotected".to_owned());
        }
        lines
    }

    /// One line for a table cell: the count and the shortest honest summary.
    ///
    /// `None` when there is nothing to report, so a screen can leave the cell empty
    /// rather than printing "none" against every properly configured key.
    pub fn factory_default_badge(&self) -> Option<String> {
        let defaults = self.factory_defaults();
        match defaults.len() {
            0 => None,
            1 => Some(defaults.into_iter().next().unwrap()),
            n => Some(format!(
                "{n} factory defaults still present: {}",
                defaults.join("; ")
            )),
        }
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
    ///
    /// Neither is the **attestation certificate in slot f9**
    /// ([`PivState::configured_slots`]). Yubico programmes it during manufacture, so it
    /// is on every key the refusal is meant to let through — and counting it made the
    /// first real bootstrap impossible: the refusal fired on a key out of the box.
    pub fn already_configured(&self) -> Vec<String> {
        let mut evidence = Vec::new();
        if let Some(piv) = &self.piv {
            let slots = piv.configured_slots();
            if !slots.is_empty() {
                evidence.push(format!(
                    "PIV slot(s) {} already hold a certificate",
                    slots.join(", ")
                ));
            }
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
                if piv.management_key_changed() {
                    "changed"
                } else {
                    "at default (or unreadable)"
                },
                if piv.pin_changed_from_default() {
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
                "FIDO2: PIN {}{}{}, forced change {}{}",
                if fido2.pin_set { "set" } else { "not set" },
                // Read, never burned — `get_info` and `get_pin_retries` spend no
                // attempt, which is why this is safe on a screen the operator merely
                // opened (`features/step-fido2-pin.md` phase 8).
                match fido2.pin_retries {
                    Some(left) => format!(", {left} attempt(s) left"),
                    None => String::new(),
                },
                match fido2.remaining_credential_slots {
                    Some(free) => format!(", {free} credential slot(s) free"),
                    None => String::new(),
                },
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
        match &self.management {
            Some(config) => lines.extend(config.describe()),
            None => lines.push("management applet: not read".to_owned()),
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
        // The management applet is the only source for *which applications are
        // enabled*, and reading it is what lets the pre-flight say "the OTP
        // application is switched off" instead of "the tool could not check".
        match super::mgmt::read(serial) {
            Ok(config) => snapshot.management = Some(config),
            Err(e) => snapshot
                .unread
                .push(format!("the management applet was not read: {e}")),
        }
    } else {
        snapshot.unread.push(format!(
            "PIV, FIDO2 and the management applet were not read: this session reads through {}, \
             which has no applet state read — the native transport does \
             (features/native-device-transport.md)",
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
            management: None,
            unread: Vec::new(),
        };
        assert!(snapshot.already_configured().is_empty());
        assert!(!snapshot.is_empty());
    }

    #[test]
    fn the_factory_attestation_certificate_is_not_evidence_of_a_previous_bootstrap() {
        // The bug this test exists for: `piv::Key::list` reports slot f9 on every
        // YubiKey, because Yubico programmes the attestation certificate there during
        // manufacture. Counting it as evidence made the phase-5 refusal fire on every
        // key attached to the tool, so no key could be bootstrapped at all.
        let snapshot = Snapshot {
            fido2: Some(Fido2State::default()),
            piv: Some(piv(&["f9"])),
            otp: Some(OtpState::default()),
            management: None,
            unread: Vec::new(),
        };
        assert!(
            snapshot.already_configured().is_empty(),
            "a factory-fresh key must not be refused: {:?}",
            snapshot.already_configured()
        );

        // And it is still *described*, because what is on the card is what the operator
        // should be shown — only the refusal narrows.
        let piv_line = snapshot
            .describe()
            .into_iter()
            .find(|l| l.starts_with("PIV:"))
            .unwrap();
        assert!(piv_line.contains("f9"), "{piv_line}");

        // A real certificate alongside it still gives the key away, and the evidence
        // names only that slot.
        let bootstrapped = Snapshot {
            piv: Some(piv(&["9c", "f9"])),
            ..Snapshot::default()
        };
        let evidence = bootstrapped.already_configured();
        assert_eq!(evidence.len(), 1, "{evidence:?}");
        assert!(evidence[0].contains("9c"), "{}", evidence[0]);
        assert!(!evidence[0].contains("f9"), "{}", evidence[0]);
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
    fn a_key_still_on_its_factory_defaults_is_badged_and_says_which() {
        // `features/step-piv-pin-puk-management-key.md` phase 6. The wizard has
        // warned about this since the pre-flight existed; the point of the badge is
        // that somebody auditing the fleet a year later sees it too.
        let snapshot = Snapshot {
            piv: Some(PivState {
                pin_is_default: Some(true),
                puk_is_default: Some(true),
                management_key_is_default: Some(true),
                ..PivState::default()
            }),
            fido2: Some(Fido2State::default()),
            otp: Some(OtpState::default()),
            ..Snapshot::default()
        };
        let defaults = snapshot.factory_defaults();
        assert_eq!(defaults.len(), 4, "{defaults:?}");
        assert!(
            defaults.iter().any(|l| l.contains("PIV PIN")),
            "{defaults:?}"
        );
        assert!(
            defaults.iter().any(|l| l.contains("PIV PUK")),
            "{defaults:?}"
        );
        assert!(
            defaults.iter().any(|l| l.contains("management key")),
            "{defaults:?}"
        );
        assert!(
            defaults.iter().any(|l| l.contains("FIDO2")),
            "an unprotected FIDO2 applet is the same state a badge is for: {defaults:?}"
        );

        let badge = snapshot.factory_default_badge().expect("badged");
        assert!(badge.starts_with("4 factory defaults"), "{badge}");
    }

    #[test]
    fn an_applet_that_did_not_say_is_never_badged_as_holding_a_default() {
        // The distinction the `Option<bool>` exists for. A busy reader answers
        // nothing, and a badge that accused a properly configured key of holding a
        // factory PIN is a badge an operator learns to ignore — after which it is
        // worth less than no badge at all.
        let unread = Snapshot {
            piv: Some(PivState::default()),
            ..Snapshot::default()
        };
        assert!(unread.factory_defaults().is_empty());
        assert_eq!(unread.factory_default_badge(), None);

        // And a key that has been through the procedure is badged for nothing.
        let configured = Snapshot {
            piv: Some(PivState {
                pin_is_default: Some(false),
                puk_is_default: Some(false),
                management_key_is_default: Some(false),
                ..PivState::default()
            }),
            fido2: Some(Fido2State {
                pin_set: true,
                ..Fido2State::default()
            }),
            ..Snapshot::default()
        };
        assert_eq!(configured.factory_default_badge(), None);
    }

    #[test]
    fn a_single_default_reads_as_a_sentence_rather_than_a_count() {
        let snapshot = Snapshot {
            piv: Some(PivState {
                pin_is_default: Some(true),
                ..PivState::default()
            }),
            fido2: Some(Fido2State {
                pin_set: true,
                ..Fido2State::default()
            }),
            ..Snapshot::default()
        };
        let badge = snapshot.factory_default_badge().expect("badged");
        assert_eq!(badge, "the PIV PIN is still the factory default");
    }

    #[test]
    fn a_managed_key_whose_management_key_was_changed_is_not_treated_as_bootstrapped() {
        // The one signal deliberately excluded: a fleet-management tool may have set
        // it without ever bootstrapping the key, and refusing on it would refuse keys
        // that are merely under management.
        let snapshot = Snapshot {
            piv: Some(PivState {
                management_key_is_default: Some(false),
                pin_is_default: Some(false),
                puk_is_default: Some(false),
                ..PivState::default()
            }),
            fido2: Some(Fido2State::default()),
            otp: Some(OtpState::default()),
            management: None,
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
