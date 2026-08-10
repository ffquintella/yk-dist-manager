# Feature: Uploading the signed term

## Summary

File the scanned, signed term against the hand-over it belongs to: stored in the
database, hashed, and exportable — so "was this key signed for?" is answerable from
the record rather than from somebody's folder of scans.

## Motivation

A distribution record that says `receipt_ref: TERM-2026-001` is a promise that a
signed document exists somewhere. Six months later, "somewhere" is a folder on a
laptop that has been replaced. The document is the evidence; keeping it beside the
record is what makes the record hold up.

## Current state

**Shipped.**

- `domain::AttachedDocument` with `DocumentKind` (`SignedTerm`, `GeneratedTerm`,
  `ReturnReceipt`, `Other`), the original filename, media type, size, a SHA-256, who
  uploaded it and when.
- Validation before anything reaches the database: non-empty, at most 8 MiB, and only
  formats a scanner produces (PDF, PNG, JPEG, TIFF, plain text). An uploaded filename
  is treated as data — any directory component is stripped.
- Stored in the `documents` table (schema v3) as a BLOB, with a foreign key to the
  distribution, so a document cannot be filed against a hand-over that does not
  exist.
- Listings never carry the bytes (`documents_for`); the content is loaded on demand
  (`document_content`) and **the digest is checked before an export**, which is
  refused on a mismatch.
- `document_counts()` drives a per-row badge on the distribution table: `none filed`
  in amber, `n filed` in green.
- Distribution screen: *upload* on any row, *Upload signed term…* in the term panel,
  and a filed-documents table with the digest and an *export* action.

## Design

### Why in the database, not a path

The earlier plan (`features/receipts-and-terms.md`) said the record would hold a
*reference* and the scan would live elsewhere. Storing the bytes is the better answer
here, and the reason is the deployment model: the database is one file that can sit on
a share and be copied wholesale. A path reference breaks the moment the database moves
or the operator's laptop is reimaged — which is precisely when the evidence is needed.

Cost, accepted knowingly:

- **Size.** A signed A4 scan is 100–500 KB; 500 keys is well under a gigabyte, which
  a share copies fine. The 8 MiB per-document cap keeps a 40-page photo album out.
- **Personal data.** A signed term carries a name, an identification number and a
  signature. That raises what a stolen copy of the database is worth, which is a
  direct argument for turning the password on
  (`features/db-password-and-encryption.md`) — and it is stated in
  `docs/security-and-compliance.md` rather than left implicit.

### Integrity

Every document carries a SHA-256 recorded at upload. Export verifies it and refuses on
a mismatch, logging `document.digest.mismatch`. That detects storage corruption and
any edit made outside the application, and it gives the operator something short to
quote in a ticket (`short_digest()`).

### What is not done here

No OCR, no signature validation, no comparison against the generated term. The tool
files what it is given and records who filed it and when. Claiming to validate a
signature would be worse than useless.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Document model with validation, media types, digest | Done | 7 unit tests |
| 2 | `documents` table, foreign key, content-free listings | Done | schema v3 |
| 3 | Upload from the distribution screen and the term panel | Done | `file-dialog` feature |
| 4 | Export with digest verification | Done | refuses a mismatch |
| 5 | "No term filed" badge per hand-over | Done | amber / green |
| 6 | View a filed document in-app | Todo | needs a PDF/image viewer; export covers it for now |
| 7 | Overdue-signature report | Todo | open hand-overs with nothing filed after N days |
| 8 | Attach the generated term automatically | Todo | so the pair (generated, signed) is complete |
| 9 | Delete/replace a wrongly filed document | Todo | append-only for now, deliberately |

Phase 9 is deliberately unbuilt: making evidence deletable needs a policy first —
who may, with what record of the deletion.

## Audit events

| Event | Detail |
|---|---|
| `term.signed_uploaded` | `kind=signed-term file=… bytes=… sha256=…` |
| `document.exported` | Target path, keyed on the digest |
| `document.digest.mismatch` | Logged at `error`; the export is refused |

## Tests

`src/domain/document.rs` (7 unit tests): digest and size recorded; tampering fails
verification; empty and oversized refused with both numbers; only scanner formats
accepted; a filename cannot carry a path (`/etc/passwd.pdf` → `passwd.pdf`); readable
size labels.

`tests/behaviour_terms_and_documents.rs`:

- `scenario_file_the_signed_term_against_the_hand_over` — listed without bytes,
  content byte-identical, digest verified.
- `scenario_the_signed_term_survives_a_restart_and_a_backup` — including that the
  `VACUUM INTO` backup carries the document.
- `scenario_a_document_for_an_unknown_hand_over_is_refused`.
- `scenario_an_unsupported_upload_is_refused_before_it_reaches_the_database`.
- `scenario_document_counts_drive_the_missing_term_badge`.

## Open questions and gates

- **Retention.** A signed term is personal data with a retention rule of its own; that
  rule is the DPO's and the ESI's, not this tool's. Nothing is deleted until it exists.
- Whether a unit is obliged to file scans in an institutional document system as well;
  if so, the reference field remains available alongside the stored copy.
- The 8 MiB cap and the accepted formats are engineering defaults, not policy — worth
  a look if a unit's scanner produces something bigger.

## References

- `src/domain/document.rs`, `src/store/mod.rs`, `src/ui/distribution.rs`
- `features/consignment-terms.md`, `features/receipts-and-terms.md`,
  `docs/security-and-compliance.md`
