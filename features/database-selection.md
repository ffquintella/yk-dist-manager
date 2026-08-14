# Feature: Choosing and creating the database file

## Summary

Open an existing database, create a new one, or switch between them from inside the
application — with the recently used files remembered, and **open** kept strictly
separate from **create**.

## Motivation

The database is the deployment (`features/storage-sqlite-single-file.md`), so which
file is open is the most consequential choice an operator makes. Before this, the
path was fixed at startup by `$YKDM_DB` or a per-user default, which failed three
real situations: a unit with a share and a local copy, an operator standing at a
different workstation, and the first run at a site that has not created a database
yet.

The sharp edge being removed: `Store::open` created the file when it was not there.
So a mistyped share path produced an empty database that looked exactly like every
record having vanished. Guessing which of "open" and "create" the operator meant is
not a convenience — it is the most alarming bug this tool could have.

## Current state

**Shipped.**

- `Store::open_existing` fails with `StoreError::Missing` when the file is not
  there; `Store::create_new` fails with `StoreError::AlreadyExists` when it is, and
  seeds the built-in templates so a fresh database is usable immediately.
  `Store::open` keeps the open-or-create behaviour for the default path and tests.
- `settings::AppSettings` (JSON, in the per-user data directory) remembers the last
  database, up to 8 recent ones, and the operator identity. **It never holds a
  password** — it sits next to the database, so storing one would defeat encrypting
  the database at all.
- Startup precedence: `$YKDM_DB`, then the last database used, then the per-user
  default. A remembered database that has gone (an unmounted share) shows the
  chooser with the reason rather than being re-created empty.
- `src/ui/database.rs` — the chooser: recent list with per-entry availability, a
  typed path, a password field, `Open`, `Create`, and native `Choose file…` /
  `New file…` dialogs behind the `file-dialog` feature (on by default).
- Settings gains *Switch database…*, *Open another…*, *Create new…*.
- Dialogs and opens are deferred through `DbRequest`, handled after the paint pass —
  a modal dialog inside a paint closure would block rendering.

## Design

### Why a separate settings file

The recent list cannot live in the database, because it is what lets the operator
*choose* a database. It is per-workstation state, not shared data, so a JSON file
next to the default database is the right home. It is written atomically
(temp + rename) and a corrupt file degrades to defaults with a log line: losing a
recent list must never stop a hand-over.

### Unreachable entries stay listed

A share that is not mounted is a network problem, not a decision to stop using that
database. Recent entries are shown with an availability marker and a *forget*
action; nothing is silently dropped.

### Passwords

Typed at the chooser, passed to the open call, cleared from the form immediately
after. `file-dialog` and `encrypted-db` are independent features, and the chooser
says plainly when a build lacks either.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Strict `open_existing` / `create_new` with typed errors | 0 | Done | the create-by-typo hazard removed |
| 2 | Settings file: last database, recents, operator identity | 0 | Done | atomic write, corruption-tolerant |
| 3 | Chooser screen: recents, typed path, password, open/create | 0 | Done | |
| 4 | Native file dialogs behind `file-dialog` | 0 | Done | `rfd`; XDG portal on Linux, no GTK |
| 5 | Switch database from Settings | 0 | Done | closes the current one first, audited |
| 6 | Warn when two operators have the same share database open | 2 | **Done** | `open_sessions` (schema **v7**) and [`store::presence`](../src/store/presence.rs): a banner under the tabs naming who else is in the register and when they were last heard from. A **warning, not a lock** — on a share SQLite serialises the writes; what nobody could otherwise see is that somebody else is working out of the same box of keys. A session silent for 15 minutes is not shown and is pruned by the next open, so a closed laptop does not haunt the banner |
| 7 | "Copy this database to…" (clone a share database locally) | — | Todo | `VACUUM INTO` already does the work |
| 8 | Remember a per-database operator identity | 2 | **Done** | `AppSettings::operators`, keyed by the path as opened — which is right *because* settings are per workstation, so two machines mounting a share at different points never have to agree. Editing the name with a register open records it for that register; with none open it sets the workstation's default, which is what an unnamed register falls back to. The Settings screen says which of the two is being edited |

## Audit events

| Event | When |
|---|---|
| `app.opened` | A database was opened, with its path |
| `db.created` | A new database was created |
| `db.closed` | The operator closed the database to switch |
| `db.open.failed` | Logged; the reason is shown on the chooser |

A failed open cannot be audited *in* the database it failed to open — those go to
the log, which is one more reason the mirror in `features/audit-trail.md` matters.

## Tests

`tests/unit_store.rs`:

- `opening_a_path_that_does_not_exist_is_refused_rather_than_created` — and asserts
  nothing was created.
- `creating_over_an_existing_file_is_refused` — and the existing data is intact.
- `a_new_database_arrives_with_its_templates`.

`tests/unit_settings.rs`:

- `the_settings_file_round_trips_and_survives_corruption` — including an assertion
  that the file contains no password field, and that a hand-edited file with
  duplicates is normalised.
- `availability_is_reported_per_entry_without_dropping_anything`.
- `the_recent_list_never_exceeds_its_cap_or_repeats_an_entry`.

## Open questions and gates

- Two operators opening the same share database is not yet detected; until the
  concurrency work lands, they are serialised by SQLite's locking and will see busy
  messages rather than corruption.
- Whether the recent list should be shared (it names share paths, which is mild
  infrastructure information) is a per-unit call.

## References

- `src/store/mod.rs`, `src/settings.rs`, `src/paths.rs`, `src/ui/database.rs`
- `features/storage-sqlite-single-file.md`, `features/db-password-and-encryption.md`
