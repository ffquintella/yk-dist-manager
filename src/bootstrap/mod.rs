//! The executor: applying a plan to a key, one step at a time, with evidence.
//!
//! The planner (`crate::template::plan`) is pure and shows what *would* happen.
//! This module is the half that actually writes, and every design decision in it
//! comes from one observation: writing to a security token is irreversible in the
//! ways that matter. A management key set to a value nobody knows leaves the PIV
//! applet administratively dead; a wrong PIN policy costs a reset and a new
//! certificate.
//!
//! ## The rules, and where each is enforced
//!
//! 1. **No confirmation, no writes.** [`Executor::run`] takes a [`Confirmation`],
//!    which cannot be constructed except by naming the serial and step count that
//!    were actually shown to the operator — and which is re-checked against the
//!    request before the first write. `features/gui-bootstrap-wizard.md` calls
//!    this "the gate for enabling execution at all", so it is a type rather than
//!    a boolean somebody could forget to test.
//! 2. **Status is persisted as it changes**, not at the end. Every transition
//!    goes through [`RunRecorder`] before the next step starts, so a run
//!    interrupted by an unplugged key leaves an accurate record rather than an
//!    optimistic one. Schema v5's per-step rows are what make this cheap.
//! 3. **A required step's failure aborts the run.** An optional step's failure is
//!    recorded and the run continues. This is the difference between "the key is
//!    unusable" and "the key works, minus the OTP slot".
//! 4. **Already-applied is a skip, not an overwrite.** Each step asks the applet
//!    what state it is in first; re-running a template on a part-configured key
//!    must not blindly rewrite a PIN the holder has already changed.
//! 5. **Secrets never reach a record.** They are borrowed for the call and
//!    dropped. `StepOutcome::detail` is built from the operation name and the
//!    typed error, neither of which can contain a value — see
//!    [`crate::secret`] and [`crate::device::write::WriteError`].
//! 6. **Resume, not restart.** [`Executor::resume`] continues from the first
//!    step that is not `Done`, so an interrupted run is finished rather than
//!    repeated.
//!
//! ## What this module does not do
//!
//! It does not talk to hardware. It calls the traits in
//! [`crate::device::write`], which are implemented by `MockWriter` today and by
//! the native transports in `features/bootstrap-engine.md` phases 5–7. That is
//! deliberate: the sequencing, the abort policy and the evidence are the parts
//! that must be right before anything touches a key, and they are testable with
//! no key attached.

use chrono::Utc;

use crate::device::write::{WriteBackend, WriteError};
use crate::domain::{BootstrapRun, RunStatus, StepKind, StepOutcome, StepStatus};
use crate::secret::{Secret, SecretKind};
use crate::template::BootstrapTemplate;
use crate::template::plan::PlannedCommand;

pub mod steps;

pub use steps::{StepContext, StepOutcomeKind, perform};

#[derive(Debug, thiserror::Error)]
pub enum ExecutionError {
    #[error(
        "this run was not confirmed: the operator agreed to {confirmed_steps} steps on serial \
         {confirmed_serial}, and the request is {actual_steps} steps on serial {actual_serial}"
    )]
    ConfirmationMismatch {
        confirmed_serial: u32,
        confirmed_steps: usize,
        actual_serial: u32,
        actual_steps: usize,
    },
    #[error("the run could not be recorded, so nothing was written to the key: {0}")]
    NotRecordable(String),
    #[error("a secret could not be produced: {0}")]
    Secret(#[from] crate::secret::SecretError),
}

/// Proof that the operator saw what was going to happen and agreed to it.
///
/// There is no `Default`, no `new()` and no public field: the only way to get one
/// is [`Confirmation::given`], which takes the serial and the step count that
/// were on screen. A caller that has not shown a confirmation cannot fabricate
/// one without writing something that looks exactly like what it is.
///
/// The re-check in [`Executor::run`] closes the remaining gap: a stale
/// confirmation from a previous plan — the operator confirmed six steps, then
/// changed the template — does not authorise the new one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Confirmation {
    serial: u32,
    steps: usize,
}

impl Confirmation {
    /// Record that the operator confirmed a run of `steps` steps on `serial`.
    ///
    /// Call this from the click handler of the confirmation dialog, and nowhere
    /// else.
    pub fn given(serial: u32, steps: usize) -> Self {
        Self { serial, steps }
    }

