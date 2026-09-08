//! The elevated side: a Windows service that performs FIDO2 operations for the
//! application (`features/windows-elevated-helper.md` phase 3).
//!
//! Entered from `main` when the executable is started with
//! [`super::SERVICE_ARG`], which the MSI registers. Everything it will agree to do
//! is [`super::protocol::Request`], and everything it will let ask is
//! [`PIPE_SDDL`].
//!
//! ## The security descriptor, written as SDDL on purpose
//!
//! A named pipe is reachable over SMB as `\\host\pipe\name`. A privileged
//! endpoint created with a careless descriptor is therefore a *remotely* reachable
//! privileged endpoint, and that is the single worst mistake available in this
//! file. Two independent defences, because one of them being wrong should not be
//! enough:
//!
//! * `PIPE_REJECT_REMOTE_CLIENTS` on the pipe itself, which is the kernel
//!   refusing a remote client regardless of who they are; and
//! * [`PIPE_SDDL`], which denies `NETWORK` and `ANONYMOUS` outright and admits
//!   only an interactive logon session and administrators.
//!
//! SDDL rather than hand-built ACLs because the descriptor then *is* the review
//! artefact: one string, checked by a test, with the ACE ordering left to the
//! operating system rather than to whoever edits this next.

use std::ptr::{null, null_mut};
use std::sync::atomic::{AtomicBool, Ordering};

