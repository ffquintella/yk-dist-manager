//! Unit tests for the domain records: validation, lifecycle, subjects.

use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::domain::holder::escape_rfc4514;
use yk_dist_manager::domain::{
    Holder, KeyStatus, MAX_TEXT, StepKind, StepStatus, ValidationError, YubiKeyRecord,
    validate_email,
};

fn device() -> DeviceInfo {
    DeviceInfo {
        serial: 20_423_633,
        model: "YubiKey 5 NFC".into(),
        firmware: "5.4.3".into(),
        form_factor: "Keychain (USB-A)".into(),
        nfc: true,
        usb_applications: vec!["FIDO2".into(), "PIV".into()],
    }
}

#[test]
fn key_starts_in_stock_and_derives_fields_from_the_device() {
    let record = YubiKeyRecord::from_device(&device());
    assert_eq!(record.serial, 20_423_633);
    assert_eq!(record.status, KeyStatus::InStock);
    assert!(!record.fips);
    assert_eq!(record.firmware_triple(), Some((5, 4, 3)));
}

#[test]
fn fips_series_is_flagged_from_the_model_name() {
    let mut info = device();
    info.model = "YubiKey 5C FIPS".into();
    assert!(YubiKeyRecord::from_device(&info).fips);
}

#[test]
fn refresh_keeps_lifecycle_and_notes() {
    let mut record = YubiKeyRecord::from_device(&device());
    record.status = KeyStatus::Distributed;
    record.notes = "engraved".into();

    let mut newer = device();
    newer.firmware = "5.7.1".into();
    record.refresh_from_device(&newer);

    assert_eq!(record.firmware, "5.7.1");
    assert_eq!(record.status, KeyStatus::Distributed, "must not be reset");
    assert_eq!(record.notes, "engraved");
    assert!(record.supports_fido_min_pin_length());
}

#[test]
fn lifecycle_transitions_are_restricted() {
    assert!(KeyStatus::InStock.can_transition_to(KeyStatus::Bootstrapped));
    assert!(KeyStatus::Bootstrapped.can_transition_to(KeyStatus::Distributed));
    assert!(KeyStatus::Distributed.can_transition_to(KeyStatus::Returned));
    assert!(KeyStatus::Distributed.can_transition_to(KeyStatus::Lost));

    // A key cannot be handed over before it is bootstrapped.
    assert!(!KeyStatus::InStock.can_transition_to(KeyStatus::Distributed));
    // Retirement is terminal.
    assert!(!KeyStatus::Retired.can_transition_to(KeyStatus::InStock));
    assert!(!KeyStatus::Retired.can_transition_to(KeyStatus::Distributed));
}

#[test]
fn holder_requires_name_email_and_unit() {
    assert!(matches!(
        Holder::new("", "ana@fgv.br", "ESI", "").unwrap_err(),
        ValidationError::Missing("full_name")
    ));
    assert!(matches!(
        Holder::new("Ana", "ana@fgv.br", "", "").unwrap_err(),
        ValidationError::Missing("unit")
    ));
    assert!(Holder::new("Ana", "ana@fgv.br", "ESI", "").is_ok());
}

#[test]
fn email_validation_rejects_malformed_addresses() {
    for bad in [
        "ana",
        "ana@",
        "@fgv.br",
        "ana@fgv",
        "ana@@fgv.br",
        "ana@fgv..br",
        "ana@.fgv.br",
        "ana space@fgv.br",
    ] {
        assert!(
            validate_email(bad).is_err(),
            "`{bad}` should have been rejected"
        );
    }
    for good in ["ana@fgv.br", "ana.maria+key@mail.fgv.br", "a_b-c%d@fgv.br"] {
        assert!(validate_email(good).is_ok(), "`{good}` should be accepted");
    }
}

#[test]
fn oversized_input_is_refused() {
    let long = "a".repeat(MAX_TEXT + 1);
    assert!(matches!(
        Holder::new(&long, "ana@fgv.br", "ESI", "").unwrap_err(),
        ValidationError::TooLong { .. }
    ));
}

#[test]
fn certificate_subject_is_rfc4514_and_excludes_the_email() {
    let holder = Holder::new("Ana Silva", "ana.silva@fgv.br", "ESI", "").unwrap();
    let subject = holder.certificate_subject("FGV", "ESI");
    assert_eq!(subject, "CN=Ana Silva,OU=ESI,O=FGV");
    assert!(
        !subject.contains('@'),
        "the e-mail belongs in the rfc822Name SAN, not the DN"
    );
}

#[test]
fn rfc4514_special_characters_are_escaped() {
    assert_eq!(escape_rfc4514("Silva, Ana"), "Silva\\, Ana");
    assert_eq!(escape_rfc4514("A+B"), "A\\+B");
    assert_eq!(escape_rfc4514("#hash"), "\\#hash");
    assert_eq!(escape_rfc4514(" pad "), "\\ pad\\ ");
}

#[test]
fn steps_that_set_a_secret_are_marked_as_such() {
    assert!(StepKind::Fido2Pin.sets_secret());
    assert!(StepKind::OtpAccessCode.sets_secret());
    assert!(StepKind::PivPinPuk.sets_secret());
    assert!(!StepKind::PivKeygen.sets_secret());
    assert!(!StepKind::Verify.sets_secret());
}

#[test]
fn step_status_round_trips_through_json() {
    let encoded = serde_json::to_string(&StepStatus::Done).unwrap();
    let decoded: StepStatus = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, StepStatus::Done);
}
