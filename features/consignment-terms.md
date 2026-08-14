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
- Distribution screen: pick a language, generate, review the rendered term, export it
  as a **PDF** or save it as text, and upload the signed copy
  (`features/signed-term-documents.md`). The panel names the template version that
  produced the text, and offers *Edit wording…*.
- **PDF output** — `crate::pdf`, a writer with no dependency behind it. A4, Courier
  from the standard fourteen fonts (nothing embedded), the footer naming the wording
  that produced the sheet, and the signature block kept off a page break. The Terms
  editor exports its preview the same way, which is how the wording reaches the people
  who own it. See *Output format* below.
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

### Columns stay where the template put them

A gap of **two or more spaces** in a term template is a *column*, not spacing. The
signature block is two columns, and its author counted the spaces so that the rule,
the name under it and the role under that all begin at the same place — column 41, in
both shipped languages, on all three lines.

Substitution alone destroys that. `{{holder.name}}` is fifteen characters; a holder
called *Yu* or *Maria da Conceição Albuquerque Fonseca* is not, so everything after the
gap slid left or right and the name no longer sat under its own rule. The template was
never wrong — the renderer was discarding the geometry the template declared.

So: **a gap that follows a substitution is resized to put what comes after it back at
the column the template gave it.** A gap never shrinks below one space, so a name too
long for the column the author allowed degrades to a single space rather than running
two fields into one word.

Two things this deliberately is not:

- **It is not a second conditional.** Line omission remains the only logic in a term.
  This is layout, and it makes the spaces an author already typed mean what they look
  like — nothing new to learn, and nothing new in a template's syntax.
- **It is not padding a template can request.** There is no `{{name|pad:41}}`. That
  would be the template language this spec rejects, and a legal document is the last
  place to introduce one.

A gap *before* the first substitution on a line is left exactly alone: nothing has
moved yet, so there is nothing to correct. That is what keeps the five-space
indentation of the wrapped clauses in section 4 intact, and it is why the rule is safe
to apply to every template rather than to a marked block.

The alternative was to restructure the shipped wording into two stacked signature
blocks, which needs no column logic at all. It was rejected because the layout of an
institutional document is its owner's decision, not ours — and because a two-column
signature block is the ordinary form, so the next unit to write one would hit exactly
the same bug.

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

Two outputs, and **one rendering**. `render_term_parts` produces the heading and the
body lines that survived line omission; `render_term` joins them into text and
`render_term_pdf` sets them on pages. Neither has its own substitution path, so the
copy the operator reviews on screen cannot disagree with the copy the holder signs —
a disagreement that would be found by a holder rather than by a test.

- **Plain text** for a ticket, a paste, a diff.
- **PDF** for the signature and the filing cabinet.

#### The PDF writer is ours, and small on purpose

`src/pdf.rs` writes the file. No TeX, no Typst, no subprocess and no new dependency,
because the deployment model is a desktop application on a workstation that has
nothing installed but this application. What makes a hand-written writer reasonable is
one restriction: the document is **monospaced text in Courier**, one of the fourteen
fonts every viewer must have, so **nothing is embedded** — which removes the
font-parsing half of a PDF writer.

Courier is also the right font rather than a concession. The term's layout — the
indented numbered clauses, the two side-by-side signature rules — is built out of
spaces in the template, and only a fixed-width font keeps it. A proportional font
would silently ruin the wording's own alignment.

The file is **uncompressed and entirely ASCII** (bytes outside printable ASCII are
octal escapes). A term is a few kilobytes either way, and an archival artefact `grep`
can read is worth more than one that is 3 KB smaller.

`render` reads no clock: `/CreationDate` is passed in, so the function is pure and a
test can assert on every byte.

#### What the restriction costs, and what is done about it

- **Encoding.** `WinAnsiEncoding` (CP1252) covers Portuguese, Spanish, English,
  French, German and Italian completely, em dash included. A character outside it
  becomes `?`. Rather than let that happen quietly, `pdf::unrepresentable` reports
  the offending characters and the GUI warns **before** the document is printed,
  saying that the text output carries them correctly. A term in a language CP1252
  cannot set needs an embedded font — see the open questions.
- **Typography.** One body size, a bold centred heading, no justification, no rules
  and no logo.

#### The footer is traceability, not decoration

