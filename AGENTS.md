# AGENTS.md — working agreement for this repository

Binding for anyone, human or agent, changing `yk-dist-manager`. A change that
violates it is not done, however well it compiles.

The tool holds an inventory of security tokens, the identity of the people who
carry them, and the record of what was applied to each key. Getting it wrong does
not produce a bug report — it produces an audit finding.

The numbered sections §1–§8 are the rules, and are referenced by number from
`Cargo.toml`, the `Makefile`, `build.rs`, `roadmap.md` and the feature files.
The unnumbered sections before them are navigation: where things are, and the
cheapest command that answers each question.

**One crate**, not a workspace: lib `yk_dist_manager` + bin `yk-dist-manager`.
`cargo -p` buys nothing here; the units you target are **a module, a feature set
and a test binary**.

---

## Architecture map

Everything except `app` and `ui` is headless — no display, no key attached. That
is what makes the whole suite runnable on any machine.

Dependency direction is downward only: `ui` → `app` → {`bootstrap`, `template`,
`device`, `report`, `term`} → `domain` → `store` → `audit`.

| Component | Path | Owns | Depends on | Tests (`tests/`) |
|---|---|---|---|---|
| `domain` | `src/domain/` | Records and their rules: keys, holders, distribution, runs, lifecycle | — | `unit_domain`, `unit_records`, `behaviour_key_lifecycle`, `property_audit_and_escaping` |
| `store` | `src/store/mod.rs` | The SQLite file: schema, migrations, pragmas, CRUD, audit insert | `domain`, `audit` | `unit_store`, `behaviour_storage`, `behaviour_two_operators` |
| `store::cloud` | `src/store/cloud.rs` | Sync-folder single-writer lock, settle wait, conflict copies | `store` | `unit_store_cloud`, `behaviour_app_cloud_lock` |
| `store::smb` | `src/store/smb/` | Reaching an SMB share (`WNetAddConnection2W`, `NetFSMountURLSync`) | `secret` | `unit_store_smb`, `behaviour_smb_share`, `behaviour_app_smb_share`, `behaviour_app_share_dropped` |
| `store::{backup,import,presence}` | `src/store/` | Backups, import, who else has the file open | `store` | `behaviour_storage`, `unit_store` |
| `audit` | `src/audit/` | Hash-chained entries, verification, file sink | — (bottom of the chain; `store` uses it, never the reverse) | `unit_audit`, `property_audit_and_escaping` |
| `device` | `src/device/` | Hardware behind `YubiKeyBackend` / `WriteBackend`; `select` picks the transport | `domain`, `secret` | `unit_device_backends`, `unit_ykman_parse`, `behaviour_key_reset`, `behaviour_applet_state_and_refusal`, `behaviour_app_transport` |
| `device::{certificate,csr,tlv}` | `src/device/` | X.509 read/check, PKCS#10 build, BER-TLV walk — pure, no card | `domain` | `behaviour_certificate_import`, `interop_csr_san` *(ignored)* |
| `template` | `src/template/` | Templates, rendering, draft, applicability, diff, plan, Ed25519 signature | `domain` | `unit_template`, `behaviour_templates`, `behaviour_app_template_signing`, `interop_template_signing` *(ignored)* |
| `bootstrap` | `src/bootstrap/` | Pre-flight findings and the step executor | `device`, `template`, `domain` | `behaviour_executor`, `behaviour_bootstrap`, `behaviour_applet_state_and_refusal` |
| `term`, `pdf`, `receipt` | `src/term/`, `src/pdf.rs`, `src/receipt.rs` | Consignment terms in each language, and the PDF they print on | `domain` | `unit_term`, `unit_pdf`, `behaviour_terms_and_documents` |
| `report` | `src/report/` | The questions the register answers, and the export bundle | `store`, `domain` | `behaviour_reports` |
| `scan` | `src/scan/` | A serial from a barcode: image, camera, and the macOS pre-flight guard | — | `camera_guard` |
| `secret`, `password` | `src/secret.rs`, `src/password.rs` | Generated-then-wiped secrets; DB password strength and unlock throttle | — | `behaviour_app_unlock_throttle`, `unit_accessibility` |
| `settings` | `src/settings.rs` | Which database to open, and the recent ones | — | `unit_settings` |
| `logging`, `logbuf`, `status` | `src/` | The one log entry point, the copyable panel, status severity | — | `unit_logging_format`, `unit_accessibility` |
| `incident`, `san`, `envelope`, `paths`, `versioning`, `browse`, `branding`, `diagnostics` | `src/*.rs` | Small headless helpers | — | `unit_accessibility`, `behaviour_key_lifecycle` |
| `app` | `src/app.rs` (7 k lines) | State, cached views, **every mutation together with its audit entry** | all of the above | `behaviour_app_*` |
| `ui` | `src/ui/` | Painting, and only painting | `app` | none — outside the coverage gate by contract (§4) |

