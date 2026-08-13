//! Behaviour tests for reading a key's applets, refusing one that is already
//! configured, and keeping the proof that a key was generated on the device
//! (`features/device-detection.md` phases 4, 5 and 6).
//!
//! These three phases are one story. The read exists so the refusal can be sound; the
//! refusal exists because overwriting a live credential cannot be undone; the
//! attestation exists because "generated on the device" is worth nothing as a claim
//! and everything as evidence.
//!
//! **No test here writes to a real key**, and none can: the executor only ever talks
//! to the traits in `device::write`, and the only implementation linked into a test
//! binary is the mock.

use yk_dist_manager::bootstrap::{
    AppletSnapshot, Confirmation, ExecutionRequest, Executor, Preflight, RunRecorder, Severity,
    Transports,
};
use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::device::write::{Fido2State, MockWriter, OtpState, PivState};
use yk_dist_manager::domain::{BootstrapRun, StepStatus, YubiKeyRecord};
use yk_dist_manager::template::plan::{PlannedCommand, plan};
use yk_dist_manager::template::{BootstrapTemplate, RenderContext};

const SERIAL: u32 = 20_423_633;

#[derive(Default)]
struct Recording {
    audit: Vec<(String, String, String)>,
}

impl RunRecorder for Recording {
    fn run_updated(&mut self, _: &BootstrapRun) -> Result<(), String> {
        Ok(())
    }
    fn audit(&mut self, event: &str, target: &str, detail: &str) -> Result<(), String> {
        self.audit
            .push((event.into(), target.into(), detail.into()));
        Ok(())
    }
}

fn template() -> BootstrapTemplate {
    BootstrapTemplate::builtin()
        .into_iter()
        .find(|t| t.id == "org-standard")
        .expect("the standard procedure ships with the build")
}

fn commands(template: &BootstrapTemplate) -> Vec<PlannedCommand> {
    let mut ctx = RenderContext::sample();
    ctx.key_serial = SERIAL.to_string();
    plan(template, &ctx).expect("the built-in template plans")
}

fn request<'a>(
    template: &'a BootstrapTemplate,
    commands: &'a [PlannedCommand],
) -> ExecutionRequest<'a> {
    ExecutionRequest {
        template,
        commands,
        serial: SERIAL,
        holder_id: None,
        operator: "felipe".into(),
        relying_party: "example.org".into(),
        certificate_subject: "CN=Ana Silva,OU=ESI".into(),
        certificate_email: "ana@example.org".into(),
        holder_display: "Ana Silva".into(),
    }
}

fn key_record() -> YubiKeyRecord {
    YubiKeyRecord::from_device(&DeviceInfo {
        serial: SERIAL,
        model: "YubiKey 5 NFC".into(),
        firmware: "5.7.4".into(),
        form_factor: "Keychain (USB-A)".into(),
        nfc: true,
        usb_applications: vec!["FIDO2".into(), "PIV".into(), "OTP".into()],
    })
}

fn preflight(applets: &AppletSnapshot) -> Vec<yk_dist_manager::bootstrap::Finding> {
    let template = template();
    let commands = commands(&template);
    let key = key_record();
    Preflight {
        commands: &commands,
        key: Some(&key),
        applets,
        can_write: true,
    }
    .run()
}

#[test]
fn scenario_a_key_that_has_already_been_bootstrapped_is_refused_with_the_way_forward() {
    // Given a key whose PIV signing slot already holds a certificate — somebody has
    // been here before, and a holder may be relying on that credential right now
    let applets = AppletSnapshot {
        piv: Some(PivState {
            occupied_slots: vec!["9c".into()],
            pin_changed_from_default: true,
            management_key_changed: true,
            pin_retries: Some(3),
        }),
        fido2: Some(Fido2State {
            pin_set: true,
            ..Fido2State::default()
        }),
        otp: Some(OtpState::default()),
        unread: Vec::new(),
    };

    // When the pre-flight runs
    let findings = preflight(&applets);

    // Then the run is blocked, not warned about
    let refusal = findings
        .iter()
        .find(|f| f.severity == Severity::Blocking && f.message.contains("already been through"))
        .unwrap_or_else(|| panic!("a configured key must block the run: {findings:?}"));

    // And the refusal names both pieces of evidence and the only way forward. A
    // decision recorded 2026-08-13: a configured key is returned to factory default by
    // the system operator, and there is no in-place re-bootstrap — so there is
    // deliberately no override to offer, which makes naming the reset obligatory.
    assert!(refusal.message.contains("9c"), "{}", refusal.message);
    assert!(refusal.message.contains("FIDO2"), "{}", refusal.message);
    assert!(
        refusal.message.contains("factory default"),
        "a refusal with no exit leaves the operator stuck: {}",
        refusal.message
    );
    assert!(
        !refusal.message.to_lowercase().contains("anyway")
            && !refusal.message.to_lowercase().contains("override"),
        "there is no override, and the wording must not imply one: {}",
        refusal.message
    );
}

