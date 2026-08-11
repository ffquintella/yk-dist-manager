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

### Changed

- **The screens are fluid.** Every card, banner, form and table now spans the
  window width, with one gutter (18px) shared by the top bar, the screen body and
  the status bar — so the product name, the screen heading and the status pill sit
  on the same left margin, and two screens no longer disagree about how wide a
  card is. Previously a card was as wide as whatever happened to be inside it: the
  Inventory table was full width, the Holders form stopped at two 340px columns,
  and the Distribution card at three 360px selects.

  - Form fields take the width of their column instead of a constant, so they grow
    with the window. Two-column forms (Holders, Distribution, Bootstrap,
    Settings → Operator) split the page evenly through `ui::form_columns`.
  - A table wider than the window scrolls sideways **inside its own card**
    (`ui::table`), instead of widening the page and leaving every other card
    narrower than the one that overflowed. The body itself now scrolls vertically
    only. Table spacing and header style are shared, so the seven tables read as
    one table.
  - New shared building blocks in `src/ui/mod.rs`: `card`, `titled_card`, `table`,
    `form_columns`, and the `GUTTER` constant. A refusal (`ui::error_label`) is
    full width and wraps rather than running off the screen.

## [0.3.0] - 2026-08-10

### Added

- **A theme, and a picker for it.** The GUI now draws itself with
  [`egui-elegance`](https://github.com/stephenberry/egui-elegance) 0.15 (egui 0.36):
  a real palette, consistent typography, cards, badges, tinted callouts, styled
  inputs and selects, and a tab bar with an underlined active tab. All eight
  screens were restyled, the database chooser included.

  - Four palettes ship — **Slate** (the default, dark), Charcoal, Frost and Paper.
    The choice lives in `settings.json` next to the operator identity, is
    cosmetic only, and an unknown or hand-edited name resolves to Slate rather
    than failing to open. Picked from *Settings → Operator → Theme*.
  - A key's lifecycle state, a plan's transport (`native` / `ykman` / `manual`)
    and a hand-over's term count now read as coloured badges rather than as text
    the operator has to parse.
  - The status bar tells routine outcomes apart from refusals and from an audit
    failure, which is painted in the danger colour and bold — AGENTS.md §3 asks
    for an audit failure to be loud, and now it looks it.
  - `domain::clamp_text` keeps NRM §5.3.5's input bound: the elegance inputs have
    no `char_limit`, so `ui::capped_input` / `capped_area` apply the cap by
    character (not byte) immediately after each field is painted.
  - Refusals stay **selectable** — `ui::error_label` reproduces the callout's
    tinted-danger frame around a real `egui::Label` rather than using
    `elegance::Callout`, whose body text cannot be copied into a ticket.

- **A place to edit the consignment term.** New **Terms** screen: pick the language,
  edit the title and body, and save. The wording of the term is institutional text
  somebody else owns, so it has to be editable by the unit that owns it — until now it
  could only be changed by editing the source.

  - Saving stores a **new version** rather than overwriting:
    `Store::save_term_template_version` numbers it one past the highest version on
    record for that `(id, language)`, so the wording a holder already signed stays
    byte-for-byte in the database, and two workstations editing the same term cannot
    both write "version 2".
  - `term::choose_template` now hands out the **newest** version of a language (and
    numerically, so `10` follows `9`), which is what makes an edit apply to the next
    term generated.
  - A draft is checked before it is stored (`TermTemplate::check`): id, language,
    title, body, the length bounds, and every `{{variable}}` known to `TermContext`.
    An unknown variable is refused at the desk instead of at the counter.
  - **Preview** renders the draft against `TermContext::sample()` — fictitious values,
    all of them filled, so the preview shows every line the real document can print.
  - *Restore built-in wording* brings back the text the build ships, *Reload stored
    version* discards an edit, and switching language with unsaved changes is refused.
  - A unit can add a language it needs (`es`, `fr-FR`, …) from the same screen.
  - New audit events `term.template_edited` and `term.template_added`, carrying the id,
    language, the new version and the version it came from.
  - The distribution screen's term panel now names the template version it rendered
    from, and offers *Edit wording…* straight into the new screen.

  Closes phase 8 of [`features/consignment-terms.md`](features/consignment-terms.md).

- **macOS application bundle.** `packaging/macos/` assembles
  `YubiKey Distribution Manager.app` — an `Info.plist` template with
  `NSCameraUsageDescription`, the version taken from `Cargo.toml`, an optional
  `icon.icns`, plist linting and code signing (ad-hoc by default,
  `--sign 'Developer ID Application: …'` for a real identity, `--dmg` to wrap it).
  No bundling crate: the layout is a few directories and one plist, so a reviewable
  script beats a dependency that can go unmaintained.

  This is what unblocks camera scanning on macOS. Verified end to end:
  `make verify-bundle` checks the layout, the plist, the version against
  `Cargo.toml` and the signature, then asks the bundled binary itself whether macOS
  sees a bundle — the camera verdict moves from "not running from an .app bundle" to
  "not yet authorised", which the permission prompt resolves.

  `make bundle`, `bundle-release`, `verify-bundle`, `run-bundled`, `dmg`.

- **`--diagnose`** (plus `--version` and `--help`): version, compiled features,
  bundle state and `Info.plist` path, camera authorisation and the cameras found,
  the database and settings paths, and whether `ykman` is on `PATH` — ending in a
  one-line verdict on whether camera scanning will work and why not if it will not.
  It opens no database, touches no key and starts no camera, so it is safe to run
  anywhere. `make diagnose`.

### Fixed

- **A database in a cloud-sync folder was opened in WAL mode.** Found by reading a
  `--diagnose` report from a real installation, where the file lived under
  `~/Library/CloudStorage/OneDrive-…/`. That is the most dangerous place for a SQLite
  database: the sync client copies the file while a writer holds it open, WAL's
  `-wal`/`-shm` sidecars are synchronised independently of the database, and a
  conflict is resolved by keeping **both** files rather than merging — so the failure
  mode is two divergent registers of who holds which key.

  `looks_like_cloud_sync()` now recognises OneDrive, Dropbox, Google Drive, iCloud
  (`Mobile Documents`), pCloud and macOS's `CloudStorage` File Provider directory.
  Such a path is classified with the network shares, so the journal mode is at least
  the conservative one, and the operator is warned in the status line, on the
  Settings screen and in `--diagnose`. Safer pragmas reduce the risk; they do not
  remove it, and the docs say so.

## [0.2.2] - 2026-08-10

### Fixed

- **Starting the camera aborted the whole application** on an unbundled macOS
  build (`cargo run`, or the binary straight from `target/`):

  ```text
  thread 'camera-scan' panicked at core/src/panicking.rs:225:5:
  panic in a function that cannot unwind
  thread caused non-unwinding panic. aborting.
  ```

  Two causes, both now handled. `nokhwa_initialize()` — which nokhwa's own
  documentation says is the caller's responsibility "before anything else" on
  macOS — was never called; it now runs in `main`, on the main thread, so the
  permission prompt appears while the operator is present. And a bare binary has no
  `Info.plist`, so nothing declares `NSCameraUsageDescription`: AVFoundation raises
  an Objective-C exception which crosses an `extern "C"` boundary as a
  *non-unwinding* panic. `catch_unwind` cannot recover from that, so the only fix is
  not to make the call.

  `scan::preflight` is that guard: it refuses before any capture backend is touched,
  with a message that names the cause and the alternative (a USB barcode reader needs
  no camera). `YKDM_ALLOW_UNBUNDLED_CAMERA=1` forces an attempt for anyone who has
  arranged access another way — documented as "may abort", because it may.

  `tests/camera_guard.rs` calls the exact entry point the button calls, so a
  regression aborts a test run rather than an operator's session.

- Camera opening now tries three format requests (highest frame rate, highest
  resolution, then whatever the device offers) instead of failing when a device has
  no match for the first.

### Changed

- **Camera scanning (`camera`) is now a default feature**, so a stock
  `cargo build` includes barcode decoding and live capture. `barcode` comes with
  it. A build without any camera code is
  `--no-default-features --features file-dialog`.

  Two obligations move from "opt-in" to "every build" as a result, and neither is
  resolved yet:

  1. A macOS bundle **must** carry `NSCameraUsageDescription` in its `Info.plist`,
     or the operating system terminates the app the first time it opens the camera.
  2. `nokhwa`'s macOS bindings pull **`block` 0.1.6**, which cargo reports as
     future-incompatible (`static of uninhabited type`; it will be a hard error in a
     later rustc). NRM §5.4.3 forbids shipping components without maintainer
     support, so this is now a **release blocker** rather than a note on an optional
     path — see `features/serial-scanning.md` and
     `features/packaging-and-release.md`.

## [0.2.1] - 2026-08-10

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
- **Test suite**: 129 unit and behaviour tests (line coverage of the headless
  core above the 80% floor), recorded `ykman` fixtures, a mock device backend,
  and read-only hardware tests that are ignored by default.
- **Documentation set**: `roadmap.md`, 31 feature specs under `features/`, and
  `docs/` covering architecture, data model, bootstrap procedure, YubiKey
  reference, security and compliance, operations and development.
- **Working agreement**: `AGENTS.md` (and `CLAUDE.md` pointing at it) with secure
  development rules, audit-coverage requirements, an 80% coverage floor, and
  changelog and semantic-versioning discipline.
- **`Makefile`** with the checks that must pass before a release
  (`make release-check`).

### Added (choosing the database, intake, and paperwork)

- **Choose or create the database file** from inside the application
  ([spec](features/database-selection.md)): a chooser screen with the recently used
  databases (each marked reachable or not), a typed path for UNC and share paths,
  native *Choose file… / New file…* dialogs behind the default `file-dialog`
  feature, and *Switch database…* in Settings.
- **`Store::open_existing` and `Store::create_new`** replace guessing: opening a
  path that does not exist is an error, and creating over an existing file is
  refused. Previously a mistyped share path silently created an empty database,
  which looked exactly like every record having vanished.
- **`settings.json`** in the per-user data directory remembers the last database, up
  to 8 recent ones, and the operator identity. It is written atomically, tolerates
  corruption by falling back to defaults, and **never contains a password**.
- **Read a serial from a barcode** ([spec](features/serial-scanning.md)): a typed
  field that a USB barcode scanner types into (no features needed, and the
  recommended path), plus camera capture via `nokhwa` and decoding via `rxing`
  behind the `camera` / `barcode` features. Ambiguity is refused rather than
  guessed — two different serials in one frame is an error.
- **Serial provenance** (`SerialSource`: device / scanned-label / manual-entry) on
  every inventory record, so a serial from a box label is never mistaken for one
  read from the hardware. Provenance only ever improves: a device read upgrades a
  scanned record and a later scan never downgrades a verified one.
- **Consignment terms** ([spec](features/consignment-terms.md)): multilingual
  templates keyed `(id, language, version)` with **pt-BR** and **en** built in,
  rendered from the records — holder name, identification number, key serial, what
  the bootstrap applied, the custody statement. A line whose placeholder resolves to
  empty is omitted, so optional fields need no conditional syntax and no term prints
  a stray `Phone:`. Language selection falls back (exact → base language → default)
  and reports what it used.
- **Optional holder fields**: identification number (CPF or the local equivalent —
  named for what it is, not for one country's document), phone and address. A
  re-registration fills them in and never blanks them.
- **Upload the signed term** ([spec](features/signed-term-documents.md)): the scan is
  filed in the database with a SHA-256, validated first (non-empty, ≤ 8 MiB, scanner
  formats only, filename stripped of any path), listed without its bytes, and
  verified before every export. A per-hand-over badge shows `none filed` or
  `n filed`.
- **Schema v2 and v3** with migrations: `keys.serial_source`; the optional holder
  fields, `term_templates` and `documents`. A test builds a v1 database by hand and
  asserts the chain carries it forward without touching the rows.

### Changed

- **Custody model decided: B — transport secret plus forced change.** Every PIN
  the operator sets is a transport PIN; the holder replaces it on first use and
  the tool retains nothing. `domain::CustodyModel` fixes the stored vocabulary
  (`transport-pin+forced-change`, `holder-set`, `escrowed:<reference>`,
  `no-secret-set`) and `domain::ChangeEnforcement` records whether the key
  enforced the change (`forcePINChange`, firmware 5.7+) or the hand-over term
  merely instructed it — PIV has no force-change flag at any firmware level, so
  there it is always procedural.
- **The signing credential is a PIV slot 9c X.509 certificate** with the holder's
  e-mail in `rfc822Name`. The OpenPGP signature-subkey alternative stays
  specified in `features/step-openpgp-signing-subkey.md` but unscheduled.
- `device::ykman::supports_ctap21_config` names the firmware 5.7 gate once;
  `supports_min_pin_length` delegates to it, so the same fact is not re-derived
  per step.

### Added (custody model B)

- `StepKind::Fido2ForcePinChange` and a `fido2-force-pin-change` step in both
  built-in templates, planned as `ykman fido access force-change` with the
  firmware gate and the procedural fallback stated on the step.
- `Makefile`: `make` alone lists every target; `make run` launches the GUI,
  `make run-native` launches it with native hardware access and password
  support, and `make coverage-core` is the coverage gate.

### Security

- Secrets are modelled as `Arg::Secret` placeholders and never rendered into a
  command string, log line, audit entry or database column; a test asserts no
  plan output can leak one.
- Personal data is limited to name, corporate e-mail, unit and an optional
  registration id, with every field length-bounded on entry.
- Nothing in this release writes to a YubiKey. The bootstrap screen is dry-run
  only until the executor lands (Wave 1).
- A signed term and an identification number are personal data now held **inside**
  the database. That is a deliberate trade for keeping the evidence with the record,
  and it is a direct argument for enabling `encrypted-db`; the consequence is stated
  in `docs/security-and-compliance.md` rather than left implicit.
- Uploaded filenames are treated as data: any directory component is stripped, so a
  name like `../../etc/passwd.pdf` cannot escape.

[Unreleased]: https://github.com/ffquintella/yk-dist-manager/compare/v0.3.0...HEAD
[0.3.0]: https://github.com/ffquintella/yk-dist-manager/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/ffquintella/yk-dist-manager/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/ffquintella/yk-dist-manager/compare/cdea137...v0.2.1
