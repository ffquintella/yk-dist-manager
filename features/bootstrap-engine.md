# Feature: Bootstrap planner and executor

## Summary

Turn a template plus a holder plus a key into a reviewable **plan**, then execute
that plan step by step against the hardware, recording the outcome of each step as
evidence — without ever recording a secret.

## Motivation

Writing to a security token is irreversible in the ways that matter: a wrong PIN
policy means a reset, and a management key set to an unknown value means the PIV
applet is administratively dead. So the operation is split in two:

- the **planner** is pure and shows exactly what will happen, before anything
  happens;
- the **executor** performs it, one step at a time, and stops on a failure that
  matters.

The plan is also the honest place to say *how* each step will be performed, since
some steps go native and some still fall back to `ykman`.

## Current state

**Planner and executor shipped; the transports behind them are not.**

`src/bootstrap/` runs a plan step by step against the write traits in
`src/device/write.rs`. What is proven, by ten scenarios against `MockWriter`:
sequencing, per-step persistence, the abort policy, idempotency, resume, the
confirmation gate, and that no secret reaches any record. What is **not** there
is a transport that talks to hardware — `MockWriter` is the only implementation
in the build, so no code path can currently write to a key.

Three decisions in the executor worth knowing about:

- **The confirmation is a type, not a flag.** `Executor::run` takes a
  `Confirmation`, which can only be built by naming the serial and step count
  that were shown, and which is re-checked against the request. A stale
  confirmation — the operator agreed to six steps, then changed the template —
  does not authorise the new plan.
- **A run that cannot be recorded does not touch the key.** The `RunRecorder`
  failure path returns before the first write. A configured key with no record of
  what was applied is the failure this whole tool exists to prevent, so the
  register being unreachable stops the run rather than being worked around.
- **A required step that was *skipped* means the run is not `Completed`.**
  `settle()` counts a skip as neither failure nor pending, so it would report
  `Completed`. That is not hypothetical: `piv-cert-import` is required by the
  standard procedure and skips on every run today, because the issuing CA is
  undecided. A key recorded as a completed bootstrap with no signing certificate
  on it is the wrong claim, so the executor emits `bootstrap.incomplete` naming
  the steps and marks the run failed.

`src/template/plan.rs` (unchanged):

- `plan(template, ctx) -> Vec<PlannedCommand>`; every command carries the step id,
  kind, rendered description, an optional `NativeOp`, an optional `ykman` argv, and
  a note explaining anything unusual.
- `Arg::Secret("FIDO2-PIN")` models secrets. `redacted()` renders `<FIDO2-PIN>`;
  there is no code path that renders the value, because the value is not there.
- `Transport::{Native, Ykman, Manual}` is derived from what is actually
  implemented, so the wizard shows the truth rather than an intention.
- `native_op(kind)` is the single table of native operations and their availability.
- The wizard builds a plan, shows it (colour-coded by transport) and can record a
  **dry run**: a `BootstrapRun` with every step `Skipped` and
  `custody = "dry run — no secret was set"`.
- `BootstrapRun::settle()` derives the run status: any failure ⇒ `Failed`, anything
  pending ⇒ `Running`, otherwise `Completed`.

Nothing in the current build writes to a key.

## Design

### Executor rules (Wave 1)

1. **Confirm before the first write.** One explicit confirmation naming the key
   serial and the steps that will run. No step runs as a side effect of navigation.
2. **One step at a time, with live status.** Each step goes
   `Pending → Running → Done | Failed | Skipped`, persisted as it changes, so an
   interrupted run leaves an accurate record rather than an optimistic one.
3. **Abort on a required failure.** A failed `required` step stops the run
   (`RunStatus::Failed`). An optional step's failure is recorded and the run
   continues.
4. **Idempotency where the hardware allows it.** Re-running a template on a
   partially bootstrapped key must detect the already-applied state and skip, not
   blindly overwrite. Where the hardware cannot tell, the step asks.
5. **Ordering constraints are real.** The PIV PIN must change before key generation
   (generation authenticates with the PIN); the management key must change before
   or with generation; the certificate can only be imported after issuance.
6. **Secrets exist only in memory, only for the step that needs them.** They are
   passed to the native call and dropped; they never reach `StepOutcome::detail`,
   the log, the audit entry, or a temporary file.
