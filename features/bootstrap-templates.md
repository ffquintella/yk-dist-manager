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

**Model, rendering and the GUI editor shipped.** `src/template/mod.rs`:

- `BootstrapTemplate { id, name, version, description, steps }`,
  `TemplateStep { id, kind, description, enabled, required, params }`.
- `render()` substitutes `{{name}}` with whitespace tolerance. An **unknown
  variable is an error**, never an empty string — a blank certificate subject is
  worse than a refused bootstrap. An unterminated `{{` is an error too.
- `validate()` rejects duplicate step ids and templates with no enabled step.
- `check()` is the gate before storing: id shape (`check_id`), the fields a
  procedure cannot do without, the length bounds, `validate()`, and a **trial
  `plan()` against `RenderContext::sample`** — including steps that arrive
  disabled, because the wizard can enable an optional step on any run.
- Two built-ins: `org-standard` (11 steps) and `fido-only` (derived subset). Neither
  is branded to an institution — the organisation comes from Settings and reaches the
  steps through `{{org}}`, so the shipped procedure is a starting point a unit edits
  rather than somebody else's policy. A database seeded by a build before v0.5.0 also
  holds that build's differently-named standard procedure; seeding adds the current id
  beside it rather than renaming anything a run recorded, and the older entry is
  retired from the Templates screen.
- Templates are stored in the database keyed `(id, version)` as a JSON body, and
  seeded idempotently: `seed_builtin_templates` never overwrites an edited
  template of the same id and version.
- Steps carry `required`; the wizard lets the operator deselect only optional ones.
- `TemplateDraft` / `StepDraft` (`src/template/draft.rs`) are the editable form:
  parameters as `name = value` text, `add_step` / `remove_step` / `move_step`, and
  a dirty check against the *stored form* so re-spacing a parameter is not an edit.
- `StoredTemplate` adds what the catalogue needs and the procedure cannot know:
  `retired_at`, the number of runs that recorded this exact version, and
  `removal_refusal()` — the reason a version cannot be deleted, shown *before* the
  click.
- **Templates screen** (`src/ui/templates.rs`): the catalogue with per-row Edit,
  Duplicate, Retire / Reinstate and Remove; an editor with the live `plans`
  verdict; and the variable reference. The Bootstrap screen links to it with
  *Manage templates…*.
- The wizard offers `latest_per_id()` — the newest version of each template that is
  not retired.

Not yet done: import / export as a file, template signing, per-key applicability
rules, and a diff view between versions.

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
not a mutation of the old one. Runs reference the version they used.

The editor does not offer an in-place edit at all: *Save as new version* is the
only write, and the number comes from the database
(`versioning::next_version`, shared with the consignment terms) rather than from
the version on the operator's screen — otherwise two workstations editing the same
template would both produce "version 2". The **id** is settled when a template is
first stored and immutable afterwards, because it is what a run records; a new id
is a new template, reached with *Duplicate*.

### Withdrawing a template: retire vs remove

Two operations, because "remove" means two different things:

| | Retire | Remove |
|---|---|---|
| Effect | withdrawn from the wizard, row kept | row deleted |
| Allowed when | always | no run recorded it **and** it is not a built-in of this build |
| Reversible | yes (*Reinstate*) | no |
| Survives re-seeding | yes — seeding asks whether `(id, version)` exists, not whether it is in use | n/a |

A version a run recorded can only be retired: a run saying it applied
`org-standard v1`, with no `org-standard v1` to look up, is not a record. A
built-in can only be retired too — deleting it would be undone by the seeding that
runs on every open, so the delete would only *look* like it worked. Removal exists
for the other case, which is real: a procedure typed by mistake, which a register
nobody can correct gets worked around in a spreadsheet.

### Signing (Phase 5)

A template decides what gets written to security hardware, so an unauthorised edit
of a template is an attack. Planned: templates carry a signature over their
canonical JSON, verified before a run; unsigned templates are usable only in a
pilot mode that is visible in the UI and in the audit entry.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Model, rendering, validation, built-ins, storage | Done | 19 unit tests |
| 2 | GUI editor with version bump on edit | **Done** | Templates screen; save is always a new version, and the id is immutable once stored |
| 2b | Add / duplicate / retire / remove a template | **Done** | schema v4 `retired_at`; removal refused where a run or the seeding would contradict it |
| 3 | Applicability rules per key (firmware, applications present) | Todo | partially covered by the firmware gates today |
| 4 | Import / export a template as a file | Todo | for sharing between units |
| 5 | Template signing and verification | Todo | pilot mode must be visible |
| 6 | Diff view between two versions | Todo | "what changed since the batch we shipped in June?" |
| 7 | Per-step retry / continue-on-failure policy | Todo | today only `required` distinguishes them |

## Audit events

