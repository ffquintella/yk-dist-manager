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
| 2 | Live run view with per-step status and touch prompts | 1 | Todo | needs the executor |
| 3 | Secret prompt / generate / show-once panels | 1 | Todo | `features/secrets-custody.md` |
| 4 | Single pre-flight confirmation naming what is irreversible | 1 | **In progress** | the *gate* exists in the engine — `bootstrap::Confirmation` cannot be forged and is re-checked against the plan, and `bootstrap::irreversible_steps` supplies what the dialog must list. The dialog itself is not painted yet, and the wizard does not call the executor |
| 5 | Resume a suspended run (unplugged key, awaiting CA) | 1 | Todo | `features/ca-integration.md` |
| 6 | Post-run summary with the evidence, and "attach to a hand-over" in one click | 1 | Todo | closes the loop with the distribution screen |
| 7 | Pre-flight checks: firmware gates, applications enabled, key already configured | 1 | **Core done** | [`bootstrap::preflight`](../src/bootstrap/preflight.rs) — firmware gates, disabled applets, already-configured state, and a block when the run would write nothing. Paint wiring pending |
| 8 | Batch mode | 2 | Todo | `features/bulk-enrollment.md` |

## Audit events

Emitted through the engine (`features/bootstrap-engine.md`): `bootstrap.dry_run`
today; `bootstrap.started`, per-step events, `bootstrap.finished`, `bootstrap.aborted`
once the executor lands.

## Tests

- `scenario_operator_plans_a_bootstrap_for_a_named_holder`
- `scenario_operator_deselects_the_optional_steps`
- `scenario_the_plan_never_shows_a_pin`
- `scenario_a_dry_run_records_intent_without_claiming_anything_was_applied`
- `plan_covers_every_enabled_step_in_order`

The wizard's logic lives in `YkDistApp::build_plan` / `record_dry_run`, which is what
those tests drive; the paint code is deliberately thin.

## Open questions and gates

- Phase 4 is the gate for enabling execution at all: no confirmation, no writes.
- Should a batch run allow unattended execution, or must every key be confirmed? A
  batch of 50 with per-key confirmation is a different workflow from an unattended one.

## References

- `src/ui/bootstrap.rs`, `src/app.rs`, `src/template/plan.rs`
- `docs/gui.md`, `docs/bootstrap-procedure.md`
