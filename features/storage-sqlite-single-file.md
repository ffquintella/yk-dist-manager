# Feature: Single-file SQLite storage

## Summary

All state in **one** SQLite file (`rusqlite`, bundled), able to live on a network
share so several operators in a unit work against the same dataset, with the
locking strategy chosen from where the file sits.

## Motivation

The deployment target is a small team in one unit. A server is disproportionate; a
spreadsheet on a share is what this tool replaces. One file on the unit's share
gives shared access, a trivial backup story (copy the file), and no service to
operate. SQLite is compiled in, so a workstation installs nothing.

The catch is that SQLite over SMB/NFS is only safe if configured for it, and the
failure mode of getting it wrong is a corrupt database — which here means losing
the record of who holds which security token.

## Current state

**Schema v1 shipped.** `src/store/mod.rs`:

- Tables: `keys`, `holders`, `bootstrap_runs`, `distributions`, `templates`,
  `audit`, plus three indexes.
- Migrations tracked in `PRAGMA user_version`; a file written by a **newer**
  build is refused (`StoreError::SchemaTooNew`) instead of being opened and
  silently mangled.
- `Location::detect()` classifies the path; `Location` decides the pragmas.
- `VACUUM INTO` backup, `PRAGMA integrity_check`, both exposed in Settings.
- Every statement parameterised. Enum values stored as readable strings
  (`in_stock`, `in_person`) rather than JSON-quoted debug output.
- Timestamps stored as RFC 3339 UTC strings, parsed explicitly, so the format does
  not depend on a crate feature.

Not yet done: multi-operator concurrency policy, retention/archival, and the
password work tracked in `features/db-password-and-encryption.md`.

## Design

### Locking, by location

| | Local disk | Network share |
|---|---|---|
| `journal_mode` | `WAL` | `DELETE` (rollback journal) |
| `synchronous` | `NORMAL` | `FULL` |
| `busy_timeout` | 5s | 20s |

**Why not WAL on a share:** WAL coordinates readers and writers through a shared
memory file (`-shm`) mapped by every process. Network filesystems do not provide
coherent shared memory, and SQLite's own documentation rules WAL out over a
network. Rollback journal plus `synchronous=FULL` is the supported configuration.
A behaviour test asserts no `-wal`/`-shm` file is created in share mode.

Detection heuristic (overridable): `\\` UNC, `//`, `/Volumes/`, `/mnt/`,
`/net/`, `/media/`. It is a heuristic on purpose — the operator can override it in
Settings, because no prefix list is right everywhere.

### Schema v1

See `docs/data-model.md` for the field-by-field reference. Shape:

```
keys(serial UNIQUE) ──┐
                      ├── distributions(key_id, holder_id, bootstrap_run_id)
holders(email UNIQUE)─┘            │
bootstrap_runs(id) ◄───────────────┘
templates(id, version)  PRIMARY KEY (id, version)
audit(seq)  + BEFORE UPDATE/DELETE triggers → RAISE(ABORT)
```

Natural keys carry `UNIQUE` constraints (`keys.serial`, `holders.email`) so
re-reading a key or re-registering a person updates rather than duplicates.

### Concurrency (Phase 4)

Two operators on the same share will collide eventually. Planned policy:

- Writes are short single-statement transactions; no transaction is held open
  across a UI interaction.
- `SQLITE_BUSY` is retried with backoff up to the busy timeout, then reported to
  the operator with a clear "another operator is writing" message.
- Records carry `updated_at`; an update that finds a newer `updated_at` than the
  one it read reports a conflict instead of overwriting.
- The audit table is insert-only, so it never conflicts.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Schema v1 + `user_version` migrations | Done | refuses a newer schema |
| 2 | Location-aware pragmas | Done | WAL vs rollback journal, tested |
| 3 | Backup (`VACUUM INTO`) + `integrity_check` | Done | Settings screen |
| 4 | Multi-operator concurrency policy | Todo | busy retry + optimistic `updated_at` |
| 5 | Schema v2: per-step run rows instead of a JSON blob | Todo | needed for step-level reporting |
| 6 | Scheduled/automatic backup with rotation | Todo | daily copy next to the file, keep N |
| 7 | Archival and retention | Todo | blocked on the ESI retention decision |
| 8 | Import from the spreadsheet this replaces | Todo | CSV mapping + dry-run preview |

## Audit events

| Event | When |
|---|---|
| `app.opened` | Database opened successfully |
| `db.migrated` | Schema version advanced (from → to) |
| `db.backup` | Backup written, with the target path |
| `db.integrity_check` | Result recorded when run |
| `db.conflict` | Phase 4: an optimistic-concurrency conflict was refused |

## Tests

`tests/behaviour_storage.rs`:

- `scenario_everything_lives_in_one_file` — after writing every record type, the
  directory contains exactly one file.
- `scenario_records_survive_a_restart` — reopen, records and chain intact.
- `scenario_a_share_hosted_database_avoids_wal` — no `-wal`/`-shm` in share mode.
- `scenario_windows_unc_paths_are_treated_as_shares`.
- `scenario_a_backup_is_a_usable_copy` — the backup opens standalone and verifies.
- `scenario_integrity_check_reports_ok_on_a_healthy_file`.
- `scenario_builtin_templates_are_seeded_once` — idempotent seeding.

## Open questions and gates

- **Segregated audit storage.** The norm wants audit data in a different instance
  from operational data; the requirement here is one file. Current answer:
  same file, immutable by trigger, plus an optional append-only mirror
  (`features/audit-trail.md`). Needs ESI sign-off.
- **Share permissions** are the real access control for an unencrypted file. Who
  may read the share is an infrastructure decision that must be recorded in the
  system registration.
- Retention period: ESI.

## References

- `src/store/mod.rs`
- `docs/data-model.md`, `docs/operations.md`
- SQLite: [WAL and network filesystems](https://sqlite.org/wal.html)
