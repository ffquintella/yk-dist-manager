# Feature: Optional database password (encryption at rest)

## Summary

The database file can optionally be protected by a password. With the
`encrypted-db` feature the file is SQLCipher-encrypted and `PRAGMA key` is applied
before any other statement; without a password it is a plain SQLite file.

## Motivation

The file holds an inventory of security tokens and the names, e-mails and units of
the people carrying them. On a network share, the file's confidentiality is
whatever the share's ACL provides — which is usually "the whole unit", and
sometimes "anyone who can browse the file server". A password makes the file
useless on its own, so a copied or backed-up file is not a personal-data leak.

It is **optional** because encryption has a real cost: lose the password and the
data is gone, and there is no recovery. A pilot with three keys should not be
forced into key management; a production dataset on a share should be.

## Current state

**Done for Wave 0.** Every phase carrying wave 0 is built, wired and tested; what
is left is phase 4, which is blocked on the ESI's cipher and KDF parameter set,
and phase 7, which is Wave 1 work behind `native-otp`.

- `StoreConfig::password: Option<String>`; `with_password` treats an empty string
  as "no password" so a blank prompt cannot create an unopenable file.
- `apply_key` runs `PRAGMA key` as the first statement (SQLCipher requires this).
- Without the feature, asking for a password returns
  `StoreError::EncryptionUnavailable`, whose message names the flag to rebuild
  with — no silent fallback to an unencrypted file.
- A wrong password surfaces as SQLite "file is not a database"
  (`SQLITE_NOTADB`/`SQLITE_CORRUPT`), which the store translates into
  `StoreError::PasswordRequired`. `StoreError::is_wrong_password` is the single
  place that distinguishes "the password was wrong" from every other reason a
  register will not open, and it is what the throttle counts.
- The chooser (`src/ui/database.rs`) prompts, clears the field immediately after
  use, and never stores the password beyond the open call.
- The app tries an unencrypted open first, so plain files open with no prompt —
  and that probe is deliberately **not** counted as a failed attempt.
- The **policy is enforced where a password is chosen**, not only displayed:
  `Store::create_new` and `Store::change_password` both refuse one below the
  floor, so the meter agrees with the refusal instead of advising against it.
- The **throttle is applied in `YkDistApp::handle_db_request`**, so every route to
  an unlock goes through it, and the disabled buttons in the chooser are the
  courtesy rather than the control.
- **Settings → Password protection** sets, changes or removes the password, with
  the meter, the password typed twice, and a confirmation of its own for removal.

## Design

### Cargo feature

```toml
encrypted-db = ["rusqlite/bundled-sqlcipher-vendored-openssl"]
```

Vendored OpenSSL is deliberate: it makes the encrypted build reproducible instead
of depending on whichever OpenSSL a workstation has. The cost is build time, which
is why it is off by default.

### Password handling rules

- Held in memory only for the duration of `Store::open`.
- Never logged, never in an audit entry, never in an error message, never in a
  window title, never written to a file.
- The UI field is `TextEdit::password(true)` and is cleared after submission.
- Failure to unlock is rate-limited (Phase 3) so the prompt is not a fast oracle.

### What SQLCipher does and does not give us

| Gives | Does not give |
|---|---|
| Whole-file encryption, including the audit table and indexes | Protection while the app is open |
| A copied/backed-up file is unreadable without the password | Per-operator access control (everyone shares one password) |
| Page-level HMAC integrity | Protection against a compromised workstation |

Per-operator credentials are a different feature
(`features/operator-auth-and-roles.md`). The database password is a
confidentiality-at-rest control, and the documentation must not oversell it.

### Password change (Phase 2)

