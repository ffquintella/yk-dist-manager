//! Unit tests for the `ykman` output parsers, against output recorded from
//! ykman 5.9.2 (`tests/fixtures/`).

use yk_dist_manager::device::ykman::{parse_info, parse_serials, supports_min_pin_length};

fn fixture(name: &str) -> String {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/fixtures")
        .join(name);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("reading {}: {e}", path.display()))
}

#[test]
fn parses_serial_list() {
    assert_eq!(parse_serials("20423633\n"), vec![20_423_633]);
    assert_eq!(
        parse_serials("20423633\n31415926\n"),
        vec![20_423_633, 31_415_926]
    );
}

#[test]
fn ignores_non_numeric_noise_in_serial_list() {
    let stdout = "WARNING: something happened\n20423633\n\n";
    assert_eq!(parse_serials(stdout), vec![20_423_633]);
}

#[test]
fn empty_serial_list_is_not_an_error() {
    assert!(parse_serials("").is_empty());
}

#[test]
fn parses_full_info_output() {
    let info = parse_info(&fixture("ykman_info_5nfc.txt")).expect("parses");
    assert_eq!(info.serial, 20_423_633);
    assert_eq!(info.model, "YubiKey 5 NFC");
    assert_eq!(info.firmware, "5.4.3");
    assert_eq!(info.form_factor, "Keychain (USB-A)");
    assert!(info.nfc);
    assert!(info.usb_applications.contains(&"FIDO2".to_owned()));
    assert!(info.usb_applications.contains(&"PIV".to_owned()));
    assert_eq!(info.usb_applications.len(), 7);
}

#[test]
fn disabled_applications_are_not_listed() {
    let info = parse_info(&fixture("ykman_info_5c_partial.txt")).expect("parses");
    assert_eq!(info.serial, 31_415_926);
    assert_eq!(info.firmware, "5.7.1");
    assert!(!info.nfc, "no NFC line in this fixture");
    assert!(info.usb_applications.contains(&"FIDO2".to_owned()));
    assert!(
        !info.usb_applications.contains(&"OpenPGP".to_owned()),
        "OpenPGP is Disabled and must not appear as enabled"
    );
}

#[test]
fn info_without_a_serial_is_rejected() {
    let err = parse_info("Device type: YubiKey 5 NFC\n").expect_err("must fail");
    assert!(err.to_string().contains("serial"), "got: {err}");
}

#[test]
fn non_numeric_serial_is_rejected_rather_than_defaulted() {
    let err = parse_info("Serial number: not-a-number\n").expect_err("must fail");
    assert!(err.to_string().contains("not a number"), "got: {err}");
}

#[test]
fn min_pin_length_is_gated_on_firmware_5_7() {
    assert!(!supports_min_pin_length("5.4.3"));
    assert!(!supports_min_pin_length("5.6.9"));
    assert!(supports_min_pin_length("5.7.0"));
    assert!(supports_min_pin_length("5.7.1"));
    assert!(supports_min_pin_length("6.0.0"));
    assert!(!supports_min_pin_length("garbage"));
}
