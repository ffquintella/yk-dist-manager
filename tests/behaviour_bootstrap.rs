//! Behaviour tests for the bootstrap workflow: plan a procedure for a holder,
//! record what was applied, and attach it to the hand-over.

use yk_dist_manager::device::{MockBackend, YubiKeyBackend};
use yk_dist_manager::domain::{BootstrapRun, Holder, RunStatus, StepKind, StepOutcome, StepStatus};
use yk_dist_manager::store::Store;
use yk_dist_manager::template::{BootstrapTemplate, RenderContext, Transport, plan};

fn holder() -> Holder {
    Holder::new("Ana Silva", "ana.silva@fgv.br", "ESI", "").unwrap()
}

#[test]
fn scenario_operator_plans_a_bootstrap_for_a_named_holder() {
    // Given a key read from the hardware and a holder
    let backend = MockBackend::single_5nfc();
    let info = backend.info(None).expect("one key attached");
    let holder = holder();

    // When the standard template is planned for them
    let ctx = RenderContext::for_holder(&holder, info.serial, "felipe", "FGV");
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx).unwrap();

    // Then every step the roadmap promises is present, bound to this person
    let kinds: Vec<StepKind> = commands.iter().map(|c| c.kind).collect();
    assert!(kinds.contains(&StepKind::Fido2Pin), "a PIN for FIDO");
    assert!(kinds.contains(&StepKind::OtpAccessCode), "a code for OTP");
    assert!(
        kinds.contains(&StepKind::Fido2Credential),
        "the initial FIDO credential, resident on the key"
    );
    assert!(
        kinds.contains(&StepKind::PivCsr),
        "the signing certificate request"
    );

    let csr = commands
        .iter()
        .find(|c| c.kind == StepKind::PivCsr)
        .unwrap();
    assert!(csr.redacted_command().contains("CN=Ana Silva"));
    assert!(
        csr.note
            .as_deref()
            .unwrap_or("")
            .contains("ana.silva@fgv.br"),
        "the certificate must carry the holder's e-mail"
    );
}

#[test]
fn scenario_the_plan_never_shows_a_pin() {
    // Given a planned bootstrap
    let ctx = RenderContext::for_holder(&holder(), 20_423_633, "felipe", "FGV");
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx).unwrap();

    // Then every secret is a placeholder, in the command and in the detail line
    for command in &commands {
        for arg in &command.args {
            if arg.is_secret() {
                assert!(command.redacted_command().contains(&arg.redacted()));
            }
        }
        assert!(!command.transport_detail().contains("--new-pin 1"));
    }
}

#[test]
fn scenario_operator_deselects_the_optional_steps() {
    // Given the standard template with its optional steps turned off
    let mut template = BootstrapTemplate::default_fgv();
    for step in &mut template.steps {
        if !step.required {
            step.enabled = false;
        }
    }

    // When it is planned
    let ctx = RenderContext::for_holder(&holder(), 20_423_633, "felipe", "FGV");
    let commands = plan(&template, &ctx).unwrap();

    // Then the deselected steps are absent, and the required ones remain
    assert!(
        !commands
            .iter()
            .any(|c| c.kind == StepKind::Fido2MinPinLength)
    );
    assert!(commands.iter().any(|c| c.kind == StepKind::Fido2Pin));
}

#[test]
fn scenario_a_recorded_run_says_what_was_applied() {
    // Given a database, a holder and a plan
    let store = Store::open_in_memory().unwrap();
    let holder = holder();
    store.insert_holder(&holder).unwrap();
    let ctx = RenderContext::for_holder(&holder, 20_423_633, "felipe", "FGV");
    let template = BootstrapTemplate::default_fgv();
    let commands = plan(&template, &ctx).unwrap();

    // When the run completes with every step applied
    let steps: Vec<StepOutcome> = commands
        .iter()
        .map(|command| {
            let mut outcome = StepOutcome::planned(
                command.step_id.clone(),
                command.kind,
                command.transport_detail(),
            );
            outcome.status = StepStatus::Done;
            outcome
        })
        .collect();
    let mut run = BootstrapRun::new(
        20_423_633,
        Some(holder.id),
        template.id.clone(),
        template.version.clone(),
        "felipe",
        steps,
    );
    run.custody = "sealed envelope 2026-08-10".into();
    run.settle();
    store.insert_run(&run).unwrap();

    // Then the run is complete and readable back, with its custody note
    assert_eq!(run.status, RunStatus::Completed);
    assert!(run.finished_at.is_some());

    let stored = store.runs().unwrap();
    assert_eq!(stored.len(), 1);
    assert_eq!(stored[0].template_id, "fgv-standard");
    assert_eq!(stored[0].custody, "sealed envelope 2026-08-10");
    assert!(stored[0].summary().contains("FIDO2 PIN"));
    assert!(stored[0].summary().contains("PIV certificate import"));
}

