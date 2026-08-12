//! Unit tests for record helpers and the plan branches the default template
//! does not exercise.

use chrono::{Duration, Utc};
use yk_dist_manager::domain::{
    ChangeEnforcement, CustodyModel, DeliveryMethod, DistributionRecord, Holder, KeyStatus,
    RunStatus, StepKind,
};
use yk_dist_manager::template::{
    Arg, BootstrapTemplate, RenderContext, TemplateStep, Transport, plan,
};

fn record(distributed_days_ago: i64, returned_days_ago: Option<i64>) -> DistributionRecord {
    let now = Utc::now();
    DistributionRecord {
        id: uuid::Uuid::new_v4(),
        key_id: uuid::Uuid::new_v4(),
        key_serial: 20_423_633,
        holder_id: uuid::Uuid::new_v4(),
        holder_display: "Ana Silva <ana.silva@example.org>".into(),
        distributed_at: now - Duration::days(distributed_days_ago),
        distributed_by: "felipe".into(),
        method: DeliveryMethod::InPerson,
        receipt_ref: "TERM-1".into(),
        bootstrap_run_id: None,
        returned_at: returned_days_ago.map(|d| now - Duration::days(d)),
        returned_to: returned_days_ago.map(|_| "felipe".to_owned()),
        notes: String::new(),
    }
}

#[test]
fn an_open_record_counts_days_up_to_now() {
    let open = record(30, None);
    assert!(open.is_open());
    assert_eq!(open.days_held(Utc::now()), 30);
}

#[test]
fn a_closed_record_counts_days_up_to_the_return() {
    let closed = record(30, Some(10));
    assert!(!closed.is_open());
    // Handed over 30 days ago, returned 10 days ago → held for 20.
    assert_eq!(closed.days_held(Utc::now()), 20);
}

#[test]
fn delivery_methods_are_all_labelled() {
    assert_eq!(DeliveryMethod::ALL.len(), 3);
    for method in DeliveryMethod::ALL {
        assert!(!method.label().is_empty());
    }
    assert_eq!(DeliveryMethod::InPerson.label(), "In person");
    assert_eq!(DeliveryMethod::Courier.label(), "Internal courier");
}

#[test]
fn every_key_status_and_step_kind_has_a_label() {
    for status in [
        KeyStatus::InStock,
        KeyStatus::Bootstrapped,
        KeyStatus::Distributed,
        KeyStatus::Returned,
        KeyStatus::Lost,
        KeyStatus::Retired,
    ] {
        assert!(!status.label().is_empty());
    }
    for kind in [
        StepKind::Fido2Pin,
        StepKind::Fido2MinPinLength,
        StepKind::Fido2ForcePinChange,
        StepKind::Fido2Credential,
        StepKind::OtpAccessCode,
        StepKind::OtpSlotConfig,
        StepKind::PivPinPuk,
        StepKind::PivManagementKey,
        StepKind::PivKeygen,
        StepKind::PivCsr,
        StepKind::PivCertImport,
        StepKind::Verify,
    ] {
        assert!(!kind.label().is_empty());
    }
}

#[test]
fn transport_labels_distinguish_the_three_paths() {
    assert_eq!(Transport::Native.label(), "native");
    assert!(Transport::Ykman.label().contains("fallback"));
    assert_eq!(Transport::Manual.label(), "manual");
}

#[test]
fn run_status_serialises_for_the_database() {
    let encoded = serde_json::to_string(&RunStatus::Aborted).unwrap();
    let decoded: RunStatus = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, RunStatus::Aborted);
}

/// A template exercising the branches `org-standard` does not: an OTP slot
/// credential, and an escrowed (rather than PIN-protected) management key.
fn variant_template() -> BootstrapTemplate {
    BootstrapTemplate {
        id: "variant".into(),
        name: "Variant".into(),
        version: "1".into(),
        description: "OTP slot programming plus an escrowed management key".into(),
        steps: vec![
            TemplateStep::new(
                "otp-slot",
                StepKind::OtpSlotConfig,
                "Program challenge-response in slot 2",
            )
            .with_param("slot", "2"),
            TemplateStep::new(
                "piv-mgmt",
                StepKind::PivManagementKey,
                "Set an escrowed management key",
            )
            .with_param("algorithm", "tdes")
            .with_param("protect", "false"),
        ],
        signature: None,
    }
}

#[test]
fn otp_slot_programming_is_planned_through_the_fallback() {
    let ctx = ctx();
    let commands = plan(&variant_template(), &ctx).unwrap();
    let otp = commands
        .iter()
        .find(|c| c.kind == StepKind::OtpSlotConfig)
        .unwrap();

    assert_eq!(otp.transport(), Transport::Ykman);
    let rendered = otp.redacted_command();
    assert!(rendered.contains("otp chalresp"), "got: {rendered}");
    assert!(rendered.contains("--generate 2"), "got: {rendered}");
    assert!(
        !otp.carries_secret(),
        "a generated challenge-response secret never reaches the command line"
    );
}