use windows_sys::Win32::Foundation::{
    CloseHandle, GENERIC_READ, GENERIC_WRITE, GetLastError, HANDLE, INVALID_HANDLE_VALUE, LocalFree,
};
use windows_sys::Win32::Security::Authorization::{
    ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows_sys::Win32::Security::{PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FlushFileBuffers, OPEN_EXISTING, PIPE_ACCESS_DUPLEX, ReadFile, WriteFile,
};
use windows_sys::Win32::System::Pipes::{
    ConnectNamedPipe, CreateNamedPipeW, DisconnectNamedPipe, PIPE_READMODE_BYTE,
    PIPE_REJECT_REMOTE_CLIENTS, PIPE_TYPE_BYTE, PIPE_WAIT,
};
use windows_sys::Win32::System::Services::{
    RegisterServiceCtrlHandlerExW, SERVICE_ACCEPT_STOP, SERVICE_CONTROL_SHUTDOWN,
    SERVICE_CONTROL_STOP, SERVICE_RUNNING, SERVICE_STATUS, SERVICE_STATUS_HANDLE, SERVICE_STOPPED,
    SERVICE_TABLE_ENTRYW, SERVICE_WIN32_OWN_PROCESS, SetServiceStatus, StartServiceCtrlDispatcherW,
};
use zeroize::Zeroize;

use super::PIPE_SDDL;
use super::protocol::{
    Fido2StateWire, PROTOCOL_VERSION, Request, Response, WireError, declared_length, decode,
    encode, frame,
};

/// One pending connection at a time. There is one key; two callers racing an
/// `authenticatorReset` is not a state worth being able to reason about.
const MAX_INSTANCES: u32 = 1;

/// The pipe's own buffer sizes. Both ends frame their messages
/// ([`super::protocol::frame`]), so these are a hint to the kernel and not a
/// bound on anything — [`super::protocol::MAX_MESSAGE`] is the bound.
const BUFFER: u32 = 8 * 1024;

/// How long the kernel makes a client wait by default, in milliseconds.
const DEFAULT_TIMEOUT_MS: u32 = 5_000;

static STOPPING: AtomicBool = AtomicBool::new(false);
static STATUS_HANDLE: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

fn wide(value: &str) -> Vec<u16> {
    value.encode_utf16().chain(std::iter::once(0)).collect()
}

fn last_error() -> u32 {
    // SAFETY: reads this thread's error code; no pointers involved.
    unsafe { GetLastError() }
}

/// Hand control to the service-control manager. Returns when the service stops.
///
/// A failure here is normal and expected in one case: somebody ran the executable
/// with [`super::SERVICE_ARG`] from a command prompt, where there is no dispatcher
/// to connect to. Saying so beats exiting silently, because that is exactly the
/// mistake somebody debugging this makes first.
pub fn run() -> i32 {
    let name = wide(super::SERVICE_NAME);
    let table = [
        SERVICE_TABLE_ENTRYW {
            lpServiceName: name.as_ptr() as *mut u16,
            lpServiceProc: Some(service_main),
        },
        SERVICE_TABLE_ENTRYW {
            lpServiceName: null_mut(),
            lpServiceProc: None,
        },
    ];
    // SAFETY: the table is NUL-terminated by its second, all-null entry and
    // outlives the call, which blocks until the service stops.
    let started = unsafe { StartServiceCtrlDispatcherW(table.as_ptr()) };
    if started == 0 {
        eprintln!(
            "yk-dist-manager: {} is the service entry point and can only be started by Windows \
             (error {}). Install the MSI, which registers it, rather than running this by hand.",
            super::SERVICE_ARG,
            last_error()
        );
        return 1;
    }
    0
}

unsafe extern "system" fn service_main(_argc: u32, _argv: *mut *mut u16) {
    let name = wide(super::SERVICE_NAME);
    // SAFETY: `name` is NUL-terminated and outlives the call.
    let handle = unsafe { RegisterServiceCtrlHandlerExW(name.as_ptr(), Some(control), null()) };
    if handle.is_null() {
        return;
    }
    STATUS_HANDLE.store(handle as usize, Ordering::SeqCst);
    report(SERVICE_RUNNING);

    tracing::info!(
        event = "device.helper.started",
        pipe = super::PIPE_NAME,
        protocol = PROTOCOL_VERSION,
        detail = "the elevated FIDO2 helper is listening"
    );

    serve();

    tracing::info!(
        event = "device.helper.stopped",
        detail = "the elevated FIDO2 helper is shutting down"
    );
    report(SERVICE_STOPPED);
}

unsafe extern "system" fn control(
    control: u32,
    _event_type: u32,
    _event_data: *mut core::ffi::c_void,
    _context: *mut core::ffi::c_void,
) -> u32 {
    if control == SERVICE_CONTROL_STOP || control == SERVICE_CONTROL_SHUTDOWN {
        STOPPING.store(true, Ordering::SeqCst);
        // Unblock the `ConnectNamedPipe` the serving thread is sitting in, by
        // connecting to it. Closing the handle from here would race the thread
        // that is using it; opening the pipe is a defined way to complete a
        // pending connect, and the loop then sees `STOPPING` and returns.
        let name = wide(super::PIPE_NAME);
        // SAFETY: `name` is NUL-terminated and outlives the call; the handle is
        // closed immediately, and a failure means the accept was not waiting.
        unsafe {
            let handle = CreateFileW(
                name.as_ptr(),
                GENERIC_READ | GENERIC_WRITE,
                0,
                null(),
                OPEN_EXISTING,
                0,
                null_mut(),
            );
            if handle != INVALID_HANDLE_VALUE {
                CloseHandle(handle);
            }
        }
    }
    0
}

fn report(state: u32) {
    let handle = STATUS_HANDLE.load(Ordering::SeqCst) as SERVICE_STATUS_HANDLE;
    if handle.is_null() {
        return;
    }
    let status = SERVICE_STATUS {
        dwServiceType: SERVICE_WIN32_OWN_PROCESS,
        dwCurrentState: state,
        dwControlsAccepted: if state == SERVICE_RUNNING {
            SERVICE_ACCEPT_STOP
        } else {
            0
        },
        dwWin32ExitCode: 0,
        dwServiceSpecificExitCode: 0,
        dwCheckPoint: 0,
        dwWaitHint: 0,
    };
    // SAFETY: `status` is fully initialised and outlives the call.
    unsafe {
        SetServiceStatus(handle, &status);
    }
}

/// A listening pipe that closes itself.
struct Listener(HANDLE);

impl Drop for Listener {
    fn drop(&mut self) {
        // SAFETY: constructed only from a handle `CreateNamedPipeW` reported
        // valid, and closed exactly once.
        unsafe {
            CloseHandle(self.0);
        }
    }
}

/// A security descriptor built from [`PIPE_SDDL`], freed when it drops.
struct Descriptor(PSECURITY_DESCRIPTOR);

impl Drop for Descriptor {
    fn drop(&mut self) {
        // SAFETY: `ConvertStringSecurityDescriptorToSecurityDescriptorW` allocates
        // with `LocalAlloc`, so `LocalFree` is its documented counterpart.
        unsafe {
            LocalFree(self.0 as _);
        }
    }
}

fn descriptor() -> Option<Descriptor> {
    let sddl = wide(PIPE_SDDL);
    let mut raw: PSECURITY_DESCRIPTOR = null_mut();
    // SAFETY: `sddl` is NUL-terminated and outlives the call; `raw` is written
    // only on success and is owned by `Descriptor` from then on.
    let ok = unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            sddl.as_ptr(),
            SDDL_REVISION_1,
            &mut raw,
            null_mut(),
        )
    };
    if ok == 0 || raw.is_null() {
        tracing::error!(
            event = "device.helper.descriptor_failed",
            error = last_error(),
            detail = "the pipe's security descriptor could not be built — refusing to listen"
        );
        return None;
    }
    Some(Descriptor(raw))
}

