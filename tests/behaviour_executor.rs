//! Behaviour tests for the bootstrap executor.
//!
//! Every scenario drives a whole run against `MockWriter`. **No test here writes
//! to a real key**, and none can: the executor only ever talks to the traits in
//! `device::write`, and the only implementation linked into a test binary is the
//! mock.
//!
//! The scenarios are named after what an operator would say happened.

use yk_dist_manager::bootstrap::{
    Confirmation, ExecutionError, ExecutionRequest, Executor, RunRecorder, Transports,
    irreversible_steps,
};
use yk_dist_manager::device::write::{Fido2State, MockWriter, OtpState, PivState, WriteError};
use yk_dist_manager::domain::{BootstrapRun, RunStatus, StepStatus};
use yk_dist_manager::template::plan::{PlannedCommand, plan};
use yk_dist_manager::template::{BootstrapTemplate, RenderContext};

const SERIAL: u32 = 20_423_633;

/// Captures everything the executor recorded, so a scenario can assert on the
/// evidence rather than on the return value alone.
#[derive(Default)]
struct Recording {
    /// One entry per persisted snapshot — the sequence a resumed or interrupted
    /// run would have left behind.
    snapshots: Vec<BootstrapRun>,
    audit: Vec<(String, String, String)>,
}

impl RunRecorder for Recording {
    fn run_updated(&mut self, run: &BootstrapRun) -> Result<(), String> {
        self.snapshots.push(run.clone());
        Ok(())
    }

    fn audit(&mut self, event: &str, target: &str, detail: &str) -> Result<(), String> {
        self.audit
            .push((event.into(), target.into(), detail.into()));
        Ok(())
    }
}

impl Recording {
    fn events(&self) -> Vec<&str> {
        self.audit.iter().map(|(e, _, _)| e.as_str()).collect()
    }

    fn detail_for(&self, event: &str) -> Option<&str> {
        self.audit
            .iter()
            .find(|(e, _, _)| e == event)
            .map(|(_, _, d)| d.as_str())
    }
}

/// A recorder that refuses to write anything — the "the register is unreachable"
/// case, where the run must not touch the key at all.
struct RefusesToRecord;

impl RunRecorder for RefusesToRecord {
    fn run_updated(&mut self, _: &BootstrapRun) -> Result<(), String> {
        Err("the share went away".into())
    }
    fn audit(&mut self, _: &str, _: &str, _: &str) -> Result<(), String> {
        Err("the share went away".into())
    }
}

fn template() -> BootstrapTemplate {
    BootstrapTemplate::builtin()
        .into_iter()
        .find(|t| t.id == "org-standard")
        .expect("the standard procedure ships with the build")
}

fn context() -> RenderContext {
    let mut ctx = RenderContext::sample();
    ctx.key_serial = SERIAL.to_string();
    ctx
}

fn commands(template: &BootstrapTemplate) -> Vec<PlannedCommand> {
    plan(template, &context()).expect("the built-in template plans")
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
        certificate_pem: None,
    }
}

#[test]
fn scenario_a_run_applies_every_step_and_records_what_it_did() {
    // Given a factory-fresh key and the standard procedure
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();

    // When the operator confirms and runs it
    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    // Then every step reached a terminal state — nothing is left Pending, which
    // would mean the record claims less than what happened
    assert!(
        run.steps
            .iter()
            .all(|s| s.status != StepStatus::Pending && s.status != StepStatus::Running),
        "a settled run leaves no step in flight: {:?}",
        run.steps
            .iter()
            .map(|s| (&s.step_id, s.status))
            .collect::<Vec<_>>()
    );

    // And the run is bracketed by a started/finished pair in the audit trail
    assert_eq!(recording.events().first(), Some(&"bootstrap.started"));
    assert_eq!(recording.events().last(), Some(&"bootstrap.finished"));

    // And the custody model recorded is B, which is what the run actually did
    assert_eq!(run.custody, "transport-pin+forced-change");
}

