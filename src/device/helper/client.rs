//! The application's side of the pipe: ask the elevated helper to do the FIDO2
//! work (`features/windows-elevated-helper.md` phase 4).
//!
//! Windows only, and thin by design. Everything that decides *what* may be asked
//! lives in [`super::protocol`], where it is pure and testable on any platform;
//! what is here is the handle, the framing loop and the mapping back onto
//! [`WriteError`]. The same split, for the same reason, as
//! [`crate::store::smb::windows`] — the decisions above the FFI, the FFI as small
//! as it can be made.
//!
//! ## Why the read has no timeout
//!
//! A FIDO2 operation on the other end is usually **waiting for somebody to touch
//! the key**. Any timeout short enough to notice a wedged service is short enough
//! to abandon an operator who is reaching for their keyring, and abandoning them
//! mid-`make_credential` leaves a key in a state the run then has to reason about.
//! So the read blocks, and the thing that actually bounds it is the authenticator,
//! which gives up on its own.
//!
//! The one exchange that does not wait for a person is [`ping`], and it is also
//! the one made at startup: a *missing* service fails immediately at
//! `CreateFileW` with `ERROR_FILE_NOT_FOUND`, which is the common case and costs
//! nothing.

use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::{
    CloseHandle, ERROR_PIPE_BUSY, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE,
    INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{CreateFileW, OPEN_EXISTING, ReadFile, WriteFile};
use windows_sys::Win32::System::Pipes::WaitNamedPipeW;
use zeroize::Zeroize;

use super::protocol::{
    AssertionRequestWire, CredentialRequestWire, PROTOCOL_VERSION, Request, Response, WireError,
    WireSecret, declared_length, decode, encode, frame,
};
use crate::device::write::{
    AssertionEvidence, AssertionRequest, CredentialEvidence, CredentialRequest, Fido2State,
    Fido2Writer, Result, WriteError,
};
use crate::secret::Secret;

/// How long to wait for a pipe that exists but is serving somebody else.
///
/// The service takes one request at a time on purpose (there is one key), so a
/// busy pipe means another operation is in flight — not a fault. Short, because
/// two concurrent operations against one key is a situation to report rather than
/// to queue behind.
const BUSY_WAIT_MS: u32 = 2_000;

/// A pipe handle that closes itself, including on an unwind.
struct Pipe(HANDLE);

impl Drop for Pipe {
    fn drop(&mut self) {
        // SAFETY: constructed only from a handle `CreateFileW` reported valid, and
        // closed exactly once because `Pipe` is neither `Copy` nor `Clone`.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: reads this thread's error code; no pointers involved.
    unsafe { GetLastError() }
}

fn connect() -> std::result::Result<Pipe, WireError> {
    let name = wide(super::PIPE_NAME);
    // Two attempts, not a loop with a deadline: either the pipe is there and free,
    // or it is there and busy (one `WaitNamedPipeW`), or it is not there at all
    // and no amount of waiting invents it.
    for attempt in 0..2 {
        // SAFETY: `name` is NUL-terminated and outlives the call; the two null
        // pointers are the documented "no security attributes" and "no template".
        let handle = unsafe {
            CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            )
        };
        if handle != INVALID_HANDLE_VALUE {
            return Ok(Pipe(handle));
        }
        let error = last_error();
        if error == ERROR_PIPE_BUSY && attempt == 0 {
            // SAFETY: same NUL-terminated name.
            unsafe { WaitNamedPipeW(name.as_ptr(), BUSY_WAIT_MS) };
            continue;
        }
        return Err(WireError::Failed {
            reason: if error == ERROR_PIPE_BUSY {
                "the helper service is busy with another operation on this key".to_owned()
            } else {
                format!("the helper service is not answering (error {error})")
            },
        });
    }
    Err(WireError::Failed {
        reason: "the helper service is busy with another operation on this key".to_owned(),
    })
}

fn write_all(pipe: &Pipe, bytes: &[u8]) -> std::result::Result<(), WireError> {
    let mut sent = 0usize;
    while sent < bytes.len() {
        let mut written = 0u32;
        // SAFETY: the slice bounds the pointer and the length; `written` is only
        // read when the call reports success.
        let ok = unsafe {
            WriteFile(
                pipe.0,
                bytes[sent..].as_ptr(),
                (bytes.len() - sent) as u32,
                &mut written,
                null_mut(),
            )
        };
        if ok == 0 || written == 0 {
            return Err(WireError::Failed {
                reason: format!(
                    "the request could not be sent to the helper ({})",
                    last_error()
                ),
            });
        }
        sent += written as usize;
    }
    Ok(())
}

fn read_exact(pipe: &Pipe, buffer: &mut [u8]) -> std::result::Result<(), WireError> {
    let mut filled = 0usize;
    while filled < buffer.len() {
        let mut read = 0u32;
        let remaining = (buffer.len() - filled) as u32;
        // SAFETY: as above — the slice bounds both pointer and length.
        let ok = unsafe {
            ReadFile(
                pipe.0,
                buffer[filled..].as_mut_ptr(),
                remaining,
                &mut read,
                null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            return Err(WireError::Failed {
                reason: format!(
                    "the helper stopped answering part-way through a reply ({})",
                    last_error()
                ),
            });
        }
        filled += read as usize;
    }
    Ok(())
}

/// One request, one answer.
///
/// The encoded request is **zeroised before this returns**, on every path. It is
/// the buffer that carries the PIN, and it is the copy nobody names — the one
/// AGENTS.md §2 is about.
pub fn exchange(request: &Request) -> std::result::Result<Response, WireError> {
    let pipe = connect()?;

    // `payload` and `framed` both carry the PIN, so both are wiped on **every**
    // path out — including the one where framing refuses an oversized message,
    // which an early `?` would have skipped.
    let mut payload = encode(request)?;
    let framing = frame(&payload);
    payload.zeroize();
    let mut framed = framing?;

    let sent = write_all(&pipe, &framed);
    framed.zeroize();
    sent?;

    let mut header = [0u8; 4];
    read_exact(&pipe, &mut header)?;
    let length = declared_length(header)?;
    let mut body = vec![0u8; length];
    read_exact(&pipe, &mut body)?;

    let response = decode::<Response>(&body);
    body.zeroize();
    response
}

/// Is a helper there, and does it speak this version?
///
/// Answers `false` for every failure, deliberately: the caller
/// ([`crate::device::elevation`]) is deciding whether to route through the helper,
/// and every way of not getting a `Pong` has the same consequence. A **version
/// mismatch is one of them**, and it is the case that catches an MSI upgrade which
/// replaced the files and failed to restart the service.
pub fn ping() -> bool {
    match exchange(&Request::Ping {
        protocol: PROTOCOL_VERSION,
    }) {
        Ok(Response::Pong { protocol, version }) if protocol == PROTOCOL_VERSION => {
            tracing::info!(
                event = "device.helper.answered",
                helper_version = %version,
                detail = "the elevated FIDO2 helper is running"
            );
            true
        }
        Ok(Response::Pong { protocol, version }) => {
            tracing::warn!(
                event = "device.helper.version_mismatch",
                helper_protocol = protocol,
                expected = PROTOCOL_VERSION,
                helper_version = %version,
                detail = "the helper service speaks a different protocol — it was probably not \
                          restarted by the last upgrade"
            );
            false
        }
        Ok(_) | Err(_) => false,
    }
}

/// Rebuild the application's error from what came back.
fn to_write_error(operation: &'static str, error: WireError) -> WriteError {
    match error {
        WireError::NotAttached { serial } => WriteError::NotAttached(serial),
        WireError::WrongSecret { retries_left } => WriteError::WrongSecret {
            applet: "FIDO2",
            retries_left,
        },
        WireError::Locked => WriteError::Locked { applet: "FIDO2" },
        WireError::Unsupported { reason } => WriteError::Unsupported { operation, reason },
        WireError::Detached => WriteError::Detached { operation },
        WireError::Failed { reason } => WriteError::Failed { operation, reason },
    }
}

/// Send a request and insist on the answer being the one this operation expects.
fn ask(request: Request) -> Result<Response> {
    let operation = request.operation();
    match exchange(&request) {
        Ok(Response::Failed { error, .. }) => Err(to_write_error(operation, error)),
        Ok(response) => Ok(response),
        Err(error) => Err(to_write_error(operation, error)),
    }
}

fn unexpected(operation: &'static str) -> WriteError {
    WriteError::Failed {
        operation,
        reason: "the helper answered with something this operation did not ask for".to_owned(),
    }
}

fn secret(value: &Secret) -> WireSecret {
    WireSecret::new(value.expose().to_owned())
}

/// [`Fido2Writer`] over the pipe.
pub struct HelperFido2 {
    expected_serial: u32,
}

impl HelperFido2 {
    pub fn for_key(serial: u32) -> Self {
        Self {
            expected_serial: serial,
        }
    }

    /// `authenticatorReset`, which is not on [`Fido2Writer`] because
    /// [`crate::device::reset`] owns it.
    pub fn reset(&self) -> Result<()> {
        match ask(Request::Reset {
            serial: self.expected_serial,
        })? {
            Response::Done => Ok(()),
            _ => Err(unexpected("fido2.reset")),
        }
    }
}

impl Fido2Writer for HelperFido2 {
    fn fido2_state(&mut self, serial: u32) -> Result<Fido2State> {
        match ask(Request::Fido2State { serial })? {
            Response::Fido2State(state) => Ok(Fido2State {
                pin_set: state.pin_set,
                min_pin_length: state.min_pin_length,
                force_pin_change_set: state.force_pin_change_set,
                resident_credentials: state.resident_credentials,
                remaining_credential_slots: state.remaining_credential_slots,
                pin_retries: state.pin_retries,
            }),
            _ => Err(unexpected("fido2.get_info")),
        }
    }

    fn set_pin(&mut self, serial: u32, new: &Secret) -> Result<()> {
        match ask(Request::SetPin {
            serial,
            new: secret(new),
        })? {
            Response::Done => Ok(()),
            _ => Err(unexpected("fido2.set_pin")),
        }
    }

    fn change_pin(&mut self, serial: u32, current: &Secret, new: &Secret) -> Result<()> {
        match ask(Request::ChangePin {
            serial,
            current: secret(current),
            new: secret(new),
        })? {
            Response::Done => Ok(()),
            _ => Err(unexpected("fido2.change_pin")),
        }
    }

    fn set_min_pin_length(&mut self, serial: u32, length: u8, pin: &Secret) -> Result<()> {
        match ask(Request::SetMinPinLength {
            serial,
            length,
            pin: secret(pin),
        })? {
            Response::Done => Ok(()),
            _ => Err(unexpected("fido2.set_min_pin_length")),
        }
    }

    fn force_pin_change(&mut self, serial: u32, pin: &Secret) -> Result<()> {
        match ask(Request::ForcePinChange {
            serial,
            pin: secret(pin),
        })? {
            Response::Done => Ok(()),
            _ => Err(unexpected("fido2.force_pin_change")),
        }
    }

    fn make_credential(
        &mut self,
        serial: u32,
        request: &CredentialRequest,
        pin: &Secret,
    ) -> Result<CredentialEvidence> {
        let wire = CredentialRequestWire {
            relying_party: request.relying_party.clone(),
            relying_party_name: request.relying_party_name.clone(),
            user_name: request.user_name.clone(),
            user_display_name: request.user_display_name.clone(),
            resident: request.resident,
            require_user_verification: request.require_user_verification,
        };
        match ask(Request::MakeCredential {
            serial,
            request: wire,
            pin: secret(pin),
        })? {
            Response::Credential(evidence) => Ok(CredentialEvidence {
                credential_id_hex: evidence.credential_id_hex,
                relying_party: evidence.relying_party,
                algorithm: evidence.algorithm,
            }),
            _ => Err(unexpected("fido2.make_credential")),
        }
    }

    fn get_assertion(
        &mut self,
        serial: u32,
        request: &AssertionRequest,
        pin: &Secret,
    ) -> Result<AssertionEvidence> {
        let wire = AssertionRequestWire {
            relying_party: request.relying_party.clone(),
            credential_id_hex: request.credential_id_hex.clone(),
            challenge_hex: request.challenge_hex.clone(),
            require_user_verification: request.require_user_verification,
        };
        match ask(Request::GetAssertion {
            serial,
            request: wire,
            pin: secret(pin),
        })? {
            Response::Assertion(evidence) => Ok(AssertionEvidence {
                credential_id_hex: evidence.credential_id_hex,
                relying_party: evidence.relying_party,
                user_verified: evidence.user_verified,
                user_present: evidence.user_present,
                counter: evidence.counter,
            }),
            _ => Err(unexpected("fido2.get_assertion")),
        }
    }
}