SQLCipher's `PRAGMA rekey` re-encrypts in place, which on a network share is the
riskiest operation in the tool. Design: `VACUUM INTO` a new encrypted file with
the new key, verify it opens and its audit chain verifies, then swap — never
`rekey` a share-hosted file in place.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Feature flag, `PRAGMA key`, unlock screen, typed errors | 0 | Done | plain files still open with no prompt |
| 2 | Password change via re-encrypt-and-swap (not in-place `rekey`) | 0 | **Done** | `Store::change_password` — backup, audit into the source, `sqlcipher_export`, verify the copy on its own connection, then swap. Never `PRAGMA rekey` |
| 3 | Unlock attempt throttling + audit of failures | 0 | **Done** | [`password::Throttle`](../src/password.rs) — three free attempts then a doubling delay capped at 30s, counted down on screen. Deliberately **not** a lockout: there is no administrator to unlock a shared register, so one would be a denial of service anybody with the file could trigger. Enforced in [`handle_db_request`](../src/app.rs), not by the disabled button; the failure goes to the log as `db.unlock.failed` with a count and nothing else |
| 4 | Explicit KDF parameters (`PRAGMA kdf_iter`, cipher page size) | — | Todo | **blocked on the ESI-approved cipher/parameter set** — the one thing here that is not the implementer's to decide (`AGENTS.md` §8). Until then the defaults are SQLCipher's |
| 5 | "Encrypt an existing plain database" migration | 0 | **Done** | the same operation as phase 2 — a plain source is just one with no key. Backup taken first, audited as `db.encrypted`, and reachable from **Settings → Password protection** |
| 6 | Password strength meter + policy | 0 | **Done** | [`password::assess`](../src/password.rs) — a 12-character floor with advice rather than mandatory character classes, because the threat is an offline attack on a copied file and composition rules push people towards `Password1!`. The meter is [`ui::password_meter`](../src/ui/mod.rs), shown wherever a password is *chosen*; the floor is enforced by `create_new` and `change_password` rather than by the screen |
| 7 | Optional: unlock with a YubiKey instead of a typed password | 1 | Todo — **blocked twice over** | HMAC-SHA1 challenge-response (OTP slot 2) as the KDF input. It needs an OTP slot to be *programmed*, which is the one write `features/step-otp-access-code.md` phase 4 and 5 deliberately leave unwritten until there is a key to verify the frame against; and turning a challenge-response into a database key **is** a KDF choice, which is the ESI's to approve (phase 4 above, `AGENTS.md` §8). Neither half is an implementer's decision, so this is the one Wave 1 row that is not merely unverified — and it is marked *optional* in its own title, so nothing depends on it |

Phase 7 is the interesting one: the tool distributes YubiKeys, so using one to
open its own database is coherent and removes the shared-password problem.

## Audit events

| Event | When |
|---|---|
| `db.unlocked` | Successful open of an encrypted file — written for an *open*, never for a create, because creating a register is not unlocking one |
| `db.unlock.failed` | Wrong password. `consecutive_failures=N` and nothing else: no password, and not even its length |
| `db.password.changed` | Phase 2, after a verified swap — and also when a password is *removed*, because that is the same change |
| `db.encrypted` | Phase 5, a plain file was converted |

Note the ordering problem: a failed unlock cannot be written to the database it
failed to open. Those events go to the log, and to the audit mirror when one is
configured (`features/audit-trail.md`) — the application does not configure one
today, so in a default deployment a refused unlock is a log line.

`db.password.changed` and `db.encrypted` are written **into the source**, before
the export, so the exported copy carries the entry: written afterwards they would
land in a file about to be replaced.

## Tests

- `scenario_opening_a_plain_file_without_encryption_support_needs_no_password`.
- `scenario_asking_for_a_password_without_the_feature_says_so` — the error names
  `encrypted-db`, so the operator knows what to do.
- Phase 1 completion needs, under `--features encrypted-db`: create with a
  password → reopen with it → reopen without it fails as `PasswordRequired` →
  reopen with a wrong one fails the same way (no distinguishable error).
- Phase 2: change the password, then assert the old one fails, the new one works,
  and the audit chain still verifies.
- Phase 3, at the prompt rather than in the type:
  [`behaviour_app_unlock_throttle.rs`](../tests/behaviour_app_unlock_throttle.rs)
  — the startup probe is not an attempt; the free attempts are free; the next one
  earns a wait; the *correct* password submitted during that wait is refused
  without being tried, and without the field being emptied; and it opens once the
  wait has run out, clearing the count and writing `db.unlocked`. The
  `not(encrypted-db)` half of that file pins the other side of the rule: a build
  that cannot encrypt refuses a password six times over and earns no delay,
  because that is a rebuild rather than a guess.
