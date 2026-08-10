# Feature: Step — OpenPGP signing subkey (alternative reading)

## Summary

The alternative interpretation of "set up the signing key with the person's e-mail,
signing ready to use": an **OpenPGP signature subkey** generated on the OpenPGP applet,
with the holder's e-mail in the key's user id.

## Motivation

"Signing key with the person's e-mail" describes two different mechanisms depending on
the ecosystem:

| | PIV / X.509 (scheduled) | OpenPGP (this spec) |
|---|---|---|
| Where | PIV applet, slot 9c | OpenPGP applet, `SIG` slot |
| Identity | Certificate with `rfc822Name` SAN | Key with a UID `Name <email>` |
| Trust | A CA chain | Web of trust, or an internal keyring |
| Signs | S/MIME mail, PDFs, Windows/macOS-integrated signing | Git commits, `gpg`-signed mail, release artefacts, files |
| Needs on the workstation | Nothing (OS smartcard support) | GnuPG installed and configured |

A team that signs Git commits and release artefacts wants the OpenPGP path. A team that
signs institutional mail and documents wants the PIV path. Both are legitimate readings
of the requirement, and the roadmap asks the question explicitly
(`roadmap.md` open question #1) rather than guessing.

This spec exists so that if the answer is OpenPGP, the work is already scoped.

## Current state

**Specified, not scheduled.** `StepKind` has no OpenPGP variant yet; adding one is the
first phase.

## Design

### The applet

The YubiKey's OpenPGP applet holds three keys: `SIG` (signature), `DEC` (decryption) and
`AUT` (authentication), plus:

- **User PIN** (default `123456`), **Admin PIN** (default `12345678`), and a reset code.
  Same rule as PIV: no factory default survives the bootstrap.
- Touch policies per key slot (firmware 5.2+), settable via `ykman openpgp keys
  set-touch`.
- Key generation **on the device** (`gpg --card-edit` → `generate`, or `ykman` for
  attestation), which is the requirement — the private key must never exist off the key.

### The identity binding

The e-mail lives in the **user id** of the OpenPGP key: `Ana Silva <ana.silva@fgv.br>`.
This is set at generation time. Consequences that differ from PIV:

- The UID is part of the key material's self-signature, so getting it wrong means
  generating again — there is no "reissue the certificate for the same key".
- Key generation over the card interface is driven by GnuPG in practice. `gpg
  --card-edit` is interactive; `--command-fd`/`--status-fd` scripting works but is
  brittle, and the alternative (implementing OpenPGP card APDUs directly) is a
  significantly larger job than the PIV path.
- The public key then has to be **published** somewhere that consumers use: an internal
  keyserver, a keyring in the repository, or attached to the holder's record here.
  Without publication, an OpenPGP signature verifies for nobody.

### Rust support

There is no equivalent of the `yubikey` crate for the OpenPGP applet in the same
maturity tier. Options:

1. Drive `gpg` as a subprocess (works, brittle, needs GnuPG installed — and GnuPG 2.5.21
   is present on the reference workstation).
2. Implement the OpenPGP card APDUs over PC/SC ourselves (`pcsc` is already a dependency
   for `native-piv`), using the OpenPGP Smart Card spec.
3. `openpgp-card` / `openpgp-card-sequoia` from the Sequoia project, which is the closest
   native option and worth evaluating first.

Option 3 first, option 1 as the fallback for generation, is the pragmatic order.

### Attestation

`ykman openpgp keys attest SIG` produces an attestation certificate proving on-device
generation, the same evidence role as PIV attestation. It should be captured for the run.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Add `StepKind::OpenPgpKeygen` / `OpenPgpPins` / `OpenPgpAttest` | Todo | only if this reading is chosen |
| 2 | Evaluate `openpgp-card-sequoia` vs `gpg` subprocess vs raw APDU | Todo | decide the transport before building |
| 3 | Change the User PIN, Admin PIN and reset code | Todo | no factory defaults |
| 4 | On-device `SIG` key generation with the holder's UID | Todo | UID is immutable once generated |
| 5 | Touch policy for the `SIG` slot | Todo | firmware 5.2+ |
| 6 | Attestation capture | Todo | evidence |
| 7 | Public-key export and publication (keyring / keyserver / record) | Todo | without this, signatures verify for nobody |
| 8 | Verification step: `gpg --card-status` equivalent, UID and fingerprint recorded | Todo | |

## Audit events

| Event | Detail |
|---|---|
| `bootstrap.step.done` | `step=openpgp-pins user_pin=changed admin_pin=changed` |
| `bootstrap.step.done` | `step=openpgp-keygen slot=SIG algorithm=… uid=<name and email> fingerprint=<hex>` |
| `openpgp.pubkey.published` | `fingerprint=… destination=…` |
| `openpgp.attestation.stored` | `slot=SIG fingerprint=…` |

The fingerprint and UID are not secrets and belong in the record — they are how a
signature is later traced back to a distributed key.

## Tests

- Unit: UID rendering from the holder record, including names with accents and commas
  (the OpenPGP UID has different escaping rules from an RFC 4514 DN — do not reuse the
  X.509 escaper).
- Behaviour (against a mock transport): PINs changed before generation; generation
  refused if a `SIG` key already exists unless explicitly overwritten.
- No test writes to real hardware; manual verification against a test key, with the
  resulting fingerprint recorded in the phase notes.

## Open questions and gates

1. **Is this the required mechanism, instead of or alongside PIV?**
   (`roadmap.md` #1.) The whole feature depends on the answer.
2. If both are wanted on the same key: the applets are independent, so it is possible —
   but it doubles the PINs the holder must manage, which the custody model must account
   for.
3. Where is the public key published, and who maintains that keyring?

## References

- `features/step-piv-signing-certificate.md` — the scheduled alternative
- [OpenPGP Smart Card spec](https://gnupg.org/ftp/specs/), [Sequoia `openpgp-card`](https://gitlab.com/openpgp-card/openpgp-card)
- `docs/yubikey-reference.md`
