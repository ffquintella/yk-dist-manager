//! The Yubico OTP applet over CCID: which slots are programmed, and clearing one
//! (`features/native-device-transport.md` phase 4a).
//!
//! ## Why this is not the frame the reference deliberately leaves unwritten
//!
//! `docs/yubikey-reference.md` records a decision, not a backlog item: the Yubico
//! OTP **configuration frame over USB HID** stays unwritten until there is a key
//! to verify it against, because a wrong frame there leaves a slot
//! write-protected by an access code nobody holds, and nothing in this tool can
//! recover from that.
//!
//! That decision is about *programming a slot*. This module does something with a
//! different shape, over a different wire:
//!
//! * **Over CCID, not HID.** The applet answers on AID `A0 00 00 05 27 20 01`, and
//!   the card protocol carries the framing — there is no seven-byte-chunked frame
//!   to build and no CRC to compute. The two things most likely to be got wrong
//!   are not ours to get wrong. [`super::mgmt`] already reaches an applet this way.
//! * **The payload is zero.** Clearing a slot writes an all-zero configuration.
//!   The bytes that would carry an access code are among the zeros, so the
//!   feared outcome — a slot protected by a value nobody holds — is not reachable
//!   from here: there is no value.
//! * **The write is checked.** The applet answers every configuration write with
//!   its status structure, so this re-reads the valid flags and confirms the slot
//!   is gone. A write that did not land is reported as a refusal rather than
//!   claimed as a reset.
//!
//! What stays on `ykman`, unchanged and by the same decision: **setting** an
//! access code and **programming** a slot (`features/step-otp-access-code.md`).
//!
//! ## Which key
//!
//! The OTP applet's own status structure carries no serial number, so the card is
//! identified the way [`super::mgmt`] identifies it — by asking the management
//! applet on the same card — before the OTP applet is selected at all. A reader
//! name carries the model and nothing more, and resetting the wrong key is the
//! worst available way to discover that.
//!
//! ## Not hardware-verified
//!
//! The status parsing and the payload are pure and covered by tests. The **card
//! exchange** was written with no key attached and says so, as
//! [`super::piv_session`] and [`super::mgmt`] do.

use super::write::{OtpState, Result, WriteError};

/// The Yubico OTP application id, selected over CCID.
#[cfg_attr(not(feature = "ccid"), allow(dead_code))]
const OTP_AID: [u8; 7] = [0xA0, 0x00, 0x00, 0x05, 0x27, 0x20, 0x01];

/// The applet's one instruction: write a configuration. `P1` says which
/// configuration.
#[cfg_attr(not(feature = "ccid"), allow(dead_code))]
const INS_CONFIG: u8 = 0x01;

/// `P1` for each slot's configuration. Not 1 and 2: the applet numbers its
/// configurations, and slot two is the third.
#[cfg_attr(not(feature = "ccid"), allow(dead_code))]
const CONFIG_SLOT_ONE: u8 = 0x01;
#[cfg_attr(not(feature = "ccid"), allow(dead_code))]
const CONFIG_SLOT_TWO: u8 = 0x03;

/// The configuration structure's size, as the applet defines it: the fixed
/// prefix, the uid, the key, the access code, the four flag bytes, two reserved
/// and the checksum.
const CONFIG_SIZE: usize = 52;

/// The current access code that accompanies a configuration write. Zero here, and
/// zero is what an unprotected slot holds.
const ACC_CODE_SIZE: usize = 6;

/// Bits of the status structure's flag word that say a slot holds a
/// configuration.
const FLAG_SLOT_ONE_VALID: u16 = 0x01;
const FLAG_SLOT_TWO_VALID: u16 = 0x02;

/// What the applet says about itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Status {
    /// Firmware, as the applet reports it. Kept because it is what makes a
    /// response recognisable as a status structure at all.
    pub version: (u8, u8, u8),
    /// Incremented by the applet every time a configuration is written. The
    /// cheapest evidence that a write was accepted.
    pub program_sequence: u8,
    /// The flag word carrying the two valid bits, and the touch and LED bits this
    /// tool has no use for.
    pub flags: u16,
}

impl Status {
    /// Does this slot hold a configuration?
    pub fn slot_programmed(&self, slot: u8) -> bool {
        let bit = match slot {
            1 => FLAG_SLOT_ONE_VALID,
            2 => FLAG_SLOT_TWO_VALID,
            _ => return false,
        };
        self.flags & bit != 0
    }

