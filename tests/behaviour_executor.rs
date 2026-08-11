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
use yk_dist_manager::device::write::{Fido2State, MockWriter, PivState, WriteError};
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
        })
        .with_piv_state(PivState {
            occupied_slots: vec!["9c".into()],
            management_key_changed: true,
            pin_changed_from_default: true,
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
    // certificate import is required by the standard procedure and skips on
    // every run today, because the issuing CA is an open question. A key with no
    // signing certificate must not be recorded as a completed bootstrap — that
    // is the one claim this register cannot be allowed to get wrong.
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
    assert!(
        cert_import.detail.contains("CA"),
        "and the record says why: {}",
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
