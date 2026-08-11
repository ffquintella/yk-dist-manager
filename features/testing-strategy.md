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

**Suites in place; CI and the coverage gate are not.**

- **259 tests** pass on the default features (`cargo test`); 258 with
  `--all-features`, the difference being one test that only exists when
  `encrypted-db` is *off*. Plus 2 hardware tests, ignored by default.
- `cargo check --no-default-features` is part of the pre-commit sweep, because the
  no-camera build is a supported configuration
  (`make check-all`).
- Core line coverage **87.07%** (region 84.45%), above the 80% floor.
- Unit suites: `unit_domain.rs`, `unit_template.rs`, `unit_audit.rs`,
  `unit_ykman_parse.rs`, `unit_store.rs` (29), `unit_device_backends.rs`,
  `unit_records.rs`, `unit_logging_format.rs`, `unit_term.rs` (30),
  `unit_settings.rs` (3), plus in-module tests for `logging`, `domain`, `custody`,
  `document` (7), `scan` (13) and `rxing_decoder` (5).
- Behaviour suites: `behaviour_distribution.rs` (10 scenarios),
  `behaviour_bootstrap.rs` (10), `behaviour_storage.rs` (11),
  `behaviour_terms_and_documents.rs` (15).
- The barcode decoder is tested against **rendered Code 128 barcodes** produced by
  rxing's own encoder, so the real decode path runs with no camera and no fixtures to
  go stale.
- `device::MockBackend` serves canned devices, can simulate hot-plug via
  `set_devices`, and can be made to fail on demand.
- Fixtures are real recorded `ykman` 5.9.2 output.
- `tests/hardware_native.rs` is `#[ignore]`d, read-only, and asserts the native and
  `ykman` transports agree.
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
- Planned (with `features/secrets-custody.md` Phase 3): run a mock bootstrap with known
  generated secrets, then grep every persisted record, the log sink and the audit sink
  for those values and fail if any appears.

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
- Known gaps inside the core, and why: `device/native.rs` (0%) is reachable only
  with hardware, so it is covered by the ignored hardware tests rather than by
  `cargo test`; `audit/mod.rs` sits at 75% because the file-sink I/O error paths
  are not simulated yet.
- The number is reported in the commit/PR description whenever it moves.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Unit suites for domain, template, audit, parsers, store, backends, log format, custody | Done | 101 tests |
| 2 | Behaviour suites for distribution, bootstrap, storage | Done | 28 scenarios |
| 3 | Mock device backend + recorded fixtures | Done | `MockBackend`, `tests/fixtures/` |
| 4 | Ignored, read-only hardware tests | Done | verified against a real 5 NFC |
| 5 | CI: fmt + clippy + test + `llvm-cov` with the 80% gate | Todo | GitHub Actions; fail the build under the floor |
| 6 | Decide on Cucumber for stakeholder-readable scenarios | Todo | only if someone outside the team needs to read them |
| 7 | Mock write transports for the executor's steps | Todo | prerequisite for Wave 1 tests |
| 8 | Secret-leak sweep over every sink | Todo | with `features/secrets-custody.md` |
| 9 | Property tests for the audit chain and the RFC 4514 escaper | Todo | `proptest`; the escaper is exactly the kind of code that benefits |
| 10 | Cross-platform CI (macOS / Windows / Linux), including the native features | Todo | PC/SC and HID behave differently per platform |

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
  those tests stay `#[ignore]`d there and the matrix only proves compilation.
- Whether a self-hosted runner with a real key is worth it for a nightly hardware run.
  It would be the only way to catch a firmware-behaviour regression automatically.

## References

- `tests/`, `src/device/mock.rs`
- `AGENTS.md` §4, `docs/development.md`
