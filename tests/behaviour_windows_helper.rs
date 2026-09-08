//! A Windows workstation that cannot reach the FIDO2 applet refuses the run
//! before it starts (`features/windows-elevated-helper.md` phase 1).
//!
//! ## Why this is not a `behaviour_app_*` test
//!
//! It would be better if it were, and it cannot be. `YkDistApp` takes its answer
//! from [`device::elevation::access`], which is derived from the *running*
//! process's platform and token and cached for the life of the process — so on the
//! machine this suite is developed on, and on two of the three CI legs, the answer
//! is permanently `Direct` and the interesting branch is unreachable through the
//! application.
//!
//! That is exactly why the pre-flight takes the access as a **field** rather than
//! asking for it: the decision is made once, at the edge, and everything below it
//! is a pure function that can be handed the answer a Windows workstation would
//! have given. The scenarios below are the ones an operator meets, driven through
//! [`bootstrap::preflight`] with the plan the standard procedure produces.
//!
//! No key, no service, no Windows, and — deliberately — no `YkDistApp`, so this
//! file writes nothing to anybody's settings.

use yk_dist_manager::bootstrap::preflight::{AppletSnapshot, Preflight, Severity, blocks};
use yk_dist_manager::device::DeviceInfo;
use yk_dist_manager::device::Fido2Access;
use yk_dist_manager::domain::{StepKind, YubiKeyRecord};
use yk_dist_manager::template::plan::{PlannedCommand, plan};
use yk_dist_manager::template::{Applicability, BootstrapTemplate, RenderContext};

fn key() -> YubiKeyRecord {
    YubiKeyRecord::from_device(&DeviceInfo {
        serial: 20_423_633,
        model: "YubiKey 5 NFC".into(),
        firmware: "5.7.4".into(),
        form_factor: "Keychain (USB-A)".into(),
        nfc: true,
        usb_applications: vec!["FIDO2".into(), "PIV".into(), "OTP".into()],
    })
}

fn standard_plan() -> Vec<PlannedCommand> {
    plan(&BootstrapTemplate::org_standard(), &RenderContext::sample())
        .expect("the standard procedure plans")
}

fn piv_only_plan() -> Vec<PlannedCommand> {
    standard_plan()
        .into_iter()
        .filter(|command| {
            !matches!(
                command.kind,
                StepKind::Fido2Pin
                    | StepKind::Fido2MinPinLength
                    | StepKind::Fido2ForcePinChange
                    | StepKind::Fido2Credential
            )
        })
        .collect()
}

fn findings(commands: &[PlannedCommand], access: Fido2Access) -> Vec<String> {
    let key = key();
    let applicability = Applicability::default();
    Preflight {
        commands,
        key: Some(&key),
        applets: &AppletSnapshot::default(),
        can_write: true,
        fido2_access: access,
        applicability: &applicability,
    }
    .run()
    .into_iter()
    .filter(|finding| finding.severity == Severity::Blocking)
    .map(|finding| finding.message)
    .collect()
}

#[test]
fn scenario_windows_without_the_helper_refuses_a_procedure_that_needs_fido2() {
    // Given a plan whose procedure includes the FIDO2 steps,
    let commands = standard_plan();

    // When the workstation is a Windows session that Windows will not let open a
    // security key, and no helper service is answering,
    let blocking = findings(&commands, Fido2Access::NeedsElevation);

    // Then the run is refused before it starts, rather than failing at the first
    // FIDO2 step with the PIV steps already written to the key.
    assert!(
        blocking
            .iter()
            .any(|message| message.contains("FIDO2 applet cannot be reached")),
        "the pre-flight did not block: {blocking:?}"
    );
}

#[test]
fn scenario_the_refusal_says_what_to_do_about_it() {
    let commands = standard_plan();
    let blocking = findings(&commands, Fido2Access::NeedsElevation);
    let message = blocking
        .iter()
        .find(|message| message.contains("FIDO2 applet cannot be reached"))
        .expect("the refusal is there");

    // Both ways out are named. A refusal that only says "no" is one an operator
    // takes to a ticket instead of solving.
    assert!(
        message.contains("MSI"),
        "the installer is not named: {message}"
    );
    assert!(
        message.contains("administrator"),
        "the interim answer is not named: {message}"
    );
    // And *why* it refuses rather than trying: a half-run key costs a factory
    // reset, which is the decision of 2026-08-13.
    assert!(
        message.contains("factory reset"),
        "the cost of starting anyway is not stated: {message}"
    );
}

#[test]
fn scenario_the_refusal_names_the_steps_that_cannot_run() {
    let commands = standard_plan();
    let blocking = findings(&commands, Fido2Access::NeedsElevation);
    let message = blocking
        .iter()
        .find(|message| message.contains("FIDO2 applet cannot be reached"))
        .expect("the refusal is there");

    // The operator's next question is "which steps?", and the answer is on the
    // screen rather than in a log.
    let fido_steps: Vec<&str> = commands
        .iter()
        .filter(|command| {
            matches!(
                command.kind,
                StepKind::Fido2Pin
                    | StepKind::Fido2MinPinLength
                    | StepKind::Fido2ForcePinChange
                    | StepKind::Fido2Credential
            )
        })
        .map(|command| command.step_id.as_str())
        .collect();
    assert!(
        !fido_steps.is_empty(),
        "the standard procedure has FIDO2 steps"
    );
    for step in fido_steps {
        assert!(
            message.contains(step),
            "the step `{step}` is not named in the refusal: {message}"
        );
    }
}

#[test]
fn scenario_a_piv_only_procedure_still_runs_on_a_workstation_with_no_helper() {
    // Given a template with no FIDO2 step — a PIV-only procedure is a real
    // deployment,
    let commands = piv_only_plan();
    assert!(!commands.is_empty(), "there are PIV steps left to plan");

    // When the same workstation cannot reach the FIDO2 applet,
    let blocking = findings(&commands, Fido2Access::NeedsElevation);

    // Then nothing is refused: blocking work over a capability the procedure never
    // needed would be refusing work for no reason.
    assert!(
        !blocking
            .iter()
            .any(|message| message.contains("FIDO2 applet cannot be reached")),
        "a PIV-only procedure was refused: {blocking:?}"
    );
}

#[test]
fn scenario_the_helper_makes_the_procedure_runnable_again() {
    let commands = standard_plan();

    // Given the same Windows session, but with the elevated helper answering,
    for access in [Fido2Access::Helper, Fido2Access::Direct] {
        let blocking = findings(&commands, access);

        // Then the FIDO2 refusal is gone. This is the whole point of the service:
        // the operator does not need to be elevated, and the procedure runs.
        assert!(
            !blocking
                .iter()
                .any(|message| message.contains("FIDO2 applet cannot be reached")),
            "{access:?} was still refused: {blocking:?}"
        );
    }
}

#[test]
fn scenario_the_refusal_actually_blocks_rather_than_warns() {
    // `blocks` is what the wizard's Start button reads. A finding that reads as a
    // refusal but does not set this would let the operator press on.
    let commands = standard_plan();
    let key = key();
    let applicability = Applicability::default();
    let findings = Preflight {
        commands: &commands,
        key: Some(&key),
        applets: &AppletSnapshot::default(),
        can_write: true,
        fido2_access: Fido2Access::NeedsElevation,
        applicability: &applicability,
    }
    .run();
    assert!(blocks(&findings), "the run was not blocked");
}
