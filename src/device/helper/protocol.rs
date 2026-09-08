//! What the application and the elevated helper say to each other
//! (`features/windows-elevated-helper.md` phase 2).
//!
//! Pure: no pipe, no FFI, no privilege. That is deliberate and it is most of the
//! value — the part of a privileged service that is worth testing hardest is the
//! part that decides what it will agree to do, and here that part compiles and
//! runs on every platform in the ordinary test suite.
//!
//! ## The security design is [`Request`]
//!
//! A `LocalSystem` process that will do whatever a caller asks to a security key
//! is a privilege-escalation surface. The only thing that keeps this one small is
//! that its request type is a **closed enum of the eight operations the tool
//! actually performs**. There is no APDU passthrough, no raw-frame endpoint, no
//! path, no command line and no "run this for me". An eighth operation is a new
//! variant, a new test and a deliberate decision — which is the point of writing
//! it as an enum rather than as a byte buffer with a verb in front.
//!
//! What the helper therefore cannot be asked to do: touch the database, read the
//! settings, write an audit entry, open a file, reach the network, or speak to the
//! PIV or OTP applets. Those last two work without elevation
//! ([`super::super::elevation`]), so routing them through a privileged process
//! would enlarge the surface to buy nothing.
//!
//! ## Secrets cross this boundary
//!
//! Six of the eight requests carry a PIN, so AGENTS.md §2 applies to the wire:
//!
//! * **Never in argv and never in an environment variable.** It travels in the
//!   message. Same argument [`crate::store::smb`] makes for calling
//!   `WNetAddConnection2W` instead of `net use`.
//! * **Never to a temporary file.** The pipe is memory to memory.
//! * **[`Debug`] is redacted** on every type here that carries one, the way
//!   [`crate::secret::Secret`] already is, and a test sweeps every variant for it.
//! * **Zeroised**, including the receive buffer — the copy that is easy to forget,
//!   because it is the one nobody named a variable after.
//!
//! ## Versioned, because the installer can leave a stale peer behind
//!
//! Both ends are the same executable ([`super`] explains why), so they cannot
//! normally disagree. They can disagree for exactly one reason: an MSI upgrade
//! that replaced the files and failed to restart the service. [`PROTOCOL_VERSION`]
//! is what turns that into a refusal naming the cause instead of a struct decoded
//! at the wrong offsets.

use serde::{Deserialize, Serialize};
use zeroize::Zeroize;

/// Bumped whenever a variant's meaning changes. Not the crate version: this
/// number moves when the *conversation* changes, which is rarer, and tying it to
/// the release version would refuse a perfectly good service after every patch.
pub const PROTOCOL_VERSION: u32 = 1;

/// The most a single message may be, in bytes.
///
/// A privileged reader with no bound is a denial-of-service and an allocation
/// bug waiting for a caller who lies about a length. Nothing here is large: the
/// biggest request carries a relying-party name, a user name and a PIN, and the
/// biggest answer carries a credential id and an attestation statement.
pub const MAX_MESSAGE: usize = 64 * 1024;

/// A PIN on its way across the pipe.
///
/// Not [`crate::secret::Secret`], which deliberately has no `Serialize` — that
/// refusal is what stops a secret reaching a record, and it is not one to weaken
/// for this. This is the *transport* type: it exists only between one process
/// writing the message and the other consuming it, it redacts its [`Debug`], and
/// it zeroises on drop.
#[derive(Clone, Serialize, Deserialize)]
#[serde(transparent)]
pub struct WireSecret(String);

impl WireSecret {
    pub fn new(value: String) -> Self {
        Self(value)
    }

    pub fn expose(&self) -> &str {
        &self.0
    }
}

/// Wiped when it drops, including on an unwind — the same guarantee
/// [`crate::secret::Secret`] gets from `Zeroizing`, written out by hand here
/// because `Zeroizing` carries no `Deserialize` without a feature this build does
/// not enable.
impl Drop for WireSecret {
    fn drop(&mut self) {
        self.0.zeroize();
    }
}