fn listen(descriptor: &Descriptor) -> Option<Listener> {
    let name = wide(super::PIPE_NAME);
    let attributes = SECURITY_ATTRIBUTES {
        nLength: size_of::<SECURITY_ATTRIBUTES>() as u32,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: 0,
    };
    // SAFETY: `name` and `attributes` outlive the call, and the descriptor is
    // owned by the caller for at least as long as this handle.
    let handle = unsafe {
        CreateNamedPipeW(
            name.as_ptr(),
            PIPE_ACCESS_DUPLEX,
            PIPE_TYPE_BYTE | PIPE_READMODE_BYTE | PIPE_WAIT | PIPE_REJECT_REMOTE_CLIENTS,
            MAX_INSTANCES,
            BUFFER,
            BUFFER,
            DEFAULT_TIMEOUT_MS,
            &attributes,
        )
    };
    if handle == INVALID_HANDLE_VALUE {
        tracing::error!(
            event = "device.helper.listen_failed",
            error = last_error(),
            detail = "the helper could not create its pipe"
        );
        return None;
    }
    Some(Listener(handle))
}

/// How many consecutive failed accepts to tolerate before giving up.
///
/// Not zero, because one failure is the stop signal arriving; not unbounded,
/// because a handle that has become permanently unusable would otherwise spin this
/// thread at full speed for as long as the machine is on.
const MAX_ACCEPT_FAILURES: u32 = 8;

