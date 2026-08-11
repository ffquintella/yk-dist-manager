# yk-dist-manager — Roadmap

Single entry point for planning. One row per tracked feature; the detail lives in
the linked spec under [`features/`](features/).

**What this tool is.** A desktop application (Rust + egui) that does two things
for a unit handing out YubiKeys:

1. **Tracks distribution** — which key (serial, model, firmware) went to which
   person, on what date, handed over by whom, against which receipt, and exactly
   what was applied to it during bootstrap.
2. **Bootstraps keys from a template** — a versioned, declarative procedure that
   sets a PIN for FIDO2 and an access code for OTP, registers the initial FIDO2
   credential resident *on the key*, and puts a PIV signing certificate carrying
   the holder's e-mail on the key so it is signing-ready on hand-over.

**How it talks to hardware.** Native Rust first: [`yubikey`](https://crates.io/crates/yubikey)
for PIV over PC/SC, [`ctap-hid-fido2`](https://crates.io/crates/ctap-hid-fido2)
for FIDO2/CTAP2 over HID, [`hidapi`](https://crates.io/crates/hidapi) for the
Yubico OTP slots. `ykman` remains a labelled fallback for what no crate covers.

**Where data lives.** One SQLite file, optionally password-protected
(SQLCipher), able to sit on a network share. Copying that file copies the whole
deployment.

---

## At a glance

| State | Count |
|---|---|
| Done | 9 |
| In progress | 9 |
| Todo | 20 |
| **Total tracked items** | **38** (across 35 specs — two items share a spec) |

Released: **v0.4.0**. Current wave: **Wave 1 — native execution.**

**Out of turn in v0.4.0** (AGENTS.md §1 asks for the reason, in this file, in the same
commit): the Inventory screen gained an **observation** per key and a **confirmed
removal** of a registered key, both requested by the operator who will run the tool.
Neither belongs to Wave 1, and both were cheap and self-contained — the `notes` column
has existed since schema v1 with nothing in the GUI to fill it, and removal needed one
store method with its refusal. They are recorded as Phase 9 of
[`features/key-inventory.md`](features/key-inventory.md), with no schema change. What
made them worth taking early: a register nobody can correct gets worked around in a
spreadsheet, and the workaround is the thing an audit finds.

Wave 0 (foundation) is in place, and v0.2.1–v0.2.2 add the paperwork and intake half
of the job: choosing or creating the database file, recording serials from a barcode,
generating the consignment term in the holder's language, and filing the signed copy
against the hand-over — and, from the Terms screen, editing that term's wording per
language. 258 tests pass (plus 2 read-only hardware tests), with 87.07% line coverage of
the headless core.

Camera scanning is a default feature, and on macOS it needs the bundled application:
an unbundled build refuses with an explanation rather than aborting (v0.2.2). Two
release blockers stand in front of any distributed artefact — the
`NSCameraUsageDescription` bundle entry and the future-incompatible `block` 0.1.6 in
`nokhwa`'s macOS bindings. See
[`features/serial-scanning.md`](features/serial-scanning.md).

## How to read this

- **Status** is a checkbox plus a word: `[x]` Done, `[/]` In progress, `[ ]` Todo.
- A feature is Done only when every phase in its spec is done. Mixed ⇒ `[/]`.
- Waves are ordered. Work the current wave; if something must jump the queue,
  edit this file in the same commit and say why (see [AGENTS.md](AGENTS.md)).
- Every feature file carries its own phase table, audit events and test list.

---

## Wave 0 — Foundation

Everything needed before a single byte is written to a key.

| Status | Feature | Notes |
|---|---|---|
| `[/]` | Native device transport | [spec](features/native-device-transport.md) — `yubikey` over PC/SC reads serial + firmware from a real key today (verified against 5 NFC / fw 5.4.3, agrees with `ykman`). FIDO2 and OTP transports are Wave 1. |
| `[x]` | `ykman` fallback + parsers | [spec](features/ykman-fallback.md) — argv-only subprocess, typed errors, parsers unit-tested against recorded output of ykman 5.9.2. |
| `[/]` | Device detection | [spec](features/device-detection.md) — read-on-demand works; hot-plug polling and multi-key selection pending. |
| `[/]` | Single-file SQLite storage | [spec](features/storage-sqlite-single-file.md) — schema **v3**, `user_version` migrations (v1→v3 tested), WAL locally / rollback journal on a share, `VACUUM INTO` backup, `integrity_check`. |
| `[x]` | Choosing / creating the database file | [spec](features/database-selection.md) — strict `open_existing` vs `create_new` (a typo can no longer create an empty database), recent-database list, native dialogs, switch from Settings. |
| `[ ]` | Optional database password | [spec](features/db-password-and-encryption.md) — `encrypted-db` feature wires `PRAGMA key`; unlock screen exists. KDF parameters, password change and re-key are Todo. |
| `[x]` | Logging | [spec](features/logging.md) — one entry point, three levels, G-002 line format, no hand-built log lines. |
| `[/]` | Audit trail | [spec](features/audit-trail.md) — SHA-256 chain, `UPDATE`/`DELETE` refused by trigger, chain verification in the GUI. Segregated storage still open (see gates). |
| `[x]` | Key inventory | [spec](features/key-inventory.md) — serial, model, firmware, form factor, FIPS flag, applications, lifecycle with guarded transitions, serial provenance (verified / scanned / typed), an editable **observation** per key, and confirmed **removal** of an intake mistake (refused once a hand-over or a run refers to the serial). |
| `[x]` | Serial from a barcode | [spec](features/serial-scanning.md) — camera decoding via `rxing` + `nokhwa`, a USB-wedge/typed path that needs no features, and provenance that only ever improves. |
| `[x]` | Holder registry | [spec](features/holder-registry.md) — minimal personal data, validated e-mail, RFC 4514 subject derivation, plus optional identification number, phone and address. |
| `[x]` | Distribution records | [spec](features/distribution-records.md) — hand-over, operator, delivery method, receipt reference, linked bootstrap run, return without rewriting history. |
| `[/]` | Bootstrap templates | [spec](features/bootstrap-templates.md) — versioned templates, `{{variable}}` rendering, two built-ins, validation. A GUI editor and template signing are pending. |
| `[/]` | Bootstrap planner | [spec](features/bootstrap-engine.md) — plan with per-step transport (native / ykman / manual) and secret placeholders; dry runs recorded. **The executor is Wave 1.** |
| `[/]` | GUI shell | [spec](features/gui-shell.md) — seven screens, unlock screen, status bar, egui 0.36 `App::ui`, themed with `egui-elegance` (four palettes, the choice persisted) and laid out fluidly (one gutter, full-width cards, columns that split the page, tables that contain their own overflow). Search, keyboard flow and window-state persistence still open. |
| `[/]` | Bootstrap wizard | [spec](features/gui-bootstrap-wizard.md) — selection, per-step opt-out, plan review, dry run. Execution progress view pending. |
| `[/]` | Testing strategy | [spec](features/testing-strategy.md) — 258 tests across unit + behaviour suites, mock backend, recorded fixtures, ignored hardware tests; 87.07% core line coverage. The gate is not yet enforced in CI. |

### Paperwork

| Status | Feature | Notes |
|---|---|---|
| `[x]` | Consignment terms | [spec](features/consignment-terms.md) — multilingual templates keyed `(id, language, version)`, pt-BR + en built in, optional fields that omit their own line, generated from the record. A **Terms screen** edits the wording and adds languages: saving stores a new version, and terms are generated from the newest. The **wording needs its owner's review**, which the editor is what makes possible. |
| `[x]` | Signed-term upload | [spec](features/signed-term-documents.md) — the scan is filed in the database with a SHA-256, verified on export, with a per-hand-over "none filed" badge. |
| `[ ]` | Receipts & terms (PDF, signature tracking) | [spec](features/receipts-and-terms.md) — the broader spec: PDF output, a signature state machine, return receipts, batch generation. |

## Wave 1 — Execute the bootstrap, natively

The point of the tool: apply the template to a key, safely, with evidence.

| Status | Feature | Notes |
|---|---|---|
| `[ ]` | Bootstrap executor | [spec](features/bootstrap-engine.md) — run the plan step by step with per-step results, abort-on-required-failure, idempotency, resume, and no secret in any record. |
| `[ ]` | Step: FIDO2 PIN | [spec](features/step-fido2-pin.md) — set/change the PIN over CTAP2, minimum length policy (fw 5.7+), retry accounting, and `forcePINChange` so the holder must replace the transport PIN (custody model B). |
| `[ ]` | Step: initial FIDO2 credential | [spec](features/step-fido2-credentials.md) — `authenticatorMakeCredential` with `rk=true` so the credential is resident on the key. `ykman` cannot do this at all. |
| `[ ]` | Step: OTP slot access code | [spec](features/step-otp-access-code.md) — the 6-byte code that write-protects a slot, plus optional slot programming. Needs the HID config frame (no crate covers it). |
| `[ ]` | Step: PIV PIN / PUK / management key | [spec](features/step-piv-pin-puk-management-key.md) — leave no factory default; prefer a PIN-protected random management key so nothing needs custody. |
| `[ ]` | Step: PIV signing certificate | [spec](features/step-piv-signing-certificate.md) — on-device key in slot 9c, CSR **with `rfc822Name` SAN**, issued certificate imported, attestation stored. The SAN is why this step goes native. |
| `[ ]` | CA integration | [spec](features/ca-integration.md) — internal CA for pilots, BastionVault PKI, and an enterprise CA profile; SAN and EKU requirements per option. |
| `[/]` | Secrets custody | [spec](features/secrets-custody.md) — **model B decided (2026-08-10)**: transport secret + forced change, nothing retained. `CustodyModel` fixes the vocabulary in code and the standard template carries the forced-change step; the prompt / generate / show-once / zeroise machinery is still Todo. |

## Wave 2 — Operations at scale

| Status | Feature | Notes |
|---|---|---|
| `[ ]` | Key lifecycle & revocation | [spec](features/key-lifecycle-and-revocation.md) — lost/stolen handling, certificate revocation, applet reset, re-issue to a new holder. |
| `[ ]` | Reports & export | [spec](features/reports-and-export.md) — inventory and distribution reports, CSV/JSON export, audit export for the ESI. |
| `[ ]` | Bulk enrolment | [spec](features/bulk-enrollment.md) — queue of keys for one template, batch progress, per-key evidence. |
| `[ ]` | Operator authentication & roles | [spec](features/operator-auth-and-roles.md) — operator identity is currently `$USER`, which is not authentication. Roles (admin / distributor / auditor), AD integration, MFA with a YubiKey on sensitive operations. |
| `[ ]` | Multi-operator concurrency | [spec](features/storage-sqlite-single-file.md) — optimistic concurrency and busy-retry policy for a shared file on a share. |

## Wave 3 — Alternatives and delivery

| Status | Feature | Notes |
|---|---|---|
| `[ ]` | OpenPGP signing subkey | [spec](features/step-openpgp-signing-subkey.md) — **not the chosen mechanism** (PIV 9c is, decided 2026-08-10). Kept specified for a unit that signs Git commits or `gpg` mail; unscheduled. |
| `[ ]` | SSH authentication via PIV | [spec](features/ssh-authentication.md) — slot 9a plus PKCS#11 for SSH, for units that want it. |
| `[/]` | Packaging & release | [spec](features/packaging-and-release.md) — the **macOS bundle is done** (`make bundle` / `verify-bundle` / `dmg`), which is what makes camera scanning work there. Developer ID signing + notarisation, Windows, Linux and CI remain, as does the `block` 0.1.6 resolution. |
| `[ ]` | FGV compliance artefacts | [spec](features/fgv-compliance.md) — classification proposal, system registration, data documentation, change/homologation records. |
| `[ ]` | CI & coverage gate | [spec](features/testing-strategy.md) — fmt + clippy + tests + `cargo llvm-cov` with an 80% floor, enforced on every push. |

---

## Engineering rules that apply to every wave

Full text in [AGENTS.md](AGENTS.md). The short version:

- **Semantic versioning** and a tag for every installed build. Schema changes bump
  `SCHEMA_VERSION` and ship a migration.
- **`CHANGELOG.md` in the same commit** as the change, Keep-a-Changelog format.
- **Coverage ≥ 80%** line coverage of the headless core (`make coverage-core`).
- **Unit *and* behaviour tests** for every feature; bug fixes start with a failing test.
- **Full audit coverage**: no state change without an audit entry; audit failure is loud.
- **No secret** in a log, an audit entry, a database column, an error or the UI.
- Native Rust for hardware; `ykman` only where nothing else exists, and labelled
  as a fallback in the plan the operator sees.

---

## Open questions

Decisions that change what gets built, and are not the implementer's to make.

1. **Which CA issues the signing certificate?** Internal CA, BastionVault PKI, or
   an enterprise CA. This decides whether we build the CSR ourselves (needed for
   the SAN) or rely on a CA profile to inject it. *Blocks the certificate step
   going live.*
2. **Where does the audit trail live?** The norm wants audit storage segregated
   from operational data; the requirement here is a *single file*. Current design:
   audit in the same file, immutable by trigger, plus an optional append-only
   mirror elsewhere. This needs ESI sign-off.
3. **Retention** of audit entries and logs — not fixed by the norm; ESI decides.
4. **Classification level** of the system. Proposed: level 3 (see
   `docs/security-and-compliance.md`). ESI validates.
5. **The term's wording.** The built-in pt-BR and en consignment terms are a
   complete, plausible draft — including the undertaking and an LGPD paragraph — but
   institutional text is not the implementer's to write. It needs review by whoever
   owns the term, and the data-protection paragraph needs the DPO. Templates are data
   precisely so that review is an edit, not a code change.
6. **The PUK under model B.** The transport PIN is handed over and changed by the
   holder; the PUK has no force-change mechanism. Default taken: hand the PUK to
   the holder in the same sealed envelope and retain nothing, which means a
   blocked PIN with a lost PUK costs an applet reset (and a new certificate).
   Retaining the PUK instead would be escrow, with a store to protect. Confirm.
7. **The OTP access code under model B.** The holder never needs it, so the
   default taken is generate-and-discard: the slot is deliberately frozen, and
   reprogramming it later requires an OTP applet reset. Confirm, or switch the
   template to put the code in the envelope.

### Answered

- **"Adjust the SSK signing certificate" — which mechanism?** *(2026-08-10)*
  **PIV slot 9c**, X.509 with `rfc822Name` = the holder's e-mail. The OpenPGP
  signature-subkey reading stays specified in
  `features/step-openpgp-signing-subkey.md` but unscheduled.
- **Custody model for the secrets a bootstrap sets.** *(2026-08-10)* **Model B —
  transport secret plus forced change.** The operator sets a temporary PIN, the
  key is marked so the holder must change it before first use, and this tool
  retains nothing. See `features/secrets-custody.md`; the two sub-decisions it
  leaves open are items 5 and 6 above.

## Decision log

| Date | Decision | Rationale |
|---|---|---|
| 2026-08-10 | Native Rust crates are the primary hardware transport; `ykman` is a fallback | Typed errors, no PATH dependency, no PIN on a command line, and the only way to create a FIDO2 credential or put a SAN in a CSR |
| 2026-08-10 | One SQLite file, `bundled`, optional SQLCipher password | A shared file on a unit share is the deployment model; nothing to install, one file to back up |
| 2026-08-10 | Rollback journal (not WAL) when the file is on a share | WAL needs shared memory and does not work over SMB/NFS |
| 2026-08-10 | Audit immutability by database trigger, not by application code | The norm requires the guarantee to come from the database |
| 2026-08-10 | egui/eframe directly, not `ironroot-gui` | `ironroot-gui` is unpublished; the port is a later, contained change |
| 2026-08-10 | Bootstrap is dry-run only until the executor lands | Nothing should touch a key before the plan, custody and audit paths are proven |
| 2026-08-10 | The signing credential is a PIV 9c X.509 certificate with the e-mail in `rfc822Name`, not an OpenPGP subkey | Works with the mail clients and document signers in use, needs nothing installed on the workstation, and the OS surfaces slot 9c for signing |
| 2026-08-10 | Custody model **B**: transport secret + forced change; nothing retained | Works for posted keys as well as desk hand-overs, and avoids creating a per-device credential store. FIDO2 enforces it in firmware from 5.7; below that, and for PIV always, the change is instructed on the hand-over term and the run records which applied |
| 2026-08-10 | `open` and `create` are separate operations, and neither guesses | `Store::open` created a missing file, so a typo'd share path produced an empty database that looked like total data loss |
| 2026-08-10 | A serial's **provenance** is recorded, and only ever improves | A serial from a box label is a claim about a key nobody has touched; treating it as equal to a device read would let a mis-scan bind a certificate to the wrong key |
| 2026-08-10 | The USB barcode wedge is the recommended intake path, with the camera as the alternative | A wedge needs no camera, no permission and no decoding; a laptop camera is fixed-focus and awkward at label distance |
| 2026-08-10 | The macOS bundle is assembled by a reviewable script, not a bundling crate | The layout is a few directories and one plist; a script works in CI, is auditable, and cannot go unmaintained. It is *verified* by asking the bundled binary about itself (`--diagnose`) rather than by assuming |
| 2026-08-10 | A cloud-sync folder is treated as a hostile location for the database | A sync client copies the file mid-write and resolves a clash by keeping both copies, so the failure mode is two divergent registers. Detected, given the conservative journal mode, and warned about in three places |
| 2026-08-10 | `camera` is a **default** feature | An operator should not need a special build to point a webcam at a label. The cost is that two problems now gate every artefact rather than an opt-in one: the macOS camera-usage declaration, and the future-incompatible `block` 0.1.6 in `nokhwa`'s macOS bindings. Both are logged as release blockers in `features/serial-scanning.md` |
| 2026-08-10 | Term templates are data, keyed `(id, language, version)`, with **line omission** instead of a conditional syntax | The wording is institutional text somebody else owns, and one documented rule ("a line whose variable is empty disappears") is far harder to get wrong in a legal document than `{{#if}}` |
| 2026-08-10 | The signed term is stored **in** the database, not as a path | The database is the unit of deployment: a path reference breaks the moment the file moves to a share, which is exactly when the evidence is needed. The cost — personal data in the file — is an argument for the password, and is documented as such |
| 2026-08-10 | The identification field is called an **identification number**, not CPF | It holds a CPF in Brazil and the local equivalent elsewhere; naming it `cpf` would have made the first non-Brazilian holder a migration |
| 2026-08-10 | The GUI is themed with `egui-elegance`, and the theme is the operator's choice | The screens looked like unstyled egui, which is a poor advertisement for a tool people are asked to trust with a key register. The crate targets exactly the egui 0.36 already in use, is one dependency with no build-script or system-library cost, and restyles the stock widgets on install rather than requiring every call site to change. Two of its defaults are deliberately not used: refusals stay selectable (a `Callout` cannot be copied) and inputs are capped by hand (`TextInput` has no `char_limit`) |
| 2026-08-10 | An edited term is stored as a **new version numbered by the database**, and generation takes the newest | The editor cannot be allowed to overwrite wording a holder has signed, and the version on the operator's screen must not decide what is written — two workstations editing the same term would otherwise both produce "version 2". The store reads what is on record and adds one |
| 2026-08-11 | An observation is stored on the key, and audited by *shape* rather than by content | The one field no device can supply is the operator's, and it is the one field that sometimes needs correcting. The audit chain refuses `UPDATE` and `DELETE` by trigger, so quoting the text into it would make a mistyped observation permanent and put uncontrolled free text in the immutable record — the entry says set / cleared / changed and how long, which is what a reviewer needs to follow the register |
| 2026-08-11 | A registered key can be **removed**, but only while no history refers to it — and removal is not the lifecycle exit | Intake produces mistakes (a mis-typed serial, a label scanned twice) and a register nobody can correct gets worked around in a spreadsheet. But a hand-over or a bootstrap run pointing at a serial nobody can look up is not a register, so `delete_key` refuses those and names retirement instead, which keeps the record. The audit entries outlive the row either way |
| 2026-08-10 | The layout is fluid — the page decides width, and a wide table scrolls inside its own card | A card is an `egui::Frame`, so it was as wide as whatever was inside it: the Inventory table filled the window while the Holders form stopped at two 340px columns, and no two screens lined up. Making the *page* horizontally scrollable would have been the other fix, but then every card on a screen becomes as wide as the widest table on it — so the overflow is contained in `ui::table` and the body scrolls vertically only |
