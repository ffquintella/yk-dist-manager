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

**Built** — a **Batch** card on the Bootstrap screen, both shapes, persisted as it goes
and resumable. [`src/batch/`](../src/batch/) holds the model and the pairing import;
schema **v8** holds `batches` and `batch_keys`.

The design decision worth stating first: **a batch drives the wizard.** Each key still
gets its own plan, its own pre-flight, its own confirmation and its own audit entries. A
batch mode with its own quieter run path would be a second way of writing to a key, and
the second way is always the one that skips a check. What the batch adds is the
bookkeeping — which is the failure mode a tool can actually fix.

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

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Stock-preparation batch: insert-next-key loop, progress, resume | 2 | **Done** | Positions are created up front so progress reads "8 of 50"; the count is a **target, not a limit**, because a box with one extra key in it is not worth stopping for |
| 2 | Duplicate-serial detection within a batch | 2 | **Done** | Against **every** position, settled or not: a key that *failed* is worth refusing too, since re-running it blindly gives a half-configured key a second, conflicting attempt. Audited `batch.key.duplicate` |
| 3 | Continue-on-failure with a needs-attention list | 2 | **Done** | The outcome is folded in on the run's own path, failure branch included — a batch that only heard about its successes would report a clean sweep of a box where seven keys failed |
| 4 | Pairing list import (CSV) with up-front validation | 2 | **Done** | **The whole file or none of it**, and every bad row reported at once. See below |
| 5 | Assigned enrolment with per-holder certificates | 2 | **Done** | The list may name the key or leave it to the box. A key with nobody left to belong to is refused rather than paired with an invented holder |
| 6 | Batch summary and evidence export | 2 | **Done** | The needs-attention list on screen, and the runs a batch produced are ordinary runs — so the bootstrap-compliance and custody-model reports already cover them |
| 7 | Batch hand-over document generation | 2 | Todo | with [`receipts-and-terms.md`](receipts-and-terms.md) phase 7 |
| 8 | Schema: `batches` table | 2 | **Done** | **v8** — `batches` + `batch_keys`. Two tables because a batch is a header and a list, and the list is what has to be written *as it goes* |

### Why the pairing list is all-or-nothing

Every other import in this tool is a preview: `store::import` plans each row, refuses the
ones it cannot read, and imports the rest — right for a spreadsheet somebody has kept by
hand for three years.

This one is the opposite, and deliberately. A half-imported pairing list means eleven keys
written with certificates naming eleven people and then a stop, with the operator holding a
box that is now part-configured and a list to reconcile by hand. A file refused at the desk
costs a minute. So every address is validated **before the first key is touched**, one bad
row rejects the file, and *all* the problems are reported together with their line numbers —
an operator fixing a spreadsheet wants every bad row in one pass.

## Audit events

| Event | Detail |
|---|---|
| `batch.started` | `batch=<id> template=<id>@<version> shape=stock\|assigned count=<n>` |
| `batch.key.done` | `batch=<id> serial=<n> run=<id>` |
| `batch.key.failed` | `batch=<id> serial=<n> reason=…` |
| `batch.key.duplicate` | The same serial was presented twice |
| `batch.finished` | `succeeded=<n> failed=<n> skipped=<n>` |
| `batch.key.skipped` | A position the operator passed over, with the reason |
| `batch.resumed` | `batch=<id> from_key=<n>` |

Per-key bootstrap events are still emitted individually: a batch is not an excuse for a
coarser audit trail.

## Tests

- Behaviour: interrupt after key 2 → reopen the register → the batch resumes at key 3,
  keys 1–2 are refused as duplicates rather than re-run, key 4 fails and the batch
  finishes `succeeded=4 failed=1`. — `tests/behaviour_batch.rs`
- Behaviour: an assigned batch keeps each key with its person across a restart — losing
  that would mean issuing a certificate to the wrong person.
- Behaviour: a register written before v8 opens with an empty batch list.
- Unit: the same serial presented twice is refused and the refusal says *where* it already
  is; a key that failed is refused too. — `src/batch/mod.rs`
- Unit: a box with one more key than expected is not an error.
- Unit: an assigned batch refuses a key with nobody left rather than inventing a holder.
- Unit: CSV pairing import rejects the whole file on one malformed e-mail, reports **every**
  bad row with its line number, refuses a repeated address or serial, and stops listing
  after twelve. — `src/batch/pairing.rs`

## Open questions and gates

- **Unattended or per-key confirmation?** *Still open, and built the safe way in the
  meantime.* Every key is confirmed individually, exactly as a single run is — which is
  never worse for safety and needs no decision to ship. Making stock preparation unattended
  would be a real saving on a box of fifty and is a **procedure owner's** call, not an
  implementer's: it removes the last human check in front of a hardware write. The batch
  is built so that answering "yes, unattended for stock" is a change to the loop and not
  to the model.
- Whether a batch requires a second operator's sign-off, given it writes to 50 security
  tokens in one session.

## References

- `features/bootstrap-engine.md`, `features/gui-bootstrap-wizard.md`
- `features/storage-sqlite-single-file.md`
