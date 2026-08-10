# Feature: Consignment terms (multilingual, generated from the record)

## Summary

Generate the term the holder signs — *termo de consignação* — from the records
already held, in the language the holder reads, with the optional fields
(identification number, phone, address) appearing only when they are filled in.

## Motivation

The hand-over record already knows the holder's name, their identification number,
the key's serial, what the bootstrap applied and who is handing it over. Retyping any
of that into a document is how a term ends up naming one serial while the database
records another — and the term is the artefact that survives an audit, so a
disagreement between the two is exactly the problem the tool exists to prevent.

Language matters because the term is a document a person signs. A holder who reads
Portuguese should not be asked to sign an English undertaking, and a visiting
researcher should not be asked to sign a Portuguese one.

The custody decision makes the wording load-bearing: under model B the PIN handed
over is a *transport* PIN, and where the firmware cannot force a change (FIDO2 below
5.7, and PIV always) the sentence instructing the holder to change it **is** the
control. See `features/secrets-custody.md`.

## Current state

**Shipped.**

- `term::TermTemplate { id, language, version, title, body }`, keyed
  `(id, language, version)` in `term_templates` (schema v3). Editing produces a new
  version; a version that was signed stays readable forever.
- Built-ins: `consignment` in **pt-BR** and **en**, both validated at build time by a
  test. A unit can add any language (a test adds `es`).
- `term::TermContext` — 18 variables covering holder, key, what was applied, the
  custody statement, operator, organisation, date, delivery method and term
  reference. `TermContext::VARIABLES` is asserted by a test, so the documentation
  cannot drift from the code.
- `term::render_term` — substitution plus **line omission**.
- `term::choose_template` — exact language, then base language (`pt` → `pt-BR`), then
  the default, then anything; the caller is told when the language it asked for was
  not available, and the GUI shows that.
- Holder gains three optional fields: `identification_number`, `phone`, `address`.
  A re-registration that omits them does not blank them — the SQL fills in, never
  clears.
- Distribution screen: pick a language, generate, review the rendered term, save it
  as text, and upload the signed copy
  (`features/signed-term-documents.md`). The panel names the template version that
  produced the text, and offers *Edit wording…*.
- **Terms screen**: edit the title and body of a term per language, with a live
  verdict on the draft, a preview against sample values, *restore the built-in
  wording*, *reload the stored version*, and a list of the versions on record.
  Saving stores a **new version** — `Store::save_term_template_version` numbers it
  one past the highest on record and never updates a row — and
  `term::choose_template` hands out the newest version of a language, so the next
  term generated uses the edit while a signed version stays exactly as it was.
  Adding a language is the same operation with nothing on record yet.

## Design

### "Identification number", not "CPF"

The field holds a CPF in Brazil and the local equivalent elsewhere, so it is named
for what it is. The pt-BR template prints *Número de identificação*; the English one
prints *Identification number*. Naming it `cpf` in the schema would have made the
first non-Brazilian holder a migration.

### Optional fields: line omission instead of conditionals

A term template carries `Telefone: {{holder.phone}}` unconditionally. At render time,
**a line containing a placeholder that resolves to empty is dropped entirely**. A
line with no placeholders is always kept.

That is the whole conditional logic, and it is deliberate: a full template language
(`{{#if}}`) would be more powerful and much easier to get subtly wrong in a legal
document. The rule is one sentence, and it produces the right output for every
optional field without the template author thinking about it.

The same rule covers a key known only from a scanned label: no model and no firmware
means those lines disappear rather than printing empty labels.

### What a term must have

`check_required` refuses to render without the holder's name and the key serial.
Everything else can legitimately be absent.

### The version number belongs to the database, not to the editor

`save_term_template_version` ignores the version on the draft it is handed: it reads
the versions already on record for that `(id, language)` and inserts one past the
highest (`term::next_version`, numeric so `10` follows `9`). Two consequences, both
wanted:

- An edit **adds**; nothing is ever updated in place. The wording somebody signed is
  still byte-for-byte in the database, which is the whole reason the version is part of
  the key.
- Two operators editing the same term from different workstations cannot both write
  "version 2" — the second one to save gets 3, and neither loses their text.

Storing goes through `TermTemplate::check`: an id, a language, a title, a body, the
length bounds, and every `{{variable}}` known to `TermContext`. A term that could not
render is refused at the editor rather than at the counter with the holder waiting.

### The preview fills every variable

The editor previews against `TermContext::sample()`, whose values are all filled and
obviously fictitious. Filled on purpose: line omission means a blank value *removes* a
line, so a sample with gaps in it would hide lines the real document will print.

### Output format

