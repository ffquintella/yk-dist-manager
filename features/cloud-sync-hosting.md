# Feature: Hosting the database in a cloud-sync folder (OneDrive)

## Summary

Make a database that lives in **OneDrive** (or Dropbox, Google Drive, iCloud) safe
enough to use, by making access to it **strictly sequential**: wait for the sync
client to finish downloading, take a lock file next to the database, refuse a second
workstation by name, and release the lock only after the upload has finished.

Implemented in [`src/store/cloud.rs`](../src/store/cloud.rs), with
`Location::CloudSync` in [`src/store/mod.rs`](../src/store/mod.rs) deciding when it
applies.

## Motivation

The design says the file belongs on a network share, and
[`storage-sqlite-single-file.md`](storage-sqlite-single-file.md) calls a sync folder
"the single most dangerous place for a SQLite file". Then a `--diagnose` report from a
real installation showed the database in
`~/Library/CloudStorage/OneDrive-…/`, because that is the shared folder the unit
actually has. Telling that operator "don't" produced neither a share nor a safer
setup — it produced an unmanaged risk plus a warning nobody can act on.

So the risk is managed instead. What can go wrong in a sync folder, and what this
feature does about each:

| Failure | What it costs | Treatment |
|---|---|---|
| The sync client replaces the file while a writer holds it open | Corruption | Rollback journal (never WAL), `synchronous=FULL`, and a wait until the file has stopped changing before the connection is opened |
| Two operators open the register at the same time | Two divergent copies, resolved by keeping **both** | The lock file: one workstation at a time, the second refused with the first operator's name |
| A session ends and the upload has not finished | The next operator opens yesterday's register | The lock is removed only *after* the file has settled, so nobody can start early |
| A clash happened anyway | The register has forked and nobody notices | Conflict copies are detected next to the database, and reported in the status line, in Settings, in the audit trail and in `--diagnose` |

## What this is not

It is not a distributed lock, and the code says so where somebody might hope
otherwise. A workstation that is offline sees neither the lock nor the data. Two
machines that create a lock inside the same sync interval are resolved by the sync
client like any other clash — the write-then-verify step catches the loser, but only
after the client has made up its mind. The lock binds workstations running this tool
and cooperating; it cannot bind anything else.

What it converts is the *common* accident — two operators opening the shared register
at the same time — from silent corruption into a refusal with a name on it. A real
network share is still the recommendation, and a scheduled backup is still required.

## Design

### The protocol

1. **Wait for the download.** `wait_until_settled` polls `(size, mtime)` until the
   file has been unchanged for `SyncPolicy::quiet` (1.5s) or `SyncPolicy::timeout`
   (15s) runs out. A timeout is not fatal: it is reported ("the file was still
   changing"), because refusing to open the register on a slow link would be worse
   than opening it and saying so. Both numbers are overridable
   (`$YKDM_SYNC_QUIET_MS`, `$YKDM_SYNC_TIMEOUT_MS`).
2. **Take `<database>.lock`.** Created with `create_new`, so two processes on one
   machine cannot both believe they have it. Contents: JSON with host, operator, pid,
   a per-run session id, the build version, `acquired_at` and `renewed_at`.
   Appended suffix rather than a replaced extension, so a database called `keys.lock`
   cannot collide with the lock for `keys.sqlite3`.
3. **Refuse anybody else.** A lock that is there is *held* — by another workstation,
   by another window on this one, or by a run that died. All three are refused. A
   lock file that cannot be parsed also counts as held, because a sync client can
   present a partial file and "unreadable ⇒ free" is the one reading that corrupts
   the register.
4. **Verify after a sync interval.** The lock is re-read after `quiet`; if it now
   names somebody else, the sync client resolved a simultaneous create in their
   favour and this session loses rather than writes.
5. **Renew while alive.** The holder record is rewritten every `RENEW_EVERY` (60s)
   from the GUI's frame loop. A renewal that finds another holder returns
   `Renewal::Lost`, and the application **closes the database** and says why.
6. **Release after the upload.** On close: audit entry (while the connection is still
   open) → close the connection → wait for the file to settle → delete the lock. The
   order is deliberate: removing the lock before the upload finished would invite the
   next workstation to start from a file still on its way. *Every* path that stops
   using a database does this — the Close button, switching database, creating
   another, and **quitting the application** (`Drop for YkDistApp`), which is what
   operators actually do. `Drop for SyncLease` is only a backstop: it removes the lock
   without waiting, because a panic cannot wait for a sync client. A `SIGKILL` or a
   power cut leaves the lock behind, and that is what staleness is for.

### Identity is per run, not per pid

`is_local()` compares a session UUID minted once per process, not host plus pid. Pids
are reused, so a lock left behind by a dead run on this workstation could otherwise be
mistaken for this session's own and silently stolen — which is precisely the
"two writers" case the lock exists to prevent.

### Staleness and taking over

`STALE_AFTER` is **15 minutes** of no renewal. Deliberately wide: a laptop that sleeps
mid-session stops renewing without releasing, and breaking its lock after a minute
would hand the register to a second operator while the first still has it open.

A stale lock is **still refused** by default. Only the operator can know that the other
machine is off rather than mid-hand-over, so the chooser offers a *Take the lock over*
button — and only once the holder has gone quiet long enough — and the break is
audited with the previous holder's name in the entry.

### Location

`Location::CloudSync` is its own variant, ahead of the share check in
`Location::detect`. It takes the share's pragmas (rollback journal,
`synchronous=FULL`, 20s busy timeout) because WAL's `-wal`/`-shm` sidecars cannot
survive a sync client, and adds the lock because — unlike on a share — the two
workstations do not share a lock manager at all. Detection is the existing
`looks_like_cloud_sync` marker list (OneDrive, Dropbox, Google Drive, iCloud
`Mobile Documents`, pCloud, macOS `CloudStorage`), and stays overridable in Settings.

