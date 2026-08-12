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

**Wiring in place, hardening pending.**

- `StoreConfig::password: Option<String>`; `with_password` treats an empty string
  as "no password" so a blank prompt cannot create an unopenable file.
- `apply_key` runs `PRAGMA key` as the first statement (SQLCipher requires this).
- Without the feature, asking for a password returns
  `StoreError::EncryptionUnavailable`, whose message names the flag to rebuild
  with — no silent fallback to an unencrypted file.
- A wrong password surfaces as SQLite "file is not a database"
  (`SQLITE_NOTADB`/`SQLITE_CORRUPT`), which the store translates into
  `StoreError::PasswordRequired`.
- `src/ui/unlock.rs` prompts at startup, clears the field immediately after use,
  and never stores the password beyond the open call.
- The app tries an unencrypted open first, so plain files open with no prompt.

Not yet done: KDF parameters, password change / re-key, cipher migration, and a
key-derivation review.

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
| 3 | Unlock attempt throttling + audit of failures | 0 | **Core done** | [`password::Throttle`](../src/password.rs) — three free attempts then a doubling delay capped at 30s. Deliberately **not** a lockout: there is no administrator to unlock a shared register, so one would be a denial of service anybody with the file could trigger. Wiring into the unlock screen is pending |
| 4 | Explicit KDF parameters (`PRAGMA kdf_iter`, cipher page size) | — | Todo | needs the ESI-approved cipher/parameter set |
| 5 | "Encrypt an existing plain database" migration | 0 | **Done** | the same operation as phase 2 — a plain source is just one with no key. Backup taken first, audited as `db.encrypted` |
| 6 | Password strength meter + policy | 0 | **Core done** | [`password::assess`](../src/password.rs) — a 12-character floor with advice rather than mandatory character classes, because the threat is an offline attack on a copied file and composition rules push people towards `Password1!`. Meter wiring is pending |
| 7 | Optional: unlock with a YubiKey instead of a typed password | 1 | Todo | HMAC-SHA1 challenge-response (OTP slot 2) as the KDF input — depends on `native-otp` |

Phase 7 is the interesting one: the tool distributes YubiKeys, so using one to
open its own database is coherent and removes the shared-password problem.

## Audit events

| Event | When |
|---|---|
| `db.unlocked` | Successful open of an encrypted file |
| `db.unlock.failed` | Wrong password (no password material in the entry) |
| `db.password.changed` | Phase 2, after a verified swap |
| `db.encrypted` | Phase 5, a plain file was converted |

Note the ordering problem: a failed unlock cannot be written to the database it
failed to open. Those events go to the log, and to the audit mirror when one is
configured (`features/audit-trail.md`).

## Tests

- `scenario_opening_a_plain_file_without_encryption_support_needs_no_password`.
- `scenario_asking_for_a_password_without_the_feature_says_so` — the error names
  `encrypted-db`, so the operator knows what to do.
- Phase 1 completion needs, under `--features encrypted-db`: create with a
  password → reopen with it → reopen without it fails as `PasswordRequired` →
  reopen with a wrong one fails the same way (no distinguishable error).
- Phase 2: change the password, then assert the old one fails, the new one works,
  and the audit chain still verifies.

## Open questions and gates

- **Cipher and KDF parameters must be the set the ESI approves**; do not invent
  them. Until then Phase 4 stays open and the defaults are SQLCipher's.
- **Password custody**: who holds the database password, and how is it recovered
  if the operator leaves? Answering "it is not recoverable" is acceptable only if
  the backup story is explicit.
- Interaction with backups: an encrypted backup needs the same password; a
  rotation plan must not orphan old backups.

## References

- `src/store/mod.rs` (`apply_key`, `is_encryption_error`), `src/ui/unlock.rs`
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
