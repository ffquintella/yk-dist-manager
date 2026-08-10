//! Unit tests for the device backends' contracts and error paths.
//!
//! No hardware: the `ykman` backend is pointed at a binary that does not exist,
//! and the mock backend stands in for a key.

use yk_dist_manager::device::{DeviceError, DeviceInfo, MockBackend, YkmanBackend, YubiKeyBackend};

#[test]
fn a_missing_ykman_says_what_to_install() {
    let backend = YkmanBackend::new("ykman-that-does-not-exist-9c3f");

    let err = backend.list_serials().expect_err("must fail");
    assert!(
        matches!(err, DeviceError::ToolMissing { .. }),
        "expected ToolMissing, got: {err}"
    );
    assert!(
        err.to_string().contains("yubikey-manager"),
        "the message must tell the operator what to install: {err}"
    );

    // The same classification on the identification path.
    let err = backend.info(None).expect_err("must fail");
    assert!(matches!(err, DeviceError::ToolMissing { .. }));
}

#[test]
fn backends_describe_their_transport() {
    let ykman = YkmanBackend::new("/opt/homebrew/bin/ykman");
    assert!(ykman.describe().contains("ykman"));
    assert!(ykman.describe().contains("/opt/homebrew/bin/ykman"));
    assert_eq!(
        ykman.binary().to_string_lossy(),
        "/opt/homebrew/bin/ykman",
        "the configured path is what gets invoked"
    );

    assert!(MockBackend::single_5nfc().describe().contains("mock"));
}

#[test]
fn the_default_ykman_backend_uses_the_bare_binary_name() {
    assert_eq!(
        YkmanBackend::default().binary().to_string_lossy(),
        "ykman",
        "resolved from PATH unless overridden"
    );
}

#[test]
fn the_mock_key_matches_the_recorded_hardware() {
    let backend = MockBackend::single_5nfc();
    let info = backend.info(None).unwrap();

    assert_eq!(info.serial, 20_423_633);
    assert_eq!(info.model, "YubiKey 5 NFC");
    assert_eq!(info.firmware, "5.4.3");
    assert!(info.nfc);
    assert_eq!(backend.list_serials().unwrap(), vec![20_423_633]);
}

#[test]
fn selecting_a_serial_that_is_not_attached_is_no_device() {
    let backend = MockBackend::single_5nfc();
    let err = backend.info(Some(1)).expect_err("must fail");
    assert!(matches!(err, DeviceError::NoDevice));
}

#[test]
fn selecting_by_serial_disambiguates_two_attached_keys() {
    let backend = MockBackend::single_5nfc();
    let first = backend.info(None).unwrap();
    let second = DeviceInfo {
        serial: 31_415_926,
        model: "YubiKey 5C FIPS".into(),
        firmware: "5.7.1".into(),
        ..first.clone()
    };
    backend.set_devices(vec![first.clone(), second.clone()]);

    // Ambiguous without a serial...
    assert!(matches!(
        backend.info(None).expect_err("must refuse"),
        DeviceError::Ambiguous(2)
    ));
    // ...but resolvable with one.
    assert_eq!(backend.info(Some(31_415_926)).unwrap().firmware, "5.7.1");
    assert_eq!(
        backend.info(Some(20_423_633)).unwrap().model,
        "YubiKey 5 NFC"
    );
    assert_eq!(backend.list_serials().unwrap().len(), 2);
}

#[test]
fn unplugging_the_last_key_is_reported_not_cached() {
    let backend = MockBackend::single_5nfc();
    assert!(backend.info(None).is_ok());

    backend.set_devices(vec![]);
    assert!(matches!(
        backend.info(None).expect_err("must fail"),
        DeviceError::NoDevice
    ));
    assert!(backend.list_serials().unwrap().is_empty());
}

#[test]
fn a_failing_transport_surfaces_its_reason() {
    let backend = MockBackend::failing("reader busy");

    let err = backend.list_serials().expect_err("must fail");
    assert!(err.to_string().contains("reader busy"), "got: {err}");
    assert!(backend.info(None).is_err());
}

#[test]
fn error_messages_name_the_situation_an_operator_can_act_on() {
    assert!(
        DeviceError::NoDevice
            .to_string()
            .contains("no YubiKey detected")
    );
    assert!(DeviceError::Ambiguous(3).to_string().contains("3"));
    assert!(
        DeviceError::Parse {
            command: "ykman info".into(),
            reason: "no serial".into(),
        }
        .to_string()
        .contains("ykman info")
    );
    assert!(
        DeviceError::Command {
            command: "ykman piv info".into(),
            message: "applet locked".into(),
        }
        .to_string()
        .contains("applet locked")
    );
}