/// Accept one caller at a time, answer, disconnect, repeat.
///
/// **The pipe is created once, outside the loop**, and each caller is disconnected
/// from the same handle rather than the handle being closed and remade. That is not
/// tidiness: creating it per caller leaves a window between the close and the next
/// `CreateNamedPipeW` in which the pipe does not exist, and a client that arrives
/// inside that window gets `ERROR_FILE_NOT_FOUND` — which
/// [`super::client::connect`] cannot distinguish from *no helper is installed*. The
/// symptom would be an operator told to install the MSI they already have,
/// intermittently, under load.
///
/// **A failure serving one caller never stops the service.** The alternative is a
/// helper that a malformed message can take down, leaving every later operation on
/// every later key reporting that nothing is installed.
fn serve() {
    let Some(descriptor) = descriptor() else {
        return;
    };
    let Some(listener) = listen(&descriptor) else {
        return;
    };
    let mut failures = 0u32;

    while !STOPPING.load(Ordering::SeqCst) {
        // SAFETY: a valid listening handle held for the whole loop; `null_mut()` is
        // the documented synchronous form.
        let connected = unsafe { ConnectNamedPipe(listener.0, null_mut()) };
        // A client that connected between the create and this call is reported as
        // `ERROR_PIPE_CONNECTED`, which is a success with a different name.
        let ready =
            connected != 0 || last_error() == windows_sys::Win32::Foundation::ERROR_PIPE_CONNECTED;

        if ready {
            failures = 0;
            if !STOPPING.load(Ordering::SeqCst) {
                // Caught, because the module docs' promise — a failure serving one
                // caller never stops the service — has to hold for a *panic* too,
                // and this is the one place it can be made to. `service_main` is
                // `extern "system"`, so an unwind reaching it aborts the process:
                // the helper would vanish, and every later operation on every later
                // key would report that nothing is installed. Everything below here
                // is hardware code that can fail in ways no test has seen.
                let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    serve_one(&listener);
                }));
                if served.is_err() {
                    tracing::error!(
                        event = "device.helper.panicked",
                        detail = "serving a caller panicked; the helper stays up and the caller \
                                  sees a dropped connection. The panic message is in the \
                                  process's own stderr, and carries no secret because nothing \
                                  here formats one"
                    );
                }
            }
        } else {
            failures += 1;
            if failures >= MAX_ACCEPT_FAILURES {
                tracing::error!(
                    event = "device.helper.accept_failed",
                    error = last_error(),
                    detail = "the helper could not accept a caller repeatedly — giving up rather \
                              than spinning; the service manager will report it stopped"
                );
                return;
            }
        }

        // Reset the instance either way, so the next `ConnectNamedPipe` on this
        // same handle can accept again.
        // SAFETY: as above.
        unsafe {
            FlushFileBuffers(listener.0);
            DisconnectNamedPipe(listener.0);
        }
    }
}

fn serve_one(listener: &Listener) {
    let mut header = [0u8; 4];
    if read_exact(listener, &mut header).is_err() {
        return;
    }
    let length = match declared_length(header) {
        Ok(length) => length,
        Err(error) => {
            // Refused *before* allocating for it, which is the half that matters
            // in a privileged process.
            let _ = answer(
                listener,
                &Response::Failed {
                    operation: "helper.frame".to_owned(),
                    error,
                },
            );
            return;
        }
    };
    let mut body = vec![0u8; length];
    let read = read_exact(listener, &mut body);
    let request = read.and_then(|()| decode::<Request>(&body));
    // The receive buffer carries the PIN. Wiped here, before anything can go
    // wrong further down and leave it lying in freed memory.
    body.zeroize();

    let response = match request {
        Ok(request) => dispatch(request),
        Err(error) => Response::Failed {
            operation: "helper.decode".to_owned(),
            error,
        },
    };
    let _ = answer(listener, &response);
}

fn answer(listener: &Listener, response: &Response) -> Result<(), WireError> {
    let payload = encode(response)?;
    let framed = frame(&payload)?;
    write_all(listener, &framed)
}

fn read_exact(listener: &Listener, buffer: &mut [u8]) -> Result<(), WireError> {
    let mut filled = 0usize;
    while filled < buffer.len() {
        let mut read = 0u32;
        let remaining = (buffer.len() - filled) as u32;
        // SAFETY: the slice bounds both the pointer and the length.
        let ok = unsafe {
            ReadFile(
                listener.0,
                buffer[filled..].as_mut_ptr(),
                remaining,
                &mut read,
                null_mut(),
            )
        };
        if ok == 0 || read == 0 {
            return Err(WireError::Failed {
                reason: "the caller stopped part-way through a request".to_owned(),
            });
        }
        filled += read as usize;
    }
    Ok(())
}