### Personal data

The lock file holds an operator name, a workstation name and a pid — the same
identity the audit trail already records, and no more. **No secret**: no password, no
PIN, no access code, on any path in this feature.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | `Location::CloudSync`, share pragmas, own label | Done | ahead of the share check in `detect` |
| 2 | Settle wait before open and before release | Done | bounded, reported, tunable from the environment |
| 3 | Lock file: acquire, refuse by name, renew, release | Done | `create_new`, per-run session id, write-then-verify |
| 4 | Deliberate take-over of an abandoned lock, audited | Done | 15-minute staleness; refused by default |
| 5 | Conflict-copy detection and reporting | Done | status line, Settings, audit, `--diagnose` |
| 6 | GUI: lock state in Settings, refusal card in the chooser, status pill | Done | `db: cloud-sync (locked)` |
| 7 | Automatic backup before the first write of a session | Todo | pairs with phase 6 of the storage spec; the cheapest answer to a fork that already happened |
| 8 | Read-only mode instead of a refusal | Todo | a second operator could *look* at the register while another writes; needs a read-only `Store` |

## Audit events

| Event | When |
|---|---|
| `db.lock.taken_over` | An abandoned lock was broken deliberately; the entry names the previous holder |
| `db.closed` | Carries "releasing the single-writer lock held by …" when a lock was held |
| `db.sync.conflict_copies` | Copies a sync client could not merge were found next to the database |

A **refused** open has no audit entry, and cannot have one: there is no open database
to write to. It is logged (`db.open.failed`, `db.lock.acquired`, `db.lock.lost`,
`db.sync.unsettled`, `db.sync.upload_pending`) and shown to the operator.

## Tests

`tests/unit_store_cloud.rs` (14):

- the lock is taken next to the database and names the operator;
- a second workstation is refused **by name**, and the message contains it;
- an abandoned lock reports `stale`, is still refused, and can be taken over — with
  the previous holder carried out for the audit entry;
- an unparseable lock file still counts as held;
- releasing removes the lock and reports the wait; a **dropped** lease still frees the
  register;
- a lease taken over by another workstation reports `Lost` on renewal;
- a renewal that is not due does not rewrite the lock, and a forced one does;
- a file still being written is reported as unsettled, and the wait is bounded;
- conflict copies are found, and our own backups and lock file are not mistaken for
  them;
- through `Store`: opening takes the lock and closing releases it; a held database
  refuses a second open; a local database takes no lock file; the lock can be declined
  and the status line says so.

`tests/behaviour_app_cloud_lock.rs` (1 scenario, the only test that drives
`YkDistApp` — it owns its binary's environment): starting on a sync-hosted path takes
the lock; closing releases it and audits "releasing the single-writer lock held by …";
another workstation's abandoned lock becomes a refusal carrying the holder and the
`stale` flag rather than a message; taking it over opens the register and writes
`db.lock.taken_over` naming the previous holder; and **quitting** the application
releases the lock as well as closing it does.

`tests/behaviour_storage.rs`:

- `scenario_two_operators_share_a_onedrive_folder_one_at_a_time`;
- `scenario_a_lock_left_by_a_crashed_session_can_be_taken_over_and_is_recorded`;
- `scenario_a_sync_client_that_forked_the_register_is_reported_not_ignored`.

`src/store/cloud.rs` unit modules cover the naming rules and the environment
override; `src/diagnostics.rs` covers the report lines.

## Open questions and gates

- **Is a sync folder an acceptable location at all?** This feature makes it
  *survivable*, not *approved*. The location of a register of security tokens, and
  whether a third-party sync client may hold it, is an **ESI** decision — as is
  whether the folder's sharing settings are acceptable access control for
  unencrypted personal data. Written down here rather than assumed.
- **The password matters more here.** A file in a sync folder is a file in somebody
  else's storage. `features/db-password-and-encryption.md` is Todo, and this feature
  is an argument for finishing it.
- **Automatic backup (phase 7)** is the only real answer to a fork that has already
  happened. Currently the operator is told and must compare the copies by hand.

## References

- `src/store/cloud.rs`, `src/store/mod.rs`
- `features/storage-sqlite-single-file.md`, `docs/operations.md`
- SQLite: [How to corrupt a database file](https://sqlite.org/howtocorrupt.html) §2.1
  (file copied while a transaction is in progress), [WAL](https://sqlite.org/wal.html)