#[test]
fn scenario_a_run_without_a_confirmation_for_this_plan_writes_nothing() {
    // The gate. A confirmation for a different key, or for a plan with a
    // different number of steps, is not a confirmation for this run.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();

    let stale = Confirmation::given(SERIAL, commands.len() - 1);
    let refused = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor.run(&request, &stale, &mut recording)
    };

    assert!(matches!(
        refused,
        Err(ExecutionError::ConfirmationMismatch { .. })
    ));
    assert!(
        key.calls().is_empty(),
        "not one byte may reach the key without a matching confirmation"
    );
    assert!(
        recording.audit.is_empty(),
        "and the run never started, so there is nothing to audit"
    );

    // The same holds for a confirmation naming another key.
    let wrong_key = Confirmation::given(999, commands.len());
    let mut executor = Executor::new(Transports { backend: &mut key });
    assert!(matches!(
        executor.run(&request, &wrong_key, &mut recording),
        Err(ExecutionError::ConfirmationMismatch { .. })
    ));
}

#[test]
fn scenario_a_required_step_that_fails_aborts_the_run() {
    // Given a key whose FIDO2 applet is locked
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL)
        .fail("fido2.set_pin", WriteError::Locked { applet: "FIDO2" });
    let mut recording = Recording::default();

    // When the run is executed
    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    // Then the run is Failed, and the steps after the failure were never
    // attempted — the whole point of aborting
    assert_eq!(run.status, RunStatus::Failed);
    let failed_at = run
        .steps
        .iter()
        .position(|s| s.status == StepStatus::Failed)
        .expect("a step failed");
    assert!(
        run.steps[failed_at + 1..]
            .iter()
            .all(|s| s.status == StepStatus::Pending),
        "nothing after an aborted required step may run"
    );

    // And the trail says it was aborted rather than merely finishing
    assert!(recording.events().contains(&"bootstrap.step.failed"));
    assert_eq!(recording.events().last(), Some(&"bootstrap.aborted"));

    // And the reason is on the record, without a secret in it
    assert!(
        run.steps[failed_at].detail.contains("locked"),
        "got: {}",
        run.steps[failed_at].detail
    );
}

#[test]
fn scenario_an_optional_step_that_fails_is_recorded_and_the_run_continues() {
    // A key that works, minus the OTP slot, is a usable key. Stopping the whole
    // hand-over for it would be the wrong trade.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);

    // `fido2-min-pin-length` is optional in the standard procedure;
    // `otp-access-code`, despite sounding peripheral, is required.
    assert!(
        !template
            .steps
            .iter()
            .find(|s| s.id == "fido2-min-pin-length")
            .expect("the step exists")
            .required,
        "this scenario needs a genuinely optional step"
    );

    let mut key = MockWriter::factory_fresh(SERIAL).fail(
        "fido2.set_min_pin_length",
        WriteError::Failed {
            operation: "fido2.set_min_pin_length",
            reason: "the applet refused the policy".into(),
        },
    );
    let mut recording = Recording::default();

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    // The run finished rather than aborting.
    assert_eq!(recording.events().last(), Some(&"bootstrap.finished"));
    // It is Failed overall — something did fail — but later steps still ran.
    assert_eq!(run.status, RunStatus::Failed);
    assert!(
        run.steps.iter().any(|s| s.status == StepStatus::Failed),
        "the failure is on the record"
    );
    assert!(
        run.steps
            .iter()
            .filter(|s| s.status == StepStatus::Done)
            .count()
            > 1,
        "steps after the optional failure still ran"
    );
}

#[test]
fn scenario_re_running_on_a_configured_key_skips_rather_than_overwrites() {
    // The failure this prevents: under model B the holder is told to change the
    // transport PIN. A second run that blindly set a PIN would replace the one
    // the holder chose, and they would not know.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);

    let mut key = MockWriter::factory_fresh(SERIAL)
        .with_fido2_state(Fido2State {
            pin_set: true,
            min_pin_length: Some(8),
            force_pin_change_set: true,
            resident_credentials: 1,
            ..Default::default()
        })
        .with_piv_state(PivState {
            occupied_slots: vec!["9c".into()],
            management_key_is_default: Some(false),
            pin_is_default: Some(false),
            puk_is_default: Some(false),
            pin_retries: Some(3),
        });
    let mut recording = Recording::default();

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    // Nothing was written to the FIDO2 or PIV applets.
    assert!(
        !key.was_called("fido2.set_pin"),
        "an existing PIN must be left alone"
    );
    assert!(
        !key.was_called("piv.generate_key"),
        "generating into an occupied slot would destroy the certificate in it"
    );

    // And the record says *why* each was skipped, not merely that it was.
    let skipped: Vec<&str> = run
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Skipped)
        .map(|s| s.detail.as_str())
        .collect();
    assert!(
        skipped.iter().any(|d| d.contains("already")),
        "a skip has to explain itself: {skipped:?}"
    );
    assert!(
        recording
            .audit
            .iter()
            .any(|(e, _, d)| e == "bootstrap.step.skipped" && d.contains("already-applied"))
    );
}

