//! Hardware tests. **Ignored by default** — they need a real YubiKey attached.
//!
//! Everything here is strictly read-only: identification and capability
//! reading. No test in this repository may write to a key.
//!
//! ```bash
//! cargo test --features native-device --test hardware_native -- --ignored --nocapture
//! ```

#![cfg(feature = "native-piv")]

use yk_dist_manager::device::{NativeBackend, YubiKeyBackend};

#[test]
#[ignore = "requires an attached YubiKey"]
fn native_backend_reads_the_attached_key() {
    let backend = NativeBackend::new();

    let serials = backend.list_serials().expect("PC/SC readers enumerated");
    assert!(
        !serials.is_empty(),
        "no YubiKey found over PC/SC — is one attached, and is the PIV applet enabled?"
    );

    let info = backend
        .info(None)
        .or_else(|_| backend.info(Some(serials[0])));
    let info = info.expect("device identified");

    println!(
        "native: serial={} firmware={} reader={}",
        info.serial, info.firmware, info.model
    );

    assert_eq!(info.serial, serials[0]);
    assert!(
        info.firmware.split('.').count() >= 2,
        "firmware version looks wrong: {}",
        info.firmware
    );
}

/// Read the FIDO2 applet's state through the real transport.
///
/// **Read-only**, like everything else here: `get_info` is a status read that
/// changes nothing. It is the half of `NativeFido2` that can be verified
/// automatically — the write half is a manual procedure against a dedicated test
/// key, recorded in `features/step-fido2-pin.md`, because setting a PIN can only
/// be undone by a reset that destroys every credential on the key.
#[cfg(feature = "native-fido")]
#[test]
#[ignore = "requires an attached YubiKey"]
fn native_fido_reads_the_applet_state_without_writing() {
    use yk_dist_manager::device::native_fido::NativeFido2;
    use yk_dist_manager::device::write::Fido2Writer;

    let serials = NativeBackend::new().list_serials().expect("PC/SC read");
    let serial = *serials.first().expect("a key is attached");

    let mut fido = NativeFido2::for_key(serial);
    let state = fido
        .fido2_state(serial)
        .expect("the FIDO2 applet answered get_info");

    println!(
        "fido2: pin_set={} min_pin_length={:?} force_pin_change={} ",
        state.pin_set, state.min_pin_length, state.force_pin_change_set
    );

    // Nothing about the *values* is asserted — a key under test may or may not
    // have a PIN. What is asserted is that the mapping produced a coherent
    // answer rather than a default-constructed one hiding a parse failure.
    if let Some(length) = state.min_pin_length {
        assert!(
            (4..=63).contains(&length),
            "a CTAP minimum PIN length outside 4..=63 means the field was misread: {length}"
        );
    }
    assert_eq!(
        state.resident_credentials, 0,
        "counting resident credentials needs the PIN, so this transport reports 0 — \
         if that ever changes, the idempotency check in bootstrap::steps must change with it"
    );

    // A key that has no PIN cannot hold a discoverable credential, which is the
    // case the executor's idempotency check actually relies on.
    if !state.pin_set {
        assert!(
            !state.force_pin_change_set,
            "a key with no PIN cannot be marked for a forced PIN change"
        );
    }
}

/// Refuse to talk to a key the run is not about.
///
/// HID exposes no serial, so this guard is the only thing standing between a run
/// and writing a PIN to the wrong key when two are attached.
#[cfg(feature = "native-fido")]
#[test]
#[ignore = "requires an attached YubiKey"]
fn native_fido_refuses_a_serial_it_was_not_opened_for() {
    use yk_dist_manager::device::native_fido::NativeFido2;
    use yk_dist_manager::device::write::{Fido2Writer, WriteError};

    let serials = NativeBackend::new().list_serials().expect("PC/SC read");
    let serial = *serials.first().expect("a key is attached");

    let mut fido = NativeFido2::for_key(serial);
    let wrong = serial.wrapping_add(1);
    assert!(
        matches!(fido.fido2_state(wrong), Err(WriteError::NotAttached(s)) if s == wrong),
        "a mismatched serial must be refused before any CTAP call"
    );
}

#[test]
#[ignore = "requires an attached YubiKey"]
fn native_and_ykman_agree_on_the_serial() {
    use yk_dist_manager::device::YkmanBackend;

    let native = NativeBackend::new().list_serials().expect("native read");
    let ykman = YkmanBackend::default().list_serials().expect("ykman read");

    assert_eq!(
        native, ykman,
        "the native transport and the ykman fallback must see the same hardware"
    );
}