    /// The applet's state in the shape the rest of the tool reads.
    ///
    /// `access_code_set` stays false: no read of any kind reports it, which
    /// [`OtpState`] documents at its own field. It is the *record* that says
    /// whether this tool ever set one.
    pub fn state(&self) -> OtpState {
        OtpState {
            slot_one_programmed: self.slot_programmed(1),
            slot_two_programmed: self.slot_programmed(2),
            access_code_set: false,
        }
    }
}

/// Read a status structure out of an applet response.
///
/// `None` for anything that is not one. The version's major byte is checked
/// against the firmware families that exist, and that check is the point: a
/// response of the wrong shape would otherwise parse as *both slots empty*, and
/// this module's caller would report a key with two programmed slots as needing
/// nothing done. Silence must not read as a clean key.
pub fn parse_status(response: &[u8]) -> Option<Status> {
    if response.len() < 6 {
        return None;
    }
    let version = (response[0], response[1], response[2]);
    if version.0 == 0 || version.0 > 9 {
        return None;
    }
    Some(Status {
        version,
        program_sequence: response[3],
        flags: u16::from(response[4]) | u16::from(response[5]) << 8,
    })
}

/// The payload that clears a slot: an all-zero configuration, and an all-zero
/// current access code behind it.
///
/// A function rather than a constant so the two properties that make this safe
/// can be asserted against the thing that is actually sent.
pub fn clear_payload() -> Vec<u8> {
    vec![0u8; CONFIG_SIZE + ACC_CODE_SIZE]
}

/// The `P1` for a slot, or `None` for a slot number that does not exist.
pub fn config_slot(slot: u8) -> Option<u8> {
    match slot {
        1 => Some(CONFIG_SLOT_ONE),
        2 => Some(CONFIG_SLOT_TWO),
        _ => None,
    }
}

/// Which slots of this key hold a configuration.
#[cfg(feature = "ccid")]
pub fn state(serial: u32) -> Result<OtpState> {
    const OP: &str = "otp.info";
    let session = Session::open(serial, OP)?;
    Ok(session.status.state())
}

/// Not compiled without a card transport. The caller reports the gap rather than
/// receiving a default, because a default here is the claim that both slots are
/// empty.
#[cfg(not(feature = "ccid"))]
pub fn state(serial: u32) -> Result<OtpState> {
    let _ = serial;
    Err(WriteError::TransportUnavailable {
        operation: "otp.info",
        feature: "native-otp",
    })
}

/// Clear every programmed slot, and say what was cleared.
///
/// The read comes first for the reason [`super::reset`] already states about the
/// fallback: asking the applet to clear a slot it never held would turn "the OTP
/// application is switched off" into "the reset did not work", and those need
/// different answers from the operator. Over CCID the applet not answering the
/// select is itself the first of those answers.
#[cfg(feature = "ccid")]
pub fn clear_slots(serial: u32) -> Result<Option<String>> {
    const OP: &str = "otp.reset";
    let mut session = Session::open(serial, OP)?;

    let slots: Vec<u8> = [1u8, 2]
        .into_iter()
        .filter(|slot| session.status.slot_programmed(*slot))
        .collect();
    if slots.is_empty() {
        return Ok(None);
    }

    for slot in &slots {
        session.clear_slot(*slot, OP)?;
    }

    // Then read the applet again, rather than believing the status word. Two
    // different things would otherwise be recorded as a reset: a write the applet
    // accepted and did nothing with, and a write whose answer this build could not
    // parse. The re-select costs one APDU and is the only evidence that means
    // anything here.
    session.select_otp(OP)?;
    let still_there: Vec<String> = slots
        .iter()
        .filter(|slot| session.status.slot_programmed(**slot))
        .map(u8::to_string)
        .collect();
    if !still_there.is_empty() {
        return Err(WriteError::Failed {
            operation: OP,
            reason: format!(
                "the applet accepted the write but OTP slot {} still holds a configuration — a \
                 slot protected by an access code refuses to be cleared without it, and this \
                 tool does not hold one",
                still_there.join(" and ")
            ),
        });
    }

    Ok(Some(format!(
        "OTP slot {} cleared natively over CCID (an all-zero configuration, confirmed by \
         re-reading the applet)",
        slots
            .iter()
            .map(u8::to_string)
            .collect::<Vec<_>>()
            .join(" and ")
    )))
}

#[cfg(not(feature = "ccid"))]
pub fn clear_slots(serial: u32) -> Result<Option<String>> {
    let _ = serial;
    Err(WriteError::TransportUnavailable {
        operation: "otp.reset",
        feature: "native-otp",
    })
}