Every page carries `consignment@2 (pt-BR) · #20423633 · TERM-2026-001` and `n / m`.
Naming the **template version** is the point: the wording is versioned precisely
because something gets signed against a version, and a signed sheet in a filing
cabinet that does not say which one leaves the versioning doing no work. A draft
exported from the editor has no version yet and says `@draft`.

`n / m` carries no word in front of it. The term is in the holder's language, and
"Page" would be in the wrong one.

#### The signature block does not get split

The shipped pt-BR term is 62 rows against a 61-row A4 page, so the default break put
the two signature rules on page 1 and the names beneath them on page 2 — a holder
signing a sheet that does not say what they are signing. The rule is stated once, in
`MIN_LAST_PAGE`: **the last page always carries at least six rows**, and the break
above it moves up until it does. A template could have marked the block instead, but
that asks the wording's owner to remember a mechanism, and forgetting would be
invisible until a term came back signed.

#### Metadata carries no personal data

`/Title` and `/Subject` travel with the file into mail clients, previews and search
indexes. They name the term, the language and the serial; the holder's name and
identification number stay in the body, which is the only place the document needs
them.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Template model keyed by `(id, language, version)` | 0 | Done | schema v3 |
| 2 | Rendering with line omission for optional fields | 0 | Done | 18 unit tests |
| 3 | Optional holder fields (identification number, phone, address) | 0 | Done | fill-in-never-blank on re-registration |
| 4 | pt-BR and en built-ins, seeded idempotently | 0 | Done | an edited template survives seeding |
| 5 | Language selection with documented fallback | 0 | Done | the GUI reports a fallback |
| 6 | Generate, review and save from the distribution screen | 0 | Done | plain text |
| 7 | PDF output | 0 | **Done** | `crate::pdf`, hand-written, no dependency and no TeX; Courier from the standard fourteen fonts; the footer names the template version; the signature block survives the page break; the Terms editor exports its preview too |
| 8 | Template editor in the GUI, with a version bump on edit | 0 | Done | Terms screen; the store assigns the version, `choose_template` takes the newest |
| 9 | Return receipt as a second template id | 0 | **Done elsewhere** | shipped as [`receipts-and-terms.md`](receipts-and-terms.md) phases 4 and 6: the return receipt *is* a second template id, so it is editable, versioned and multilingual for free. Recorded here rather than deleted, because this spec is where a reader looks for it |
| 10 | Print directly | — | Todo — gates no wave | *Export as PDF…* covers it: the operator prints the file their platform already knows how to print. A print dialog would be a second path to the same bytes, and the one that goes through a file is the one that leaves evidence |

## Audit events

| Event | Detail |
|---|---|
| `term.generated` | `holder=… language=… template=consignment@1` |
| `term.saved` | `format=pdf path=…` — the format matters because the two are filed differently: a signed PDF comes back as a scan, a text copy goes into a ticket |
| `term.signed_uploaded` | See `features/signed-term-documents.md` |
| `term.template_edited` | `id=… language=… version=2 previous=1`, target `term:consignment@pt-BR` |
| `term.template_added` | Same shape with `previous=none`, for a language that had nothing on record |

## Tests

`tests/unit_term.rs` (30 tests), notably:

- `optional_fields_that_are_empty_take_their_whole_line_with_them` — no stray
  `Telefone:` for a holder without one.
- `a_line_without_variables_is_always_kept`.
- Columns: `a_signature_block_lines_up_whatever_length_the_name_is` — four names from
  two to thirty-eight characters, asserting the operator's column matches both the
  rule above it and the role below it; `the_english_signature_block_lines_up_too`;
  `a_name_too_long_for_its_column_keeps_one_space_rather_than_touching_the_next_field`;
  `a_single_space_is_spacing_and_is_never_touched` (`{{org}} — {{org.unit}}` must not
  be re-spaced); `the_indentation_of_a_clause_is_left_alone`;
  `a_column_is_kept_when_the_value_is_shorter_than_the_placeholder_too` — the padding
  has to grow as well as shrink.
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

The PDF (phase 7). `tests/unit_pdf.rs` (41 tests) covers the writer itself, because
there is no library between this code and the bytes a viewer parses:

- **Structure** — `the_cross_reference_table_points_at_every_object`,
  `every_cross_reference_entry_is_the_twenty_bytes_the_format_requires`,
  `the_declared_stream_length_is_the_real_one`. A viewer refuses a file that gets any
  of these wrong, so they are asserted rather than trusted.
