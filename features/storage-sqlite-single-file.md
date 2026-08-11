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

**Schema v5** turns a bootstrap run's steps into rows (`bootstrap_run_steps`)
instead of a JSON blob in `bootstrap_runs.steps`. Three reasons: step-level
reporting becomes a `GROUP BY` rather than a parse of every run; the Wave 1
executor writes one step outcome at a time, and a blob has to be rewritten whole
each time, so an interrupted run loses the steps that had already succeeded; and
the column stores the same readable strings as the rest of the schema
(`fido2-pin`, `done`) rather than serde's variant names. The backfill is Rust,
not SQL, so the mapping is `StepKind::slug` itself; a blob that cannot be parsed
leaves that run with no step rows and a loud log line, rather than refusing to
open the register.

**Backups are taken by the tool**, not just recommended to the operator
([`store::backup`](../src/store/backup.rs)): `VACUUM INTO` on a schedule
(daily by default), keeping the newest N (7 by default), with pruning that only
ever deletes a filename it can parse as one of ours. A cloud-hosted register
also gets one at open, before this session can write — see
[`cloud-sync-hosting.md`](cloud-sync-hosting.md) phase 7.

**The spreadsheet this tool replaces can be imported**
([`store::import`](../src/store/import.rs)): preview first, always, then apply.

Not yet done: multi-operator concurrency policy (Wave 2), retention/archival
(blocked on ESI), and the password work tracked in
`features/db-password-and-encryption.md`.

## Design

### Locking, by location

| | Local disk | Network share | Cloud-sync folder |
|---|---|---|---|
| `journal_mode` | `WAL` | `DELETE` (rollback journal) | `DELETE` |
| `synchronous` | `NORMAL` | `FULL` | `FULL` |
| `busy_timeout` | 5s | 20s | 20s |
| single-writer lock file | — | — | **yes** ([spec](cloud-sync-hosting.md)) |

**Why not WAL on a share:** WAL coordinates readers and writers through a shared
memory file (`-shm`) mapped by every process. Network filesystems do not provide
coherent shared memory, and SQLite's own documentation rules WAL out over a
network. Rollback journal plus `synchronous=FULL` is the supported configuration.
A behaviour test asserts no `-wal`/`-shm` file is created in share mode.

Detection heuristic (overridable): `\\` UNC, `//`, `/Volumes/`, `/mnt/`,
`/net/`, `/media/`, **and any path that looks like a cloud-sync folder**. It is a
heuristic on purpose — the operator can override it in Settings, because no prefix list
is right everywhere.

A caller that *knows* does not guess: `StoreConfig::with_location` states the location
outright, and [`smb::ShareConnection::store_config`](smb-share-hosting.md) uses it,
because a share this application just connected is on a network filesystem whatever
mount point the operating system chose. The heuristic is for paths nobody can vouch
for.

### Getting to the share is part of the storage story

"Put the register on a network share" was advice nobody could act on while the
application could only open a path somebody else had mounted, and the symptom of that
not having happened was "is the share mounted?". So the share can now be **connected
from inside the application** — as the signed-in user (the default, and on Windows the
whole mechanism), as a guest, or as a named account whose password is typed and
dropped. See [`smb-share-hosting.md`](smb-share-hosting.md).

### Cloud-sync folders are the worst case, and are now called out

A `--diagnose` report from a real installation showed the database living in
`~/Library/CloudStorage/OneDrive-…/`. That is the single most dangerous place for a
SQLite file, and it was being opened in **WAL** mode because the path is under `$HOME`
and matched no share prefix:

- the sync client copies the file while a writer holds it open;
- WAL's `-wal` and `-shm` sidecars are synchronised independently of the database, so
  the three can arrive out of step;
- and a sync conflict is resolved by keeping **both** files, not by merging — so the
  failure mode is two divergent registers of who holds which security token.

