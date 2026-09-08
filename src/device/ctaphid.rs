//! `authenticatorReset` over CTAPHID, in process
//! (`features/native-device-transport.md` phase 2a).
//!
//! [`super::native_fido`] covers every FIDO2 operation the run needs except the
//! one the *reset* needs, because [`ctap_hid_fido2`] implements no
//! `authenticatorReset`: the command is not in its API surface and its CTAPHID
//! layer is a private module, so there is nothing to call and nothing to borrow.
//! That single gap is what kept [`super::reset`] shelling out to `ykman fido
//! reset` — and a workstation without `ykman` therefore could not reset the one
//! applet whose reset this tool exists to perform.
//!
//! So the frames are written here. There are three of them, and the whole
//! conversation is:
//!
//! 1. `CTAPHID_INIT` on the broadcast channel, with an eight-byte nonce, to be
//!    given a channel. The nonce is echoed and **checked**: a device that answers
//!    a different nonce is answering somebody else's request.
//! 2. `CTAPHID_CBOR` carrying one byte — `0x07`, `authenticatorReset`. The command
//!    takes no parameters, which is why this module is short: there is no CBOR to
//!    encode, only a byte to frame.
//! 3. Reads until the authenticator answers, forwarding nothing and waiting
//!    through `CTAPHID_KEEPALIVE` while it holds out for the touch.
//!
//! ## Why this is not the OTP decision over again
//!
//! `docs/yubikey-reference.md` records a deliberate refusal to hand-roll the
//! Yubico OTP configuration frame, because a wrong frame there leaves a slot
//! write-protected by a code nobody holds. Nothing of that shape exists here:
//! `authenticatorReset` carries no payload to get wrong, and the authenticator
//! either performs it or answers a status byte. A misframed request is refused,
//! not half-applied — the failure mode is "the reset did not happen", which is
//! exactly what the operator already sees when `ykman` is missing.
//!
//! ## The window, and who races it
//!
//! CTAP allows an authenticator to refuse a reset that does not arrive within a
//! few seconds of power-up, and Yubico's does. Winning that race is
//! [`super::reinsert`]'s job, not this module's — but it is worth noting what
//! moving off the subprocess buys: `ykman` had to start a Python interpreter
//! inside that window. This sends the frame from a process that is already
//! running.
//!
//! ## Not hardware-verified
//!
//! The framing is covered by unit tests built from the CTAPHID packet layout, and
//! the packet layout is the one [`ctap_hid_fido2`] uses on the key this repository
//! has already verified reads and writes against. The **exchange** was written
//! with no key attached, and says so — the same statement
//! [`super::piv_session`] and [`super::mgmt`] carry, for the same reason.

// Most of what is below is the frame layout, and in a build without the HID
// transport there is nothing to send it to. The constants stay compiled rather
// than being `cfg`-ed one by one, so the packet layout reads as one piece in every
// build — the `ykman`-only build simply has no caller for them. Applied only where
// that is true, so real dead code in the native build is still a warning.
#![cfg_attr(not(feature = "native-fido"), allow(dead_code))]

use super::write::{Result, WriteError};

/// The FIDO HID usage page. A security key is found by what it declares it
/// speaks, not by a vendor id: this list would otherwise have to name every
/// authenticator ever shipped.
const FIDO_USAGE_PAGE: u16 = 0xF1D0;
/// `FIDO_USAGE_CTAPHID`, the one usage within that page.
const FIDO_USAGE: u16 = 0x01;

/// A CTAPHID packet, before the report id is put in front of it.
const PACKET: usize = 64;
/// What an initialisation packet has room for after its seven-byte header.
const INIT_PAYLOAD: usize = PACKET - 7;

/// Bit 7 of the command byte, set on every initialisation packet.
const FRAME_INIT: u8 = 0x80;
const CTAPHID_INIT: u8 = FRAME_INIT | 0x06;
const CTAPHID_CBOR: u8 = FRAME_INIT | 0x10;
const CTAPHID_KEEPALIVE: u8 = FRAME_INIT | 0x3B;
const CTAPHID_ERROR: u8 = FRAME_INIT | 0x3F;

/// The channel every conversation starts on.
const BROADCAST_CID: [u8; 4] = [0xFF, 0xFF, 0xFF, 0xFF];

/// `authenticatorReset`. No parameters, which is the whole payload.
const AUTHENTICATOR_RESET: u8 = 0x07;

