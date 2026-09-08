//! The elevated FIDO2 helper's protocol and access decision, from outside the
//! crate (`features/windows-elevated-helper.md` phases 1 and 2).
//!
//! Headless and cross-platform on purpose. The part of a privileged service worth
//! testing hardest is the part that decides what it will agree to do, and that
//! part is pure here — so it runs on the developer's Mac, on the Linux CI leg and
//! on the Windows one, rather than only on the platform the service exists for.
//!
//! What this file cannot cover, and does not pretend to: the pipe, the service
//! dispatcher and the security descriptor's *effect*. Those are `#[cfg(windows)]`
//! FFI, compiled by CI's Windows leg and checked from a Mac by the scratch-crate
//! technique recorded for `WNetAddConnection2W`. The descriptor's *text* is
//! checked here, because it is the one line in this feature where a mistake is
//! exploitable from another machine.

use yk_dist_manager::device::elevation::{Availability, Fido2Access, decide};
use yk_dist_manager::device::helper::protocol::{
    AssertionRequestWire, CredentialRequestWire, MAX_MESSAGE, PROTOCOL_VERSION, Request, Response,
    WireError, WireSecret, declared_length, decode, encode, frame,
};
use yk_dist_manager::device::helper::{PIPE_NAME, PIPE_SDDL, SERVICE_ARG, SERVICE_NAME};

const PIN: &str = "471102";
const OTHER_PIN: &str = "938471";

fn every_request() -> Vec<Request> {
    vec![
        Request::Ping {
            protocol: PROTOCOL_VERSION,
        },
        Request::Fido2State { serial: 20_423_633 },
        Request::SetPin {
            serial: 20_423_633,
            new: WireSecret::new(PIN.into()),
        },
        Request::ChangePin {
            serial: 20_423_633,
            current: WireSecret::new(PIN.into()),
            new: WireSecret::new(OTHER_PIN.into()),
        },
        Request::SetMinPinLength {
            serial: 20_423_633,
            length: 8,
            pin: WireSecret::new(PIN.into()),
        },
        Request::ForcePinChange {
            serial: 20_423_633,
            pin: WireSecret::new(PIN.into()),
        },
        Request::MakeCredential {
            serial: 20_423_633,
            request: CredentialRequestWire {
                relying_party: "example.org".into(),
                relying_party_name: "Example".into(),
                user_name: "ana.souza@example.org".into(),
                user_display_name: "Ana Souza".into(),
                resident: true,
                require_user_verification: true,
            },
            pin: WireSecret::new(PIN.into()),
        },
        Request::GetAssertion {
            serial: 20_423_633,
            request: AssertionRequestWire {
                relying_party: "example.org".into(),
                credential_id_hex: "a1b2c3".into(),
                challenge_hex: "00112233".into(),
                require_user_verification: true,
            },
            pin: WireSecret::new(PIN.into()),
        },
        Request::Reset { serial: 20_423_633 },
    ]
}

#[test]
fn every_request_survives_the_wire_unchanged() {
    for request in every_request() {
        let encoded = encode(&request).expect("encodes");
        let back: Request = decode(&encoded).expect("decodes");
        assert_eq!(back.operation(), request.operation());
        assert_eq!(back.needs_user_presence(), request.needs_user_presence());
    }
}

/// The sweep AGENTS.md §2 asks for, on the one path where a secret leaves the
/// process. `Debug` is what a `tracing` field, a panic message and an `unwrap`
/// all reach for, so it is the rendering that has to be safe.
#[test]
fn no_pin_ever_reaches_a_debug_rendering() {
    for request in every_request() {
        let rendered = format!("{request:?}");
        assert!(
            !rendered.contains(PIN) && !rendered.contains(OTHER_PIN),
            "a PIN reached Debug for {}: {rendered}",
            request.operation()
        );
        assert!(
            !rendered.contains(PIN),
            "the redaction did not cover every secret field of {}",
            request.operation()
        );
    }
}