#[test]
fn scenario_an_interrupted_run_resumes_from_where_it_stopped() {
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);

    // Given a run that died when the key was pulled out
    let mut key = MockWriter::factory_fresh(SERIAL).fail(
        "fido2.force_pin_change",
        WriteError::Detached {
            operation: "fido2.force_pin_change",
        },
    );
    let mut recording = Recording::default();
    let confirmation = Confirmation::given(SERIAL, commands.len());

    let interrupted = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };
    assert_eq!(interrupted.status, RunStatus::Failed);
    let done_first_time = interrupted
        .steps
        .iter()
        .filter(|s| s.status == StepStatus::Done)
        .count();
    assert!(done_first_time > 0, "some steps had already succeeded");

    // When the key is plugged back in and the run is resumed
    let mut key = MockWriter::factory_fresh(SERIAL).with_fido2_state(Fido2State {
        pin_set: true,
        ..Default::default()
    });
    let mut resumed_recording = Recording::default();

    let resumed = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .resume(&request, interrupted.clone(), &mut resumed_recording)
            .unwrap()
    };

    // Then the steps that had succeeded were not repeated
    assert!(
        !key.was_called("fido2.set_pin"),
        "a step that already succeeded must not run twice — it would replace a \
         transport PIN that may already be on a slip"
    );
    assert_eq!(
        resumed_recording.events().first(),
        Some(&"bootstrap.resumed")
    );
    assert!(
        resumed
            .steps
            .iter()
            .filter(|s| s.status == StepStatus::Done)
            .count()
            >= done_first_time,
        "resuming makes progress rather than losing it"
    );
}

#[test]
fn scenario_a_run_that_cannot_be_recorded_does_not_touch_the_key() {
    // A key configured with no record of what was applied is the exact failure
    // this tool exists to prevent, so an unreachable register stops the run
    // before the first write rather than after it.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let refused = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor.run(&request, &confirmation, &mut RefusesToRecord)
    };

    assert!(matches!(refused, Err(ExecutionError::NotRecordable(_))));
    assert!(
        key.calls().is_empty() && key.calls().is_empty() && key.calls().is_empty(),
        "nothing may be written to a key the register cannot describe"
    );
}

#[test]
fn scenario_no_secret_reaches_the_run_record_or_the_audit_trail() {
    // The blunt sweep. Every value the run generated is checked against every
    // string it persisted — the step details, the audit entries, and the run's
    // own fields.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let mut executor = Executor::new(Transports { backend: &mut key });
    let run = executor
        .run(&request, &confirmation, &mut recording)
        .unwrap();

    let secrets = executor.take_secrets();
    assert!(
        !secrets.is_empty(),
        "the run must have generated something, or this test proves nothing"
    );

    // Everything that was persisted, as one haystack.
    let mut haystack = String::new();
    for snapshot in &recording.snapshots {
        haystack.push_str(&snapshot.custody);
        for step in &snapshot.steps {
            haystack.push_str(&step.detail);
            haystack.push_str(&step.step_id);
        }
    }
    for (event, target, detail) in &recording.audit {
        haystack.push_str(event);
        haystack.push_str(target);
        haystack.push_str(detail);
    }
    for step in &run.steps {
        haystack.push_str(&step.detail);
    }

    for secret in &secrets {
        assert!(
            !haystack.contains(secret.expose()),
            "a {} reached a persisted record",
            secret.kind().slug()
        );
    }

    // And the trail still says a secret *was* generated — the fact is recorded
    // even though the value is not.
    let generated = recording
        .detail_for("secret.generated")
        .expect("generation is audited");
    assert!(generated.contains("kind="), "got: {generated}");
    assert!(generated.contains("length="), "got: {generated}");
}