/// `CTAP1_ERR_SUCCESS`.
const CTAP_SUCCESS: u8 = 0x00;

/// How long to wait for the touch before giving up.
///
/// The authenticator's own patience, not ours: CTAP puts the user-presence
/// timeout at 30 seconds and Yubico's key blinks for about that long. Ending
/// earlier would abandon a reset the key is still willing to perform.
const TOUCH_PATIENCE_MS: u64 = 30_000;

/// One read's wait. Short enough that the deadline above is honoured to within a
/// blink, long enough not to spin.
const READ_TIMEOUT_MS: i32 = 250;

/// What the authenticator said, once the framing is off.
///
/// Separated from the exchange so the classification of every status byte is a
/// unit test rather than something that only shows up with a key in a port.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Answer {
    /// The applet is back at factory default.
    Reset,
    /// The authenticator refused, with its own status byte.
    Refused(u8),
}

/// Build an initialisation packet, report id included.
///
/// Returns the 65 bytes that go to the device: a leading `0x00` report id, then
/// the channel, the command, the big-endian payload length, and the payload.
/// Only payloads that fit one packet are built here — the two commands this
/// module sends are 8 bytes and 1 byte.
pub fn init_packet(cid: [u8; 4], command: u8, payload: &[u8]) -> Option<Vec<u8>> {
    if payload.len() > INIT_PAYLOAD {
        return None;
    }
    let mut packet = vec![0u8; 1 + PACKET];
    packet[1..5].copy_from_slice(&cid);
    packet[5] = command;
    packet[6] = (payload.len() >> 8) as u8;
    packet[7] = (payload.len() & 0xFF) as u8;
    packet[8..8 + payload.len()].copy_from_slice(payload);
    Some(packet)
}

/// The header of a response packet: its command, its declared payload length,
/// and the first payload byte.
///
/// A read comes back as the 64 bytes of the packet with no report id — which is
/// what `hidapi` delivers for a device whose reports are unnumbered — but a
/// platform that does prepend one is tolerated rather than misparsed.
pub fn response_header(packet: &[u8]) -> Option<(u8, u16, u8)> {
    let frame = match packet.len() {
        n if n > PACKET && packet[0] == 0x00 => &packet[1..],
        n if n >= 7 => packet,
        _ => return None,
    };
    if frame.len() < 8 {
        return None;
    }
    let command = frame[4];
    let declared = u16::from(frame[5]) << 8 | u16::from(frame[6]);
    Some((command, declared, frame[7]))
}

/// The channel the authenticator allocated, if this is our `CTAPHID_INIT`
/// answer.
///
/// The nonce is what makes it ours. Two processes may be initialising at the
/// same time on the broadcast channel, and taking the first answer that arrives
/// would take the other one's channel.
pub fn channel_from_init(packet: &[u8], nonce: &[u8; 8]) -> Option<[u8; 4]> {
    let frame = match packet.len() {
        n if n > PACKET && packet[0] == 0x00 => &packet[1..],
        n if n >= PACKET => packet,
        _ => return None,
    };
    if frame[4] != CTAPHID_INIT || &frame[7..15] != nonce.as_slice() {
        return None;
    }
    Some([frame[15], frame[16], frame[17], frame[18]])
}

/// What the operator is told about a status byte.
///
/// Every arm names what to do next, because "0x30" names nothing. The two that
/// matter most are the timing window and the touch: both are the operator's to
/// fix, and both read as a broken tool if nobody says so.
pub fn describe_status(status: u8) -> String {
    match status {
        0x30 => "the authenticator refused the reset as not allowed — it accepts one only in \
                 the first seconds after it powers up, so the key has to be plugged in again"
            .to_owned(),
        0x27 => "the reset was declined at the key — the touch was refused rather than missed"
            .to_owned(),
        0x2F | 0x3A => "the authenticator waited for the touch and gave up — the key was not \
                        touched while it was blinking"
            .to_owned(),
        0x01 => "the authenticator does not implement a CTAP2 reset — a U2F-only key has \
                 nothing to reset and no way to be asked"
            .to_owned(),
        other => format!(
            "the authenticator refused with CTAP status 0x{other:02x}, which this build has no \
             wording for — the status byte is reported rather than guessed at"
        ),
    }
}