#[test]
fn scenario_a_failed_step_marks_the_whole_run_failed() {
    // Given a run whose certificate import failed
    let mut run = BootstrapRun::new(
        20_423_633,
        None,
        "fgv-standard",
        "1",
        "felipe",
        vec![
            done("fido2-pin", StepKind::Fido2Pin),
            failed("piv-cert-import", StepKind::PivCertImport),
        ],
    );

    // When the run settles
    run.settle();

    // Then it is Failed, and the tally shows exactly what happened
    assert_eq!(run.status, RunStatus::Failed);
    let (done, failed, skipped, pending) = run.tally();
    assert_eq!((done, failed, skipped, pending), (1, 1, 0, 0));
}

#[test]
fn scenario_a_run_with_pending_steps_is_still_running() {
    let mut run = BootstrapRun::new(
        20_423_633,
        None,
        "fgv-standard",
        "1",
        "felipe",
        vec![
            done("fido2-pin", StepKind::Fido2Pin),
            StepOutcome::planned("verify", StepKind::Verify, ""),
        ],
    );
    run.settle();
    assert_eq!(run.status, RunStatus::Running);
    assert!(run.finished_at.is_none());
}

#[test]
fn scenario_a_dry_run_records_intent_without_claiming_anything_was_applied() {
    // Given a plan that was reviewed but not executed
    let ctx = RenderContext::for_holder(&holder(), 20_423_633, "felipe", "FGV");
    let commands = plan(&BootstrapTemplate::fido_only(), &ctx).unwrap();
    let steps: Vec<StepOutcome> = commands
        .iter()
        .map(|c| {
            let mut outcome = StepOutcome::planned(c.step_id.clone(), c.kind, "");
            outcome.status = StepStatus::Skipped;
            outcome
        })
        .collect();

    // When it is stored
    let mut run = BootstrapRun::new(20_423_633, None, "fido-only", "1", "felipe", steps);
    run.settle();

    // Then nothing is reported as applied
    assert_eq!(run.summary(), "fido-only 1 — nothing applied");
    let (done, _, skipped, _) = run.tally();
    assert_eq!(done, 0);
    assert!(skipped > 0);
}

#[test]
fn scenario_credential_creation_cannot_fall_back_to_ykman() {
    // Given the credential step
    let ctx = RenderContext::for_holder(&holder(), 20_423_633, "felipe", "FGV");
    let commands = plan(&BootstrapTemplate::default_fgv(), &ctx).unwrap();
    let credential = commands
        .iter()
        .find(|c| c.kind == StepKind::Fido2Credential)
        .unwrap();

    // Then it is native-only: ykman has no command that creates credentials
    assert_eq!(credential.transport(), Transport::Native);
    assert!(credential.program.is_none());
}

#[test]
fn scenario_no_key_attached_is_reported_clearly() {
    let backend = MockBackend::new(vec![]);
    let err = backend.info(None).expect_err("must fail");
    assert!(
        err.to_string().contains("no YubiKey detected"),
        "got: {err}"
    );
}

#[test]
fn scenario_two_keys_attached_is_refused_rather_than_guessed() {
    let backend = MockBackend::single_5nfc();
    let mut first = backend.info(None).unwrap();
    let mut second = first.clone();
    second.serial = 31_415_926;
    first.serial = 20_423_633;
    backend.set_devices(vec![first, second]);

    let err = backend.info(None).expect_err("must refuse");
    assert!(err.to_string().contains("more than one"), "got: {err}");
}

fn done(id: &str, kind: StepKind) -> StepOutcome {
    let mut outcome = StepOutcome::planned(id, kind, "");
    outcome.status = StepStatus::Done;
    outcome
}

fn failed(id: &str, kind: StepKind) -> StepOutcome {
    let mut outcome = StepOutcome::planned(id, kind, "");
    outcome.status = StepStatus::Failed;
    outcome
}
