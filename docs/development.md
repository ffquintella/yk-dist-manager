# Development

## Setup

```bash
rustup update stable          # developed on 1.96, edition 2024
cargo build
cargo test
```

Optional but useful:

```bash
cargo install cargo-llvm-cov  # coverage; the floor is 80%
brew install ykman            # the fallback transport (macOS)
```

## Layout

```
src/
  main.rs        entry point: logging, eframe
  lib.rs         module map
  app.rs         YkDistApp: state, cached views, every mutation + its audit entry
  ui/            one module per screen; painting only
  domain/        records and their rules (no I/O)
  device/        YubiKeyBackend: native (PC/SC), ykman (subprocess), mock
  template/      templates, rendering, and the execution plan
  store/         the single SQLite file: schema, migrations, CRUD, audit insert
  audit/         hash chain, verification, file sink
  logging.rs     the one logging entry point
  branding.rs    the embedded window icon
assets/
  logo.svg         the only hand-drawn artwork; everything else is rendered from it
  render-icons.sh  `make icons`: PNGs, the macOS .icns, the embedded RGBA blob
  icons/           generated, and committed — see below
tests/
  unit_*.rs        one behaviour per test
  behaviour_*.rs   one Given/When/Then scenario per test
  hardware_*.rs    ignored by default; read-only
  fixtures/        recorded real tool output
features/        one spec per feature
docs/            this folder
```

Boundaries and the reasoning behind them: [architecture.md](architecture.md).

## The loop

```bash
cargo fmt --all
cargo clippy --all-targets --all-features    # must be warning-free
cargo test
cargo llvm-cov --all-features --workspace --summary-only
```

macOS bundle (needed for camera scanning, since a bare binary has no `Info.plist`):

```bash
make bundle          # assemble target/bundle/YubiKey Distribution Manager.app
make verify-bundle   # layout, plist, version, signature, and the binary's own --diagnose
make run-bundled
```

The installers ([packaging-and-release.md](../features/packaging-and-release.md) phases 3c
and 4). Each wraps an artefact that has already been built and checked, rather than building
one, so the app and the installer cannot disagree about what shipped:

```bash
make pkg             # macOS: bundle-release, then wrap it in a .pkg
make verify-pkg      # extracts the payload and runs --diagnose from inside it
```

```bat
rem Windows, from an existing release build:
powershell -File packaging\windows\msi.ps1
powershell -File packaging\windows\verify-msi.ps1

rem Does WiX still accept the authoring? Needs no build, keeps no MSI:
powershell -File packaging\windows\msi.ps1 -LinkOnly
```

`-LinkOnly` is what CI's Windows leg runs on every commit. It links `Package.wxs` against a
placeholder in place of the binary and deletes the result, which answers the one question a
reader of the file cannot — does every reference resolve? — in seconds and without a release
build. Two versions were spent learning that the alternative is finding out from a tag.