/// Turn an answer into the engine's typed failure.
pub fn classify(answer: Answer, operation: &'static str) -> Result<()> {
    match answer {
        Answer::Reset => Ok(()),
        Answer::Refused(status) => Err(WriteError::Failed {
            operation,
            reason: describe_status(status),
        }),
    }
}

/// Send `authenticatorReset` to the one attached authenticator.
///
/// `serial` is not sent to the key — CTAPHID carries no serial number, which is
/// the same limitation [`super::native_fido`] documents — and is used only to
/// name the key in a failure. What stands in for identification is the refusal
/// to act when more than one authenticator is attached.
#[cfg(feature = "native-fido")]
pub fn reset(serial: u32, operation: &'static str) -> Result<()> {
    let device = open(serial, operation)?;
    let cid = init(&device, operation)?;

    let request =
        init_packet(cid, CTAPHID_CBOR, &[AUTHENTICATOR_RESET]).ok_or(WriteError::Failed {
            operation,
            reason: "the reset request did not fit one CTAPHID packet, which cannot happen for a \
                     one-byte command"
                .into(),
        })?;
    write(&device, &request, operation)?;

    classify(await_answer(&device, operation)?, operation)
}

/// Not compiled without a HID transport. The caller reports the gap; nothing here
/// can substitute for it.
#[cfg(not(feature = "native-fido"))]
pub fn reset(serial: u32, operation: &'static str) -> Result<()> {
    let _ = serial;
    Err(WriteError::TransportUnavailable {
        operation,
        feature: "native-fido",
    })
}

/// Open the one attached authenticator.
///
/// Two or more is a refusal rather than a choice, for the reason
/// [`super::native_fido`] states at length: HID gives no serial, so picking one
/// would be picking at random, and the operation is destructive.
#[cfg(feature = "native-fido")]
fn open(serial: u32, operation: &'static str) -> Result<hidapi::HidDevice> {
    // Asked before the open, not diagnosed after it
    // (`features/windows-elevated-helper.md` phase 1). On Windows `hidclass`
    // enumerates this device and then refuses the handle, so the failure below
    // would otherwise be reported as "another process may hold it" — advice that
    // sends an operator closing browsers at a problem no process is causing.
    if !super::elevation::direct_open_permitted() {
        return Err(WriteError::ElevationRequired { operation });
    }

    let api = hidapi::HidApi::new().map_err(|e| WriteError::Failed {
        operation,
        reason: format!("no USB HID access on this workstation: {e}"),
    })?;

    let paths: Vec<_> = api
        .device_list()
        .filter(|d| d.usage_page() == FIDO_USAGE_PAGE && d.usage() == FIDO_USAGE)
        .map(|d| d.path().to_owned())
        .collect();

    match paths.len() {
        0 => Err(WriteError::NotAttached(serial)),
        1 => api.open_path(&paths[0]).map_err(|e| WriteError::Failed {
            operation,
            reason: format!(
                "the security key was found but could not be opened — another process may hold \
                 it: {e}"
            ),
        }),
        n => Err(WriteError::Failed {
            operation,
            reason: format!(
                "{n} security keys are attached and CTAPHID carries no serial number, so this \
                 transport cannot tell them apart — leave only the key being reset attached"
            ),
        }),
    }
}

/// `CTAPHID_INIT`: ask for a channel, and prove the answer is ours.
#[cfg(feature = "native-fido")]
fn init(device: &hidapi::HidDevice, operation: &'static str) -> Result<[u8; 4]> {
    let mut nonce = [0u8; 8];
    getrandom::fill(&mut nonce).map_err(|e| WriteError::Failed {
        operation,
        reason: format!("no randomness for the CTAPHID channel nonce: {e}"),
    })?;

    let request =
        init_packet(BROADCAST_CID, CTAPHID_INIT, &nonce).ok_or_else(|| WriteError::Failed {
            operation,
            reason: "the channel request did not fit one CTAPHID packet".to_owned(),
        })?;
    write(device, &request, operation)?;

    // A handful of reads rather than one: the broadcast channel may carry another
    // process's answer, and the nonce check discards those instead of failing.
    for _ in 0..8 {
        let packet = read(device, operation)?;
        if let Some(cid) = channel_from_init(&packet, &nonce) {
            return Ok(cid);
        }
    }
    Err(WriteError::Failed {
        operation,
        reason: "the security key never answered the channel request with the nonce that was \
                 sent — another process may be talking to it"
            .to_owned(),
    })
}

