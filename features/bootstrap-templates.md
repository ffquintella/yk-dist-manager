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
- Two built-ins: `org-standard` (11 steps) and `fido-only` (derived subset). Because
  `fido-only` is cut from `org-standard` — steps *and* their order — its version is
  taken from `org_standard` rather than written beside it: a correction to the
  procedure has to arrive under a new version of both, or the register keeps the
  uncorrected subset for ever. Neither
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

**Done for Wave 0.** What is left in this spec is Wave 1: per-key
applicability rules (phase 3) and the per-step retry policy (phase 7).

- **Files** (`src/template/portable.rs`): `TemplateFile` wraps one version as
  pretty-printed JSON with its provenance outside the signed bytes. Export writes
  the procedure *and* its canonical bytes; import reads, verifies, previews and only
  then stores.
- **Signatures** (`src/template/signing.rs`): Ed25519 over
  [canonical bytes](#the-canonical-bytes-are-a-wire-format-2026-08-12) that cover the
  procedure and its id but not its version. `Trust` distinguishes five states, and
  only one of them may run where signatures are required.
- **Diffs** (`src/template/diff.rs`): steps matched by id, so *moved* is its own kind
  of change — the one that matters most in a feature whose known production failure
  was two steps in the wrong order.

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
of a template is an attack: change `pin_policy`, point `san_email` elsewhere, drop
the step that forces the holder to change the transport PIN, and every key prepared
from then on is quietly wrong. The audit trail says *who* changed a template, which
is accountability after the fact. A signature is what stops the changed template
from being used.

**The application verifies and cannot sign.** That is not a limitation to be lifted
later, it follows from AGENTS.md §2: a signing key is a secret, and this tool holds
no secret anywhere it can persist. So the private half lives wherever the
organisation keeps its keys and signing is an out-of-band step over the canonical
bytes, which are documented (and exported beside every template) precisely so that
`openssl`, an HSM or a smartcard can produce the signature with nothing in between.
`tests/interop_template_signing.rs` runs the documented `openssl` commands and feeds
the result back through `verify`, so the runbook is a tested claim rather than a
plausible one.

What that costs, stated plainly: **a template edited in this application is
unsigned** until whoever holds the key signs it again. Pilot mode is what makes that
workable, and the spec's condition on it is met three times over — a banner on the
Templates screen, a sentence in the run confirmation, and a `template.unsigned_used`
audit entry per run.

The five verdicts are separate states because they call for opposite responses:

| `Trust` | What it means | What to do |
|---|---|---|
| `Signed` | verified against a key in Settings | the only state that may run where signatures are required |
| `Unsigned` | no signature at all | pilot mode, or get it signed |
| `UnknownKey` | signed by a key id this deployment does not have | add the key *if you trust it* — a signature nobody can check is not a signature |
| `Invalid` | a key we have, and it does not verify | **the procedure has been altered since it was signed.** Do not run it |
| `UnknownAlgorithm` | an algorithm this build cannot check | treated as unverified, never as trusted |

`Unsigned` and `Invalid` are the pair that must never be conflated: the first is a
deployment that has not started signing, the second is an attack or a damaged file.

### The canonical bytes are a wire format (2026-08-12)

A signature is over one exact encoding, so the encoding is a compatibility surface
from the moment anything is signed with it. Three decisions follow.

**Netstrings, not `serde_json`.** Serialising the struct would tie every signature
to serde's output and to the struct's field order, so adding a field would silently
invalidate every signature in the field. The encoding is instead written out by
hand as length-prefixed strings (`<len>:<bytes>`), which need no escaping and cannot
be made ambiguous by a value that contains a separator. A golden-vector test pins
the exact bytes; if it fails, the answer is a new format tag and not a new
expectation.

**The version is not signed.** A version number is assigned by whichever database
stored the template (`versioning::next_version`), so two units importing the same
signed procedure will number it differently. A signature over the version would
break on import — it would be verifying local bookkeeping rather than the
procedure. The **id** is signed, so a signed procedure cannot be re-labelled as a
different template.

**Every field of every step is covered, in order.** Enabled, required, kind,
description and each parameter, with the step count written before the steps so one
template's steps cannot be re-partitioned into another's. Order is covered because
order is the order of execution: `org-standard` v1 could not complete on hardware
precisely because two FIDO2 steps were the wrong way round, and a signature that
did not cover the order would have authorised that swap. One test changes each field
in turn and asserts the signature breaks every time.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Model, rendering, validation, built-ins, storage | 0 | Done | 19 unit tests |
| 2 | GUI editor with version bump on edit | 0 | **Done** | Templates screen; save is always a new version, and the id is immutable once stored |
| 2b | Add / duplicate / retire / remove a template | 0 | **Done** | schema v4 `retired_at`; removal refused where a run or the seeding would contradict it |
| 3 | Applicability rules per key (firmware, applications present) | 1 | Todo | partially covered by the firmware gates today |
| 4 | Import / export a template as a file | 0 | **Done** | [`template::portable`](../src/template/portable.rs) — one readable JSON wrapper per version, exported with the **canonical bytes** beside it; import is a *preview* (trust verdict plus a diff against what this register holds) and then a decision. The receiving register assigns the version; an import that changes nothing stores nothing |
| 5 | Template signing and verification | 0 | **Done** | [`template::signing`](../src/template/signing.rs) — Ed25519 over documented canonical bytes, **verification only**: the private key never comes near this application. Trusted public keys live in Settings; a run is refused when signatures are required and one does not verify; pilot mode is a banner on the Templates screen, a sentence in the confirmation, and a `template.unsigned_used` entry per run |
| 6 | Diff view between two versions | 0 | **Done** | [`template::diff`](../src/template/diff.rs) — structural, matched by step id, so a reordered step reads as **moved** rather than as a removal and an addition. Reached from a catalogue row (*Compare*) and shown in the import preview |
| 7 | Per-step retry / continue-on-failure policy | 1 | Todo | today only `required` distinguishes them |

## Audit events

| Event | When |
|---|---|
| `template.seeded` | Built-ins inserted on first open (count, logged) |
| `template.created` | A template id was stored for the first time |
| `template.changed` | A new version of an existing template was stored |
| `template.retired` | A version was withdrawn from the wizard |
| `template.reinstated` | A retired version was put back in use |
| `template.removed` | A version was deleted outright |
| `template.exported` | A version was written to a file — with its fingerprint and the path, so "where did this come from" is answerable at the other end |
| `template.imported` | A version arrived from a file: the version assigned here, the previous newest, the fingerprint, the **signature verdict**, and the file it came from |
| `template.unsigned_used` | A run used a template whose signature does not verify, under pilot mode. Written **before the first write to the key**, not after the run, so the entry exists for the run that crashed |

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

### Phases 4, 5 and 6

Unit tests live with the code (`src/template/signing.rs`, `diff.rs`, `portable.rs`
— 33 between them). The ones worth naming:

- `the_canonical_encoding_is_pinned_byte_for_byte` — the golden vector. A refactor
  that changes the encoding fails here instead of silently rejecting every signed
  template in the field.
- `every_field_of_a_step_is_covered_by_the_signature` and
  `reordering_the_steps_breaks_the_signature` — twelve single-field mutations and a
  swap, each of which must break verification on its own.
- `the_version_is_not_signed_because_the_database_assigns_it`,
  `a_signature_survives_renumbering_but_not_an_edited_step`.
- `an_unknown_key_id_is_not_treated_as_signed`,
  `an_unknown_algorithm_is_refused_rather_than_ignored`,
  `a_malformed_key_in_the_settings_blames_the_settings` — the operator has to be
  sent to the right problem.
- `a_reordered_step_is_reported_as_moved_not_as_a_removal_and_an_addition`,
  `the_same_procedure_under_two_numbers_is_reported_as_identical`.
- `a_template_round_trips_through_a_file_byte_for_byte`,
  `an_unplannable_template_is_refused_at_the_file_boundary`,
  `a_file_from_a_newer_build_is_refused_by_name`.

Behaviour, in `tests/behaviour_templates.rs`: a procedure crosses between two
registers through a file and arrives step for step under a version the receiver
assigned; importing the same file twice stores nothing; an import that differs
becomes a new version rather than overwriting the one a run recorded; a file that
cannot be planned reaches neither the reader nor the store; a signed procedure
survives the journey *and* the renumbering while a tampered one does not; editing a
signed procedure produces an unsigned version; a duplicate does not inherit the
signature; and an operator asks what changed since the batch they shipped.

`tests/behaviour_app_template_signing.rs` drives the application: read a file (which
stores nothing), see `UnknownKey` because the deployment has no key yet, add the key
in Settings, read again and see it verify, store it, and find the import audited with
`signature=verified`. Then requiring signatures refuses the unsigned built-ins by
name, and pilot mode allows them.

`tests/interop_template_signing.rs` is **ignored by default** and runs the documented
`openssl` signing commands for real, because a documented command line nobody has run
is a guess:

```bash
cargo test --test interop_template_signing -- --ignored --nocapture
```

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
- ~~**Whose key signs a procedure, and where does it live?**~~ **Answered by the
  owner, 2026-08-13: the application generates one by default, and offers an
  interface to import an external key.** That reverses the shape this phase shipped
  with — verification only, no private key anywhere near the tool — and it is the
  owner's call to make.

  **The consequence, stated rather than discovered:** whatever the application can
  sign with, anybody who can open the register can sign with. The control goes from
  "only the holder of the organisation's key approves a procedure" to "an operator
  with the register and its password approves a procedure". Against that: a signing
  key nobody has is a control that is never switched on, and a weaker control that is
  actually on beats a stronger one that is not.

  **Not yet built.** What the implementation has to carry, so it is not decided
  again later:

  1. The generated private key is **encrypted at rest** — the obvious home is the
     register, which can itself be SQLCipher-encrypted, under a key derived from a
     passphrase the operator types rather than one sitting beside it. It is never in a
     log, an audit entry, an error message or a `Debug` output, and it is never
     exported except by an explicit, confirmed action.
  2. **Signing is audited**: which key, which procedure and version, which operator.
     The one thing that makes "an operator approved this" answerable afterwards.
  3. The **import path** is what keeps the stronger arrangement available: a
     deployment with an HSM, a smartcard or an offline machine imports the public half
     only and signs out of band, exactly as this phase does today. That path must not
     regress — it is the one the ESI would choose.
  4. Verification is unchanged and still uses the public half, so a procedure signed
     by an imported key verifies on a workstation that has never held a private one.
- **Is Ed25519 the algorithm the organisation wants?** *(ESI)* Chosen here for having
  no parameters to get wrong; named inside every signature, so a build meeting an
  algorithm it does not know refuses rather than treating the template as unsigned.
  Adding a second algorithm later is additive. This is the same class of decision as
  the database cipher (`features/db-password-and-encryption.md` phase 4) and is
  recorded so it is ratified rather than inherited.
- **When does pilot mode end?** Off is the shipped default because the built-in
  procedures are unsigned and a fresh install must be able to bootstrap a key. That
  makes it an *operational* decision to turn it on, not a code change — and one worth
  making, because a control that is never switched on is documentation.

## References

- `src/template/mod.rs`, `src/template/draft.rs`, `src/template/signing.rs`,
  `src/template/diff.rs`, `src/template/portable.rs`, `src/ui/templates.rs`
- `src/store/mod.rs` (`import_template`, `TemplateImport`), `src/app.rs`
  (`template_trust`, `template_run_permission`, `export_template`,
  `read_template_file`, `apply_template_import`, `add_template_key`)
- `docs/operations.md` — the runbooks: share a procedure, and have one signed
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

**And the same correction was needed for `fido-only`, which the first pass missed.**
Its steps are a filtered view of `org-standard`, so the corrected ordering reached
its constructor for free — but it still declared version 1, and seeding compares
`(id, version)`, so every register already holding `fido-only v1` kept the broken
ordering and met the same refusal under the other template. Its version is now taken
from `org_standard`, which makes the bump automatic and this class of miss
structural rather than a thing to remember. The scenario above now runs over
`BootstrapTemplate::builtin()` rather than the standard procedure alone, for the same
reason: a rule proved on one of two built-ins was what let this through.