Plain text today, which is honest about what it is: reviewable on screen, saveable,
pasteable into a ticket, and printable. PDF is the next phase — the content is the
part that needed to be right first, and a text term that says the correct things
beats a beautifully typeset one that does not.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Template model keyed by `(id, language, version)` | Done | schema v3 |
| 2 | Rendering with line omission for optional fields | Done | 18 unit tests |
| 3 | Optional holder fields (identification number, phone, address) | Done | fill-in-never-blank on re-registration |
| 4 | pt-BR and en built-ins, seeded idempotently | Done | an edited template survives seeding |
| 5 | Language selection with documented fallback | Done | the GUI reports a fallback |
| 6 | Generate, review and save from the distribution screen | Done | plain text |
| 7 | PDF output | Todo | pure-Rust writer; no TeX dependency on a workstation |
| 8 | Template editor in the GUI, with a version bump on edit | Done | Terms screen; the store assigns the version, `choose_template` takes the newest |
| 9 | Return receipt as a second template id | Todo | closes the custody loop |
| 10 | Print directly | Todo | platform print dialog |

## Audit events

| Event | Detail |
|---|---|
| `term.generated` | `holder=… language=… template=consignment@1` |
| `term.saved` | Path written |
| `term.signed_uploaded` | See `features/signed-term-documents.md` |
| `term.template_edited` | `id=… language=… version=2 previous=1`, target `term:consignment@pt-BR` |
| `term.template_added` | Same shape with `previous=none`, for a language that had nothing on record |

## Tests

`tests/unit_term.rs` (30 tests), notably:

- `optional_fields_that_are_empty_take_their_whole_line_with_them` — no stray
  `Telefone:` for a holder without one.
- `a_line_without_variables_is_always_kept`.
- `a_term_carries_the_name_and_identification_number_from_the_record`.
- `language_selection_prefers_an_exact_match`,
  `language_selection_falls_back_to_the_base_language`,
  `an_unknown_language_falls_back_to_the_default_rather_than_failing`.
- `a_term_cannot_be_issued_without_a_name_or_a_serial`.
- `every_documented_variable_resolves`.
- `a_key_known_only_by_serial_still_produces_a_term`.

Editing (phase 8):

- `the_next_version_is_one_past_the_highest_number_on_record` and
  `version_ten_wins_over_version_nine` — text ordering would put `10` before `9`; the
  numbering must not.
- `a_hand_named_version_does_not_block_the_numbering`.
- `generating_a_term_takes_the_newest_version_of_the_language`, and
  `the_base_language_fallback_also_takes_the_newest_version`.
- `each_language_is_listed_once_at_its_newest_version`.
- `a_template_with_an_unknown_variable_is_refused_before_it_is_stored`,
  `a_term_template_needs_a_title_and_a_body`,
  `a_term_body_is_length_bound_like_every_other_input`.
- `the_builtin_wording_passes_the_editor_check` — the shipped text is storable text.
- `an_unsaved_edit_is_recognised_as_one`.
- `the_sample_context_fills_every_variable_so_no_preview_line_is_hidden`.

`tests/behaviour_terms_and_documents.rs`:

- `scenario_generate_the_term_for_a_hand_over_in_portuguese` / `..._in_english`.
- `scenario_term_templates_are_seeded_once_and_survive_an_edit`.
- `scenario_a_unit_adds_its_own_language`.
- `scenario_optional_holder_fields_are_filled_in_not_blanked_by_a_later_edit`.
- `scenario_the_operator_edits_the_wording_and_the_next_term_uses_it` — version 2 is
  generated from, version 1 is still readable.
- `scenario_the_editor_refuses_wording_that_could_not_render` — nothing is stored.
- `scenario_a_unit_adds_a_language_through_the_editor`.
- `scenario_editing_the_wording_is_audited` — event, target and detail, with the
  chain still verifying.
- `scenario_two_edits_in_a_row_number_themselves`.

## Open questions and gates

- **The wording is not ours.** The undertaking, the data-protection paragraph and the
  signature block are institutional text. The built-in templates are a *starting
  point* drafted to be complete and plausible; they need review by whoever owns the
  term, and the LGPD paragraph needs the DPO's sign-off. Until then a unit should
  treat them as a draft they edit, which is why templates are data.
- Whether a signed term is mandatory before a key leaves is a unit policy decision.
- Retention of terms and of the holder's identification number — DPO.

## References

- `src/term/mod.rs`, `src/domain/holder.rs`, `src/ui/distribution.rs`
- `features/signed-term-documents.md`, `features/secrets-custody.md`,
  `features/receipts-and-terms.md` (the original, broader spec)