- Phase 6, at both ends: `create_new` refuses a password below the floor **and
  creates no file** while doing it
  ([`behaviour_storage.rs`](../tests/behaviour_storage.rs)), and every step of the
  meter has its own words
  ([`unit_accessibility.rs`](../tests/unit_accessibility.rs)) so that a bar of
  colour is never the only thing distinguishing "refused" from "weak".
- Phases 2, 5 and 6 through the application:
  [`behaviour_app_password_change.rs`](../tests/behaviour_app_password_change.rs)
  — plain → encrypted → changed → plain, with the register still open and complete
  at every step, the old password dead, the audit chain unbroken across three
  exports, and a backup of the register as it stood before the first one. It also
  pins the two refusals that must happen **before** the store is consumed (a weak
  password, and two fields that disagree), because a refusal reached after that
  point would close the register in order to say no.

## Open questions and gates

- **Cipher and KDF parameters must be the set the ESI approves**; do not invent
  them. Until then Phase 4 stays open and the defaults are SQLCipher's. It is
  marked **—** in the Wave column for that reason: a wave does not wait on somebody
  else's decision.
- **Password custody**: who holds the database password, and how is it recovered
  if the operator leaves? Answering "it is not recoverable" is acceptable only if
  the backup story is explicit. What the application can now do about it is change
  the password when the person who knew it leaves — an export-and-swap from
  Settings, audited, with the old file backed up first.
- Interaction with backups: an encrypted backup needs the same password; a
  rotation plan must not orphan old backups. **The screen says so** where the
  operator can act on it, and it is worth restating here: a password change does
  not reach copies already taken, so after one the older backups need the password
  they were taken with. Scheduled backups are named by the second they were taken
  in, and one taken for a re-key is skipped when a copy of that second already
  exists (`features/storage-sqlite-single-file.md` phase 6) — which is why a second
  password change inside the same second reuses the first change's copy. Not worth
  a special name for a case that only a test produces.

## References

- `src/store/mod.rs` (`apply_key`, `is_encryption_error`, `create_new`,
  `change_password`, `StoreError::is_wrong_password`), `src/password.rs`
- `src/app.rs` (`handle_db_request`, `change_database_password`,
  `note_unlock_failure`), `src/ui/database.rs`, `src/ui/settings.rs`,
  `src/ui/mod.rs` (`password_meter`)