    pub fn serial(&self) -> u32 {
        self.serial
    }

    pub fn steps(&self) -> usize {
        self.steps
    }
}

/// Everything a run needs, gathered before the first write.
pub struct ExecutionRequest<'a> {
    pub template: &'a BootstrapTemplate,
    pub commands: &'a [PlannedCommand],
    pub serial: u32,
    pub holder_id: Option<uuid::Uuid>,
    pub operator: String,
    /// The relying party a resident credential is bound to, from Settings.
    pub relying_party: String,
    /// Subject and SAN for the CSR, already rendered by the planner's context.
    pub certificate_subject: String,
    pub certificate_email: String,
    /// The holder's display name, for the credential's user entry.
    pub holder_display: String,
}

impl ExecutionRequest<'_> {
    /// Which of the planned steps are `required` by the template.
    ///
    /// The plan carries step ids; the template carries whether each is required.
    /// Looked up rather than duplicated into the plan, so the two cannot drift.
    fn is_required(&self, step_id: &str) -> bool {
        self.template
            .steps
            .iter()
            .find(|s| s.id == step_id)
            .map(|s| s.required)
            .unwrap_or(false)
    }
}

/// Where progress and evidence go.
///
/// An abstraction rather than a `&Store` so the engine can be driven by a test
/// with no database, and so a recording failure is a first-class outcome: if the
/// run cannot be recorded, it must not proceed. A key configured with no record
/// of what was applied is the exact failure this whole tool exists to prevent.
pub trait RunRecorder {
    /// Persist the run and its step outcomes as they stand.
    fn run_updated(&mut self, run: &BootstrapRun) -> Result<(), String>;

    /// Append an audit entry. Secret-free by construction at every call site.
    fn audit(&mut self, event: &str, target: &str, detail: &str) -> Result<(), String>;
}

/// The write transport a run needs.
///
/// One handle, because there is one key. A struct rather than a bare `&mut dyn`
/// so the native transports can be added alongside without changing every
/// signature in this module.
pub struct Transports<'a> {
    pub backend: &'a mut dyn WriteBackend,
}

/// Applies a plan to a key.
pub struct Executor<'a> {
    transports: Transports<'a>,
    /// Secrets generated for this run, kept only until it ends.
    secrets: Vec<Secret>,
}

impl<'a> Executor<'a> {
    pub fn new(transports: Transports<'a>) -> Self {
        Self {
            transports,
            secrets: Vec::new(),
        }
    }

    /// Execute a plan from the beginning.
    pub fn run(
        &mut self,
        request: &ExecutionRequest<'_>,
        confirmation: &Confirmation,
        recorder: &mut dyn RunRecorder,
    ) -> Result<BootstrapRun, ExecutionError> {
        // Rule 1, checked against *this* request rather than trusted: a
        // confirmation for a different key or a different plan is not a
        // confirmation for this one.
        if confirmation.serial != request.serial || confirmation.steps != request.commands.len() {
            return Err(ExecutionError::ConfirmationMismatch {
                confirmed_serial: confirmation.serial,
                confirmed_steps: confirmation.steps,
                actual_serial: request.serial,
                actual_steps: request.commands.len(),
            });
        }

        let steps: Vec<StepOutcome> = request
            .commands
            .iter()
            .map(|c| StepOutcome::planned(&c.step_id, c.kind, "pending"))
            .collect();

        let mut run = BootstrapRun::new(
            request.serial,
            request.holder_id,
            &request.template.id,
            &request.template.version,
            &request.operator,
            steps,
        );
        run.status = RunStatus::Running;
        run.custody = crate::domain::CustodyModel::DEFAULT.as_str().to_owned();

        recorder
            .audit(
                "bootstrap.started",
                &format!("serial:{}", request.serial),
                &format!(
                    "template={} version={} steps={} operator={}",
                    request.template.id,
                    request.template.version,
                    request.commands.len(),
                    request.operator
                ),
            )
            .map_err(ExecutionError::NotRecordable)?;

        self.drive(request, &mut run, recorder)
    }