/// Read until the authenticator answers the CBOR command.
///
/// `CTAPHID_KEEPALIVE` is the authenticator saying *still waiting for the touch*,
/// and is the expected traffic for most of this function's life.
#[cfg(feature = "native-fido")]
fn await_answer(device: &hidapi::HidDevice, operation: &'static str) -> Result<Answer> {
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(TOUCH_PATIENCE_MS);

    while std::time::Instant::now() < deadline {
        let packet = match read(device, operation) {
            Ok(packet) => packet,
            // A read that times out inside the window is the authenticator saying
            // nothing yet, not a failure: the touch has not happened.
            Err(WriteError::Detached { .. }) => continue,
            Err(other) => return Err(other),
        };
        let Some((command, _, status)) = response_header(&packet) else {
            continue;
        };
        match command {
            CTAPHID_CBOR => {
                return Ok(if status == CTAP_SUCCESS {
                    Answer::Reset
                } else {
                    Answer::Refused(status)
                });
            }
            CTAPHID_ERROR => {
                return Err(WriteError::Failed {
                    operation,
                    reason: format!(
                        "the security key rejected the request at the transport layer (CTAPHID \
                         error 0x{status:02x}) — the reset was not performed"
                    ),
                });
            }
            CTAPHID_KEEPALIVE => continue,
            _ => continue,
        }
    }

    Err(WriteError::Failed {
        operation,
        reason: format!(
            "the authenticator neither performed nor refused the reset within {}s — it was most \
             likely waiting for a touch that did not come",
            TOUCH_PATIENCE_MS / 1000
        ),
    })
}

#[cfg(feature = "native-fido")]
fn write(device: &hidapi::HidDevice, packet: &[u8], operation: &'static str) -> Result<()> {
    device
        .write(packet)
        .map(|_| ())
        .map_err(|e| WriteError::Failed {
            operation,
            reason: format!("the security key did not accept the frame: {e}"),
        })
}