#[test]
fn an_unprotected_management_key_is_planned_as_a_secret() {
    let commands = plan(&variant_template(), &ctx()).unwrap();
    let mgmt = commands
        .iter()
        .find(|c| c.kind == StepKind::PivManagementKey)
        .unwrap();

    // Without --protect, a value must be supplied — so it is a secret, and the
    // custody question becomes real.
    assert!(mgmt.carries_secret());
    assert!(mgmt.args.contains(&Arg::Secret("PIV-MGMT-KEY")));
    let rendered = mgmt.redacted_command();
    assert!(rendered.contains("<PIV-MGMT-KEY>"), "got: {rendered}");
    assert!(!rendered.contains("--protect"), "got: {rendered}");
    assert!(rendered.contains("--algorithm tdes"), "got: {rendered}");
}

#[test]
fn a_literal_argument_renders_as_itself() {
    let literal = Arg::literal("9c");
    assert_eq!(literal.redacted(), "9c");
    assert!(!literal.is_secret());
    assert_eq!(Arg::Secret("PIV-PIN").redacted(), "<PIV-PIN>");
}

#[test]
fn a_plan_without_a_serial_omits_the_device_selector() {
    let mut ctx = ctx();
    ctx.key_serial = String::new();
    let commands = plan(&variant_template(), &ctx).unwrap();
    for command in &commands {
        assert!(
            !command.redacted_command().contains("--device"),
            "no serial means no device selector: {}",
            command.redacted_command()
        );
    }
}

#[test]
fn the_verify_step_names_what_it_reads_back() {
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    let verify = commands
        .iter()
        .find(|c| c.kind == StepKind::Verify)
        .unwrap();
    assert!(verify.transport_detail().contains("get_info"));
    assert!(verify.note.as_deref().unwrap().contains("evidence"));
}

#[test]
fn native_steps_report_the_native_call_as_their_detail() {
    let commands = plan(&BootstrapTemplate::org_standard(), &ctx()).unwrap();
    let keygen = commands
        .iter()
        .find(|c| c.kind == StepKind::PivKeygen)
        .unwrap();
    assert_eq!(keygen.transport(), Transport::Native);
    assert_eq!(keygen.transport_detail(), "yubikey::piv::generate");
    assert!(
        keygen.native.as_ref().unwrap().feature == "native-piv",
        "the plan states which feature the native path needs"
    );
}

fn ctx() -> RenderContext {
    let holder = Holder::new("Ana Silva", "ana.silva@example.org", "ESI", "").unwrap();
    RenderContext::for_holder(&holder, 20_423_633, "felipe", "Example Organisation")
}

#[test]
fn a_dry_run_records_that_no_secret_was_set() {
    // The custody vocabulary is fixed (model B is the default for real runs), so
    // a dry run must say "no secret" rather than leave free text behind.
    let note = CustodyModel::NoSecretSet.note(None);
    assert_eq!(note, "no-secret-set");
    assert_eq!(CustodyModel::parse(&note), Some(CustodyModel::NoSecretSet));
    assert!(!CustodyModel::NoSecretSet.hands_a_secret_to_the_holder());
}

#[test]
fn model_b_needs_an_out_of_band_channel_but_nothing_retained() {
    let model = CustodyModel::DEFAULT;
    assert_eq!(model, CustodyModel::TransportPinForcedChange);
    assert!(
        model.hands_a_secret_to_the_holder(),
        "a transport PIN has to reach the holder somehow"
    );
    assert!(
        !model.requires_reference(),
        "nothing is retained, so there is no reference to record"
    );
    assert_eq!(model.note(None), "transport-pin+forced-change");
}

#[test]
fn escrow_is_the_only_model_that_records_a_pointer() {
    for model in CustodyModel::ALL {
        assert_eq!(
            model.requires_reference(),
            model == CustodyModel::Escrowed,
            "{model:?} disagrees about needing a reference"
        );
        assert!(!model.label().is_empty());
    }
}

#[test]
fn a_pre_5_7_key_cannot_enforce_the_change_itself() {
    // The reference key here is firmware 5.4.3, so model B falls back to an
    // instruction on the hand-over term — and the run must say which applied.
    assert_eq!(
        ChangeEnforcement::for_fido2("5.4.3").as_str(),
        "instructed-on-handover"
    );
    assert_eq!(
        ChangeEnforcement::for_fido2("5.7.1").as_str(),
        "enforced-by-firmware"
    );
    assert_eq!(
        ChangeEnforcement::for_piv().as_str(),
        "instructed-on-handover",
        "PIV has no force-change flag at any firmware level"
    );
}
