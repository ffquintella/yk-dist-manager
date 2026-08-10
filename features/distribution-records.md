# Feature: Distribution records

## Summary

The hand-over event: which key went to which person, when, **by whom**, how it was
delivered, against which receipt, and which bootstrap run was applied to it. Plus
the return, recorded without rewriting the original.

## Motivation

This is the feature the tool exists for. The record has to survive the question
asked a year later — "who gave Ana this key, and what was on it?" — which means it
must capture the operator who performed the hand-over, not just the date, and it
must link to the evidence of what was applied.

It also has to survive a key changing hands twice. Editing a record in place to
point at the new holder destroys exactly the information an audit needs.

## Current state

**Done for the basics.** `src/domain/distribution.rs`, `src/ui/distribution.rs`:

- Fields: `key_id` + `key_serial`, `holder_id` + `holder_display`,
  `distributed_at`, `distributed_by`, `method`, `receipt_ref`, `bootstrap_run_id`,
  `returned_at`, `returned_to`, `notes`.
- `is_open()` and `days_held(now)` for the table and reports.
- Recording a hand-over also moves the key to `Distributed`; a refused transition is
  reported ("recorded, but status not updated: …") rather than hidden.
- `mark_returned` only closes a record whose `returned_at` is `NULL`, so a second
  return attempt is refused instead of overwriting who received it.
- The Distribution screen offers to attach the most recent bootstrap run for the key,
  and the table shows the run summary ("FIDO2 PIN, PIV certificate import, …").

Not yet done: receipt generation, transfer between holders as a first-class action,
and reporting.

## Design

### Denormalised fields are deliberate

`key_serial` and `holder_display` are copies. A distribution report must stay
readable when a key is retired or a holder record is later restricted, and a report
must not silently change its historical content because a name was corrected today.
The foreign keys remain for navigation; the copies are the record.

### Correction policy

- A distribution is **append-only in spirit**: the only mutable fields are the
  return fields.
- A mistake is corrected by a new record plus an audit entry explaining it, never by
  editing the original.
- A key moving from Ana to Bruno is: close Ana's record (return), then a new record
  for Bruno. A behaviour test asserts both survive.

### Delivery method

`InPerson` / `Courier` / `Post`. It matters because it changes the evidence: an
in-person hand-over gets a signature on the spot; a posted key gets a tracking
reference in `receipt_ref` and the signed term comes back later. Phase 4 adds a
pending-signature state so a posted key is not indistinguishable from a signed
hand-over.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Record with operator, method, receipt reference | Done | |
| 2 | Link to the bootstrap run; show what was applied | Done | run summary in the table |
| 3 | Return handling that does not rewrite history | Done | second return refused |
| 4 | Pending-signature state for remote delivery | Todo | with an age warning for unsigned hand-overs |
| 5 | Transfer as a first-class action (close + open, one confirmation) | Todo | currently two manual steps |
| 6 | Receipt / responsibility term generation | Todo | `features/receipts-and-terms.md` |
| 7 | Overdue and unaccounted reporting | Todo | `features/reports-and-export.md` |
| 8 | Bulk hand-over (a batch to one unit) | Todo | `features/bulk-enrollment.md` |

## Audit events

| Event | When |
|---|---|
| `key.distributed` | Hand-over recorded, with holder, method and receipt reference |
| `key.returned` | Return recorded, with who received it |
| `key.transferred` | Phase 5 |
| `distribution.corrected` | A correcting record was created, with the reason |

## Tests

`tests/behaviour_distribution.rs`:

- `scenario_hand_a_key_to_a_person_and_see_who_holds_it`
- `scenario_a_key_cannot_be_distributed_straight_from_stock`
- `scenario_returning_a_key_closes_the_record_without_rewriting_history`
- `scenario_a_key_cannot_be_returned_twice`
- `scenario_one_key_reissued_to_a_second_holder_keeps_both_records`

Unit coverage for `is_open` / `days_held` sits with the domain tests.

## Open questions and gates

- Is a signed responsibility term mandatory before a key leaves, or is the reference
  optional? That is a unit policy decision and changes whether `receipt_ref` is
  required (Phase 4).
- Retention of distribution records after a holder leaves — DPO/ESI.

## References

- `src/domain/distribution.rs`, `src/ui/distribution.rs`, `src/app.rs`
- `docs/data-model.md`, `docs/operations.md`