#[test]
fn scenario_a_run_missing_a_required_step_is_not_reported_as_completed() {
    // The case that makes this rule necessary rather than theoretical: the
    // certificate import is required by the standard procedure and skips on every
    // *first* run, because the issuer is manual — the CSR has to exist before a CA
    // can sign anything (`features/ca-integration.md` phase 1). A key with no
    // signing certificate must not be recorded as a completed bootstrap — that is
    // the one claim this register cannot be allowed to get wrong.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    let cert_import = run
        .steps
        .iter()
        .find(|s| s.step_id == "piv-cert-import")
        .expect("the standard procedure imports a certificate");
    assert_eq!(
        cert_import.status,
        StepStatus::Skipped,
        "nothing broke — there is simply no certificate to import yet"
    );
    // The record has to say what happens next, not merely that nothing happened:
    // this run's own request is what the operator has to take to the CA.
    assert!(
        cert_import.detail.contains("no certificate supplied"),
        "and the record says why: {}",
        cert_import.detail
    );
    assert!(
        cert_import.detail.contains("resume"),
        "and what to do about it: {}",
        cert_import.detail
    );

    assert_ne!(
        run.status,
        RunStatus::Completed,
        "a run missing a required step is not a completed bootstrap"
    );
    let incomplete = recording
        .detail_for("bootstrap.incomplete")
        .expect("the gap is audited, not merely implied by the status");
    assert!(incomplete.contains("piv-cert-import"), "got: {incomplete}");
    assert!(
        incomplete.contains("not ready to hand over"),
        "the entry has to say what it means for the operator: {incomplete}"
    );
}

#[test]
fn scenario_the_confirmation_lists_what_cannot_be_undone() {
    // `features/bootstrap-engine.md` rule 8: no rollback pretence. The dialog has
    // to name the steps that cannot be reversed, so the operator agrees to the
    // real thing.
    let template = template();
    let commands = commands(&template);
    let irreversible = irreversible_steps(&commands);

    assert!(
        !irreversible.is_empty(),
        "the standard procedure does contain irreversible steps"
    );
    let kinds: Vec<&str> = irreversible.iter().map(|c| c.kind.slug()).collect();
    assert!(
        kinds.contains(&"piv-keygen"),
        "generating over a slot destroys what was in it: {kinds:?}"
    );
}

#[test]
fn scenario_a_transport_that_drops_a_frame_is_retried_and_the_step_still_succeeds() {
    // `features/bootstrap-templates.md` phase 7. A busy reader or a dropped frame
    // is the failure a second attempt actually fixes, and before this the whole
    // run was abandoned for one — a key left half-configured because a card
    // transport hiccuped once.
    let mut template = template();
    for step in &mut template.steps {
        if step.id == "piv-pin-puk" {
            step.attempts = 3;
        }
    }
    let commands = commands(&template);
    let request = request(&template, &commands);

    // Given a key whose PIN/PUK write fails once, at the transport
    let mut key = MockWriter::factory_fresh(SERIAL).fail(
        "piv.set_pin_and_puk",
        WriteError::Failed {
            operation: "piv.set_pin_and_puk",
            reason: "the card did not answer".into(),
        },
    );
    let mut recording = Recording::default();

    // When the run goes ahead
    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    // Then the step succeeded on its second attempt
    let step = run
        .steps
        .iter()
        .find(|s| s.step_id == "piv-pin-puk")
        .expect("the step is in the run");
    assert_eq!(step.status, StepStatus::Done, "{}", step.detail);
    assert!(
        step.detail.contains("attempt 2 of 3"),
        "the record says it took two goes, because a flaky reader is worth \
         investigating: {}",
        step.detail
    );

    // And the retry is in the audit trail rather than only in the step's detail
    let retried = recording
        .detail_for("bootstrap.step.retried")
        .expect("a retry is a thing that happened to a key, so it is audited");
    assert!(retried.contains("piv-pin-puk"), "got: {retried}");
    assert!(retried.contains("attempt=1/3"), "got: {retried}");
}

