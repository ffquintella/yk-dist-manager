# AGENTS.md — working agreement for this repository

This file is the contract for anyone (human or agent) changing `yk-dist-manager`.
It is binding: a change that violates it is not done, however well it compiles.

The tool holds an inventory of security tokens, the identity of the people who
carry them, and the record of what was applied to each key. Getting it wrong
does not produce a bug report — it produces an audit finding.

---

## 1. Read before you write

| File | Why |
|---|---|
| [`roadmap.md`](roadmap.md) | What is planned, in what order, and what is already done |
| [`features/*.md`](features/) | The specification of the feature you are touching |
| [`docs/architecture.md`](docs/architecture.md) | Module boundaries and why they are where they are |
| [`docs/security-and-compliance.md`](docs/security-and-compliance.md) | The rules that are not negotiable |
| [`CHANGELOG.md`](CHANGELOG.md) | What shipped, and what is queued under `[Unreleased]` |

**Follow the roadmap.** Work on what the roadmap says comes next. If you believe
something else should come first, change `roadmap.md` in the same commit and say
why — do not silently reorder the plan by implementing out of turn. Unplanned
work needs a feature file before it needs code.

---

## 2. Secure development rules

These come from the institutional *system acquisition, development and maintenance
norm* (NRM) and its secure-systems guide (G-002), mapped to this codebase in
[`docs/security-and-compliance.md`](docs/security-and-compliance.md).

**Secrets**

- A PIN, PUK, management key or OTP access code **never** reaches a log, an
  audit entry, a database column, an error message, a UI label or a panic
  message. In plans, secrets exist only as `Arg::Secret` placeholders.
- A secret lives in memory for the shortest possible time, and is never written
  to a temporary file.
- No credential of any kind in the repository — not in code, tests, fixtures,
  configuration or Git history. A secret that ever reached a commit is an
  incident: rotate it and escalate; deleting the file is not a fix.
- Custody of a secret is *recorded* (where it went), never *stored* (the value).

**Input and output**

- Every input has a maximum length (`domain::MAX_TEXT`, `domain::MAX_NOTE`).
- Every SQL statement is parameterised. No user value is ever formatted into a
  query string. Identifiers are literals in the source, never data.
- No user string is ever passed through a shell. Subprocesses get an argv
  vector.
- Errors go to the log **and** to a visible place in the UI, never to a
  `unwrap()` that takes the app down and never silently swallowed.

**Hardware**

- Prefer the native Rust crates (`yubikey`, `ctap-hid-fido2`, `hidapi`) over
  shelling out to `ykman`. `ykman` is a documented fallback for operations no
  crate covers, and must be labelled as such in the plan the operator sees.
- Any operation that writes to a key is **explicit, previewed and confirmed**.
  Nothing mutates hardware as a side effect of opening a screen.
- A destructive operation (reset of an applet, overwrite of a slot) names what
  will be lost before it runs.

**Data protection**

- Personal data is limited to what the certificate and the distribution record
  need: name, corporate e-mail, unit, optional registration id.
- Do not add a personal-data field without recording it in
  `docs/security-and-compliance.md`; new categories of personal data need the
  DPO's assessment, which is not yours to grant.

---

## 3. Audit coverage — full, no exceptions

Every state change is auditable. Concretely, a change that adds a mutation must
add its audit entry in the same commit.

- Mutations go through `Store`, and every mutation called from the UI is
  followed by `YkDistApp::record(...)`.
- The `audit` table refuses `UPDATE` and `DELETE` by trigger. Never remove or
  work around those triggers; never add a code path that edits history.
- Audit failure is loud: it is logged at `error` and surfaced in the status bar.
  Never `let _ = append_audit(...)`.
- Minimum event set: application opened, key added, key refreshed, key status
  changed, holder registered, key distributed, key returned, bootstrap planned,
  every bootstrap step outcome, template changed, backup taken, export taken.
- Audit entries carry *who, what, which entity, when* and a secret-free detail
  string.
- New feature ⇒ new events ⇒ listed in the feature file and covered by a test
  asserting the entry is written.

Logs are separate from audit: one logging entry point (`logging::init`), three
levels, the G-002 line format. Never build a log line by hand.

---

## 4. Tests

Two kinds, both required:

- **Unit tests** — `tests/unit_*.rs` and `#[cfg(test)]` modules next to the
  code. One behaviour per test, no hardware, no network, no clock dependence.
- **Behaviour tests** — `tests/behaviour_*.rs`. One scenario per test, written
  `// Given` / `// When` / `// Then`, exercising a workflow end to end through
  `Store` and the domain, the way the GUI does.

Rules:

- A bug fix starts with a failing test that reproduces it.
- Hardware is exercised through `device::MockBackend`. Tests never require a key
  to be plugged in, and never mutate a real key.
- Fixtures for parsers are **recorded real output** (`tests/fixtures/`), with the
  tool version noted in the test module docs.
- No test asserts on a secret value, because no secret should exist to assert on.

### Coverage: keep it above 80%

The floor is **80% line coverage of the headless core** — everything except the
egui paint code. That is the measurement to keep green:

```bash
make coverage-core     # the gate
# = cargo llvm-cov --all-features --workspace --summary-only \
#     --ignore-filename-regex '(src/ui/|src/app\.rs|src/main\.rs)'

make coverage          # whole crate, including untested paint code
make coverage-html     # browsable report
```

Current: **85.34%** core line coverage (84.58% region), 812 tests.
Whole-crate line coverage is lower, the difference being the egui paint code the
gate excludes.

- `src/ui/`, `src/app.rs` and `src/main.rs` are excluded from the gate because
  painting is not unit tested. That exclusion is a *contract*, not an amnesty:
  logic belongs in `YkDistApp` methods or lower, where it is covered. If you
  cannot test something, it is in the wrong place — move it down rather than
  claiming an exemption.
- A change that drops core coverage below 80% is not ready.
- Report the number in the PR/commit description when it moves.

Before any commit:

```bash
cargo fmt --all
cargo clippy --all-targets --all-features   # must be warning-free
cargo test --all-features
```

---

## 5. Changelog and versions

**Keep `CHANGELOG.md` current in the same commit as the change.** Format:
[Keep a Changelog](https://keepachangelog.com/en/1.1.0/); categories `Added`,
`Changed`, `Fixed`, `Removed`, `Security`. New work goes under `[Unreleased]`.

**Semantic versioning** ([semver.org](https://semver.org/)) for
`Cargo.toml`/tags — while `0.y.z`, `0.MINOR` behaves as the breaking slot:

| Change | Bump |
|---|---|
| Database schema change requiring migration; template format change; removed feature | MINOR while 0.x, MAJOR from 1.0 |
| New feature, new template step, new screen | MINOR (0.x: PATCH→MINOR at your discretion, be generous) |
| Bug fix, docs, refactor with no behaviour change | PATCH |

Release steps:

1. `CHANGELOG.md`: move `[Unreleased]` into `[x.y.z] - YYYY-MM-DD`.
2. Bump `version` in `Cargo.toml`; commit.
3. Tag `vX.Y.Z`. Every build installed anywhere comes from a tag — never from a
   working tree.
4. Any schema change ships with a migration and a bumped `SCHEMA_VERSION`, and
   a database written by a newer build must be refused, not silently used.

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
