# Feature: Bootstrap wizard (GUI)

## Summary

The screen where an operator binds a key, a holder and a template together, reviews
exactly what will happen, and then runs it.

## Motivation

Everything the bootstrap does is irreversible in some degree. The wizard exists so the
operator sees the whole plan — including *how* each step will be performed and where a
secret will be needed — before the first byte reaches the key. "Review, then execute"
is the whole design.

It is also where honesty about the transport lives: a step that still runs through
`ykman`, or that needs a manual action, must say so on screen rather than in a
document nobody reads at the desk.

## Current state

**Selection, review and dry run shipped.** `src/ui/bootstrap.rs`:

- Selection: key serial (typed or read from the attached key), holder, template, with
  the template description shown. The template list offers the **newest version of each
  template in use** (`template::latest_per_id`) — an older version stays in the database
  for the runs that applied it, but offering it for a new run would be offering a
  superseded procedure. *Manage templates…* opens the Templates screen on the selected
  template (`features/bootstrap-templates.md`).
- Per-step checkboxes; required steps are shown as required and cannot be deselected.
- Changing the template clears the step selection and the plan, so a stale plan cannot
  be executed against a different template.
- "Build plan" renders the plan into a table: step, transport (colour + text), the
  operation, and the note. Secrets appear as `<FIDO2-PIN>`.
- "Record dry run" persists a `BootstrapRun` with every step `Skipped` and audits
  `bootstrap.dry_run`.
- "Execute on key (Wave 2)" exists and is **disabled**, so the screen states its own
  limitation instead of implying capability.

Not yet done: the run view (live progress), secret prompts, confirmation, resume.

## Design

### The plan table is the contract

Four columns, and each earns its place:

| Column | Why |
|---|---|
| Step | What is being done, in the template's order |
| Transport | `native` / `ykman` / `manual` — the operator knows what will actually happen |
| Operation | The native call or the redacted `ykman` command; monospace and selectable so it can be pasted into a ticket |
| Note | The caveat: firmware gate, "ykman cannot do this", SAN limitation, ordering trap |

Colour is paired with text, never used alone (accessibility).

### Run view (Phase 2)

While a run executes:

- Steps show `Pending / Running / Done / Failed / Skipped` with the elapsed time.
- The current step is highlighted, and a failure stops the run and shows the reason
  in place.
- A "touch the key now" prompt appears when the touch policy requires it — otherwise
  the operator sees an application that has apparently frozen.
- The key must not be unplugged; if it is, the run suspends and can be resumed.

### Secret prompts (Phase 3)

Driven by `features/secrets-custody.md`. Rules for the UI: a secret is typed into a
password field or generated and shown once; it is never displayed in the plan table,
never in the run log, and the panel is dismissed deliberately. If the holder is setting
their own PIN, the screen says so and hands the keyboard over.

### Confirmation (Phase 4)