/// The whole reason this type is not a `String`.
impl std::fmt::Debug for WireSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("<redacted>")
    }
}

/// What the application asks the helper to do. Closed, and every variant names
/// the serial the run is about so the helper can refuse a key that is not it.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "op", rename_all = "kebab-case")]
pub enum Request {
    /// Is a helper there, and does it speak this version? Carries no serial and
    /// touches no hardware: it is the probe [`super::super::elevation`] makes at
    /// startup, and it must not make a key blink.
    Ping {
        protocol: u32,
    },
    Fido2State {
        serial: u32,
    },
    SetPin {
        serial: u32,
        new: WireSecret,
    },
    ChangePin {
        serial: u32,
        current: WireSecret,
        new: WireSecret,
    },
    SetMinPinLength {
        serial: u32,
        length: u8,
        pin: WireSecret,
    },
    ForcePinChange {
        serial: u32,
        pin: WireSecret,
    },
    MakeCredential {
        serial: u32,
        request: CredentialRequestWire,
        pin: WireSecret,
    },
    GetAssertion {
        serial: u32,
        request: AssertionRequestWire,
        pin: WireSecret,
    },
    /// `authenticatorReset`. Named as its own variant rather than folded into a
    /// generic "run this CTAP command", because the whole point of the enum is
    /// that the destructive operation is one somebody chose to expose.
    Reset {
        serial: u32,
    },
}

impl Request {
    /// The operation name that goes into a [`WireError`] and a log line. `'static`
    /// because it is a label chosen here, never a value from the wire.
    pub fn operation(&self) -> &'static str {
        match self {
            Request::Ping { .. } => "helper.ping",
            Request::Fido2State { .. } => "fido2.get_info",
            Request::SetPin { .. } => "fido2.set_pin",
            Request::ChangePin { .. } => "fido2.change_pin",
            Request::SetMinPinLength { .. } => "fido2.set_min_pin_length",
            Request::ForcePinChange { .. } => "fido2.force_pin_change",
            Request::MakeCredential { .. } => "fido2.make_credential",
            Request::GetAssertion { .. } => "fido2.get_assertion",
            Request::Reset { .. } => "fido2.reset",
        }
    }

    /// Does performing this require the operator to touch the key?
    ///
    /// Recorded on the type rather than left to the reader because it is the
    /// standing review question for every variant added later: the argument that
    /// this service is not worth attacking rests on the fact that everything it
    /// mutates needs somebody standing at the machine, and that argument survives
    /// only as long as the answer below stays `true` for every mutating variant.
    pub fn needs_user_presence(&self) -> bool {
        match self {
            Request::Ping { .. } | Request::Fido2State { .. } => false,
            Request::SetPin { .. }
            | Request::ChangePin { .. }
            | Request::SetMinPinLength { .. }
            | Request::ForcePinChange { .. }
            | Request::MakeCredential { .. }
            | Request::GetAssertion { .. }
            | Request::Reset { .. } => true,
        }
    }
}

/// A failure, carried across the pipe in a shape [`super::super::write::WriteError`]
/// can be rebuilt from.
///
/// Deliberately not a serialised `WriteError`: that type is the application's, and
/// making it `Serialize` would put a type used all over the write path behind a
/// wire format. This is the narrow subset the helper can actually produce.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum WireError {
    NotAttached { serial: u32 },
    WrongSecret { retries_left: u8 },
    Locked,
    Unsupported { reason: String },
    Detached,
    Failed { reason: String },
}

/// What the helper answers.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "kebab-case")]
pub enum Response {
    /// The answer to [`Request::Ping`], and the only one that carries a version.
    Pong {
        protocol: u32,
        version: String,
    },
    Fido2State(Fido2StateWire),
    Done,
    Credential(CredentialWire),
    Assertion(AssertionWire),
    Failed {
        operation: String,
        error: WireError,
    },
}