    /// Continue a run that stopped part-way.
    ///
    /// Steps already `Done` are left alone — re-running a step that succeeded
    /// would rewrite a secret the holder may already have changed. Everything
    /// else is attempted from the first non-`Done` step onward.
    pub fn resume(
        &mut self,
        request: &ExecutionRequest<'_>,
        mut run: BootstrapRun,
        recorder: &mut dyn RunRecorder,
    ) -> Result<BootstrapRun, ExecutionError> {
        let resuming_from = run
            .steps
            .iter()
            .position(|s| s.status != StepStatus::Done)
            .unwrap_or(run.steps.len());

        run.status = RunStatus::Running;
        run.finished_at = None;
        recorder
            .audit(
                "bootstrap.resumed",
                &format!("serial:{}", request.serial),
                &format!(
                    "template={} version={} from_step={} completed={}",
                    request.template.id, request.template.version, resuming_from, resuming_from
                ),
            )
            .map_err(ExecutionError::NotRecordable)?;

        self.drive(request, &mut run, recorder)
    }

    /// The loop shared by `run` and `resume`.
    fn drive(
        &mut self,
        request: &ExecutionRequest<'_>,
        run: &mut BootstrapRun,
        recorder: &mut dyn RunRecorder,
    ) -> Result<BootstrapRun, ExecutionError> {
        recorder
            .run_updated(run)
            .map_err(ExecutionError::NotRecordable)?;

        let target = format!("serial:{}", request.serial);
        let mut aborted = false;

        for index in 0..run.steps.len() {
            if run.steps[index].status == StepStatus::Done {
                continue;
            }
            let command = &request.commands[index];
            let required = request.is_required(&command.step_id);

            // Rule 2: Running is persisted before the write, so an interruption
            // during the call is visible as "this step was in flight".
            run.steps[index].status = StepStatus::Running;
            run.steps[index].started_at = Some(Utc::now());
            recorder
                .run_updated(run)
                .map_err(ExecutionError::NotRecordable)?;

            let context = StepContext {
                serial: request.serial,
                relying_party: &request.relying_party,
                holder_display: &request.holder_display,
                certificate_subject: &request.certificate_subject,
                certificate_email: &request.certificate_email,
            };

            // The plan carries the rendered command; the template carries the
            // parameters a step reads. Looked up rather than copied into the
            // plan, so the two cannot disagree about a slot number.
            let Some(template_step) = request
                .template
                .steps
                .iter()
                .find(|s| s.id == command.step_id)
            else {
                // A plan whose step is not in its own template is a bug, not an
                // operator error — recorded as a failure rather than panicking
                // in front of a key.
                run.steps[index].status = StepStatus::Failed;
                run.steps[index].detail = format!(
                    "step {} is not in template {}",
                    command.step_id, request.template.id
                );
                recorder
                    .run_updated(run)
                    .map_err(ExecutionError::NotRecordable)?;
                aborted = true;
                break;
            };

            let result = perform(
                command,
                template_step,
                &context,
                &mut self.transports,
                &mut self.secrets,
                recorder,
            );

            let step = &mut run.steps[index];
            step.finished_at = Some(Utc::now());

            match result {
                Ok(StepOutcomeKind::Applied { detail }) => {
                    step.status = StepStatus::Done;
                    step.detail = detail;
                    recorder
                        .audit(
                            "bootstrap.step.done",
                            &target,
                            &format!("step={} kind={}", step.step_id, step.kind.slug()),
                        )
                        .map_err(ExecutionError::NotRecordable)?;
                }
                // Rule 4: the applet already had this, so nothing was written.
                Ok(StepOutcomeKind::AlreadyApplied { detail }) => {
                    step.status = StepStatus::Skipped;
                    step.detail = detail.clone();
                    recorder
                        .audit(
                            "bootstrap.step.skipped",
                            &target,
                            &format!(
                                "step={} kind={} reason=already-applied",
                                step.step_id,
                                step.kind.slug()
                            ),
                        )
                        .map_err(ExecutionError::NotRecordable)?;
                }
                Ok(StepOutcomeKind::NotApplicable { detail }) => {
                    step.status = StepStatus::Skipped;
                    step.detail = detail.clone();
                    recorder
                        .audit(
                            "bootstrap.step.skipped",
                            &target,
                            &format!(
                                "step={} kind={} reason=not-applicable",
                                step.step_id,
                                step.kind.slug()
                            ),
                        )
                        .map_err(ExecutionError::NotRecordable)?;
                }
                Err(error) => {
                    step.status = StepStatus::Failed;
                    // The typed error's Display carries no secret — asserted in
                    // `device::write`'s tests — so this is safe to persist.
                    step.detail = error.detail();
                    recorder
                        .audit(
                            "bootstrap.step.failed",
                            &target,
                            &format!(
                                "step={} kind={} reason={}",
                                step.step_id,
                                step.kind.slug(),
                                error.detail()
                            ),
                        )
                        .map_err(ExecutionError::NotRecordable)?;

                    // Rule 3, plus the one case that stops everything regardless:
                    // with the key gone, every later step would fail identically.
                    if required || error.is_fatal_to_the_run() {
                        aborted = true;
                        recorder
                            .run_updated(run)
                            .map_err(ExecutionError::NotRecordable)?;
                        break;
                    }
                }
            }

            recorder
                .run_updated(run)
                .map_err(ExecutionError::NotRecordable)?;
        }

        // Anything never reached stays Pending, which `settle` reads as "still
        // running" — so an aborted run is marked explicitly rather than being
        // left looking like one that is still in progress.
        if aborted {
            run.status = RunStatus::Failed;
            run.finished_at = Some(Utc::now());
        } else {
            run.settle();
        }

        // A *required* step that was skipped did not fail — nothing broke — but
        // the procedure did not complete either, and `settle()` cannot tell:
        // it counts a skip as neither failure nor pending, so the run would
        // report `Completed`.
        //
        // That would be a false record, and the case is not hypothetical. The
        // certificate import is required by the standard procedure and skips on
        // every run today, because the issuing CA is still an open question
        // (`features/ca-integration.md`). A key handed over as "Completed" with
        // no signing certificate on it is exactly the wrong thing for this
        // register to claim.
        let unmet: Vec<&str> = run
            .steps
            .iter()
            .filter(|s| s.status != StepStatus::Done && request.is_required(&s.step_id))
            .map(|s| s.step_id.as_str())
            .collect();

        if !unmet.is_empty() && run.status == RunStatus::Completed {
            run.status = RunStatus::Failed;
            recorder
                .audit(
                    "bootstrap.incomplete",
                    &target,
                    &format!(
                        "required steps did not complete: {} — the key is not ready to hand over",
                        unmet.join(",")
                    ),
                )
                .map_err(ExecutionError::NotRecordable)?;
        }

        let (done, failed, skipped, pending) = run.tally();
        recorder
            .run_updated(run)
            .map_err(ExecutionError::NotRecordable)?;
        recorder
            .audit(
                if aborted {
                    "bootstrap.aborted"
                } else {
                    "bootstrap.finished"
                },
                &target,
                &format!(
                    "status={:?} done={done} failed={failed} skipped={skipped} pending={pending} \
                     custody={}",
                    run.status, run.custody
                ),
            )
            .map_err(ExecutionError::NotRecordable)?;

        Ok(run.clone())
    }