One confirmation before the first write, naming the key serial, the holder and the
count of steps, and listing the steps that cannot be undone. Not a per-step
confirmation — that trains people to click through.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Selection, per-step opt-out, plan review, dry run | 0 | Done | |
| 2 | Live run view with per-step status and touch prompts | 1 | **Done** | per-step status as words, the tally, and the custody model. Touch prompts still come from the transport's own stderr |
| 3 | Secret prompt / generate / show-once panels | 1 | **Done** | the show-once panel, dismissed deliberately and audited as `secret.shown`; values are wiped on dismissal and on drop |
| 4 | Single pre-flight confirmation naming what is irreversible | 1 | **Done** | the dialog lists the irreversible steps and the pre-flight findings, and the *Execute* button is disabled when the pre-flight blocks or the build has no transport |
| 5 | Resume a suspended run (unplugged key, awaiting CA) | 1 | **Partly done** | The awaiting-CA case is built, and it had to be: the pre-flight refuses a fresh run on a configured key with no override, so a run whose import skipped could not otherwise be finished. *Import the certificate and finish the run* resumes the run held by the wizard, refusing if the plan on screen no longer lines up with it. **Done since**: an unfinished run **from an earlier session** is picked up off the register — [`bootstrap::resumable`](../src/bootstrap/mod.rs) lists what is still open, `step_selection` rebuilds which optional steps that run included, `resume_refusal` refuses a procedure that no longer lines up, and the version the run *recorded* is **pinned** in the wizard, because a superseded version is not in the list the selector offers and the CA taking days is exactly when it will have been superseded. **Also done**: a run nobody will finish is closed with *Abandon* — [`BootstrapRun::abandon`](../src/domain/bootstrap.rs) moves it to `Aborted` and `resumable` stops offering it, and that is all it does. The steps stay as recorded, the run stays on the register, and the confirmation says so: a record of what reached a key is not the register's to erase (§3). Without it the only exit from the list was finishing a run, so a cancelled certificate request sat beside the real outstanding work for good |
| 6 | Post-run summary with the evidence, and "attach to a hand-over" in one click | 1 | **Done** | the summary and its evidence, plus *Attach to a hand-over…* on a **completed** run, which opens the Distribution form with the key, its holder and *that* run attached. Explicitly that run, not "the newest one on this serial" — the two differ exactly where it matters, on a key that was reset and bootstrapped again. It records nothing: a hand-over is a statement that a person took possession of a key, which nothing the tool can see tells it |
| 7 | Pre-flight checks: firmware gates, applications enabled, key already configured | 1 | **Done** | [`bootstrap::preflight`](../src/bootstrap/preflight.rs), shown on the confirmation. Applet state comes from [`device::applets`](../src/device/applets.rs) (`device-detection.md` phase 4). The *applications enabled* check reads only a **non-empty** list: empty means never read — the native transport cannot see the management applet's flags — and each such step raises a `Warning` naming both gaps rather than a `Skip`. Reading emptiness as "disabled" once cut the standard procedure down to its PIV steps on a fully-enabled key |
| 8 | Batch mode | 2 | **Done** | The **Batch** card on this screen, driving this wizard: same plan, same pre-flight, same confirmation, same run view. See [`bulk-enrollment.md`](bulk-enrollment.md) |
| 9 | Save the sealed-envelope slip from the show-once panel | 1 | **Done** | *Save the sealed slip…* beside *I have written them down*, writing [`envelope::render`](../src/envelope.rs)'s PDF to a path the operator picks (`YkDistApp::save_transport_slip` / `write_transport_slip`). This is how model B is executed for a **posted** key, which `features/secrets-custody.md` makes a required part of the procedure rather than polish — until it was wired up the renderer was reachable from nowhere and the PIN got copied off the panel by hand. `envelope::DISPOSAL_WARNING` sits beside the button, not after the click: where a file carrying a live PIN may be written is decided in the chooser, and a build with no chooser refuses rather than writing next to a database that is routinely on a share. Audited `secret.slip.saved` before the bytes land |

## Audit events

Emitted through the engine (`features/bootstrap-engine.md`): `bootstrap.dry_run`
today; `bootstrap.started`, per-step events, `bootstrap.finished`, `bootstrap.aborted`
once the executor lands.

From the screen rather than the engine: `bootstrap.abandoned`, when an operator
closes an unfinished run. Distinct from `bootstrap.aborted` on purpose — that one is
the executor stopping mid-run, this one is a person deciding a run is over — and it
carries the procedure, its version, when the run started and the tally it was closed
at, so the entry says what was left behind.

## Tests

- `scenario_operator_plans_a_bootstrap_for_a_named_holder`
- `scenario_operator_deselects_the_optional_steps`
- `scenario_the_plan_never_shows_a_pin`
- `scenario_a_dry_run_records_intent_without_claiming_anything_was_applied`
- `plan_covers_every_enabled_step_in_order`
- `scenario_a_posted_key_gets_a_sealed_slip_and_the_trail_says_so` (phase 9)
- `scenario_an_unfinished_run_nobody_will_finish_is_closed_without_being_erased`
  (`tests/behaviour_executor.rs`)
- `scenario_an_unfinished_run_is_closed_and_stays_on_the_register`
  (`tests/behaviour_app_abandoned_run.rs`)

The wizard's logic lives in `YkDistApp::build_plan` / `record_dry_run`, which is what
those tests drive; the paint code is deliberately thin.

## Open questions and gates

- Phase 4 is the gate for enabling execution at all: no confirmation, no writes.
- Should a batch run allow unattended execution, or must every key be confirmed? A
  batch of 50 with per-key confirmation is a different workflow from an unattended one.

## References

- `src/ui/bootstrap.rs`, `src/app.rs`, `src/template/plan.rs`
- `docs/gui.md`, `docs/bootstrap-procedure.md`