- `the_fonts_are_the_two_standard_ones_and_nothing_is_embedded`.
- `the_whole_file_is_ascii_so_it_stays_readable`.
- `a_document_with_nothing_at_all_is_still_an_openable_file`.
- **Encoding** — `an_accented_name_is_written_in_the_encoding_the_font_declares`,
  `an_em_dash_survives_because_the_encoding_is_cp1252_not_latin1`,
  `the_characters_the_built_in_wording_uses_are_all_representable` (this one fails if a
  future edit to the shipped text introduces a character that would print as `?`),
  `a_character_the_encoding_cannot_carry_is_reported_before_it_is_printed`,
  `a_question_mark_the_document_really_contains_is_not_reported_as_missing`,
  `parentheses_and_backslashes_are_escaped_so_the_stream_stays_valid` — an unescaped
  `)` in a holder's name would terminate the string early and corrupt the page.
- **Wrapping** — `a_long_line_wraps_at_the_last_space_that_fits`,
  `a_continuation_line_keeps_the_indentation_of_the_line_it_came_from`,
  `a_word_longer_than_the_line_is_split_rather_than_overflowing_the_page`,
  `wrapping_never_loses_a_word_and_never_exceeds_the_line` — across every column
  count from 16 to the real page width.
- **Pagination** — `a_page_break_never_drops_a_line`,
  `the_heading_takes_room_on_the_first_page_and_is_not_repeated`,
  `nothing_is_drawn_below_the_bottom_margin`,
  `every_page_carries_the_footer_and_says_which_page_it_is`,
  `the_last_page_is_never_a_stub_carrying_a_line_or_two`,
  `the_shipped_term_keeps_its_signature_block_together` — the regression the
  `MIN_LAST_PAGE` rule exists for, asserted on the wording that actually ships.
- `the_creation_date_comes_from_the_caller_not_from_the_clock`.

`tests/unit_term.rs`:

- `the_pdf_carries_every_line_the_text_carries` — the two outputs are the same
  document, which is what `render_term_parts` exists to guarantee.
- `a_line_omitted_for_a_missing_optional_field_is_absent_from_the_pdf_too`.
- `a_pdf_term_cannot_be_issued_without_a_name_or_a_serial` — a PDF is not a way
  around `check_required`.
- `the_pdf_footer_names_the_wording_that_produced_the_sheet`,
  `a_draft_out_of_the_editor_is_marked_as_one_in_the_footer`.
- `the_pdf_metadata_carries_no_personal_data`.
- `both_shipped_languages_produce_a_pdf`.

`tests/behaviour_terms_and_documents.rs`:

- `scenario_the_term_is_exported_as_the_pdf_the_holder_signs`.
- `scenario_an_edited_wording_is_the_one_the_pdf_footer_names` — version 2 is on the
  page and named in the footer, version 1 is still on record.
- `scenario_a_term_in_a_language_the_pdf_font_cannot_set_is_reported_not_silently_mangled`.

Verified once by hand, outside the suite: the output is parsed and rendered by
CoreGraphics (`qlmanage -t`), which is how the split signature block was found.

## Open questions and gates

- **The wording is not ours.** The undertaking, the data-protection paragraph and the
  signature block are institutional text. The built-in templates are a *starting
  point* drafted to be complete and plausible; they need review by whoever owns the
  term, and the LGPD paragraph needs the DPO's sign-off. Until then a unit should
  treat them as a draft they edit, which is why templates are data.
  *Exporting the editor's preview as a PDF (phase 7) is what makes that review
  practical: the reviewer reads the document, not a template full of
  `{{variables}}`.*
- **A term in a language CP1252 cannot set** (Japanese, Chinese, Greek, Cyrillic,
  Arabic, Hebrew) needs an **embedded font**, which means shipping a font file and its
  licence, subsetting it, and writing a CID font — a piece of work of its own. Today
  the operator is warned precisely and the text output is correct. Whether any unit
  needs such a language is a question for the unit, not an assumption to build on.
- Whether a signed term is mandatory before a key leaves is a unit policy decision.
- Retention of terms and of the holder's identification number — DPO.

## References

- `src/term/mod.rs`, `src/pdf.rs`, `src/domain/holder.rs`, `src/ui/distribution.rs`,
  `src/ui/terms.rs`
- `features/signed-term-documents.md`, `features/secrets-custody.md`,
  `features/receipts-and-terms.md` (the original, broader spec)