Reasoning, not repeated here: [`docs/architecture.md`](docs/architecture.md)
(module boundaries and why each is where it is),
[`docs/data-model.md`](docs/data-model.md) (schema),
[`docs/yubikey-reference.md`](docs/yubikey-reference.md) (what each hardware crate
can and cannot do), [`docs/gui.md`](docs/gui.md) (screens).

### Feature flags, and what each costs to build

`default = ["file-dialog", "camera", "native-device"]`.

| Flag | Pulls in | Needed when you touch |
|---|---|---|
| `native-device` = `native-piv` + `native-fido` + `native-otp` | `yubikey`, `pcsc`, `aes`, `ctap-hid-fido2`, `hidapi` | `src/device/native*`, `piv_*`, `reset`, `write` |
| `camera` ⊃ `barcode` | `nokhwa`, `rxing`, `image` | `src/scan/` |
| `file-dialog` | `rfd` | the database-chooser path |
| **`encrypted-db`** | **SQLCipher + vendored OpenSSL — the most expensive thing in this build** | only the five `#[cfg(feature = "encrypted-db")]` files: `behaviour_storage`, `behaviour_app_password_change`, `behaviour_app_password_on_a_share`, `behaviour_app_unlock_throttle`, and `store`'s key handling |

`--all-features` is the defaults plus `encrypted-db`. It is a final-validation
command, never an inner-loop one.

---

## The loop

```
pick the component from the map → read only it and its features/<feature>.md
      ↓
make the whole coherent change (batch related edits; do not compile between them)
      ↓
Level 1  cargo check --lib            seconds — is it even valid?
      ↓
Level 2  the closest test binary      cargo test --lib / --test <one>
      ↓
Level 3  once, when the change is finished
```

Investigation parallelises — read two modules at once, search two paths at once.
**Cargo does not**: one build directory, one lock. Two concurrent `cargo`
commands in this repo serialise, and the second one's wall clock includes the
first one's. Run them in sequence, cheapest first.

---

## Validation levels

Every command below names real targets in this repository — no placeholders to
substitute.

### Level 1 — while editing

```bash
cargo fmt --all                       # free
cargo check --lib                     # cheapest thing that type-checks the change
cargo check                           # lib + bin — after touching src/ui/ or src/app.rs
cargo check --test behaviour_storage  # lib + one test file, when the edit is in the test
```

Not here: `cargo check --all-targets` builds 43 integration test crates, two
examples and the bin.

### Level 2 — the component you changed

```bash
cargo test --lib                          # ~491 in-source tests in ONE binary — best value in the repo
cargo test --lib store::                  # narrower
cargo test --test unit_store              # one integration binary
cargo test --test unit_store -- upsert    # one test
cargo test --test behaviour_executor --test behaviour_bootstrap   # a component's set
cargo clippy --lib                        # lint what you touched, not every target
```

Take the binary names from the **Tests** column of the map. `cargo test` links one
binary per file in `tests/`; naming them is the whole saving.

### Level 3 — before calling the change done

```bash
cargo test                                                 # 43 binaries, default features
cargo clippy --all-targets --all-features -- -D warnings   # must be warning-free
cargo test --all-features                                  # adds the five encrypted-db files
make coverage-core                                         # THE GATE: core lines ≥ 80%
```

`make release-check` is exactly `fmt` + `lint` + `test-all` + `coverage-core`.
Run it once, at the end.

### Level 4 — on request, or when releasing

```bash
make check-all     # every shipped feature combination compiles
make hardware      # read-only; needs an attached YubiKey
cargo test --test interop_csr_san -- --ignored --nocapture           # needs `openssl`
cargo test --test interop_template_signing -- --ignored --nocapture  # needs `openssl`
```

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) is the authoritative
complete validation: fmt, clippy, the no-default-features build, `cargo test
--all-features`, the coverage gate, and a build on macOS / Windows / Linux. Do not
reproduce the cross-platform matrix locally.

### Keep the cache