7. **Resume, not restart.** A run interrupted by an unplugged key can be resumed
   from the first non-`Done` step.
8. **No rollback pretence.** Some steps cannot be undone. The UI says which, before
   the run — it does not offer an "undo" that would silently fail.

### Evidence produced

Per step: id, kind, status, start and end time, and a secret-free detail line
(currently `[native] yubikey::piv::generate` style). Per run: template id and
version, operator, key serial, holder, custody note, and status. The run is then
attachable to the distribution record, which is how "what was applied on the
bootstrap" gets answered.

### Where the executor lives

A `bootstrap::Executor` taking `&dyn YubiKeyBackend` for reads plus per-applet
write traits, so the whole engine is testable against mocks. **No test may write to
real hardware**; the hardware path is exercised manually against a dedicated test
key and recorded in the phase notes.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Planner with transports and secret placeholders | Done | 19 template tests |
| 2 | Dry-run recording | Done | `bootstrap.dry_run` audited |
| 3 | Executor skeleton: sequencing, status persistence, abort policy | **Done** | [`src/bootstrap/`](../src/bootstrap/); 10 scenarios against `MockWriter` |
| 4 | Secret input: prompt, generate, show-once, zeroise | **Done** | [`src/secret.rs`](../src/secret.rs) — `features/secrets-custody.md` phase 3 |
| 5 | FIDO2 steps live | Todo | the step logic and its idempotency check are written; what is missing is the transport behind `native-fido` |
| 6 | PIV steps live | Todo | same, behind `native-piv`. The **certificate import** additionally waits on the CA decision |
| 7 | OTP step live | Todo | same, behind `native-otp` |
| 8 | Verification step reading the key back | **Done** | reads all three applets and stores the end state as the step's detail |
| 9 | Resume an interrupted run | **Done** | `Executor::resume` continues from the first non-`Done` step |
| 10 | Idempotency detection ("already applied") | **Done** | every step reads its applet's state first and skips rather than overwriting |

## Audit events

| Event | When |
|---|---|
| `bootstrap.dry_run` | A plan was recorded without execution |
| `bootstrap.started` | Execution began (template id + version, serial, operator) |
| `bootstrap.step.done` | A step succeeded (step id, kind) |
| `bootstrap.step.failed` | A step failed (step id, reason — no secret) |
| `bootstrap.step.skipped` | A step was skipped, with why (unsupported firmware, deselected, already applied) |
| `bootstrap.finished` | Run settled, with the final status and the tally |
| `bootstrap.aborted` | A required step failed, or the operator stopped the run |
| `bootstrap.resumed` | An interrupted run was continued, naming the step it resumed from |
| `bootstrap.incomplete` | The run finished with a required step skipped — the key is not ready to hand over |

## Tests

`tests/behaviour_bootstrap.rs` (10 scenarios):

- `scenario_operator_plans_a_bootstrap_for_a_named_holder` — asserts the plan
  contains a PIN for FIDO, an OTP access code, a resident credential and the CSR,
  bound to this holder.
- `scenario_the_plan_never_shows_a_pin`
- `scenario_operator_deselects_the_optional_steps`
- `scenario_a_recorded_run_says_what_was_applied`
- `scenario_a_failed_step_marks_the_whole_run_failed`
- `scenario_a_run_with_pending_steps_is_still_running`
- `scenario_a_dry_run_records_intent_without_claiming_anything_was_applied`
- `scenario_credential_creation_cannot_fall_back_to_ykman`
- plus the no-key and two-keys cases

Executor phases add: abort-on-required-failure, resume from a partial run, and one
scenario per step against a mock write trait.

## Open questions and gates

- ~~Custody model~~ **decided 2026-08-10 (model B)**, so Phase 4 is unblocked. The
  executor must set the transport secret, mark the key for forced change where the firmware
  allows it, and record which enforcement applied.
- Whether a bootstrap requires a second operator's confirmation for a batch is an
  operational policy question.
- The CA decision blocks the PIV certificate steps in Phase 6
  (`features/ca-integration.md`).

## References

- `src/template/plan.rs`, `src/domain/bootstrap.rs`, `src/app.rs`
- `docs/bootstrap-procedure.md`
