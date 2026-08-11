# Feature: Testing strategy, coverage and CI

## Summary

Two required test kinds — **unit** and **behaviour** — a mock device backend so no test
ever needs (or touches) real hardware, recorded fixtures for every parser, and a floor of
**80% line coverage of the headless core**, measured with `cargo llvm-cov`.

## Motivation

The tool writes to security hardware and holds the custody record for it. Two failure
modes matter more than "the code panics": a workflow that quietly records the wrong
thing, and a change that leaks a secret into a place it can persist. Unit tests catch
the first kind of bug in isolation; behaviour tests catch a workflow that is broken as a
whole even though every part passes.

## Current state

**Suites in place, and CI now enforces the gate.**

- **545 tests** pass on the default features (`cargo test`); one fewer
  with `--all-features`, the difference being a test that only exists when
  `encrypted-db` is *off*. Plus 4 hardware tests, ignored by default.
- **CI runs on every push and pull request**
  ([`.github/workflows/ci.yml`](../.github/workflows/ci.yml)): fmt, clippy with
  `-D warnings`, the no-default-features build, the full test suite, and the
  coverage gate. A second job compiles on macOS, Windows and Linux including
  `native-device`.
- `make coverage-core` was labelled "THE GATE" but only *printed* a number and
  exited 0, so a change that dropped below the floor passed `make release-check`.
  It now passes `--fail-under-lines`, and the build agrees with `AGENTS.md` §4
  that such a change "is not ready".
- `cargo check --no-default-features` is part of the pre-commit sweep, because the
  no-camera build is a supported configuration
  (`make check-all`).
- Core line coverage **86.91%** (region 85.79%), above the 80% floor. It fell from
  88.25% when `device/native_fido.rs` joined the build: like `native.rs`, it is
  reachable only with hardware, so it counts against the gate at 0%.
- Unit suites: `unit_domain.rs` (15), `unit_template.rs` (50), `unit_audit.rs` (17),
  `unit_ykman_parse.rs` (8), `unit_store.rs` (30), `unit_device_backends.rs` (9),
  `unit_records.rs` (16), `unit_logging_format.rs` (6), `unit_term.rs` (45),
  `unit_pdf.rs` (41), `unit_settings.rs` (3), `unit_store_cloud.rs` (14 — the
  cloud-sync single-writer lock), `unit_store_smb.rs` (16 — reaching an SMB share),
  plus 146 in-module tests across `logging`, `domain`, `custody`, `document`, `scan`,
  `rxing_decoder`, `diagnostics`, `settings`, `pdf`, `store::backup`,
  `store::import` and `store::smb`.
- Behaviour suites: `behaviour_distribution.rs` (10 scenarios),
  `behaviour_bootstrap.rs` (10), `behaviour_storage.rs` (25),
  `behaviour_templates.rs` (13), `behaviour_terms_and_documents.rs` (18),
  `behaviour_executor.rs` (10 — a bootstrap run against mock write transports),
  `behaviour_smb_share.rs` (6 — a register on a share), plus the two suites that
  drive `YkDistApp` and therefore own their binary's environment, one scenario each:
  `behaviour_app_cloud_lock.rs` and `behaviour_app_smb_share.rs` (see their module
  docs).
- **Three mockable seams**, for the same reason: `device::MockBackend` means no test
  needs a key to *read*, `device::write::MockWriter` means none needs one to
  *write* — it is the only implementation of the write traits in a **default**
  build, so no test can reach hardware even by mistake, and the one that exists
  under `native-fido` is never constructed by a non-hardware test — and `store::smb::MockConnector` (behind
  `app.share_connector`) means none needs a file server, a network or a credential
  that exists anywhere.
- Two files are platform-gated and therefore only compiled where they run —
  `store/smb/macos.rs` and `store/smb/windows.rs`. The parts that can be tested
  anywhere were deliberately moved out of them (`store::smb::mounts` parses the mount
  table, `store::smb::system` holds the no-privilege refusal), and what is left is the
  FFI itself: `windows.rs` is verified by compiling it for
  `x86_64-pc-windows-msvc`, since bundled SQLite cannot cross-compile without an MSVC
  toolchain.
- The barcode decoder is tested against **rendered Code 128 barcodes** produced by
  rxing's own encoder, so the real decode path runs with no camera and no fixtures to
  go stale.
- `device::MockBackend` serves canned devices, can simulate hot-plug via
  `set_devices`, and can be made to fail on demand.
- Fixtures are real recorded `ykman` 5.9.2 output.
- `tests/hardware_native.rs` is `#[ignore]`d and **strictly read-only**: it asserts
  the native and `ykman` transports agree on the serial, that the FIDO2 applet's
  state maps coherently, and that the FIDO2 transport refuses a serial it was not
  opened for. Verified against a YubiKey 5 NFC (5.4.3) and a 5.7.4.
- `cargo clippy --all-targets --all-features` is warning-free.

## Design

### The two kinds

**Unit** (`tests/unit_*.rs`, `#[cfg(test)]` modules) — one behaviour per test, pure,
no clock dependence, no I/O beyond a temp directory. They cover validation, lifecycle
rules, rendering, planning, hashing and parsing.