fn write_all(listener: &Listener, bytes: &[u8]) -> Result<(), WireError> {
    let mut sent = 0usize;
    while sent < bytes.len() {
        let mut written = 0u32;
        // SAFETY: as above.
        let ok = unsafe {
            WriteFile(
                listener.0,
                bytes[sent..].as_ptr(),
                (bytes.len() - sent) as u32,
                &mut written,
                null_mut(),
            )
        };
        if ok == 0 || written == 0 {
            return Err(WireError::Failed {
                reason: "the caller stopped listening for the answer".to_owned(),
            });
        }
        sent += written as usize;
    }
    Ok(())
}

/// The whole vocabulary, in one place.
///
/// Every arm either performs one named FIDO2 operation or refuses. There is no
/// default arm that does something generic, and there is nothing here that takes a
/// path, a command or a byte string to send to the card.
fn dispatch(request: Request) -> Response {
    let operation = request.operation().to_owned();
    if let Request::Ping { protocol } = request {
        // Answered even when the protocol differs, so the caller can *report* the
        // mismatch rather than time out against a silent service. This is the
        // exchange that catches an upgrade which replaced the files and did not
        // restart the service.
        if protocol != PROTOCOL_VERSION {
            tracing::warn!(
                event = "device.helper.version_mismatch",
                caller_protocol = protocol,
                expected = PROTOCOL_VERSION,
                detail = "an application speaking a different protocol asked for a ping"
            );
        }
        return Response::Pong {
            protocol: PROTOCOL_VERSION,
            version: env!("CARGO_PKG_VERSION").to_owned(),
        };
    }

    tracing::info!(
        event = "device.helper.request",
        operation = %operation,
        presence = request.needs_user_presence(),
        detail = "performing a FIDO2 operation for the application"
    );

    match perform(request) {
        Ok(response) => response,
        Err(error) => Response::Failed { operation, error },
    }
}