/// [`super::super::write::Fido2State`] on the wire. A copy rather than a
/// `Serialize` on the original, for the reason [`WireError`] gives.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct Fido2StateWire {
    pub pin_set: bool,
    pub min_pin_length: Option<u8>,
    pub force_pin_change_set: bool,
    pub resident_credentials: usize,
    pub remaining_credential_slots: Option<usize>,
    pub pin_retries: Option<u8>,
}

/// [`super::super::write::CredentialRequest`] on the wire. Field for field, and
/// deliberately not a subset: a request that dropped `require_user_verification`
/// on the way across would make the helper create a credential the template did
/// not ask for, and the run would record the template's words rather than what
/// happened.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialRequestWire {
    pub relying_party: String,
    pub relying_party_name: String,
    pub user_name: String,
    pub user_display_name: String,
    pub resident: bool,
    pub require_user_verification: bool,
}

/// [`super::super::write::AssertionRequest`] on the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssertionRequestWire {
    pub relying_party: String,
    pub credential_id_hex: String,
    pub challenge_hex: String,
    pub require_user_verification: bool,
}

/// [`super::super::write::CredentialEvidence`] on the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct CredentialWire {
    pub credential_id_hex: String,
    pub relying_party: String,
    pub algorithm: String,
}

/// [`super::super::write::AssertionEvidence`] on the wire.
#[derive(Debug, Clone, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct AssertionWire {
    pub credential_id_hex: String,
    pub relying_party: String,
    pub user_verified: bool,
    pub user_present: bool,
    pub counter: u32,
}

/// Frame a message: a four-byte big-endian length, then the JSON.
///
/// Length-prefixed even though the pipe is in message mode, because the framing
/// then does not depend on a flag set at creation time by the other end. A reader
/// that trusts message boundaries it did not establish is a reader that
/// desynchronises the first time somebody opens the pipe in byte mode.
pub fn frame(payload: &[u8]) -> Result<Vec<u8>, WireError> {
    if payload.len() > MAX_MESSAGE {
        return Err(WireError::Failed {
            reason: format!(
                "the message is {} bytes, and the helper accepts at most {MAX_MESSAGE}",
                payload.len()
            ),
        });
    }
    let mut framed = Vec::with_capacity(payload.len() + 4);
    framed.extend_from_slice(&(payload.len() as u32).to_be_bytes());
    framed.extend_from_slice(payload);
    Ok(framed)
}

/// Read the length a frame declares, and refuse one that is too big **before**
/// allocating for it.
pub fn declared_length(header: [u8; 4]) -> Result<usize, WireError> {
    let length = u32::from_be_bytes(header) as usize;
    if length > MAX_MESSAGE {
        return Err(WireError::Failed {
            reason: format!(
                "the peer declared a {length}-byte message, and the limit is {MAX_MESSAGE}"
            ),
        });
    }
    Ok(length)
}

pub fn encode<T: Serialize>(value: &T) -> Result<Vec<u8>, WireError> {
    serde_json::to_vec(value).map_err(|e| WireError::Failed {
        reason: format!("the message could not be encoded: {e}"),
    })
}