**Behaviour** (`tests/behaviour_*.rs`) — one *scenario* per test, written as
`// Given` / `// When` / `// Then`, driving the same `Store` and domain calls the GUI
drives. A `World` struct provides the Given steps. Named after what an operator does:
`scenario_returning_a_key_closes_the_record_without_rewriting_history`.

This is deliberately Cucumber's structure without Cucumber's machinery: the readability
comes from the naming and the comments, and there is no `.feature`-to-Rust indirection
to maintain. If external stakeholders ever need to read the scenarios, revisit that
(Phase 6).

### Hardware policy

- **No test writes to a YubiKey.** Ever. Not even an ignored one.
- Reads against real hardware live in `tests/hardware_native.rs`, ignored by default so
  `cargo test` is deterministic on any machine.
- Write paths are tested against mock transports. Real-hardware verification of a write
  step is a **manual** procedure against a dedicated test key, and its result is
  recorded in the phase notes of the feature file — so the evidence exists even though
  it is not automated.

### Secret-leak tests

A category of its own, because it is the failure that matters most:

- `no_plan_output_can_leak_a_secret` — no rendered plan output contains a
  secret-looking literal, and every secret argument renders as `<PLACEHOLDER>`.
- `scenario_the_plan_never_shows_a_pin`.
- `a_secrets_debug_output_never_contains_its_value` — the backstop for a panic
  message, a stray `dbg!` or a mis-pointed `tracing` field.
- `the_mock_records_that_a_secret_was_supplied_and_never_which` — so the sweep
  below needs no exception for the test double itself.
- `scenario_no_secret_reaches_the_run_record_or_the_audit_trail` — the blunt
  instrument: run a whole mock bootstrap, then grep every persisted snapshot, every
  step detail and every audit entry against every value the run generated. Extends
  to the log sink once the executor is wired to the GUI.

### Coverage

```bash
make coverage-core    # the gate: headless core, floor 80%
make coverage         # whole crate, including paint code
make coverage-html    # browsable report
```

- Floor: **80% line coverage of the headless core** — everything except
  `src/ui/`, `src/app.rs` and `src/main.rs`.
- Excluding paint code is a contract, not an amnesty: logic belongs in
  `YkDistApp` methods or below, where it *is* covered. Untestable code is a
  design smell.
- Known gaps inside the core, and why: `device/native.rs` and
  `device/native_fido.rs` (0%) are reachable only with hardware, so they are
  covered by the ignored hardware tests rather than by `cargo test`; `audit/mod.rs` sits at 75% because the file-sink I/O error paths
  are not simulated yet.
- The number is reported in the commit/PR description whenever it moves.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Unit suites for domain, template, audit, parsers, store, backends, log format, custody | Done | 101 tests |
| 2 | Behaviour suites for distribution, bootstrap, storage | Done | 28 scenarios |
| 3 | Mock device backend + recorded fixtures | Done | `MockBackend`, `tests/fixtures/` |
| 4 | Ignored, read-only hardware tests | Done | verified against a real 5 NFC |
| 5 | CI: fmt + clippy + test + `llvm-cov` with the 80% gate | **Done** | [`.github/workflows/ci.yml`](../.github/workflows/ci.yml); `make coverage-core` now passes `--fail-under-lines`, so it gates instead of reporting |
| 6 | Decide on Cucumber for stakeholder-readable scenarios | Todo | only if someone outside the team needs to read them |
| 7 | Mock write transports for the executor's steps | **Done** | `device::write::MockWriter` — records that a call carried a secret, never which |
| 8 | Secret-leak sweep over every sink | **Done** for the engine | `scenario_no_secret_reaches_the_run_record_or_the_audit_trail` greps every persisted snapshot and audit entry of a full mock run against every value it generated. Extends to the log sink once the executor is wired to the GUI |
| 9 | Property tests for the audit chain and the RFC 4514 escaper | Todo | `proptest`; the escaper is exactly the kind of code that benefits |
| 10 | Cross-platform CI (macOS / Windows / Linux), including the native features | **Done** | the matrix compiles `native-device` on all three; it cannot *run* the hardware tests — see below |

## Commands

```bash
cargo test                       # unit + behaviour
cargo test --all-features
cargo test --features native-device --test hardware_native -- --ignored --nocapture
cargo clippy --all-targets --all-features
cargo fmt --all
make coverage-core               # the gate
make coverage                    # whole crate, for context
```

## Open questions and gates

- CI runner access for the native features: PC/SC on a hosted runner has no reader, so
  those tests stay `#[ignore]`d there and the matrix only proves compilation. This is
  stated in the workflow file itself rather than left for someone to discover — a green
  matrix must not be read as "the hardware transports were exercised".
- Whether a self-hosted runner with a real key is worth it for a nightly hardware run.
  It would be the only way to catch a firmware-behaviour regression automatically.

## References

- `tests/`, `src/device/mock.rs`
- `AGENTS.md` §4, `docs/development.md`