/// One packet, or [`WriteError::Detached`] when nothing arrived in time.
///
/// The timeout is reported as *detached* rather than as a failure because that is
/// what the caller does with it: [`await_answer`] keeps waiting, and only the
/// deadline ends the wait.
#[cfg(feature = "native-fido")]
fn read(device: &hidapi::HidDevice, operation: &'static str) -> Result<Vec<u8>> {
    let mut buffer = vec![0u8; PACKET];
    match device.read_timeout(&mut buffer, READ_TIMEOUT_MS) {
        // Nothing, or too little to be a frame: the authenticator has not spoken
        // yet. Both are the same thing to the caller.
        Ok(read) if read < 8 => Err(WriteError::Detached { operation }),
        Ok(_) => Ok(buffer),
        Err(e) => Err(WriteError::Failed {
            operation,
            reason: format!("the security key stopped answering: {e}"),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A response packet as the device sends it: no report id, 64 bytes.
    fn response(command: u8, payload: &[u8]) -> Vec<u8> {
        let mut frame = vec![0u8; PACKET];
        frame[0..4].copy_from_slice(&[0x01, 0x02, 0x03, 0x04]);
        frame[4] = command;
        frame[5] = (payload.len() >> 8) as u8;
        frame[6] = (payload.len() & 0xFF) as u8;
        frame[7..7 + payload.len()].copy_from_slice(payload);
        frame
    }

    #[test]
    fn the_reset_request_is_one_packet_carrying_one_byte() {
        // Given the whole of `authenticatorReset`
        let packet = init_packet(
            [0x01, 0x02, 0x03, 0x04],
            CTAPHID_CBOR,
            &[AUTHENTICATOR_RESET],
        )
        .expect("a one-byte payload fits");

        // Then the report id leads, the channel follows, and the length is 1
        assert_eq!(packet.len(), 65);
        assert_eq!(packet[0], 0x00, "report id");
        assert_eq!(&packet[1..5], &[0x01, 0x02, 0x03, 0x04]);
        assert_eq!(packet[5], 0x90, "CTAPHID_CBOR with bit 7 set");
        assert_eq!((packet[6], packet[7]), (0x00, 0x01));
        assert_eq!(packet[8], 0x07, "authenticatorReset");
        assert!(
            packet[9..].iter().all(|b| *b == 0),
            "nothing else is sent — the command has no parameters"
        );
    }

    #[test]
    fn a_payload_too_big_for_one_packet_is_refused_rather_than_truncated() {
        assert!(init_packet(BROADCAST_CID, CTAPHID_CBOR, &[0u8; 58]).is_none());
        assert!(init_packet(BROADCAST_CID, CTAPHID_CBOR, &[0u8; 57]).is_some());
    }

    #[test]
    fn the_channel_is_taken_only_from_an_answer_carrying_our_own_nonce() {
        // Given a nonce we sent
        let nonce = [9u8, 8, 7, 6, 5, 4, 3, 2];
        let mut payload = nonce.to_vec();
        payload.extend_from_slice(&[0xAA, 0xBB, 0xCC, 0xDD]);
        payload.extend_from_slice(&[0x02, 0x05, 0x07, 0x04, 0x05]);

        // When the answer echoes it
        let ours = response(CTAPHID_INIT, &payload);
        assert_eq!(
            channel_from_init(&ours, &nonce),
            Some([0xAA, 0xBB, 0xCC, 0xDD])
        );

        // Then another process's answer on the same broadcast channel is not ours
        let theirs = response(CTAPHID_INIT, &[0u8; 17]);
        assert_eq!(
            channel_from_init(&theirs, &nonce),
            None,
            "a different nonce is somebody else's channel"
        );

        // And neither is an answer to a different command
        let wink = response(FRAME_INIT | 0x08, &payload);
        assert_eq!(channel_from_init(&wink, &nonce), None);
    }

    #[test]
    fn a_report_id_in_front_of_the_frame_is_tolerated() {
        let nonce = [1u8, 2, 3, 4, 5, 6, 7, 8];
        let mut payload = nonce.to_vec();
        payload.extend_from_slice(&[0x11, 0x22, 0x33, 0x44]);
        payload.extend_from_slice(&[0u8; 5]);

        let mut with_id = vec![0x00];
        with_id.extend_from_slice(&response(CTAPHID_INIT, &payload));

        assert_eq!(
            channel_from_init(&with_id, &nonce),
            Some([0x11, 0x22, 0x33, 0x44]),
            "the same frame, offset by a report id, is the same frame"
        );
        assert_eq!(
            response_header(&with_id).map(|(command, _, _)| command),
            Some(CTAPHID_INIT)
        );
    }

    #[test]
    fn a_truncated_frame_is_not_read_as_a_status() {
        assert_eq!(response_header(&[]), None);
        assert_eq!(response_header(&[0x01, 0x02, 0x03]), None);
    }

    #[test]
    fn only_a_zero_status_is_a_reset() {
        assert!(matches!(classify(Answer::Reset, "fido2.reset"), Ok(())));
        for status in [0x30u8, 0x27, 0x2F, 0x3A, 0x01, 0x7F] {
            let error = classify(Answer::Refused(status), "fido2.reset")
                .expect_err("a non-zero status is a refusal");
            assert!(
                matches!(error, WriteError::Failed { operation, .. } if operation == "fido2.reset")
            );
        }
    }

    #[test]
    fn the_two_refusals_an_operator_can_act_on_say_what_to_do() {
        // The timing window, which is the common one and reads as a broken tool
        // unless the wording names the power cycle.
        let window = describe_status(0x30);
        assert!(window.contains("powers up"), "{window}");
        assert!(window.contains("plugged in again"), "{window}");

        // The touch, which is the other one.
        let touch = describe_status(0x2F);
        assert!(touch.contains("touch"), "{touch}");
        assert_eq!(describe_status(0x3A), touch, "both timeouts read the same");
    }

    #[test]
    fn a_status_this_build_has_no_wording_for_is_reported_rather_than_guessed() {
        let unknown = describe_status(0x5C);
        assert!(unknown.contains("0x5c"), "{unknown}");
        assert!(unknown.contains("no wording"), "{unknown}");
    }

    #[test]
    fn nothing_this_module_sends_or_says_can_carry_a_secret() {
        // A factory reset destroys the PIN rather than authenticating with it,
        // so there is no secret in this conversation at all (AGENTS.md §2).
        let request = init_packet(BROADCAST_CID, CTAPHID_CBOR, &[AUTHENTICATOR_RESET]).unwrap();
        assert_eq!(
            request[8..].iter().filter(|b| **b != 0).count(),
            1,
            "the payload is one command byte and nothing else"
        );
        for status in 0u8..=0xFF {
            let text = describe_status(status);
            assert!(!text.to_lowercase().contains("pin "), "{text}");
        }
    }
}
