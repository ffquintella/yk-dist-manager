# Feature: Receipts and responsibility terms

## Summary

Render the hand-over document from the record — who received which key, when, what was
applied to it, and what they are agreeing to — then track its signature and store the
reference.

## Motivation

The distribution record already holds a `receipt_ref` field, which today the operator
fills in by hand after producing the document somewhere else. That is the wrong way
round: the record has every fact the document needs, so the document should come from
the record. It also removes the failure mode where the term says one serial and the
database says another.

The term matters beyond bureaucracy: it is where the holder acknowledges that the key is
a credential, that the PIN is theirs alone, and that a loss must be reported
immediately. Without that acknowledgement, the loss procedure has no basis.

## Current state

**Partly shipped, in two narrower features.**

- Generating the term from the record, in multiple languages, with optional fields,
  **and exporting it as a PDF**:
  [`consignment-terms.md`](consignment-terms.md) — **done**.
- Filing the signed scan against the hand-over:
  [`signed-term-documents.md`](signed-term-documents.md) — **done**.

**Done for Wave 0.** What is left in this spec is phase 7, batch generation, which
is Wave 2 and belongs with [`bulk-enrollment.md`](bulk-enrollment.md).

- The sealed-envelope slip: [`src/envelope.rs`](../src/envelope.rs).
- **The signature state machine** ([`src/receipt.rs`](../src/receipt.rs)): five states,
  derived rather than stored, with a threshold the unit sets and a banner where the
  hand-overs are.
- **The return receipt**: a second term template id, so the wording is editable,
  versioned and multilingual for free.

`receipt_ref` remains a free-text field for a unit's own document reference — and can
now be filled in **after** the hand-over, which is the case that matters: a posted
key's term comes back days later, and until now there was nowhere to put the number.

## Design

### What the term contains

Generated from the record, no retyping:

- Holder: name, corporate e-mail, unit, registration (if used).
- Key: serial, model, firmware, form factor.
- What was applied: the bootstrap run summary — template id and version plus the steps
  that succeeded — so the holder knows the key carries a signing certificate in their
  name.
- Certificate details, when one was issued: subject, e-mail SAN, issuer, validity.
- **Custody statement — load-bearing under model B**: the PINs handed over are *transport*
  PINs and must be changed on first use. Where the firmware cannot enforce that (FIDO2 below
  5.7, and PIV always), this sentence is the *only* mechanism, so its wording is not
  boilerplate. The term states which secrets were enforced by the key and which are the
  holder's responsibility (`features/secrets-custody.md`).
- Obligations: the key identifies the holder; the PIN is not to be shared; loss or
  suspected compromise is reported immediately, to whom, and by what channel; the key is
  returned when the holder leaves or changes role.
- Hand-over: date, delivery method, the operator who handed it over.
- Signature blocks for holder and operator.

### Format

Two outputs from one template:

- **PDF** for signature and filing — the archival artefact.
- **Plain text / Markdown** so the same content can be pasted into a ticket.

Implementation options: a Typst or LaTeX template invoked as a subprocess (best
typography, external dependency), or a pure-Rust PDF writer (self-contained, plainer
output). Given the deployment model — a desktop app that should not need a TeX
installation — the pure-Rust route wins unless the unit already has a document
pipeline.

> **Settled, 2026-08-11.** The pure-Rust route, and further: no PDF *crate* either.
> `src/pdf.rs` writes the file, which is affordable because the document is
> monospaced text in Courier — a standard-fourteen font, so nothing is embedded and
> the font-parsing half of a writer disappears. The plainer output is not a loss here:
> the term's clause indentation and signature rules are built out of spaces, so a
> fixed-width font is what preserves them. See
> [`consignment-terms.md`](consignment-terms.md) *Output format*.

The document template must be editable without recompiling, like the bootstrap
templates, and versioned the same way, because the wording is legal text that someone
else owns.

### Signature tracking (Phase 4)

| State | Meaning | What it asks for |
|---|---|---|
| `NotRequired` | This unit does not use terms | nothing |
| `Pending` | Handed over, not signed, inside the threshold | nothing yet — a posted key is legitimately here |
| `Overdue` | Unsigned for longer than the unit accepts | file the scan, or record the unit's own reference |
| `Signed` | The scan is filed, **or** a reference was recorded | nothing |
| `MissingOnReturn` | The key came back and no term was ever signed | nothing to chase — a permanent gap, counted anyway |

