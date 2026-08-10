# Feature: Bootstrap templates

## Summary

The bootstrap procedure is **data**: a named, versioned list of steps with
parameters, where parameter values can interpolate `{{holder.email}}`-style
variables. Changing the procedure does not mean changing Rust.

## Motivation

"We always set a PIN and a signing certificate" is a habit until it is written
down; then it is a procedure. Making it declarative buys three things:

1. **Consistency** — every key of a given batch gets the same steps in the same
   order, and the record says which template and which *version* ran.
2. **Variation without forks** — a FIDO-only key for a contractor, a full PIV
   key for a staff member: two templates, one executor.
3. **Auditability** — the run stores `template_id` + `template_version`, so
   "what was applied to this key" is answerable years later even after the
   template has evolved.

## Current state

**Model and rendering shipped.** `src/template/mod.rs`:

- `BootstrapTemplate { id, name, version, description, steps }`,
  `TemplateStep { id, kind, description, enabled, required, params }`.
- `render()` substitutes `{{name}}` with whitespace tolerance. An **unknown
  variable is an error**, never an empty string — a blank certificate subject is
  worse than a refused bootstrap. An unterminated `{{` is an error too.
- `validate()` rejects duplicate step ids and templates with no enabled step.
- Two built-ins: `fgv-standard` (10 steps) and `fido-only` (derived subset).
- Templates are stored in the database keyed `(id, version)` as a JSON body, and
  seeded idempotently: `seed_builtin_templates` never overwrites an edited
  template of the same id and version.
- Steps carry `required`; the wizard lets the operator deselect only optional ones.

Not yet done: a GUI editor, template signing, and per-key applicability rules.

## Design

### Variables

| Variable | Source |
|---|---|
| `holder.name`, `holder.email`, `holder.unit` | the selected holder |
| `key.serial`, `key.model` | the detected key |
| `operator` | the logged-in operator |
| `org`, `org.unit` | Settings / the holder's unit |
| `date` | today, `YYYY-MM-DD` |

`RenderContext::VARIABLES` is the authoritative list and is asserted by a test that
renders each one — so the docs cannot drift from the code.

### Step parameters

Parameters are `BTreeMap<String, String>` — ordered, diffable, and serialisable
without a schema per step kind. Each `StepKind` documents the parameters it reads,
and a missing one is a typed error naming the step and the parameter
(`TemplateError::MissingParam`), surfaced before anything touches the key.

Example (`piv-csr`):

```
slot       = 9c
subject    = CN={{holder.name}},OU={{org.unit}},O={{org}}
san_email  = {{holder.email}}
hash       = sha256
```

### Versioning

`(id, version)` is the primary key, so an edited procedure is a **new version**,
not a mutation of the old one. Runs reference the version they used. Bumping the
version is the template author's decision; the GUI editor (Phase 2) will force it
rather than allow an in-place edit of a version that has already run.

### Signing (Phase 5)

A template decides what gets written to security hardware, so an unauthorised edit
of a template is an attack. Planned: templates carry a signature over their
canonical JSON, verified before a run; unsigned templates are usable only in a
pilot mode that is visible in the UI and in the audit entry.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Model, rendering, validation, built-ins, storage | Done | 19 unit tests |
| 2 | GUI editor with version bump on edit | Todo | never edit a version that has run |
| 3 | Applicability rules per key (firmware, applications present) | Todo | partially covered by the firmware gates today |
| 4 | Import / export a template as a file | Todo | for sharing between units |
| 5 | Template signing and verification | Todo | pilot mode must be visible |
| 6 | Diff view between two versions | Todo | "what changed since the batch we shipped in June?" |
| 7 | Per-step retry / continue-on-failure policy | Todo | today only `required` distinguishes them |

## Audit events

| Event | When |
|---|---|
| `template.seeded` | Built-ins inserted on first open (count) |
| `template.created` | A new template or version was saved |
| `template.changed` | An unrun template version was edited (Phase 2) |
| `template.unsigned_used` | Phase 5: a run used an unsigned template |

## Tests

`tests/unit_template.rs` (19 tests), notably:

- `renders_known_variables`, `whitespace_inside_braces_is_tolerated`
- `unknown_variable_is_an_error_not_an_empty_string`
- `unterminated_placeholder_is_an_error`
- `every_documented_variable_resolves` — docs cannot drift
- `builtin_templates_validate`, `duplicate_step_ids_are_rejected`,
  `template_with_no_enabled_step_is_rejected`
- `a_missing_parameter_is_reported_with_the_step_id`
- `fido_only_template_omits_piv_and_otp`

`tests/behaviour_storage.rs`: `scenario_builtin_templates_are_seeded_once`,
`scenario_a_stored_template_round_trips`.

## Open questions and gates

- Who may author or edit a template? This should be an admin-only action once roles
  exist (`features/operator-auth-and-roles.md`).
- Should a template be pinned per batch, so a whole procurement batch is guaranteed
  to have had the identical procedure?

## References

- `src/template/mod.rs`, `src/store/mod.rs` (`templates`, `seed_builtin_templates`)
- `docs/bootstrap-procedure.md`, `features/bootstrap-engine.md`