#[cfg(feature = "native-fido")]
fn perform(request: Request) -> Result<Response, WireError> {
    use crate::device::write::{AssertionRequest, CredentialRequest, Fido2Writer};
    use crate::secret::Secret;

    /// The application's [`crate::device::write::WriteError`], on its way back.
    fn wire(error: crate::device::write::WriteError) -> WireError {
        use crate::device::write::WriteError as W;
        match error {
            W::NotAttached(serial) => WireError::NotAttached { serial },
            W::WrongSecret { retries_left, .. } => WireError::WrongSecret { retries_left },
            W::Locked { .. } => WireError::Locked,
            W::Unsupported { reason, .. } => WireError::Unsupported { reason },
            W::TransportUnavailable { feature, .. } => WireError::Unsupported {
                reason: format!("the helper was built without the `{feature}` feature"),
            },
            W::ElevationRequired { .. } => WireError::Failed {
                reason: "the helper itself is not elevated, which should be impossible for a \
                         service — check what account it runs as"
                    .to_owned(),
            },
            W::Detached { .. } => WireError::Detached,
            W::Failed { reason, .. } => WireError::Failed { reason },
        }
    }

    /// Rebuild a PIN, **re-validating it here**.
    ///
    /// The privileged side does not trust the unprivileged side's checks. The
    /// application validates a PIN before it ever reaches the wire, and that is
    /// the check an operator's mistake meets; this is the check a caller who is
    /// not the application meets, and it costs one function call.
    fn pin(value: &super::protocol::WireSecret) -> Result<Secret, WireError> {
        Secret::from_operator_input(crate::secret::SecretKind::Fido2Pin, value.expose()).map_err(
            |e| WireError::Unsupported {
                reason: format!("the PIN in that request is not one this applet accepts: {e}"),
            },
        )
    }

    let mut fido = crate::device::native_fido::NativeFido2::for_key(serial_of(&request));

    match request {
        // `dispatch` answers a ping and never reaches here. Written as a refusal
        // rather than `unreachable!` all the same: a panic in this function unwinds
        // through `service_main`, which is `extern "system"`, and Rust aborts on
        // that — so an "impossible" arm would turn a wrong refactor into a service
        // that dies instead of one that says no.
        Request::Ping { .. } => Err(WireError::Failed {
            reason: "a ping reached the hardware dispatcher, which should not happen".to_owned(),
        }),
        Request::Fido2State { serial } => fido
            .fido2_state(serial)
            .map(|state| {
                Response::Fido2State(Fido2StateWire {
                    pin_set: state.pin_set,
                    min_pin_length: state.min_pin_length,
                    force_pin_change_set: state.force_pin_change_set,
                    resident_credentials: state.resident_credentials,
                    remaining_credential_slots: state.remaining_credential_slots,
                    pin_retries: state.pin_retries,
                })
            })
            .map_err(wire),
        Request::SetPin { serial, new } => fido
            .set_pin(serial, &pin(&new)?)
            .map(|()| Response::Done)
            .map_err(wire),
        Request::ChangePin {
            serial,
            current,
            new,
        } => fido
            .change_pin(serial, &pin(&current)?, &pin(&new)?)
            .map(|()| Response::Done)
            .map_err(wire),
        Request::SetMinPinLength {
            serial,
            length,
            pin: supplied,
        } => fido
            .set_min_pin_length(serial, length, &pin(&supplied)?)
            .map(|()| Response::Done)
            .map_err(wire),
        Request::ForcePinChange {
            serial,
            pin: supplied,
        } => fido
            .force_pin_change(serial, &pin(&supplied)?)
            .map(|()| Response::Done)
            .map_err(wire),
        Request::MakeCredential {
            serial,
            request,
            pin: supplied,
        } => fido
            .make_credential(
                serial,
                &CredentialRequest {
                    relying_party: request.relying_party,
                    relying_party_name: request.relying_party_name,
                    user_name: request.user_name,
                    user_display_name: request.user_display_name,
                    resident: request.resident,
                    require_user_verification: request.require_user_verification,
                },
                &pin(&supplied)?,
            )
            .map(|evidence| {
                Response::Credential(super::protocol::CredentialWire {
                    credential_id_hex: evidence.credential_id_hex,
                    relying_party: evidence.relying_party,
                    algorithm: evidence.algorithm,
                })
            })
            .map_err(wire),
        Request::GetAssertion {
            serial,
            request,
            pin: supplied,
        } => fido
            .get_assertion(
                serial,
                &AssertionRequest {
                    relying_party: request.relying_party,
                    credential_id_hex: request.credential_id_hex,
                    challenge_hex: request.challenge_hex,
                    require_user_verification: request.require_user_verification,
                },
                &pin(&supplied)?,
            )
            .map(|evidence| {
                Response::Assertion(super::protocol::AssertionWire {
                    credential_id_hex: evidence.credential_id_hex,
                    relying_party: evidence.relying_party,
                    user_verified: evidence.user_verified,
                    user_present: evidence.user_present,
                    counter: evidence.counter,
                })
            })
            .map_err(wire),
        Request::Reset { serial } => crate::device::ctaphid::reset(serial, "fido2.reset")
            .map(|()| Response::Done)
            .map_err(wire),
    }
}

#[cfg(feature = "native-fido")]
fn serial_of(request: &Request) -> u32 {
    match request {
        Request::Ping { .. } => 0,
        Request::Fido2State { serial }
        | Request::SetPin { serial, .. }
        | Request::ChangePin { serial, .. }
        | Request::SetMinPinLength { serial, .. }
        | Request::ForcePinChange { serial, .. }
        | Request::MakeCredential { serial, .. }
        | Request::GetAssertion { serial, .. }
        | Request::Reset { serial } => *serial,
    }
}

/// A helper built without the transport can still be installed, started and
/// pinged — and it says what it is missing rather than failing obscurely.
#[cfg(not(feature = "native-fido"))]
fn perform(_request: Request) -> Result<Response, WireError> {
    Err(WireError::Unsupported {
        reason: "the helper was built without the `native-fido` feature".to_owned(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_one_caller_is_served_at_a_time() {
        // There is one key. Two callers racing an `authenticatorReset` is not a
        // state worth being able to reason about.
        assert_eq!(MAX_INSTANCES, 1);
    }
}
