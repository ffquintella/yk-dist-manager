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
        Holder::new("", "ana@example.org", "ESI", "").unwrap_err(),
        ValidationError::Missing("full_name")
    ));
    assert!(matches!(
        Holder::new("Ana", "ana@example.org", "", "").unwrap_err(),
        ValidationError::Missing("unit")
    ));
    assert!(Holder::new("Ana", "ana@example.org", "ESI", "").is_ok());
}

#[test]
fn email_validation_rejects_malformed_addresses() {
    for bad in [
        "ana",
        "ana@",
        "@example.org",
        "ana@example",
        "ana@@example.org",
        "ana@example..org",
        "ana@.example.org",
        "ana space@example.org",
    ] {
        assert!(
            validate_email(bad).is_err(),
            "`{bad}` should have been rejected"
        );
    }
    for good in [
        "ana@example.org",
        "ana.maria+key@mail.example.org",
        "a_b-c%d@example.org",
    ] {
        assert!(validate_email(good).is_ok(), "`{good}` should be accepted");
    }
}

#[test]
fn oversized_input_is_refused() {
    let long = "a".repeat(MAX_TEXT + 1);
    assert!(matches!(
        Holder::new(&long, "ana@example.org", "ESI", "").unwrap_err(),
        ValidationError::TooLong { .. }
    ));
}

#[test]
fn certificate_subject_is_rfc4514_and_excludes_the_email() {
    let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "").unwrap();
    let subject = holder.certificate_subject("Example Organisation", "IT");
    assert_eq!(subject, "CN=Ana Silva,OU=IT,O=Example Organisation");
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

// ----------------------------------------- observations on a key record

#[test]
fn an_observation_is_summarised_for_one_line() {
    use yk_dist_manager::domain::key::summarise_note;

    // Absent reads like every other empty cell.
    assert_eq!(summarise_note("", 20), "—");
    assert_eq!(summarise_note("   ", 20), "—");
    // Newlines are folded: a table cell is one line.
    assert_eq!(
        summarise_note("box 2\nconnector bent", 40),
        "box 2 connector bent"
    );
    // Longer than the cell is cut with an ellipsis, and never mid-character.
    assert_eq!(summarise_note("áéíóú and more", 5), "áéíóú…");
    assert_eq!(summarise_note("exactly", 7), "exactly");
}

#[test]
fn an_observation_change_is_audited_by_shape_not_by_content() {
    use yk_dist_manager::domain::key::note_audit_detail;

    // The point of the helper: the trail says what moved, never what it says,
    // because an audit entry cannot be corrected and free text sometimes must be.
    let set = note_audit_detail("", "arrived in NF-8891");
    assert!(set.contains("note=set"));
    assert!(set.contains("chars=18"));
    assert!(!set.contains("NF-8891"));

    assert!(note_audit_detail("old", "").contains("note=cleared"));
    assert!(note_audit_detail("old", "new text").contains("note=changed"));
    assert!(note_audit_detail("", "").contains("note=unchanged"));
}

#[test]
fn removal_detail_names_what_the_register_is_losing() {
    use yk_dist_manager::domain::{SerialSource, YubiKeyRecord};

    let mut record = YubiKeyRecord::from_serial(20_423_633, SerialSource::ManualEntry);
    record.notes = "typed the wrong serial".into();
    let detail = record.removal_audit_detail();

    assert!(detail.contains("status=in_stock"));
    assert!(detail.contains("source=manual-entry"));
    // A key recorded from a serial alone has no model to name, and says so
    // rather than leaving an empty field.
    assert!(detail.contains("model=(unknown)"));
    assert!(detail.contains("note_chars=22"));
    assert!(!detail.contains("wrong serial"), "the note is not quoted");
}

#[test]
fn stored_and_audited_names_are_one_spelling() {
    use yk_dist_manager::domain::{KeyStatus, SerialSource};
    use yk_dist_manager::store::{key_status_str, serial_source_str};

    for status in [
        KeyStatus::InStock,
        KeyStatus::Bootstrapped,
        KeyStatus::Distributed,
        KeyStatus::Returned,
        KeyStatus::Lost,
        KeyStatus::Retired,
    ] {
        assert_eq!(status.audit_name(), key_status_str(status));
    }
    for source in SerialSource::ALL {
        assert_eq!(source.audit_name(), serial_source_str(source));
    }
}
