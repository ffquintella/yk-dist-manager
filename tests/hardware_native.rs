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