`looks_like_cloud_sync()` recognises OneDrive, Dropbox, Google Drive, iCloud
(`Mobile Documents`), pCloud and macOS's `CloudStorage` File Provider directory. Such a
path is now its own location — `Location::CloudSync` — which takes the share's pragmas
*and* a **single-writer lock file**, because unlike two workstations on a share, two
workstations behind a sync client share no lock manager at all.

That protocol is specified in [`cloud-sync-hosting.md`](cloud-sync-hosting.md): wait
until the file has stopped changing, take `<database>.lock`, refuse a second
workstation by name, release the lock only after the upload, and report the copies a
sync client leaves when it could not merge. The operator is still warned in the status
line, in the Settings screen and in `--diagnose`.

Safer pragmas and a cooperative lock reduce the risk; they do not remove it. The
recommendation stays a real network share, or a local file with a scheduled backup.

### Schema

See `docs/data-model.md` for the field-by-field reference. Shape at v3:

```
keys(serial UNIQUE, serial_source) ──┐
                                     ├── distributions(key_id, holder_id, bootstrap_run_id)
holders(email UNIQUE, +optional) ────┘            │        │
bootstrap_runs(id) ◄──────────────────────────────┘        │
templates(id, version)       PRIMARY KEY (id, version)     │
term_templates(id, language, version)                      │
documents(distribution_id, sha256, content BLOB) ◄─────────┘
audit(seq)  + BEFORE UPDATE/DELETE triggers → RAISE(ABORT)
```

`documents` holds the signed terms as blobs, which is why the file can grow: budget
roughly 100–500 KB per signed scan (`features/signed-term-documents.md`).

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
| 1b | Strict `open_existing` / `create_new` | Done | [spec](database-selection.md) — a typo can no longer create an empty database |
| 1c | Schema v2 (serial provenance), v3 (optional holder fields, term templates, documents) and v4 (`templates.retired_at`) | Done | the v1→v4 chain is covered by a test that builds a v1 file by hand |
| 2 | Location-aware pragmas | Done | WAL vs rollback journal, tested |
| 2b | Cloud-sync detection: safe pragmas plus a visible warning | Done | found by a `--diagnose` report from a real installation |
| 2c | Cloud-sync hosting: `Location::CloudSync` + single-writer lock | Done | [spec](cloud-sync-hosting.md) — the installation that prompted 2b needed the folder to *work*, not just to be warned about |
| 2d | Connect the SMB share from the application: anonymous, named account, or the signed-in user | In progress | [spec](smb-share-hosting.md) — the mechanism and all three platform backends are done and tested; the chooser card is Todo. This is what makes "use a real share" actionable rather than advice |
| 3 | Backup (`VACUUM INTO`) + `integrity_check` | Done | Settings screen |
| 4 | Multi-operator concurrency policy | Todo | busy retry + optimistic `updated_at` — **Wave 2**, tracked in the roadmap under that wave |
| 5 | Per-step run rows instead of a JSON blob | **Done** | schema **v5**: `bootstrap_run_steps`, backfilled in Rust, blob column dropped |
| 6 | Scheduled/automatic backup with rotation | **Done** | [`store::backup`](../src/store/backup.rs) — daily by default, keep 7, pruning only names it can parse |
| 7 | Archival and retention | Todo | **blocked on the ESI retention decision** (open question 3) |
| 8 | Import from the spreadsheet this replaces | **Done** | [`store::import`](../src/store/import.rs) — column mapping, preview, then apply |

## Audit events

| Event | When |
|---|---|
| `app.opened` | Database opened successfully |
| `db.migrated` | Schema version advanced (from → to) |
| `db.backup` | Backup written, with the target path |
| `db.integrity_check` | Result recorded when run |
| `db.conflict` | Phase 4: an optimistic-concurrency conflict was refused |
| `db.closed` | The database was closed; names the single-writer lock when one was held |
| `db.lock.taken_over` | An abandoned cloud-sync lock was broken deliberately |
| `db.sync.conflict_copies` | A sync client left copies it could not merge next to the file |

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
