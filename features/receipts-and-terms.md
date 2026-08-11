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

What is left in *this* spec: the sealed-envelope slip, the signature state machine
with an age warning, the return receipt, and batch generation. `receipt_ref` remains a
free-text field for a unit's own document reference, alongside the stored copy.

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

### Signature tracking

| State | Meaning |
|---|---|
| `NotRequired` | The unit does not use terms |
| `Pending` | Generated, not yet signed — with an age warning |
| `Signed` | Reference recorded (document id, scan path, or e-signature id) |

A posted key sits in `Pending` until the signed term comes back, which is exactly the
gap `features/distribution-records.md` Phase 4 exists to make visible.

> **Superseded.** This spec originally said the signed document would *not* be stored
> in the database, only referenced. That was reversed on 2026-08-10: the database is
> the unit of deployment, so a path reference breaks the moment the file moves to a
> share — exactly when the evidence is needed. The bytes are stored, hashed, and
> size-capped; the personal-data consequence is documented in
> `docs/security-and-compliance.md`. See
> [`signed-term-documents.md`](signed-term-documents.md).

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 0 | Sealed-envelope slip for the transport secrets | Todo | **required by custody model B** for any hand-over that is not face to face; shares this feature's rendering |
| 1 | Text term generated from the record | **Done** | [consignment-terms.md](consignment-terms.md) |
| 2 | PDF output | **Done** | [consignment-terms.md](consignment-terms.md) — the pure-Rust route won: `crate::pdf`, no dependency at all, Courier from the standard fourteen fonts so nothing is embedded |
| 3 | Editable, versioned, multilingual template | **Done** | keyed `(id, language, version)` |
| 4 | Signature state machine with an age warning | Todo | with distribution Phase 4 |
| 5 | Store the signed document | **Done** | [signed-term-documents.md](signed-term-documents.md) — stored **in** the database, reversing the original plan; the reasoning is in that spec |
| 6 | Return receipt (the mirror document) | Todo | closes the custody loop |
| 7 | Batch generation for a bulk hand-over | Todo | `features/bulk-enrollment.md` |

## Audit events

| Event | Detail |
|---|---|
| `receipt.generated` | `serial=… holder=… format=pdf template=v2` |
| `receipt.signed` | `serial=… reference=…` |
| `receipt.pending_overdue` | Phase 4: an unsigned term passed its threshold |

## Tests

- Unit: the rendered term contains the serial, the holder's e-mail, the template version
  and the run summary; a run with no certificate produces a term without certificate
  claims (never a blank "Certificate: " line).
- Unit: a holder name with a comma or an accent renders correctly in both outputs.
- Behaviour: generate → mark signed → the distribution record carries the reference and
  the audit chain verifies.

## Open questions and gates

- **The wording is not ours.** The responsibility term is institutional text; it needs
  the owner's approval (and probably the DPO's, since it is where the holder is informed
  about the personal data involved).
- Retention of signed terms — DPO/ESI.
- Whether an e-signature flow is available, which would remove the scan-and-file step
  entirely.

## References

- `src/domain/distribution.rs` (`receipt_ref`), `features/distribution-records.md`
- `docs/operations.md`