/// The value still has to get through — a redaction that also hid it from the
/// helper would be a very quiet way to break every FIDO2 step.
#[test]
fn a_redacted_secret_still_carries_its_value() {
    let secret = WireSecret::new(PIN.into());
    assert_eq!(secret.expose(), PIN);
    assert_eq!(format!("{secret:?}"), "<redacted>");

    let encoded = encode(&Request::SetPin {
        serial: 1,
        new: WireSecret::new(PIN.into()),
    })
    .expect("encodes");
    match decode::<Request>(&encoded).expect("decodes") {
        Request::SetPin { new, .. } => assert_eq!(new.expose(), PIN),
        other => panic!("decoded as {}", other.operation()),
    }
}

/// The property the whole security argument rests on: everything the helper will
/// change on a key needs somebody standing at the machine to touch it. If a
/// variant is ever added that mutates without presence, this fails — and the
/// reasoning in `features/windows-elevated-helper.md` has to be revisited rather
/// than quietly outgrown.
#[test]
fn every_mutating_request_requires_a_touch() {
    for request in every_request() {
        let reads_only = matches!(request, Request::Ping { .. } | Request::Fido2State { .. });
        assert_eq!(
            request.needs_user_presence(),
            !reads_only,
            "{} disagrees with its own presence requirement",
            request.operation()
        );
    }
}

#[test]
fn the_vocabulary_is_closed() {
    // No passthrough, no generic verb, no path. An operation the enum does not
    // name is refused at the decode, before anything privileged looks at it.
    for attempt in [
        br#"{"op":"exec","command":"cmd.exe"}"#.as_slice(),
        br#"{"op":"apdu","bytes":[0,1,2]}"#.as_slice(),
        br#"{"op":"read-file","path":"C:/register.sqlite3"}"#.as_slice(),
        br#"{"op":"reset"}"#.as_slice(), // right verb, missing the serial
    ] {
        assert!(
            decode::<Request>(attempt).is_err(),
            "the helper would have accepted {}",
            String::from_utf8_lossy(attempt)
        );
    }
}

#[test]
fn a_frame_declares_its_own_length_and_the_reader_bounds_it() {
    let framed = frame(b"hello").expect("frames");
    assert_eq!(&framed[..4], &5u32.to_be_bytes());
    assert_eq!(&framed[4..], b"hello");

    // The half that matters in a privileged process: the declared length is
    // refused *before* it is allocated for.
    let oversized = ((MAX_MESSAGE + 1) as u32).to_be_bytes();
    assert!(declared_length(oversized).is_err());
    assert!(frame(&vec![0u8; MAX_MESSAGE + 1]).is_err());
    assert_eq!(
        declared_length(MAX_MESSAGE_HEADER).expect("at the limit"),
        MAX_MESSAGE
    );
}

const MAX_MESSAGE_HEADER: [u8; 4] = (MAX_MESSAGE as u32).to_be_bytes();

#[test]
fn a_failure_comes_back_as_something_the_caller_can_act_on() {
    let response = Response::Failed {
        operation: "fido2.set_pin".into(),
        error: WireError::WrongSecret { retries_left: 2 },
    };
    let encoded = encode(&response).expect("encodes");
    match decode::<Response>(&encoded).expect("decodes") {
        Response::Failed { error, operation } => {
            assert_eq!(operation, "fido2.set_pin");
            assert_eq!(error, WireError::WrongSecret { retries_left: 2 });
        }
        _ => panic!("a failure decoded as something else"),
    }
}

// ---------------------------------------------------------------- the decision

#[test]
fn a_platform_that_does_not_guard_the_interface_always_goes_direct() {
    for helper in [false, true] {
        assert_eq!(
            decide(Availability {
                guarded_platform: false,
                elevated: false,
                helper_answering: helper,
            }),
            Fido2Access::Direct
        );
    }
}