One build directory, reused. **Never** `cargo clean` or `make clean` to "make
sure": every dependency, including bundled SQLite and vendored OpenSSL, rebuilds
from zero. Do not change `CARGO_TARGET_DIR`, and do not alternate feature sets
beyond what the work needs — each distinct set is its own set of artefacts to
build, cache and fingerprint.

---

## 1. Read before you write

**Follow the roadmap.** Work on what [`roadmap.md`](roadmap.md) says comes next.
If something else should come first, change `roadmap.md` in the same commit and
say why — do not silently reorder the plan by implementing out of turn. Unplanned
work needs a feature file before it needs code.

| Read | For |
|---|---|
| [`features/<feature>.md`](features/) | The specification of what you are touching: phases, audit events, tests owed |
| [`docs/architecture.md`](docs/architecture.md) | Module boundaries and why they are where they are |
| [`docs/security-and-compliance.md`](docs/security-and-compliance.md) | The rules that are not negotiable |
| [`roadmap.md`](roadmap.md), [`CHANGELOG.md`](CHANGELOG.md) | What is planned and what shipped — **`grep` for your section**, do not read either whole |

### Do not inspect

| Path | Why |
|---|---|
| `target/` (tens of GB), `target/llvm-cov-target/` | Build output |
| `.claude/worktrees/` | Copies of this repo; every match there is a duplicate |
| `vendor/block/` | A patched copy of a dependency — read its `README.md` before touching it, and nothing else |
| `Cargo.lock` (170 KB) | Generated |
| `CHANGELOG.md` (139 KB), `roadmap.md` (100 KB) | You must **update** both (§6), but reading either whole costs ~30 k tokens each |
| `docs/operations.md` (933 lines), `docs/gui.md` (539 lines) | Read the section you need |
| `assets/`, `packaging/`, `tests/fixtures/*.pem` | Binary, or recorded output |

---

## 2. Secure development rules

From the institutional acquisition, development and maintenance norm (NRM) and its
secure-systems guide (G-002), mapped to this codebase in
[`docs/security-and-compliance.md`](docs/security-and-compliance.md).

**Secrets**

- A PIN, PUK, management key or OTP access code **never** reaches a log, an audit
  entry, a database column, an error message, a UI label or a panic message. In
  plans, secrets exist only as `Arg::Secret` placeholders.
- Shortest possible life in memory; never written to a temporary file.
- No credential of any kind in the repository — code, tests, fixtures,
  configuration or Git history. One that ever reached a commit is an incident:
  rotate it and escalate. Deleting the file is not a fix.
- Custody of a secret is *recorded* (where it went), never *stored* (the value).

**Input and output**

- Every input has a maximum length (`domain::MAX_TEXT`, `domain::MAX_NOTE`).
- Every SQL statement is parameterised. No user value is ever formatted into a
  query string; identifiers are literals in the source, never data.
- No user string is passed through a shell. Subprocesses get an argv vector.
- Errors go to the log **and** to a visible place in the UI — never an `unwrap()`
  that takes the app down, never silently swallowed.

**Hardware**

- Prefer the native Rust crates (`yubikey`, `ctap-hid-fido2`, `hidapi`) over
  shelling out to `ykman`, which is a documented fallback for operations no crate
  covers and must be labelled as such in the plan the operator sees.
- Any operation that writes to a key is **explicit, previewed and confirmed**.
  Nothing mutates hardware as a side effect of opening a screen.
- A destructive operation (reset of an applet, overwrite of a slot) names what
  will be lost before it runs.

**Data protection**

- Personal data is limited to what the certificate and the distribution record
  need: name, corporate e-mail, unit, optional registration id.
- Do not add a personal-data field without recording it in
  `docs/security-and-compliance.md`; a new category needs the DPO's assessment,
  which is not yours to grant (§8).

---

## 3. Audit coverage — full, no exceptions

Every state change is auditable: a commit that adds a mutation adds its audit
entry.

- Mutations go through `Store`, and every mutation called from the UI is followed
  by `YkDistApp::record(...)`.
- The `audit` table refuses `UPDATE` and `DELETE` by trigger. Never remove or work
  around those triggers; never add a code path that edits history.
- Audit failure is loud: logged at `error` and surfaced in the status bar. Never
  `let _ = append_audit(...)`.
- Minimum event set: application opened, key added, key refreshed, key status
  changed, holder registered, key distributed, key returned, bootstrap planned,
  every bootstrap step outcome, template changed, backup taken, export taken.
- Entries carry *who, what, which entity, when* and a secret-free detail string.
- New feature ⇒ new events ⇒ listed in the feature file and covered by a test
  asserting the entry is written.

