# Changelog

All notable changes to this project are documented here.

Format: [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).
Versioning: [Semantic Versioning](https://semver.org/). While the version is
`0.y.z`, the MINOR slot carries breaking changes.

<!--
Maintenance instructions (see AGENTS.md §5):
* Every behaviour change adds an entry under [Unreleased], in the right category:
  Added / Changed / Fixed / Removed / Security.
* On release: move [Unreleased] into a dated version section, bump the version in
  Cargo.toml, commit, and tag vX.Y.Z. Nothing is installed anywhere except from a tag.
* A database schema change also bumps store::SCHEMA_VERSION and ships a migration.
-->

## [Unreleased]

### Added

- Project foundation: `yk-dist-manager` replaces the IronRoot desktop template.
- **Domain records** for the whole distribution question: YubiKey inventory
  (serial, model, firmware, form factor, FIPS flag, enabled applications),
  holders (name, corporate e-mail, unit, registration), distribution events
  (date, operator who handed the key over, delivery method, receipt reference,
  return) and bootstrap runs (template id and version, per-step outcome,
  custody note).
- **Guarded key lifecycle**: `InStock → Bootstrapped → Distributed → Returned →
  Retired`, with illegal transitions refused by the store rather than applied.
- **Native hardware transport** (`native-piv`/`native-fido`/`native-otp`
  features): the `yubikey` crate reads serial and firmware over PC/SC. Verified
  against a real YubiKey 5 NFC (firmware 5.4.3); the reading agrees with `ykman`.
- **`ykman` fallback backend** with argv-only invocation, typed errors, and
  parsers for `ykman list --serials` and `ykman info` unit-tested against output
  recorded from ykman 5.9.2.
- **Bootstrap templates**: versioned, declarative, with `{{holder.email}}`-style
  rendering, structural validation, and two built-ins (`fgv-standard`,
  `fido-only`).
- **Bootstrap planner**: renders a template plus a holder into an execution plan
  where every step declares its transport (native / `ykman` fallback / manual)
  and every secret is a placeholder, so a plan can be shown, logged and stored
  without carrying a PIN.
- **Single-file SQLite storage** (`rusqlite`, bundled): schema v1 with
  `user_version` migrations, WAL on local disk, rollback journal +
  `synchronous=FULL` + 20s busy timeout when the file is on a network share,
  `VACUUM INTO` backup and `PRAGMA integrity_check`.
- **Optional database password** via SQLCipher behind the `encrypted-db`
  feature, with an unlock screen and a clear error when the build lacks it.
- **Hash-chained audit trail** stored in the database, with `UPDATE` and `DELETE`
  refused by `BEFORE` triggers, plus chain verification from the GUI and a
  standalone append-only file sink.
- **Single logging entry point** emitting the G-002 line format
  (`[dd/mm/aaaa] hh:mm:ss ; evento ; detalhes`) with three levels.
- **egui GUI** with six screens (Inventory, Holders, Distribution, Bootstrap,
  Audit, Settings) plus the unlock screen, on eframe 0.36.
- **Test suite**: 119 unit and behaviour tests (89.70% line coverage of the
  headless core), recorded `ykman` fixtures, a mock device backend, and
  read-only hardware tests that are ignored by default.
- **Documentation set**: `roadmap.md`, 31 feature specs under `features/`, and
  `docs/` covering architecture, data model, bootstrap procedure, YubiKey
  reference, security and compliance, operations and development.
- **Working agreement**: `AGENTS.md` (and `CLAUDE.md` pointing at it) with secure
  development rules, audit-coverage requirements, an 80% coverage floor, and
  changelog and semantic-versioning discipline.
- **`Makefile`** with the checks that must pass before a release
  (`make release-check`).

### Security

- Secrets are modelled as `Arg::Secret` placeholders and never rendered into a
  command string, log line, audit entry or database column; a test asserts no
  plan output can leak one.
- Personal data is limited to name, corporate e-mail, unit and an optional
  registration id, with every field length-bounded on entry.
- Nothing in this release writes to a YubiKey. The bootstrap screen is dry-run
  only until the executor and the custody model land (Wave 1).

[Unreleased]: https://github.com/ffquintella/yk-dist-manager/compare/cdea137...HEAD