/// The CCID conversation with the OTP applet.
///
/// Its own session, for the reason [`super::mgmt`] gives: selecting an applet
/// discards the previously selected one's state. This one selects the management
/// applet first — only to establish which key it is holding — and then the OTP
/// applet, and does not go back.
#[cfg(feature = "ccid")]
struct Session {
    card: pcsc::Card,
    /// The last status structure the applet gave, from the select or from the
    /// most recent write.
    status: Status,
}

#[cfg(feature = "ccid")]
impl Session {
    fn open(serial: u32, operation: &'static str) -> Result<Self> {
        let ctx = pcsc::Context::establish(pcsc::Scope::User).map_err(|e| WriteError::Failed {
            operation,
            reason: format!("no PC/SC service: {e}"),
        })?;
        let mut names = vec![0u8; ctx.list_readers_len().unwrap_or(2048)];
        let readers = ctx
            .list_readers(&mut names)
            .map_err(|e| WriteError::Failed {
                operation,
                reason: format!("no readers: {e}"),
            })?;

        for reader in readers {
            if !reader.to_string_lossy().to_lowercase().contains("yubikey") {
                continue;
            }
            let Ok(card) = ctx.connect(reader, pcsc::ShareMode::Shared, pcsc::Protocols::ANY)
            else {
                continue;
            };
            let mut candidate = Self {
                card,
                status: Status {
                    version: (0, 0, 0),
                    program_sequence: 0,
                    flags: 0,
                },
            };
            if !candidate.is_key(serial, operation) {
                continue;
            }
            // The right key. From here a failure to reach the applet is reported
            // rather than skipped past: another reader will not answer for it.
            candidate.select_otp(operation)?;
            return Ok(candidate);
        }
        Err(WriteError::NotAttached(serial))
    }

    /// Is this card the key the caller named? Asked of the management applet,
    /// whose encoding [`super::mgmt`] already parses.
    fn is_key(&mut self, serial: u32, operation: &'static str) -> bool {
        let mut apdu = vec![0x00, 0xA4, 0x04, 0x00, super::mgmt::MGMT_AID.len() as u8];
        apdu.extend_from_slice(&super::mgmt::MGMT_AID);
        if !matches!(self.transmit(&apdu, operation), Ok((_, 0x9000))) {
            return false;
        }
        let Ok((data, 0x9000)) = self.transmit(
            &[0x00, super::mgmt::INS_READ_CONFIG, 0x00, 0x00, 0x00],
            operation,
        ) else {
            return false;
        };
        super::mgmt::parse_page(&data)
            .map(|fields| super::mgmt::from_tlvs(&fields).serial == Some(serial))
            .unwrap_or(false)
    }

    fn select_otp(&mut self, operation: &'static str) -> Result<()> {
        let mut apdu = vec![0x00, 0xA4, 0x04, 0x00, OTP_AID.len() as u8];
        apdu.extend_from_slice(&OTP_AID);
        let (data, sw) = self.transmit(&apdu, operation)?;
        if sw != 0x9000 {
            return Err(WriteError::Unsupported {
                operation,
                reason: format!(
                    "the Yubico OTP application did not answer over CCID (status 0x{sw:04x}) — it \
                     is switched off on this key, or CCID is"
                ),
            });
        }
        self.status = parse_status(&data).ok_or_else(|| WriteError::Failed {
            operation,
            reason: "the OTP application answered the select with something that is not its \
                     status structure, so which slots are programmed cannot be read — it is not \
                     read as 'both empty'"
                .to_owned(),
        })?;
        Ok(())
    }

    /// Write an all-zero configuration to one slot.
    ///
    /// The applet answers with a fresh status structure, which is kept when it
    /// parses — but [`clear_slots`] re-selects afterwards regardless, because a
    /// write whose answer could not be read must not be the thing that decides
    /// whether the slot is gone.
    fn clear_slot(&mut self, slot: u8, operation: &'static str) -> Result<()> {
        let p1 = config_slot(slot).ok_or_else(|| WriteError::Failed {
            operation,
            reason: format!("there is no OTP slot {slot}"),
        })?;
        let payload = clear_payload();
        let mut apdu = vec![0x00, INS_CONFIG, p1, 0x00, payload.len() as u8];
        apdu.extend_from_slice(&payload);

        let (data, sw) = self.transmit(&apdu, operation)?;
        if sw != 0x9000 {
            return Err(WriteError::Failed {
                operation,
                reason: format!("clearing OTP slot {slot}: card status 0x{sw:04x}"),
            });
        }
        if let Some(status) = parse_status(&data) {
            self.status = status;
        }
        Ok(())
    }