pub fn decode<T: for<'de> Deserialize<'de>>(bytes: &[u8]) -> Result<T, WireError> {
    serde_json::from_slice(bytes).map_err(|e| WireError::Failed {
        reason: format!("the message could not be read: {e}"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_request() -> Vec<Request> {
        vec![
            Request::Ping {
                protocol: PROTOCOL_VERSION,
            },
            Request::Fido2State { serial: 1 },
            Request::SetPin {
                serial: 1,
                new: WireSecret::new("123456".into()),
            },
            Request::ChangePin {
                serial: 1,
                current: WireSecret::new("123456".into()),
                new: WireSecret::new("654321".into()),
            },
            Request::SetMinPinLength {
                serial: 1,
                length: 8,
                pin: WireSecret::new("123456".into()),
            },
            Request::ForcePinChange {
                serial: 1,
                pin: WireSecret::new("123456".into()),
            },
            Request::MakeCredential {
                serial: 1,
                request: CredentialRequestWire {
                    relying_party: "example.org".into(),
                    user_name: "ana".into(),
                    resident: true,
                    ..Default::default()
                },
                pin: WireSecret::new("123456".into()),
            },
            Request::GetAssertion {
                serial: 1,
                request: AssertionRequestWire {
                    relying_party: "example.org".into(),
                    ..Default::default()
                },
                pin: WireSecret::new("123456".into()),
            },
            Request::Reset { serial: 1 },
        ]
    }

    #[test]
    fn every_request_survives_a_round_trip() {
        for request in every_request() {
            let encoded = encode(&request).expect("encodes");
            let back: Request = decode(&encoded).expect("decodes");
            assert_eq!(
                back.operation(),
                request.operation(),
                "a request came back as a different operation"
            );
        }
    }

    /// The sweep AGENTS.md §2 asks for, on the one path where a secret leaves the
    /// process. `Debug` is what a `tracing` field, a panic message and an
    /// `unwrap()` all reach for.
    #[test]
    fn no_debug_rendering_of_any_request_contains_a_pin() {
        for request in every_request() {
            let rendered = format!("{request:?}");
            assert!(
                !rendered.contains("123456") && !rendered.contains("654321"),
                "a PIN reached Debug for {}: {rendered}",
                request.operation()
            );
        }
    }

    #[test]
    fn a_wire_secret_still_hands_over_the_value_it_hides() {
        let secret = WireSecret::new("123456".into());
        assert_eq!(secret.expose(), "123456");
        assert_eq!(format!("{secret:?}"), "<redacted>");
    }

    #[test]
    fn every_mutating_request_needs_a_touch() {
        // The property the whole security argument rests on. If a variant is ever
        // added that mutates the key without user presence, this fails and the
        // reasoning in the module docs has to be revisited rather than quietly
        // outgrown.
        for request in every_request() {
            let readonly = matches!(request, Request::Ping { .. } | Request::Fido2State { .. });
            assert_eq!(
                request.needs_user_presence(),
                !readonly,
                "{} disagrees with its own presence requirement",
                request.operation()
            );
        }
    }

    #[test]
    fn an_unknown_operation_is_refused_rather_than_guessed() {
        let decoded: Result<Request, _> = decode(br#"{"op":"format-the-disk"}"#);
        assert!(decoded.is_err());
    }

    #[test]
    fn a_frame_carries_its_own_length() {
        let framed = frame(b"hello").expect("frames");
        assert_eq!(&framed[..4], &5u32.to_be_bytes());
        assert_eq!(&framed[4..], b"hello");
        assert_eq!(declared_length([0, 0, 0, 5]).expect("reads"), 5);
    }

    #[test]
    fn an_oversized_message_is_refused_on_both_sides() {
        let huge = vec![0u8; MAX_MESSAGE + 1];
        assert!(frame(&huge).is_err());
        // And the reader refuses the *declared* length before allocating for it,
        // which is the half that matters in a privileged process.
        let header = ((MAX_MESSAGE + 1) as u32).to_be_bytes();
        assert!(declared_length(header).is_err());
    }

    #[test]
    fn a_response_survives_a_round_trip() {
        let responses = vec![
            Response::Pong {
                protocol: PROTOCOL_VERSION,
                version: "0.18.3".into(),
            },
            Response::Fido2State(Fido2StateWire::default()),
            Response::Done,
            Response::Credential(CredentialWire::default()),
            Response::Assertion(AssertionWire::default()),
            Response::Failed {
                operation: "fido2.set_pin".into(),
                error: WireError::WrongSecret { retries_left: 2 },
            },
        ];
        for response in responses {
            let encoded = encode(&response).expect("encodes");
            let _: Response = decode(&encoded).expect("decodes");
        }
    }
}
