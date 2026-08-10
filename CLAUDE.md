# CLAUDE.md

This file is read at the start of every session.

**See [`AGENTS.md`](AGENTS.md) and follow the instructions there.** It is the
binding working agreement: secure-development rules, audit coverage, test and
coverage requirements, changelog and semantic-versioning discipline, and the
decisions that need a human owner.

## Project in one paragraph

`yk-dist-manager` is a Rust + egui desktop tool that tracks YubiKey distribution
(which key went to whom, when, by whom, and what was applied) and applies a
templated bootstrap procedure to each key: a PIN for FIDO2, an access code for
the OTP slots, the initial FIDO2 credential resident on the key, and a PIV
signing certificate carrying the holder's e-mail so the key is signing-ready at
hand-over. Hardware is driven by native Rust crates (`yubikey`,
`ctap-hid-fido2`, `hidapi`) with `ykman` as a labelled fallback. All data lives
in one SQLite file that can sit on a network share and can optionally be
password-protected.

## Key files

| File | Purpose |
|---|---|
| [`AGENTS.md`](AGENTS.md) | **Full instructions — read first.** |
| [`roadmap.md`](roadmap.md) | Wave plan, feature status, open questions, decision log. Update after every phase. |
| [`features/*.md`](features/) | One spec per feature, with phases, audit events and tests. |
| [`CHANGELOG.md`](CHANGELOG.md) | Keep-a-Changelog; update in the same commit as the change. |
| [`docs/`](docs/) | Architecture, data model, bootstrap procedure, YubiKey reference, security and compliance, operations, development. |

## Tracking rules (short form)

After finishing any feature or phase, update **all four**:

1. `CHANGELOG.md` — under `[Unreleased]`, correct category.
2. `roadmap.md` — status checkbox and note.
3. `features/<feature>.md` — phase state and *Current state*.
4. `docs/` — whenever behaviour, schema or procedure changed.

## Commands

```bash
cargo fmt --all
cargo clippy --all-targets --all-features   # must be warning-free
cargo test                                  # unit + behaviour suites
cargo test --all-features
make coverage-core                          # keep core line coverage ≥ 80%
cargo run                                   # launch the GUI
make help                                   # every task, including the coverage gate
```

Hardware tests are ignored by default and are read-only:

```bash
cargo test --features native-device --test hardware_native -- --ignored --nocapture
```

## Hard rules you will be held to

- **No secret anywhere it can persist**: not in logs, audit entries, database
  columns, error messages, UI labels or temporary files.
- **Full audit coverage**: every state change writes an audit entry; a failure to
  audit is logged at `error` and shown to the operator, never ignored.
- **Never write to a real key** from a test, or as a side effect of opening a
  screen. Hardware writes are explicit, previewed and confirmed.
- **Follow the roadmap.** Out-of-turn work needs the roadmap updated in the same
  commit, with the reason.
- **Coverage ≥ 80%**, unit *and* behaviour tests, bug fixes start with a failing
  test.
