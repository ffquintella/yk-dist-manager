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

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Feature flag, `PRAGMA key`, unlock screen, typed errors | Done | plain files still open with no prompt |
| 2 | Password change via re-encrypt-and-swap (not in-place `rekey`) | Todo | verify before swap; audit the event |
| 3 | Unlock attempt throttling + audit of failures | Todo | 3 fails → delay, per AGENTS.md |
| 4 | Explicit KDF parameters (`PRAGMA kdf_iter`, cipher page size) | Todo | needs the ESI-approved cipher/parameter set |
| 5 | "Encrypt an existing plain database" migration | Todo | one-way, confirmed, backup taken first |
| 6 | Password strength meter + policy | Todo | reuse the FGV password guidance |
| 7 | Optional: unlock with a YubiKey instead of a typed password | Todo | HMAC-SHA1 challenge-response (OTP slot 2) as the KDF input — depends on `native-otp` |

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