A posted key sits in `Pending` until the signed term comes back, which is exactly the
gap `features/distribution-records.md` Phase 4 exists to make visible.

Five states rather than the three originally planned, and the two extra ones are the
useful part:

**`Overdue` is separate from `Pending`** because they ask for different things. Every
hand-over is unsigned for a while; only some are unsigned for too long, and a warning
that fired on the day of the hand-over would be one nobody reads. The threshold is
per-unit (`settings.signatures.overdue_after_days`, 14 days by default) — **one
threshold, not one per delivery method**, because two would mean an operator working
out which applies to the row in front of them.

**`MissingOnReturn` is separate from `Overdue`** because a key that is back in the
drawer is not something to chase, and telling an operator to chase it wastes an
afternoon. It is still counted: the term was evidence of custody *while the key was
held*, so a hand-over that never had one leaves a permanent gap, and hiding it once
the key returned would be the tool tidying away its own history.

**Derived, never stored.** A `signature_state` column would be a second truth needing
an update when a document is filed, when a reference is typed, and — impossibly — when
a day passes.

**A document on file is not a signature.** The state reads what is filed *per kind*: a
term this tool generated and attached says nothing about whether anybody signed it, and
counting attachments would be the tool marking its own homework.

#### `receipt.pending_overdue` is written once per hand-over, ever

A term going overdue has no click behind it, so there is no natural moment to audit it,
and both obvious placements are wrong: from the paint pass it would write an entry per
frame, and on every open it would rewrite the same fact every session until the trail
is mostly duplicates of something it already holds.

So **the trail is its own marker**: the existing `receipt.pending_overdue` entries are
read, and only a hand-over not already named is written. No new column, idempotent for
the life of the register — and it works precisely because the audit table cannot be
rewritten. The check runs when the register is opened, when a key is returned, and when
the threshold changes in Settings.

An entry stays after the term is signed. The situation improved; it still happened.

### The return receipt (Phase 6)

The mirror document, and **a second template id rather than a second feature**
(`term::RETURN_ID` — the same work as [`consignment-terms.md`](consignment-terms.md)
phase 9). That is what makes it editable, versioned, multilingual and printable for
free; a bespoke document type would have duplicated all four.

Three things the wording carries, because each is a question somebody asks months
later:

1. **Both ends of the custody** — the hand-over date *and* the return date, so the
   receipt is legible on its own. That is why `TermContext` gained `handover.date`,
   `return.date` and `return.to`: `date` alone means "today", which is the wrong
   answer on a document about something that started in February.
2. **What happens to the credentials.** A returned key whose certificate is still
   valid is a credential in a drawer. Saying so on the receipt is what makes the
   revocation somebody's job rather than an assumption.
3. **Signatures on both sides.** A return the holder did not sign for is a return only
   the unit is asserting — which is exactly what the `no receipt` badge says until the
   signed copy is filed.

It is offered on **returned rows only**: a receipt for a key still in somebody's pocket
would be a document contradicting the register. And a filed receipt does not settle the
*term* — different documents, different questions, and conflating them would mark a
hand-over as acknowledged because the return was.

> **Superseded.** This spec originally said the signed document would *not* be stored
> in the database, only referenced. That was reversed on 2026-08-10: the database is
> the unit of deployment, so a path reference breaks the moment the file moves to a
> share — exactly when the evidence is needed. The bytes are stored, hashed, and
> size-capped; the personal-data consequence is documented in
> `docs/security-and-compliance.md`. See
> [`signed-term-documents.md`](signed-term-documents.md).

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 0 | Sealed-envelope slip for the transport secrets | 0 | **Done** | [`src/envelope.rs`](../src/envelope.rs), on this feature's PDF writer. Deliberately **not** stored in the database — a slip is a courier, not evidence |
| 1 | Text term generated from the record | 0 | **Done** | [consignment-terms.md](consignment-terms.md) |
| 2 | PDF output | 0 | **Done** | [consignment-terms.md](consignment-terms.md) — the pure-Rust route won: `crate::pdf`, no dependency at all, Courier from the standard fourteen fonts so nothing is embedded |
| 3 | Editable, versioned, multilingual template | 0 | **Done** | keyed `(id, language, version)` |
| 4 | Signature state machine with an age warning | 0 | **Done** | [`crate::receipt`](../src/receipt.rs) — five states derived from the record, what is filed **per kind** and the clock; a per-unit threshold (14 days by default, or terms turned off entirely); a banner on the Distribution screen; and `receipt.pending_overdue` written **once per hand-over, ever**, using the trail itself as the marker. Recording the unit's own reference *after* the hand-over is now possible, which is what `receipt.signed` records |
| 5 | Store the signed document | 0 | **Done** | [signed-term-documents.md](signed-term-documents.md) — stored **in** the database, reversing the original plan; the reasoning is in that spec |
| 6 | Return receipt (the mirror document) | 0 | **Done** | a second template **id** (`term::RETURN_ID`), so it inherits versioning, the editor, both languages and the PDF writer. Offered on returned rows only, filed as `DocumentKind::ReturnReceipt`, and tracked separately from the term — a signed receipt does not settle a term nobody signed |
| 7 | Batch generation for a bulk hand-over | 2 | Todo | `features/bulk-enrollment.md` |

