# Feature: Bulk enrolment

## Summary

Bootstrap many keys in one sitting: a queue of keys against one template, with per-key
evidence, resumable progress, and a batch hand-over at the end.

## Motivation

A procurement batch arrives as a box of 50 keys. Doing them one at a time through the
wizard works but invites the two failure modes of repetitive work: losing track of which
key was already done, and stopping paying attention to the confirmation dialog.

A batch mode is safer than repeated single runs precisely because it keeps the counting
and the "which key am I holding" bookkeeping in the tool.

## Current state

**Not started.** The wizard handles one key at a time; a batch is 50 manual runs.

## Design

### Two batch shapes

**Stock preparation** — bootstrap keys with no holder yet: PIN policies, management key,
OTP protection, and (where the certificate does not need a holder) nothing else. Keys end
as `Bootstrapped`, ready to be assigned. This is safe to run quickly because no
personal binding happens.

**Assigned enrolment** — a list of (holder, key) pairs, where each key gets a certificate
carrying that holder's e-mail. This needs the holder list up front, from a CSV or from
the holder registry, and cannot be unattended if the holder sets their own PIN.

The distinction matters: stock preparation can be a fast loop, assigned enrolment is
inherently paced by people. Conflating them produces a batch mode that is wrong for both.

### Flow

1. Choose the template and the batch shape; for assigned enrolment, load the pairing list
   (and validate every e-mail *before* starting, not per key).
2. Pre-flight the whole batch where possible: firmware gates, applications enabled.
3. For each key: prompt "insert the next key", detect it, refuse if it is already in the
   batch (a duplicated serial means the same key was inserted twice — a real and easy
   mistake), run the plan, record the run, mark progress.
4. On failure: record, mark that key as needing attention, and continue with the next —
   a batch must not stop dead on key 7 of 50.
5. At the end: a summary of succeeded / failed / skipped, with the per-key evidence, and
   an option to generate the hand-over documents in one go.

### Resumability

A batch is persisted as it goes, not at the end. Closing the app, an unplugged key, or a
crash leaves a batch that can be reopened and continued from the next unprocessed key.
Anything less means someone will redo keys that were already done, and re-running a
bootstrap on an already-configured key is exactly what
`features/bootstrap-engine.md` Phase 10 has to protect against.

### Data model

A `batches` table: id, template id and version, shape, operator, started/finished,
and per-key rows linking to the `bootstrap_runs` they produced. This is schema v3
territory (v2 is per-step run rows), so it comes after the executor.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Stock-preparation batch: insert-next-key loop, progress, resume | Todo | needs the executor first |
| 2 | Duplicate-serial detection within a batch | Todo | catches the same key inserted twice |
| 3 | Continue-on-failure with a needs-attention list | Todo | |
| 4 | Pairing list import (CSV) with up-front validation | Todo | validate all e-mails before starting |
| 5 | Assigned enrolment with per-holder certificates | Todo | paced by the custody model |
| 6 | Batch summary and evidence export | Todo | `features/reports-and-export.md` |
| 7 | Batch hand-over document generation | Todo | `features/receipts-and-terms.md` |
| 8 | Schema v3: `batches` table | Todo | after per-step run rows |

## Audit events

| Event | Detail |
|---|---|
| `batch.started` | `batch=<id> template=<id>@<version> shape=stock|assigned count=<n>` |
| `batch.key.done` | `batch=<id> serial=<n> run=<id>` |
| `batch.key.failed` | `batch=<id> serial=<n> reason=…` |
| `batch.key.duplicate` | The same serial was presented twice |
| `batch.finished` | `succeeded=<n> failed=<n> skipped=<n>` |
| `batch.resumed` | `batch=<id> from_key=<n>` |

Per-key bootstrap events are still emitted individually: a batch is not an excuse for a
coarser audit trail.

## Tests

- Behaviour: a 5-key batch against a mock backend completes, produces 5 runs, and the
  audit trail contains one `batch.key.done` per key plus the individual bootstrap events.
- Behaviour: key 3 fails → the batch continues, finishes with `failed=1`, and key 3 is on
  the needs-attention list.
- Behaviour: the same serial presented twice is refused, not double-processed.
- Behaviour: interrupt after key 2 → reopen → the batch resumes at key 3, and keys 1–2 are
  not re-run.
- Unit: CSV pairing import rejects the whole file on a malformed e-mail rather than
  importing half of it.

## Open questions and gates

- **Unattended or per-key confirmation?** Stock preparation could be unattended;
  assigned enrolment probably cannot. This decides Phase 1's UX.
- Whether a batch requires a second operator's sign-off, given it writes to 50 security
  tokens in one session.

## References

- `features/bootstrap-engine.md`, `features/gui-bootstrap-wizard.md`
- `features/storage-sqlite-single-file.md`