#[test]
fn scenario_a_rejected_secret_is_never_retried_however_many_attempts_the_template_allows() {
    // The rule that makes the budget safe: retrying a rejected PIN spends the
    // applet's own retry counter, and three goes at a wrong PIV PIN is how a PIN
    // gets blocked. The template asks for three attempts and gets one.
    let mut template = template();
    for step in &mut template.steps {
        step.attempts = 3;
    }
    let commands = commands(&template);
    let request = request(&template, &commands);

    let mut key = MockWriter::factory_fresh(SERIAL).fail(
        "fido2.set_pin",
        WriteError::WrongSecret {
            applet: "FIDO2",
            retries_left: 2,
        },
    );
    let mut recording = Recording::default();

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    let attempts = key
        .calls()
        .iter()
        .filter(|c| c.operation == "fido2.set_pin")
        .count();
    assert_eq!(
        attempts, 1,
        "a rejected secret is attempted once, whatever the template says"
    );
    assert!(
        !recording.events().contains(&"bootstrap.step.retried"),
        "and nothing claims it was retried: {:?}",
        recording.events()
    );
    assert_eq!(run.status, RunStatus::Failed);
}

#[test]
fn scenario_the_attempt_budget_is_bounded_however_the_template_was_edited() {
    // The stored form is JSON, and a hand edit could put 200 in it. A budget is a
    // retry, not a loop against a key.
    let mut template = template();
    for step in &mut template.steps {
        step.attempts = 200;
    }
    assert!(
        template.check().is_err(),
        "such a template cannot be stored at all"
    );
    for step in &template.steps {
        assert_eq!(
            step.attempt_budget(),
            5,
            "and if one reaches the executor anyway, it is clamped"
        );
    }
}

#[test]
fn scenario_an_unfinished_run_from_an_earlier_session_can_be_picked_up() {
    // `features/gui-bootstrap-wizard.md` phase 5. The CA takes three days; by the
    // time the certificate comes back the register has been closed and reopened, so
    // the run exists only as rows in the database. Before this, a resume could only
    // continue a run still held in memory from the same session — the short half of
    // the wait the manual issuer makes routine.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();

    // Given a run that produced its request and stopped, because no certificate
    // had been issued yet
    let confirmation = Confirmation::given(SERIAL, commands.len());
    let first = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };
    assert_ne!(
        first.status,
        RunStatus::Completed,
        "the import had nothing to import"
    );

    // When the register is asked what is still open — the wizard's question after a
    // restart, answered from the stored runs rather than from memory
    let stored = vec![first.clone()];
    let open = yk_dist_manager::bootstrap::resumable(&stored);
    assert_eq!(open.len(), 1, "the unfinished run is offered");
    assert_eq!(open[0].id, first.id);

    // Then the plan it was made from can be rebuilt: same steps, same order
    let selection = yk_dist_manager::bootstrap::step_selection(&template, &first);
    assert_eq!(selection.len(), template.steps.len());
    assert!(
        selection.iter().all(|included| *included),
        "every step of this run was included, so every one is selected again"
    );
    assert_eq!(
        yk_dist_manager::bootstrap::resume_refusal(&template, &first),
        None,
        "nothing stands in the way of continuing it"
    );

    // And a completed run is not offered, because there is nothing left to do
    let mut finished = first.clone();
    finished.status = RunStatus::Completed;
    for step in &mut finished.steps {
        step.status = StepStatus::Done;
    }
    assert!(
        yk_dist_manager::bootstrap::resumable(&[finished]).is_empty(),
        "a finished run is not unfinished business"
    );
}

#[test]
fn scenario_a_run_cannot_be_resumed_against_a_procedure_that_has_since_changed() {
    // The refusal that keeps a resume safe. The executor indexes the run's recorded
    // steps against a freshly built plan, so a plan that no longer lines up would
    // write one step's parameters under another step's name.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();
    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    // A different version of the same id is refused by name
    let mut renumbered = template.clone();
    renumbered.version = "9".into();
    let refusal = yk_dist_manager::bootstrap::resume_refusal(&renumbered, &run)
        .expect("a different version is not the same procedure");
    assert!(refusal.contains("version"), "{refusal}");

    // And so is a version that has dropped a step the run recorded
    let mut trimmed = template.clone();
    let dropped = trimmed.steps.remove(0).id;
    let refusal = yk_dist_manager::bootstrap::resume_refusal(&trimmed, &run)
        .expect("a procedure missing a recorded step cannot be resumed against");
    assert!(refusal.contains(&dropped), "{refusal}");
}