    /// Send one APDU, following `61xx` continuations.
    fn transmit(&mut self, apdu: &[u8], operation: &'static str) -> Result<(Vec<u8>, u16)> {
        let mut collected = Vec::new();
        let mut request = apdu.to_vec();
        loop {
            let mut buf = vec![0u8; 1024];
            let response =
                self.card
                    .transmit(&request, &mut buf)
                    .map_err(|e| WriteError::Failed {
                        operation,
                        reason: format!("the card did not answer: {e}"),
                    })?;
            if response.len() < 2 {
                return Err(WriteError::Failed {
                    operation,
                    reason: "truncated response from the card".into(),
                });
            }
            let split = response.len() - 2;
            let sw = u16::from(response[split]) << 8 | u16::from(response[split + 1]);
            collected.extend_from_slice(&response[..split]);
            if sw & 0xFF00 == 0x6100 {
                request = vec![0x00, 0xC0, 0x00, 0x00, (sw & 0x00FF) as u8];
                continue;
            }
            return Ok((collected, sw));
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(flags: u16) -> Vec<u8> {
        vec![5, 7, 4, 3, (flags & 0xFF) as u8, (flags >> 8) as u8]
    }

    #[test]
    fn the_valid_bits_are_what_say_a_slot_is_programmed() {
        // Given a factory-fresh key
        let empty = parse_status(&status(0x00)).expect("a status structure");
        assert!(!empty.slot_programmed(1));
        assert!(!empty.slot_programmed(2));

        // When one slot holds a configuration
        let one = parse_status(&status(0x01)).expect("a status structure");
        assert!(one.slot_programmed(1));
        assert!(!one.slot_programmed(2));

        // Then the other bit is the other slot, and the touch bits are neither
        let two = parse_status(&status(0x0A)).expect("a status structure");
        assert!(
            !two.slot_programmed(1),
            "0x08 is slot two's touch bit, not slot one's valid bit"
        );
        assert!(two.slot_programmed(2));
    }

    #[test]
    fn a_response_that_is_not_a_status_structure_is_not_read_as_an_empty_key() {
        // This is the whole reason the version is checked. A key with two
        // programmed slots reported as empty would be recorded as needing no
        // reset — the register would then say a key was returned to factory
        // default when it still carried the previous holder's credential.
        assert_eq!(parse_status(&[]), None);
        assert_eq!(
            parse_status(&[5, 7, 4, 3, 0]),
            None,
            "five bytes is not six"
        );
        assert_eq!(
            parse_status(&[0, 0, 0, 0, 0, 0]),
            None,
            "an all-zero answer is not firmware 0.0.0"
        );
        assert_eq!(
            parse_status(&[0x6A, 0x82, 0, 0, 0, 0]),
            None,
            "a status word read as a structure would say both slots are empty"
        );
    }

    #[test]
    fn the_version_is_kept_because_it_is_what_makes_the_answer_recognisable() {
        let parsed = parse_status(&status(0x03)).expect("a status structure");
        assert_eq!(parsed.version, (5, 7, 4));
        assert_eq!(parsed.program_sequence, 3);
    }

    #[test]
    fn clearing_a_slot_can_carry_no_access_code_because_it_carries_nothing() {
        // The decision recorded in docs/yubikey-reference.md fears one outcome: a
        // slot protected by a code nobody holds. The payload sent here is the
        // reason that outcome is unreachable from this module.
        let payload = clear_payload();
        assert_eq!(payload.len(), 58, "the configuration and the access code");
        assert!(
            payload.iter().all(|b| *b == 0),
            "every byte is zero, so no byte is a code"
        );
    }

    #[test]
    fn slot_two_is_the_applets_third_configuration_not_its_second() {
        assert_eq!(config_slot(1), Some(0x01));
        assert_eq!(config_slot(2), Some(0x03));
        assert_eq!(config_slot(0), None);
        assert_eq!(config_slot(3), None, "there are two slots");
    }

    #[test]
    fn the_state_handed_upwards_never_claims_to_know_about_an_access_code() {
        let parsed = parse_status(&status(0x03)).expect("a status structure");
        let state = parsed.state();
        assert!(state.slot_one_programmed);
        assert!(state.slot_two_programmed);
        assert!(
            !state.access_code_set,
            "no read of any kind reports this — only the register knows"
        );
    }
}