| Event | When |
|---|---|
| `template.seeded` | Built-ins inserted on first open (count, logged) |
| `template.created` | A template id was stored for the first time |
| `template.changed` | A new version of an existing template was stored |
| `template.retired` | A version was withdrawn from the wizard |
| `template.reinstated` | A retired version was put back in use |
| `template.removed` | A version was deleted outright |
| `template.unsigned_used` | Phase 5: a run used an unsigned template |

Every entry carries `id`, `version`, the previous version (or `none`), the step
count and the run count. **Never the procedure text**: the steps live in the
database under that version, and an audit entry cannot be corrected — the same
reasoning as the key observation (`features/key-inventory.md`).

## Tests

`tests/unit_template.rs` (50 tests), notably:

- `renders_known_variables`, `whitespace_inside_braces_is_tolerated`
- `unknown_variable_is_an_error_not_an_empty_string`
- `unterminated_placeholder_is_an_error`
- `every_documented_variable_resolves`,
  `the_sample_context_resolves_every_documented_variable` — docs cannot drift, and
  the pre-save gate cannot refuse a valid template for want of a sample value
- `builtin_templates_validate`, `the_builtin_templates_pass_the_pre_save_gate`,
  `duplicate_step_ids_are_rejected`, `template_with_no_enabled_step_is_rejected`
- `a_missing_parameter_is_reported_with_the_step_id`,
  `the_gate_refuses_a_template_that_cannot_be_planned`,
  `the_gate_checks_steps_that_arrive_disabled_too`
- `parameters_round_trip_through_the_editor_text`,
  `a_parameter_line_that_is_not_a_pair_is_refused_naming_the_step`
- `a_step_added_by_hand_carries_the_parameters_its_kind_reads` — every `StepKind`
- `an_id_must_be_lower_case_hyphenated`,
  `a_second_step_of_the_same_kind_gets_its_own_id`
- `moving_a_step_changes_the_order_of_execution`,
  `a_draft_is_dirty_only_when_the_stored_form_differs`
- `the_wizard_is_offered_the_newest_version_of_each_template`,
  `versions_sort_numerically_not_alphabetically`
- `a_version_a_run_recorded_cannot_be_removed_and_the_refusal_names_retirement`,
  `a_builtin_version_cannot_be_removed_because_it_would_come_back`,
  `an_edited_builtin_version_is_no_longer_the_builtin`
- `fido_only_template_omits_piv_and_otp`

`tests/behaviour_templates.rs` (13 scenarios): a unit adds its own template and
the wizard offers it; an edit is stored as a new version and the old one stays
readable; the version number comes from the database and not from the draft; a
template typed by mistake is removed; one a run recorded cannot be; a retired
template is not offered, survives re-seeding, and can be reinstated; a template
that cannot be planned never reaches the database; every change is audited and the
chain still verifies.

`tests/behaviour_storage.rs`: `scenario_builtin_templates_are_seeded_once`,
`scenario_a_stored_template_round_trips`. `tests/unit_store.rs`:
`a_v1_database_migrates_forward_keeping_its_rows` covers the v4 column.

## Open questions and gates

- **Who may author or edit a template?** Now that the editor exists this is live,
  not hypothetical: a template decides what is written to security hardware, and
  today any operator with the database open can change one. It should be an
  admin-only action once roles exist (`features/operator-auth-and-roles.md`), and
  until then the control is the audit trail — every change is attributed and the
  chain is verifiable. **ESI owns the decision**; the assumption in the meantime is
  that the operator running the tool is the person authorised to define the
  procedure.
- Should a template be pinned per batch, so a whole procurement batch is guaranteed
  to have had the identical procedure?

## References

- `src/template/mod.rs`, `src/template/draft.rs`, `src/ui/templates.rs`
- `src/store/mod.rs` (`templates`, `template_catalogue`, `save_template_version`,
  `retire_template`, `delete_template`, `seed_builtin_templates`)
- `src/versioning.rs` — the numbering shared with the consignment terms
- `docs/bootstrap-procedure.md`, `docs/gui.md`, `features/bootstrap-engine.md`

## `org-standard` is at version 2 (2026-08-11)

v1's FIDO2 steps were ordered `pin -> min-pin-length -> force-pin-change -> …
-> credential`, which cannot complete on hardware: a key marked
`forcePINChange` refuses its PIN for everything except changing it, so the
credential step is handed a PIN the authenticator rejects. Found on a 5.7.4 key.

The correction is **v2**, not an edit to v1, and that follows from this feature's
own rule: seeding asks whether `(id, version)` exists, never whether it is
correct, precisely so that what a run recorded stays what it recorded. An
installation seeded by the older build therefore keeps v1 — broken, on record,
and explainable — and gets v2 beside it on the next open. `latest_per_id` means
the wizard offers v2.

This is the same shape as the `org-standard` rename in v0.5.0: a built-in is
corrected by *adding*, never by rewriting the id or version a run referred to.
Covered by `scenario_a_register_holding_the_broken_procedure_is_offered_the_corrected_one`.
