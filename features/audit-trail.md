# Feature: Audit trail (append-only, hash-chained)

## Summary

Every state change is recorded in an append-only trail where each entry carries the
SHA-256 hash of the previous one. The table refuses `UPDATE` and `DELETE` by
database trigger, so immutability is a property of the database rather than a
promise from the application.

## Motivation

This tool is the record of custody for security tokens. Its value depends entirely
on the trail being trustworthy: "the record says Ana received serial 20423633 on
10/08/2026, and nobody has edited that since". A log file that an operator can edit
answers nothing.

NRM §5.3.1 is explicit: login, account creation and account changes must always
be audited; audit data must live in a different instance from operational data;
nobody may delete or alter audit records, and that must be guaranteed by
**database restrictions**, not by the application; and the audit table must
prioritise cheap inserts.

## Current state

**Chain and immutability shipped.**

- `src/audit/mod.rs` defines `AuditEntry` (`seq`, `at`, `actor`, `event`,
  `target`, `details`, `prev_hash`, `hash`), the canonical payload the hash covers,
  and `verify()`.
- `Store::append_audit` reads the chain head, links, hashes and inserts in one
  statement. `Store::verify_audit` re-checks the whole chain.
- Schema v1 creates `audit_no_update` and `audit_no_delete` `BEFORE` triggers that
  `RAISE(ABORT, 'audit trail is append-only')`.
- `AuditLog` is a standalone append-only JSONL sink using the same entry type and
  chain rules — the basis for the segregated mirror.
- The Audit screen lists entries newest-first and has a "Verify chain" button.
- `YkDistApp::record` logs at `error` and shows `AUDIT FAILURE: …` in the status
  bar when an append fails. Never `let _ = `.

Not yet done: the segregated mirror, an external chain-head witness, export, and
coverage of the events that Wave 1+ features introduce.

## Design

### Chain

```
entry.hash = SHA256(seq | at | actor | event | target | details | prev_hash)
prev_hash of entry 1 = 64 zeros (GENESIS)
```

Field order is part of the format; changing it invalidates every existing chain, so
it is documented here and asserted by tests. `verify()` checks three things per
entry: sequence continuity (detects deletion), `prev_hash` linkage (detects
insertion and reordering), and the entry's own hash (detects edits).

The chain is **tamper-evident**, not tamper-proof: someone with write access to the
file could rebuild the whole chain. Defence in depth for that:

1. The triggers stop every ordinary path, including a SQL console (Done).
2. Share ACLs limit who can write the file at all (infrastructure).
3. The append-only mirror on separate storage means a rebuilt chain no longer
   matches the mirror (Phase 2).
4. An external witness — periodically publishing the chain head somewhere the
   operator cannot rewrite — closes the remaining gap (Phase 5).

### What must be audited

Minimum set, kept in sync with the feature files:

| Event | Source |
|---|---|
| `app.opened` | app startup |
| `key.added`, `key.refreshed`, `key.status_changed`, `key.note_changed`, `key.removed` | inventory |
| `holder.registered` | holders |
| `key.distributed`, `key.returned` | distribution |
| `bootstrap.dry_run` | wizard |
| `bootstrap.started`, `bootstrap.step.done`, `bootstrap.step.failed`, `bootstrap.finished` | executor (Wave 1) |
| `template.created`, `template.changed`, `template.retired`, `template.reinstated`, `template.removed` | Templates screen — a new version, or a withdrawal (`template.seeded` is logged, not audited) |
| `term.generated`, `term.saved`, `term.signed_uploaded` | consignment terms — `term.saved` names the format (`format=pdf path=…`), since the two outputs are filed differently |
| `term.template_edited`, `term.template_added` | Terms screen — a new version of the wording |
| `db.backup`, `db.migrated`, `db.unlocked`, `db.unlock.failed` | storage |
| `export.taken` | reports |
| `operator.login`, `operator.login.failed`, `operator.role.changed` | Wave 2 auth |

Rules for `details`: secret-free, one `key=value` per fact, and enough to
reconstruct what happened without opening another system.

### Not a log

Logs (`src/logging.rs`) are for diagnosis and may be rotated away. Audit is for
accountability and is never rewritten. The two never share a mechanism.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Chain + DB triggers + verification + GUI | Done | 6 unit + 1 behaviour test |
| 2 | Segregated append-only mirror | Todo | second path (ideally a different share with append-only ACL); divergence is an alert |
| 3 | Full event coverage for Wave 1 executor steps | Todo | one entry per step outcome |
| 4 | Audit export for the ESI (signed, with a verification note) | Todo | `features/reports-and-export.md` |
| 5 | External chain-head witness | Todo | periodic head publication; makes a rebuild detectable |
| 6 | Audit screen: filter by event, actor, serial, date range | Todo | currently a flat 500-entry list |
| 7 | Verification on open, not just on demand | Todo | warn at startup if the chain is broken |

## Audit events emitted by this feature itself

| Event | When |
|---|---|
| `audit.verified` | Chain verification passed (entry count) |
| `audit.verify.failed` | Chain verification failed (reason) |
| `audit.append.failed` | An append failed — logged at `error`, shown to the operator |

## Tests

`tests/unit_audit.rs`:

- `first_entry_links_to_genesis`
- `entries_chain_and_verify`
- `reopening_continues_the_chain` — sequence does not restart
- `editing_an_entry_breaks_the_chain`
- `deleting_an_entry_breaks_the_chain`
- `empty_chain_is_valid`

`tests/behaviour_storage.rs`:

- `scenario_the_audit_trail_cannot_be_rewritten`
- `scenario_a_backup_is_a_usable_copy` — the backup's chain verifies

Phase 3 adds: for every mutation in `Store`, a behaviour test asserting the
matching audit entry exists with the expected event name.

## Open questions and gates

- **Segregation vs single file** — the norm wants a separate instance; the
  deployment wants one file. Current answer: same file + triggers + optional
  mirror. **ESI must sign this off.**
- **Retention** — not fixed by the norm. ESI decides; until then nothing is ever
  deleted.
- Audit of *reads* (who looked at the holder list) is not currently in scope; if
  the classification lands at level 3, ask whether it should be.

## References

- `src/audit/mod.rs`, `src/store/mod.rs` (`append_audit`, `SCHEMA_V1` triggers)
- `docs/security-and-compliance.md`, `AGENTS.md` §3