Logs are separate from audit: one logging entry point (`logging::init`), three
levels, the G-002 line format. Never build a log line by hand.

---

## 4. Tests

Two kinds, both required:

- **Unit** — `tests/unit_*.rs` and `#[cfg(test)]` modules next to the code. One
  behaviour per test; no hardware, no network, no clock dependence.
- **Behaviour** — `tests/behaviour_*.rs`. One scenario per test, written
  `// Given` / `// When` / `// Then`, exercising a workflow end to end through
  `Store` and the domain, the way the GUI does.

Rules:

- A bug fix **starts with a failing test** that reproduces it.
- Hardware is exercised through `device::MockBackend`. Tests never require a key
  to be plugged in, and never mutate a real key — not even an ignored one.
- Parser fixtures are recorded real output (`tests/fixtures/`), with the tool
  version noted in the test module docs.
- No test asserts on a secret value, because no secret should exist to assert on.

### Coverage: keep it above 80%

The floor is **80% line coverage of the headless core** — everything except the
egui paint code.

```bash
make coverage-core     # THE GATE: cargo llvm-cov --all-features --fail-under-lines 80,
                       # ignoring src/ui/, src/app.rs, src/main.rs, vendor/
make coverage-html     # browsable, when you need to find the gap
```

Current: **86.11%** core line coverage (85.23% region), 913 tests on the default
features and 918 with `--all-features`.

- `src/ui/`, `src/app.rs` and `src/main.rs` are excluded because painting is not
  unit tested. That exclusion is a **contract, not an amnesty**: logic belongs in
  `YkDistApp` methods or lower, where it is covered. If you cannot test something,
  it is in the wrong place — move it down rather than claiming an exemption.
- A change that drops core coverage below 80% is not ready.
- Report the number in the commit description when it moves.

Before any commit, Level 3 above: `cargo fmt --all`, `cargo clippy --all-targets
--all-features`, `cargo test --all-features`.

---

## 5. Changelog and versions

**Keep `CHANGELOG.md` current in the same commit as the change.** Format:
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); categories `Added`,
`Changed`, `Fixed`, `Removed`, `Security`. New work goes under `[Unreleased]`.

**Semantic versioning** for `Cargo.toml`/tags — while `0.y.z`, `0.MINOR` behaves
as the breaking slot:

| Change | Bump |
|---|---|
| Database schema change requiring migration; template format change; removed feature | MINOR while 0.x, MAJOR from 1.0 |
| New feature, new template step, new screen | MINOR (0.x: PATCH→MINOR at your discretion, be generous) |
| Bug fix, docs, refactor with no behaviour change | PATCH |

Release steps:

1. `CHANGELOG.md`: move `[Unreleased]` into `[x.y.z] - YYYY-MM-DD`.
2. Bump `version` in `Cargo.toml`; commit.
3. `make release` — it tags `releases/vX.Y.Z` and pushes it, which is what starts
   the release build. It refuses a dirty tree, a changelog with no section for the
   version, a tag that already exists, and a commit the remote has never seen;
   `make release-dry-run` makes every check and creates nothing. Every build
   installed anywhere comes from a tag — never from a working tree.
4. Any schema change ships with a migration and a bumped `SCHEMA_VERSION`, and a
   database written by a newer build must be refused, not silently used.

---

## 6. Tracking discipline

After finishing any feature or phase, update **all** of:

1. `CHANGELOG.md` — entry under `[Unreleased]`.
2. `roadmap.md` — status checkbox and note.
3. `features/<feature>.md` — mark the phase done, update *Current state*.
4. `docs/` — if behaviour, schema or procedure changed.

A commit that changes behaviour and touches none of these is incomplete.

---

## 7. Commits

- Imperative subject, one concern per commit.
- Say what changed and why; reference the feature file.
- Do not commit `target/`, database files, backups (`*.sqlite3`, `*.backup.*`),
  or anything containing a real serial number tied to a real person.

---

## 8. Decisions that are not yours to make

Flag these and stop; do not assume approval:

| Decision | Owner |
|---|---|
| Architecture security premises; any change to them | ESI |
| Pre-production security verification | ESI |
| New category of personal data; privacy notice; consent | DPO |
| Assessment of a system processing personal data under the organisation's control | DCI |
| Integration with a corporate system (AD, PKI, CA) | ESI |
| Retention period for audit and logs | ESI |
| Classification level of this system | ESI |

When something is blocked on one of these, write the assumption down in the
feature file, implement everything that does not depend on it, and say plainly
what is pending.
