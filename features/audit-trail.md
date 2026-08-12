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

**The segregated mirror is wired up.** `StoreConfig::with_audit_mirror` points at
a second location — ideally a different share with an append-only ACL — and every
entry is copied there **verbatim** by `AuditLog::append_existing`: the same
sequence, timestamp and hashes, not a second chain about the same events. That
distinction is the whole feature. A mirror that re-derived each entry would get
this machine's clock and its own sequence, so its hashes would differ from the
database's for every entry and the two could never be compared.

`Store::mirror_status()` compares them, and the case it catches is the one the
triggers and the hash chain cannot: a chain **rebuilt** consistently in the
database still verifies against itself, and only a copy on storage the operator
cannot rewrite shows that it changed. A sloppy edit — content changed, hash not
recomputed — is caught by verification alone and needs no mirror. Both are
covered by a scenario.

A mirror failure never fails the mutation: undoing a hand-over because a second
file could not be written would lose the fact being recorded. It is logged at
`error` and surfaced through `mirror_status()`.

**The chain is verified at open** (`Store::chain_status()`), not only when
somebody presses the button, bounded at 20 000 entries so a register with a year
of history does not delay the first frame — past that the status says *not
checked* rather than implying it passed.

**The Audit screen can be filtered** by event, actor, target and date range
(`audit::AuditFilter`). The filter lives in the audit module rather than the paint
code because it is the part worth testing, and `src/ui/` is outside the coverage
gate. The row limit applies *after* the filter, so "everything about serial
20423633" does not come back empty because the newest 500 entries concern another
key. A filtered list says so, because one that looks like the whole trail is how
somebody concludes an event never happened.

Not yet done: an external chain-head witness, export, and coverage of the events
the Wave 1 executor introduces.

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

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Chain + DB triggers + verification + GUI | 0 | Done | 6 unit + 1 behaviour test |
| 2 | Segregated append-only mirror | 0 | **Done** | `StoreConfig::with_audit_mirror`; entries copied **verbatim**, divergence is an alert. *Whether* segregation is required here is still the ESI's call |
| 3 | Full event coverage for Wave 1 executor steps | 1 | **Done** | `bootstrap.started` / `.step.done` / `.step.failed` / `.step.skipped` / `.finished` / `.aborted` / `.resumed` / `.incomplete`, plus `secret.generated` and `secret.change_enforcement` |
| 4 | Audit export for the ESI (signed, with a verification note) | 2 | Todo | **Wave 2**, with `features/reports-and-export.md` |
| 5 | External chain-head witness | — | Todo | periodic head publication; makes a rebuild detectable even without the mirror |
| 6 | Audit screen: filter by event, actor, serial, date range | 0 | **Done** | `audit::AuditFilter` + `Store::audit_entries_matching`; the limit applies *after* the filter |
| 7 | Verification on open, not just on demand | 0 | **Done** | `Store::chain_status()`, checked at open up to 20 000 entries |

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
- **Retention** — **one year, configurable** *(2026-08-11)*. `settings::RetentionPolicy`
  carries the period, defaults to 12 months and refuses anything under
  `MIN_MONTHS = 6`; `RetentionPolicy::FOREVER` keeps everything. Note what the
  setting does *not* do yet: nothing is deleted by it. The audit table refuses
  `DELETE` by trigger, so honouring a finite period means deciding how a
  retention pass is allowed to break the hash chain — which is its own piece of
  work, not a side effect of this setting.
- Audit of *reads* (who looked at the holder list) is not in scope. The
  classification landed at **level 2** on 2026-08-11, so the question that would
  have forced this — level 3 — did not arise. Revisit if the level ever moves.

## References

- `src/audit/mod.rs`, `src/store/mod.rs` (`append_audit`, `SCHEMA_V1` triggers)
- `docs/security-and-compliance.md`, `AGENTS.md` §3
