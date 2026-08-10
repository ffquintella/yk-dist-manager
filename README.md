# yk-dist-manager

Desktop tool (Rust + [egui](https://github.com/emilk/egui)) for handing out
YubiKeys and keeping an honest record of it.

It answers two questions that a spreadsheet answers badly:

1. **Where are our keys?** Which serial went to which person, on what date,
   handed over by whom, against which signed receipt — and what was actually
   applied to that key during bootstrap.
2. **Was it set up the same way as the others?** A versioned bootstrap template
   is applied to every key, so "we always set a PIN and a signing certificate"
   becomes a procedure with evidence instead of a habit.

## What the bootstrap does

The default template (`fgv-standard`) applies, in order:

| Step | What it does |
|---|---|
| FIDO2 PIN | Sets the PIN that guards FIDO2 |
| FIDO2 minimum PIN length | Raises the floor to 6 (firmware 5.7+, skipped on older keys) |
| Forced PIN change | Marks the key so the holder must replace the transport PIN (firmware 5.7+; instructed on the hand-over term below that) |
| OTP access code | Writes the 6-byte code that write-protects the OTP slot |
| Initial FIDO2 credential | Registers a **discoverable credential resident on the key** |
| PIV PIN + PUK | Replaces both factory defaults |
| PIV management key | Random, stored on the key, guarded by the PIN — nothing to escrow |
| PIV key generation | Signing key generated **on the device**, slot 9c |
| PIV certificate request | CSR carrying `CN=<holder>` and `rfc822Name=<holder e-mail>` |
| PIV certificate import | Issued certificate written to slot 9c |
| Verification | Reads the key back and stores the end state as evidence |

Templates are data: steps can be deselected, parameters carry
`{{holder.email}}`-style variables, and a template has an id and a version that
end up in the distribution record.

**Custody of the secrets** follows model B: every PIN the operator sets is a *transport*
PIN, the holder replaces it on first use, and the tool retains nothing. Each run records the
model and whether the change was enforced by the key or instructed on the hand-over term.
See [`features/secrets-custody.md`](features/secrets-custody.md).

> **Status.** The bootstrap screen is currently **dry run only**: it builds and
> records the plan, but nothing is written to a key until the executor lands
> (Wave 1 in [`roadmap.md`](roadmap.md)). Reading a key works today.

## How it talks to the hardware

Native Rust first — no subprocess, typed errors, and no PIN on a command line:

| Applet | Crate | Transport |
|---|---|---|
| PIV | [`yubikey`](https://crates.io/crates/yubikey) | PC/SC (CCID) |
| FIDO2 / CTAP2 | [`ctap-hid-fido2`](https://crates.io/crates/ctap-hid-fido2) | USB HID |
| Yubico OTP | [`hidapi`](https://crates.io/crates/hidapi) | USB HID feature reports |

[`ykman`](https://github.com/Yubico/yubikey-manager) stays as a labelled
fallback for the operations no crate covers yet (OTP slot configuration,
management-applet metadata such as form factor). The wizard tells the operator
which transport each step will use. See
[`docs/yubikey-reference.md`](docs/yubikey-reference.md) for the capability
matrix — including the two places where `ykman` simply cannot do the job:
creating a FIDO2 credential, and putting an e-mail SAN in a CSR.

## Where the data lives

One SQLite file. That is the whole deployment.

- **Single file** — inventory, holders, distributions, bootstrap runs, templates
  and the audit trail. Copy the file and you have copied everything.
- **Network share friendly** — a file under `/Volumes/…`, `/mnt/…` or a Windows
  UNC path is detected and opened in rollback-journal mode with
  `synchronous=FULL` and a 20-second busy timeout, because WAL does not work over
  SMB/NFS.
- **Optional password** — build with `--features encrypted-db` for a
  SQLCipher-encrypted file; the app asks for the password at startup. Without a
  password it is a plain SQLite file.
- **Append-only audit** — every state change is recorded with the hash of the
  previous entry, and the table refuses `UPDATE` and `DELETE` at the database
  level, not by application convention.

## Build and run

Requires a recent stable Rust (developed on 1.96) and, for the `ykman` fallback,
`ykman` 5.x on `PATH`.

```bash
make run          # or: cargo run
make              # list every task
```

With native hardware access and an encrypted database:

```bash
cargo run --features native-device,encrypted-db
```

Pick the database file with `YKDM_DB`; otherwise the per-user data directory is
used:

```bash
YKDM_DB=/Volumes/ti-share/yubikeys/yk-dist-manager.sqlite3 cargo run
```

## Tests

```bash
cargo test                                   # 129 unit + behaviour tests
cargo clippy --all-targets --all-features    # warning-free
make coverage-core                           # 90.26% — floor is 80%
```

`make help` lists the rest.

Hardware tests are read-only and ignored by default:

```bash
cargo test --features native-device --test hardware_native -- --ignored --nocapture
```

## Documentation

| Document | Contents |
|---|---|
| [`roadmap.md`](roadmap.md) | Waves, feature status, open questions, decision log |
| [`features/`](features/) | One spec per feature: phases, audit events, tests |
| [`docs/architecture.md`](docs/architecture.md) | Module boundaries and data flow |
| [`docs/data-model.md`](docs/data-model.md) | Schema, field by field |
| [`docs/bootstrap-procedure.md`](docs/bootstrap-procedure.md) | The procedure, step by step, with the real commands |
| [`docs/yubikey-reference.md`](docs/yubikey-reference.md) | Native vs `ykman` capability matrix, firmware gates, gotchas |
| [`docs/security-and-compliance.md`](docs/security-and-compliance.md) | Secrets, personal data, audit, FGV norm mapping |
| [`docs/operations.md`](docs/operations.md) | Runbooks: distribute, return, lost key, backup |
| [`docs/development.md`](docs/development.md) | Layout, conventions, how to add a step |
| [`AGENTS.md`](AGENTS.md) | Working agreement: secure development, audit coverage, tests, changelog, semver |

## Licence

MIT — see [LICENSE](LICENSE).
