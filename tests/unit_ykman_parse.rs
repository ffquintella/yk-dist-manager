//! Unit tests for the `ykman` output parsers, against output recorded from
//! ykman 5.9.2 (`tests/fixtures/`).

use yk_dist_manager::device::ykman::{
    parse_info, parse_otp_info, parse_serials, supports_min_pin_length,
};

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

// ------------------------------------------------------- `ykman otp info`
//
// The read that says whether an OTP slot already holds a configuration
// (`features/device-detection.md` phase 4). It is a labelled fallback — no crate in
// this graph exposes the OTP HID status frame — and it feeds the phase-5 refusal, so a
// wrong answer here reports a programmed slot as free to overwrite.

#[test]
fn both_slots_are_read_from_ykman_otp_info() {
    let state = parse_otp_info("Slot 1: programmed\nSlot 2: empty\n").expect("parses");
    assert!(state.slot_one_programmed);
    assert!(!state.slot_two_programmed);
}

#[test]
fn a_programmed_second_slot_is_not_confused_with_the_first() {
    let state = parse_otp_info("Slot 1: empty\nSlot 2: programmed\n").expect("parses");
    assert!(!state.slot_one_programmed);
    assert!(state.slot_two_programmed);
}

#[test]
fn an_unrecognised_state_word_is_never_treated_as_empty() {
    // The failure that matters. If a future `ykman` says something this parser does
    // not know, the slot must not silently become "free to overwrite" — it stays
    // unprogrammed *and* the line contributes nothing, so the applet reads as unknown
    // rather than as clean.
    let state = parse_otp_info("Slot 1: something-new\nSlot 2: empty\n").expect("parses");
    assert!(
        !state.slot_one_programmed,
        "an unknown word must not be read as programmed either — it is simply not evidence"
    );

    // And a page with no recognisable line at all is an error, not an empty key.
    let error = parse_otp_info("The OTP application is disabled.\n").expect_err("must fail");
    assert!(error.to_string().contains("disabled"), "got: {error}");
}

#[test]
fn extra_output_around_the_slot_lines_is_ignored() {
    // `ykman` prints headers and blank lines, and has added fields between versions.
    // Matching on the words rather than on line position is what makes that survivable.
    let state = parse_otp_info(
        "Device type: YubiKey 5 NFC\n\nSlot 1: programmed (Yubico OTP)\nSlot 2: empty\n",
    )
    .expect("parses");
    assert!(state.slot_one_programmed);
    assert!(!state.slot_two_programmed);
}

#[test]
fn an_access_code_is_never_claimed_from_a_read_because_it_cannot_be_read() {
    // Neither the status frame nor `ykman otp info` reports whether a slot carries an
    // access code; the only way to find out is to try a write and be rejected. So this
    // read must never assert one, or the pre-flight would skip setting a code the key
    // does not have.
    let state = parse_otp_info("Slot 1: programmed\nSlot 2: programmed\n").expect("parses");
    assert!(
        !state.access_code_set,
        "a read must not claim an access code it has no way of seeing"
    );
}