## Audit events

| Event | Detail |
|---|---|
| `receipt.generated` | The return receipt was rendered: `kind=return holder=… language=… template=…@…`. The consignment term keeps its own `term.generated` |
| `receipt.signed` | The unit's own reference was recorded for a signed term: `reference=…`. A document number, not personal data, and the thing somebody will search the trail for |
| `receipt.pending_overdue` | An unsigned term passed its threshold: `distribution:<id>`, with `unsigned_days` and the threshold. **Once per hand-over, ever** — the trail is its own marker |

## Tests

- Unit: the rendered term contains the serial, the holder's e-mail, the template version
  and the run summary; a run with no certificate produces a term without certificate
  claims (never a blank "Certificate: " line).
- Unit: a holder name with a comma or an accent renders correctly in both outputs.
- Behaviour: generate → mark signed → the distribution record carries the reference and
  the audit chain verifies.

### Phase 4

`src/receipt.rs` (11 unit tests): a fresh hand-over is pending and not yet a problem;
the day the threshold falls on is **not** yet overdue (an off-by-one here fires a day
early on every hand-over in the register, which is how a warning gets ignored); a filed
scan settles it and so does the unit's own reference; **a generated term on file does
not**; a returned key with no term is a permanent gap and not a chase; a unit with terms
turned off is not nagged; the tally counts every kind of gap and says nothing at all
when there is nothing outstanding.

`tests/behaviour_terms_and_documents.rs`: the same three through the store, so the
per-kind document counting is exercised against real rows rather than a constructed
`Filed`.

`tests/behaviour_app_overdue_terms.rs` is the one that earns its keep: the overdue term
is recorded and the fresh one is not, running the check twice more writes nothing, **and
closing and reopening the register writes nothing** — the case a single session cannot
show and the one a naive implementation gets wrong. Then recording the unit's reference
settles it, the entry that recorded it as overdue **stays**, and the chain still
verifies.

### Phase 6

`tests/unit_term.rs`: the built-ins cover every document in every language (asserted as
a product, so adding one and forgetting the other fails here rather than in front of a
holder); the receipt names both ends of the custody and says the credentials get
revoked, in both languages; and a consignment term carries **no** return lines while
the key is held — which is line omission doing the work instead of a conditional in the
template.

`tests/behaviour_terms_and_documents.rs`: the receipt renders from a February hand-over
returned in August with both dates, becomes a PDF, and moves the return from
`Undocumented` to `Documented` once the signed copy is filed — while leaving the term's
own state alone. And the wording is edited like any other term: a new version, the old
one kept for the receipts already issued under it.

## Open questions and gates

- **The wording is not ours.** The responsibility term is institutional text; it needs
  the owner's approval (and probably the DPO's, since it is where the holder is informed
  about the personal data involved).
- Retention of signed terms — DPO/ESI.
- Whether an e-signature flow is available, which would remove the scan-and-file step
  entirely.

## References

- `src/receipt.rs`, `src/term/mod.rs` (`RETURN_ID`, `handover.date`, `return.date`,
  `return.to`), `src/domain/distribution.rs` (`receipt_ref`)
- `src/store/mod.rs` (`filed_documents`, `set_receipt_ref`), `src/app.rs`
  (`signature_state`, `outstanding_paperwork`, `check_overdue_signatures`,
  `generate_return_receipt`, `record_receipt_reference`)
- `src/ui/distribution.rs`, `src/ui/settings.rs` (the threshold), `src/ui/terms.rs`
  (the document picker)
- `features/distribution-records.md`, `docs/operations.md`