#[test]
fn scenario_the_credential_a_run_registered_is_readable_off_the_record_afterwards() {
    // `features/step-fido2-credentials.md` phase 4. The credential id is what a
    // relying party keeps in its own database, so being able to read it back off the
    // register is what makes "this key holds that credential" checkable.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();
    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    let evidence = yk_dist_manager::bootstrap::credential_evidence(&run);
    assert_eq!(evidence.len(), 1, "{evidence:?}");
    assert!(!evidence[0].credential_id_hex.is_empty());
    assert_eq!(evidence[0].relying_party, "example.org");
    assert_eq!(evidence[0].algorithm, "ES256");
    assert_eq!(evidence[0].user_name, "ana@example.org");
}

#[test]
fn scenario_protecting_an_otp_slot_records_where_the_code_went_and_never_the_code() {
    // `features/step-otp-access-code.md` phase 7. Custody is *recorded*, never
    // *stored*: under the model the owner confirmed on 2026-08-11 the code travels
    // to the holder on the sealed slip, which is what keeps a protected slot
    // reprogrammable later without an applet reset.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);

    // Given a key whose slot 1 already holds a configuration — an access code
    // write-protects a configuration, and there is nothing to protect on an empty
    // slot
    let mut key = MockWriter::factory_fresh(SERIAL).with_otp_state(OtpState {
        slot_one_programmed: true,
        ..Default::default()
    });
    let mut recording = Recording::default();

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    // Then the slot was protected
    let step = run
        .steps
        .iter()
        .find(|s| s.step_id == "otp-access-code")
        .expect("the step is in the run");
    assert_eq!(step.status, StepStatus::Done, "{}", step.detail);
    assert!(
        step.detail.contains("mode-switched"),
        "the consequence is on the record, not only in the pre-flight: {}",
        step.detail
    );

    // And custody is on the trail, with no value in it
    let custody = recording
        .audit
        .iter()
        .filter(|(event, _, _)| event == "secret.custody")
        .map(|(_, _, detail)| detail.as_str())
        .find(|detail| detail.contains("otp-access-code"))
        .expect("custody of the access code is audited");
    assert!(
        custody.contains("custody=sealed-envelope"),
        "got: {custody}"
    );
    assert!(custody.contains("retained=no"), "got: {custody}");
    // That the entry carries no *value* is asserted where it belongs and once, by
    // `scenario_no_secret_reaches_the_run_record_or_the_audit_trail`, which greps
    // every audit entry of a full run against every secret that run generated. A
    // hand-written check here would be a weaker copy of it.
}

#[test]
fn scenario_an_empty_otp_slot_is_a_skip_with_a_reason_rather_than_a_failed_write() {
    // `ykman otp settings` refuses an empty slot outright — an access code protects
    // a configuration. Discovering that as a subprocess error halfway through a
    // procedure is what the pre-flight and this skip exist to prevent.
    let template = template();
    let commands = commands(&template);
    let request = request(&template, &commands);
    let mut key = MockWriter::factory_fresh(SERIAL);
    let mut recording = Recording::default();

    let confirmation = Confirmation::given(SERIAL, commands.len());
    let run = {
        let mut executor = Executor::new(Transports { backend: &mut key });
        executor
            .run(&request, &confirmation, &mut recording)
            .unwrap()
    };

    let step = run
        .steps
        .iter()
        .find(|s| s.step_id == "otp-access-code")
        .expect("the step is in the run");
    assert_eq!(step.status, StepStatus::Skipped, "{}", step.detail);
    assert!(
        step.detail.contains("holds no configuration"),
        "the reason has to name what is missing: {}",
        step.detail
    );
    assert!(
        !key.was_called("otp.set_access_code"),
        "and nothing was attempted against the key"
    );
}