    /// The secrets this run generated, moved out for the show-once panel.
    ///
    /// Takes them rather than lending them: once handed to the panel, the
    /// executor holds nothing, so there is one owner to wipe and not two.
    pub fn take_secrets(&mut self) -> Vec<Secret> {
        std::mem::take(&mut self.secrets)
    }
}

/// Which secret a step needs, if any. Shared by the executor and the confirmation
/// dialog, which lists what will be generated.
pub fn secret_for(kind: StepKind) -> Option<SecretKind> {
    SecretKind::for_step(kind)
}

/// The steps of a plan that cannot be undone, for the confirmation dialog.
///
/// `features/gui-bootstrap-wizard.md` requires the confirmation to list these,
/// and `features/bootstrap-engine.md` rule 8 forbids offering an undo that would
/// silently fail. Being explicit here is the alternative.
pub fn irreversible_steps(commands: &[PlannedCommand]) -> Vec<&PlannedCommand> {
    commands
        .iter()
        .filter(|c| {
            matches!(
                c.kind,
                StepKind::PivKeygen
                    | StepKind::PivManagementKey
                    | StepKind::OtpAccessCode
                    | StepKind::Fido2Credential
            )
        })
        .collect()
}

/// Errors that mean the key is in an unknown state and needs looking at.
pub fn leaves_key_in_unknown_state(error: &WriteError) -> bool {
    matches!(error, WriteError::Detached { .. })
}