#[test]
fn windows_without_elevation_or_a_helper_is_a_refusal() {
    // The branch the pre-flight exists to catch. Attempting the open here is what
    // leaves a key carrying a PIV PIN and an unfinished procedure.
    let decided = decide(Availability {
        guarded_platform: true,
        elevated: false,
        helper_answering: false,
    });
    assert_eq!(decided, Fido2Access::NeedsElevation);
    assert!(!decided.is_usable());
}

#[test]
fn an_elevated_operator_is_not_quietly_rerouted_through_a_service() {
    // Somebody running the application as administrator is usually the person
    // diagnosing this machine. Routing them through a service they may be about to
    // stop would make the diagnosis harder — the same argument `select::decide`
    // makes for honouring a transport override.
    assert_eq!(
        decide(Availability {
            guarded_platform: true,
            elevated: true,
            helper_answering: true,
        }),
        Fido2Access::Direct
    );
}

#[test]
fn an_ordinary_windows_session_goes_through_the_helper() {
    let decided = decide(Availability {
        guarded_platform: true,
        elevated: false,
        helper_answering: true,
    });
    assert_eq!(decided, Fido2Access::Helper);
    assert!(decided.is_usable());
}

// ------------------------------------------------------------- the descriptor

/// The one line in this feature where a mistake is exploitable from another
/// machine: a named pipe is reachable as `\\host\pipe\name` over SMB.
#[test]
fn the_pipe_is_denied_to_the_network_and_to_anonymous() {
    let deny_anonymous = PIPE_SDDL.find("(D;;GA;;;AN)").expect("denies ANONYMOUS");
    let deny_network = PIPE_SDDL.find("(D;;GA;;;NU)").expect("denies NETWORK");
    let allow_interactive = PIPE_SDDL
        .find("(A;;GRGW;;;IU)")
        .expect("allows INTERACTIVE");
    let allow_admins = PIPE_SDDL
        .find("(A;;GA;;;BA)")
        .expect("allows Administrators");

    // Deny entries first — the convention, and the order Windows evaluates.
    assert!(deny_anonymous < allow_interactive);
    assert!(deny_network < allow_interactive);
    assert!(deny_network < allow_admins);
}

#[test]
fn the_pipe_admits_nobody_it_does_not_name() {
    assert!(
        !PIPE_SDDL.contains(";WD)"),
        "the descriptor admits Everyone"
    );
    assert!(
        !PIPE_SDDL.contains(";AU)"),
        "the descriptor admits all authenticated users"
    );
    assert!(
        !PIPE_SDDL.contains(";BU)"),
        "the descriptor admits every local user"
    );
}

#[test]
fn an_interactive_caller_cannot_rewrite_the_descriptor_that_admits_them() {
    // `GA` for an interactive user would include WRITE_DAC.
    assert!(!PIPE_SDDL.contains("(A;;GA;;;IU)"));
    assert!(
        PIPE_SDDL.starts_with("O:SYG:SYD:P"),
        "not SYSTEM-owned, or not protected"
    );
}

#[test]
fn the_pipe_is_named_on_this_machine_and_nowhere_else() {
    // `\\.\pipe\…` is local. A name beginning `\\somehost\` would be a pipe on
    // another machine — not a thing to open, and not a thing to create.
    assert!(PIPE_NAME.starts_with(r"\\.\pipe\"));
    assert!(!PIPE_NAME[4..].contains(r"\\"));
}

#[test]
fn the_service_names_are_ones_the_installer_can_use() {
    // The MSI's ServiceInstall and ServiceControl both key on this name, and
    // `parse_args` refuses an unknown leading-dash argument — so a service arg
    // without one would start a GUI with no window station.
    assert!(SERVICE_ARG.starts_with("--"));
    assert!(!SERVICE_NAME.is_empty());
    assert!(
        SERVICE_NAME.chars().all(|c| c.is_ascii_alphanumeric()),
        "a service name with a space or a slash cannot be used by `sc`"
    );
}