- `features/storage-sqlite-single-file.md`, `docs/security-and-compliance.md`
- [SQLCipher design](https://www.zetetic.net/sqlcipher/design/)

## Why the policy is a length floor and not a composition rule (2026-08-11)

Worth writing down, because it looks lax next to the usual advice.

This password is not a login. There is no account to lock out, no reset e-mail
and no administrator; losing it loses the data. And it is a **file** password —
the threat is a copied file (a backup on a share, a sync client's conflict copy,
a stolen laptop), where nobody is typing guesses at a prompt. They are running a
cracker against the file at whatever rate the hardware allows.

Against that, **length is the only thing that buys time**, and a composition rule
reliably produces `Password1!` — short, decorated, and in every wordlist. So:

* a floor of **12 characters**, counted in characters rather than bytes;
* character classes as *advice*, worth at most one step of the meter;
* refusals only for the things that are weak at any length — a repeated
  character, and a monotone alphabet or keyboard run.

The meter's reach is stated rather than implied: it has no dictionary and no
period detector, so `123456789012345` scores Weak rather than being refused, and
a test says so. Adding a strength library would mean carrying a dependency that
needs keeping current forever for one text field.

Throttling exists to make scripted guessing *at the prompt* pointless, not to
pretend it affects an offline attack: three free attempts, then a doubling delay
capped at thirty seconds so a mistyped password never looks like a hung
application. It is **not** a lockout, and the message never implies one — there
is no administrator to lift it, so a lockout on a shared register would be a
denial of service anybody holding the file could trigger.

## Changing the password is an export-and-swap, never an in-place re-key

`Store::change_password` covers phases 2 and 5 with one operation, because they
*are* one: export the whole database into a new file under a different key, prove
the copy is good, and only then swap. A plain source is simply one with no key,
and removing a password is an export with an empty one.

SQLCipher's `PRAGMA rekey` re-encrypts in place, and on a share that is the
riskiest thing this tool could do: it rewrites every page of a file a sync client
may be copying and another workstation may be about to open, with no intermediate
state that is a valid database. An interruption half-way leaves a file that is
neither the old key nor the new one.

The order is load-bearing:

1. **Check the policy first.** Doing the backup and the export only to refuse
   would be a lot of work to arrive at "no", and would leave a stray file.
2. **Back up.** The one operation that rewrites the whole register is the one
   that most needs a copy of where it started. The copy is readable with the
   *old* password.
3. **Audit into the source**, before the export, so the export carries the entry.
   Written afterwards it would land in a file about to be replaced.
4. **Export**, then carry `user_version` across by hand — `sqlcipher_export`
   copies schema and rows but not pragmas, and a copy arriving at version 0
   would be re-migrated from scratch on the next open.
5. **Verify on a separate connection**: it opens under the new password, schema
   version matches, `integrity_check` passes, and the audit chain verifies. The
   question is "will this open when it is reopened", and the handle that wrote it
   cannot answer that. A copy failing any check is deleted; the original is
   untouched.
6. **Swap**, closing the connection first (Windows will not replace an open
   file), keeping the original under `.replaced` until the new file is in place,
   and putting it back if the second rename fails — no register at all is worse
   than a failed password change.

The method consumes the `Store`, because afterwards the handle points at a file
that is no longer the register.

### What consuming the `Store` means for the screen (2026-08-12)

That signature is right for the operation and awkward for the caller, and the
awkwardness is where the bugs would be, so
[`YkDistApp::change_database_password`](../src/app.rs) is shaped around it:

* **Every refusal that can be reached at all is reached before the store is
  taken** — no `encrypted-db`, a read-only session, no password to remove, a
  mismatch between the two fields, a password below the floor. Otherwise the
  application would close the register in order to explain why it would not
  change its password, which is a strange thing to do to somebody mid-hand-over.
* **On success it reopens** at the returned path with the new password. That is
  also the proof the new password works, done by the code rather than discovered
  by the operator tomorrow morning. It reopens through `reopen_current` and
  **not** `open_database`, which matters for a register on a share: the latter
  begins by releasing what is open, and releasing disconnects a share this session
  connected — on a local file nothing notices, on a share it takes the file away
  before the reopen, so a password change that worked reports as one that failed.
  `reopen_current` also reuses the config the register was opened with, so the
  location stays as it was *stated* rather than re-detected from a mount point.
  Pinned by
  [`behaviour_app_password_on_a_share.rs`](../tests/behaviour_app_password_on_a_share.rs).
* **On failure it goes back to the chooser** with the reason. The register itself
  is fine — that is what the export-and-swap order guarantees — but this session
  has no handle on it and never kept the old password, so the honest state is
  "locked, here is what happened" rather than a screen pretending to be open.

## Where the meter and the throttle appear, and where they deliberately do not

Both are wired in exactly one place each, and neither lives in paint code:

* **The policy** is enforced by the store (`create_new`, `change_password`). The
  meter is advice with the same source of truth, so it cannot promise something
  the save will refuse.
* **The throttle** is enforced in `handle_db_request`, which every route to an
  unlock passes through — a click, the recent list, a share, a lock taken over.
  The chooser also disables those buttons while a wait runs, which is a courtesy
  to the operator and not the mechanism; a control that existed only in the paint
  pass would be one keyboard shortcut away from being bypassed, and could not be
  tested.

The meter is shown only where a password is being **chosen**: the Settings card,
and the chooser's field when the path names a file that does not exist yet. It is
*not* shown when unlocking an existing register — grading a password the operator
already has tells them nothing they can act on, and the number it would put on
screen is a judgement about a file this application is not being asked to change.