#[test]
fn scenario_a_factory_fresh_key_is_not_refused() {
    // The other half of the same rule: the refusal has to be about evidence, not
    // about having read anything at all. A key that answered every read and is clean
    // must pass.
    let applets = AppletSnapshot {
        piv: Some(PivState::default()),
        fido2: Some(Fido2State::default()),
        otp: Some(OtpState::default()),
        unread: Vec::new(),
    };
    let findings = preflight(&applets);
    assert!(
        !findings
            .iter()
            .any(|f| f.message.contains("already been through")),
        "{findings:?}"
    );
}

#[test]
fn scenario_an_applet_that_could_not_be_read_does_not_produce_a_refusal_it_cannot_justify() {
    // The dangerous middle case. With nothing read, the tool knows nothing — and it
    // must neither refuse (it has no evidence) nor quietly imply the key is clean.
    //
    // What makes that safe is that the *gaps travel with the snapshot*: the operator
    // sees "PIV was not read", so a clean-looking pre-flight against an unread applet
    // is visibly a pre-flight that did not look.
    let nothing = AppletSnapshot::default();
    let findings = preflight(&nothing);
    assert!(
        !findings
            .iter()
            .any(|f| f.message.contains("already been through")),
        "an unread applet is not evidence of a previous bootstrap: {findings:?}"
    );
    assert!(nothing.is_empty());
    let described = nothing.describe();
    assert!(
        described.iter().all(|line| line.contains("not read")),
        "every applet has to say it was not read: {described:?}"
    );
}

#[test]
fn scenario_generating_a_key_keeps_the_proof_that_it_happened_on_the_device() {
    // Given a factory-fresh key whose firmware can attest
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();

    // When the run applies the procedure
    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    // Then the key-generation step carries the attestation certificate in its record.
    // The certificate is public, so nothing here must not persist — and it is what
    // lets an auditor check, years later and without the key, that the private key was
    // generated on the card rather than loaded onto it.
    let keygen = run
        .steps
        .iter()
        .find(|s| s.step_id == "piv-keygen")
        .expect("the standard procedure generates a signing key");
    assert_eq!(keygen.status, StepStatus::Done, "{:?}", keygen.detail);
    let detail = keygen.detail.as_str();
    assert!(
        detail.contains("attestation") && detail.contains("BEGIN CERTIFICATE"),
        "the proof has to be in the record, not only read: {detail}"
    );

    // And the verification step re-read it rather than repeating the earlier claim —
    // verification is about the key as it is now.
    assert!(key.was_called("piv.attest"), "{:?}", key.operations());
    let verify = run
        .steps
        .iter()
        .find(|s| s.step_id == "verify")
        .expect("the standard procedure verifies");
    assert!(
        verify.detail.as_str().contains("attested_9c=yes"),
        "{:?}",
        verify.detail
    );
}

#[test]
fn scenario_a_key_that_cannot_attest_still_completes_but_says_the_proof_is_missing() {
    // Firmware below 4.3 has no attestation. That is a missing proof, not a failed
    // generation — failing the step would leave a key whose signing slot was changed
    // and whose run is marked failed, which is the worst of both. So the run completes
    // and the record says the claim is unproven, in those words, because a detail that
    // simply omitted attestation would read identically to one where nobody looked.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL).without_attestation();
    let mut recording = Recording::default();

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    let keygen = run
        .steps
        .iter()
        .find(|s| s.step_id == "piv-keygen")
        .expect("planned");
    assert_eq!(
        keygen.status,
        StepStatus::Done,
        "a missing proof must not fail a generation that succeeded: {:?}",
        keygen.detail
    );
    let detail = keygen.detail.as_str();
    assert!(
        detail.contains("NO attestation") && detail.contains("unproven"),
        "the absence has to be stated: {detail}"
    );

    let verify = run.steps.iter().find(|s| s.step_id == "verify").unwrap();
    assert!(
        verify.detail.contains("attested_9c=no"),
        "{:?}",
        verify.detail
    );
}

#[test]
fn scenario_the_read_never_reports_a_secret() {
    // AGENTS.md §2. The snapshot is shown on a screen and written to the trail, so
    // every field of it has to be a state, a count or a slot — never a value.
    let applets = AppletSnapshot {
        piv: Some(PivState {
            occupied_slots: vec!["9a".into(), "9c".into()],
            management_key_changed: true,
            pin_changed_from_default: true,
            pin_retries: Some(1),
        }),
        fido2: Some(Fido2State {
            pin_set: true,
            min_pin_length: Some(8),
            force_pin_change_set: true,
            resident_credentials: 1,
        }),
        otp: Some(OtpState {
            slot_one_programmed: true,
            slot_two_programmed: false,
            access_code_set: false,
        }),
        unread: Vec::new(),
    };
    let described = applets.describe().join(" | ");

    // What it does say: the states an operator needs.
    assert!(described.contains("9c"));
    assert!(described.contains("1 attempt(s) left"));
    assert!(described.contains("minimum PIN length 8"));

    // What it must never say. A PIN, a PUK, a management key or an access code has no
    // representation in the snapshot at all — these are the words that would show up
    // if one had been threaded through by mistake.
    for forbidden in ["123456", "12345678", "010203", "hex", "secret", "password"] {
        assert!(
            !described.to_lowercase().contains(forbidden),
            "the applet description must carry no secret material: found {forbidden:?} in \
             {described}"
        );
    }
}