`verify-msi.ps1` installs the package for real, checks what landed (including the Start Menu
shortcut's target), interrogates the installed binary and uninstalls — so it needs an
elevated shell. Without one it runs the static checks and warns; under
`YKDM_VERIFY_RELEASE=1` a skip is a failure, which is why CI is where this always runs in
full. WiX 6 is installed on first use as a .NET global tool, pinned — set `YKDM_WIX_VERSION`
to move the pin.

Changing the icon ([application-icon.md](../features/application-icon.md)):

```bash
make icons           # renders assets/logo.svg to every size, and checks the blob
```

`assets/logo.svg` is the only artwork edited by hand. Everything the script writes
is generated *and* committed, because `make bundle` has to produce an icon on a
machine with no rasteriser and `include_bytes!` needs the blob at compile time.
Edit the SVG, run the target, commit both — a stale blob fails
`src/branding.rs`'s tests, not the launch. The script needs
`brew install librsvg imagemagick`.

Feature combinations that must keep compiling:

```bash
cargo check                                  # default: native transports + camera
cargo check --no-default-features --features file-dialog,camera   # the ykman-only build
cargo check --no-default-features --features file-dialog          # no camera either
cargo check --features encrypted-db          # builds vendored OpenSSL; slow
cargo check --all-features
```

Hardware check (read-only, needs a key):

```bash
cargo test --test hardware_native -- --ignored --nocapture
```

## Conventions

- **Comments explain why, not what.** The surrounding code sets the density; match it.
- **Errors are typed** (`thiserror`) with messages an operator can act on: name the file, the
  field, the flag to enable. No `unwrap()` on anything that can fail in the field.
- **No I/O in a paint closure.** Read from the cached vectors; mutate in an `app` method.
- **Every mutation is followed by its audit entry**, in the same method.
- **No secret in a field that can be logged, stored or rendered.** Plans carry
  `Arg::Secret` labels.
- **Parameterised SQL only.** Identifiers are source literals, never data.
- **Length-bound every input** (`MAX_TEXT` / `MAX_NOTE`) and set `char_limit` on the widget.

## How to add a bootstrap step

1. **Write the spec first**: `features/step-<name>.md`, with phases, audit events and tests.
   Add the row to `roadmap.md`.
2. `StepKind` in [`src/domain/bootstrap.rs`](../src/domain/bootstrap.rs): add the variant, its
   `label()`, its `slug()` (the default step id), its place in `ALL` — which is the order the
   Templates screen offers — and whether it `sets_secret()`.
2b. `TemplateStep::for_kind()` in [`src/template/mod.rs`](../src/template/mod.rs): the
   description and **the parameters this kind reads**, with the values the standard procedure
   uses. This is what "Add a step" inserts, and
   `a_step_added_by_hand_carries_the_parameters_its_kind_reads` asserts that a freshly added
   step of every kind plans — so a kind whose parameters are only known to `plan_step` fails
   the suite rather than surprising an operator.
3. `native_op()` in [`src/template/plan.rs`](../src/template/plan.rs): declare the native call,
   the crate, the feature flag, and honestly whether it is `available`.
4. `plan_step()`: build the `ykman` fallback argv (secrets as `Arg::Secret`) and the note
   explaining any caveat.
5. Add the step to a template in [`src/template/mod.rs`](../src/template/mod.rs) with its
   parameters, and document them in the spec and in
   [bootstrap-procedure.md](bootstrap-procedure.md).
6. Tests: the plan renders, no secret leaks, a missing parameter is reported, the transport is
   correctly labelled. Then the behaviour scenario for the workflow.
7. Executor implementation (Wave 1) against a mock write trait. **No test writes to real
   hardware**; verify manually against a dedicated test key and record it in the spec's phase
   notes.
8. `CHANGELOG.md`, `roadmap.md`, the feature file, and the docs — all four, same commit.

## How to add a record type or a field

1. `domain/` — the struct and its validation. No I/O.
2. `store/` — the column in a **new** `SCHEMA_V2` block, a bumped `SCHEMA_VERSION`, and the
   migration. Never edit `SCHEMA_V1`: existing files were created with it.
3. Row decoding: add the column to the `row_to_*` function and keep the index order matching
   the `SELECT`.
4. If it is personal data: update [data-model.md](data-model.md) §Personal data summary and
   [security-and-compliance.md](security-and-compliance.md). A new *category* needs the DPO.
5. Audit event for its mutation.
6. Unit test for the validation, behaviour test for the workflow, and a round-trip through the
   store.

## How to add a screen

1. `src/ui/<name>.rs` with `pub fn show(app: &mut YkDistApp, ui: &mut egui::Ui)`.
2. Register it in `ui/mod.rs`, add the `Tab` variant and its label, and dispatch it in
   `YkDistApp::ui`.
3. Keep the logic in `app` methods. If you cannot test something, it is in the wrong place.
4. Use `ui::screen_header` and `ui::error_label` so screens look and behave alike.
5. In tables, use the deferred-mutation pattern: record an intent in the loop, apply it after
   the closure.

## Test conventions

- **Unit** (`tests/unit_*.rs`): one behaviour per test, pure, named for the behaviour —
  `unknown_variable_is_an_error_not_an_empty_string`.
- **Behaviour** (`tests/behaviour_*.rs`): one scenario per test, `// Given` / `// When` /
  `// Then`, named for what an operator does —
  `scenario_returning_a_key_closes_the_record_without_rewriting_history`.
- Databases in tests are `Store::open_in_memory()` or a `tempfile::tempdir()`.
- Hardware is `device::MockBackend`. Fixtures are **recorded real output** with the tool
  version noted in the module docs.
- A bug fix starts with a failing test.

Details and the coverage policy: [`../features/testing-strategy.md`](../features/testing-strategy.md).

## Before you commit

Read [`../AGENTS.md`](../AGENTS.md). The short version: follow the roadmap; update
`CHANGELOG.md`, `roadmap.md`, the feature file and the docs in the same commit; keep clippy
clean and coverage above 80%; never leave a mutation without an audit entry; and never let a
secret near anything that persists.
