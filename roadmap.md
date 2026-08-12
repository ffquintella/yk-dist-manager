# yk-dist-manager — Roadmap

Single entry point for planning. One row per tracked feature; the detail lives in
the linked spec under [`features/`](features/).

**What this tool is.** A desktop application (Rust + egui) that does two things
for a unit handing out YubiKeys:

1. **Tracks distribution** — which key (serial, model, firmware) went to which
   person, on what date, handed over by whom, against which receipt, and exactly
   what was applied to it during bootstrap.
1. **Bootstraps keys from a template** — a versioned, declarative procedure that
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
| Done | 18 |
| In progress | 10 |
| Todo | 13 |
| **Total tracked items** | **41** (across 38 specs — two items share a spec) |

> Counted from the tables below rather than adjusted by hand, because they had drifted
> twice:
>
> ```bash
> for s in 'x' '/' ' '; do grep -cF "| \`[$s]\`" roadmap.md; done
> ```
>
> Six rows moved to `[x]` when this section was re-derived from the phase tables: they
> had every wave-0 phase done and were still `[/]` because of their **Wave 1** rows,
> which the Wave column exists to stop counting. Bootstrap templates joined them when
> its own wave-0 phases finished. See
> [What stands between here and a closed Wave 0](#what-stands-between-here-and-a-closed-wave-0).

Released: **v0.8.0**. Current wave: **Wave 1 — native execution.**

**Out of turn, released in v0.6.0** (AGENTS.md §1 asks for the reason, in this file, in the
same commit): **hosting the database in a OneDrive folder** —
[`features/cloud-sync-hosting.md`](features/cloud-sync-hosting.md) — was built at the
request of the operator who will run the tool. It belongs to Wave 0's storage work, not
to Wave 1. The reason it could not wait: a `--diagnose` report from a real installation
showed the register already living in `~/Library/CloudStorage/OneDrive-…/`, because that
is the shared folder the unit has. v0.4.0 answered that with detection, conservative
pragmas and a warning — which manages nothing, since the operator has nowhere else to
put it. So the risk is now managed: `Location::CloudSync` waits for the sync client,
takes a `<database>.lock` next to the file, refuses a second workstation **by name**,
releases only after the upload, and reports the copies a sync client leaves when it
could not merge. Two operators sharing that folder now collide with a refusal instead of
with two divergent registers of who holds which security token. No schema change. It is
not an endorsement of the location — see the ESI gate in the spec and in
[Open questions](#open-questions).

**Also out of turn, released in v0.7.0**: **connecting the SMB share from inside the
application** — [`features/smb-share-hosting.md`](features/smb-share-hosting.md). It
belongs to Wave 0's storage work, not to Wave 1, and it was taken now because it is the
missing half of the sentence every storage document here ends with. "Put the register on
a real network share" was advice nobody could act on: the tool could only open a path
somebody else had mounted, so the answer to a share that was not mounted was
`is the share mounted?` — a message that names the problem and offers nothing. That is
also *why* one installation keeps the register in OneDrive: getting to the unit's share
was harder than not. So the share is now reachable from the chooser — as the account the
operator is already signed in with (the default, and on **Windows** the whole mechanism,
since a UNC path is authenticated by the session's own token), as a **guest**, or as a
**named account** whose password is typed and dropped. Native APIs, not `net use` /
`mount_smbfs`, because a password on a command line is readable by every process on the
workstation. A share the operating system already mounted is used and **not** unmounted
on close; one this session made is released on every path that stops using the database,
including quitting. No schema change. On Linux an unprivileged process cannot mount
CIFS, and the refusal says so and names the alternative rather than failing quietly.
Which share, and whether a named service account may be used at all, are **ESI**
decisions this makes possible rather than makes — see the gates in the spec.

Also out of turn, released in v0.6.0: the **Templates screen** — phase 2 of
[`features/bootstrap-templates.md`](features/bootstrap-templates.md), plus add /
duplicate / retire / remove — was built at the request of the operator who will run
the tool. It belongs to Wave 0, which is not finished, rather than to Wave 1, and it
was worth taking now for the same reason the Terms editor was: the procedure is
content somebody else owns. "Templates are data, not code" was only true for a
reader of the source — changing a step meant editing Rust and shipping a build, so
in practice the procedure was as hard-coded as if it had been a constant. It also
front-runs the executor usefully: the pre-save gate (`check()` plans every template
against sample data) is the same gate a run will need, and it is now covered by
tests before anything writes to a key. Schema **v4** (`templates.retired_at`) ships
with a migration.

**Also out of turn, released in v0.6.0**: an **application icon**
([`features/application-icon.md`](features/application-icon.md)), requested by the
same operator. It belongs to Wave 0 alongside the GUI shell, and it is small: one
SVG, a render script, and 30 lines embedding the result. Taken now because it is
cheap and because the alternative was shipping a bundle with a placeholder icon —
during a hand-over the tool sits beside the register, the term to sign and a
terminal, and an application identified by reading its title bar is one that gets
mis-clicked. The mark is deliberately institution-neutral, consistent with v0.5.0
removing every organisation name from the build: replacing it is one SVG and
`make icons`. Whether the organisation running the tool wants its own identity
there is recorded as an open gate in the feature file, not decided here.

**Out of turn in v0.4.0**: the Inventory screen gained an **observation** per key and
a **confirmed removal** of a registered key, both requested by the operator who will
run the tool.
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
language. The Templates screen now does the same for the bootstrap procedure itself.
**662 tests** pass with the default features and **679** with `--all-features` (plus 4
read-only hardware tests and one ignored `openssl` interop test), with **84.5%**
(region 83.9%) line coverage of the headless core — enforced by CI against a floor of
80% rather than reported. The register can also
be opened from the unit's **SMB share**, connected by the application itself, and its
password can be set, changed or removed from Settings.

> The coverage figures quoted here had drifted again: they read 86.91% / 85.79%, and
> `make coverage-core` on the released 0.7.1 says 83.51% / 82.69%. Corrected to what
> the gate actually measures, which is the number CI enforces.

**Wave 1 has started.** The bootstrap executor exists and is proven against mock
transports; the secret machinery it needs exists. Nothing in this build can write to a
key — `MockWriter` is the only implementation of the write traits — which is the
intended state until the native transports land and are verified against hardware.

## What stands between here and a closed Wave 0

Wave 0 cannot be marked done today. This is the whole of what is left, derived from
the phase tables in [`features/`](features/) rather than written from memory — a
phase gates this wave if and only if its **Wave** column says `0`:

| Feature | Phase | What is missing |
|---|---|---|
| [Device detection](features/device-detection.md) | 2, 3 | background hot-plug polling; the picker for when several keys are attached |
| [Receipts & terms](features/receipts-and-terms.md) | 4, 6 | the signature state machine with an age warning; the return receipt |
| [SMB share hosting](features/smb-share-hosting.md) | 9 | reconnecting a share that drops mid-session |
| [Application icon](features/application-icon.md) | 7 | the icon on the unlock screen and in an About box |
| [Testing strategy](features/testing-strategy.md) | 9 | property tests for the audit chain and the RFC 4514 escaper |

**Seven phases across five features, none of them blocked on anything.** No decision
is outstanding for any of them, no hardware is needed, and nothing waits on Wave 1.
It is work, not a queue.

Bootstrap templates left this list when phases 4, 5 and 6 landed: files, signatures
and the diff are built, so its row is `[x]` above and what remains in that spec is
Wave 1.

Regenerate this list rather than trusting it — the prose here drifted twice before
it was derived:

```bash
awk -F'|' '$4 ~ /^ *0 *$/ && $5 !~ /[Dd]one/ {print FILENAME" phase"$2": "$3}' features/*.md
```

### What is *not* on that list, and why

Three groups of unticked boxes exist elsewhere in the specs and none of them gates
this wave. They are named here because they used to be listed as Wave 0 blockers,
and looking for them is otherwise the obvious mistake.

**Phases that gate a later wave.** The bootstrap engine's PIV and OTP steps, the
PIV/OTP/management native transports, per-applet reads, the wizard's resume and
post-run summary, template applicability rules and retry policy, unlocking with a
YubiKey: all **Wave 1**, all in the Wave 1 table below. A Windows `.ico` and a Linux
`hicolor` icon are **Wave 3**, with the packaging they attach to. Concurrency,
batch mode, the signed audit export and the cross-check transport are **Wave 2**.

**Phases marked `—`, which gate no wave by definition.** Some of them are marked
that way because they are held by a decision that is not the implementer's
(AGENTS.md §8). These three are every such decision left anywhere in the specs —
[Open questions](#open-questions) is otherwise empty:

| Question | Owner | Phase it holds |
|---|---|---|
| The approved cipher and KDF parameter set | **ESI** | [db-password](features/db-password-and-encryption.md) 4 (`—`) — explicit `kdf_iter` and cipher page size. The defaults are SQLCipher's until then |
| May an already-configured key be re-bootstrapped, and by whom? | operational | [device-detection](features/device-detection.md) 5, which is **Wave 1** rather than `—`, so it is not holding this wave either way |

The interface language was the third of these until 2026-08-12: it is **English**,
which closes [gui-shell](features/gui-shell.md) phase 9 as *not needed* rather than
done. The holder-facing documents stay multilingual — a different audience, and a
different decision.

The rest of the `—` phases are optional by choice, not blocked: an external
chain-head witness ([audit-trail](features/audit-trail.md) 5), Kerberos on macOS
([SMB](features/smb-share-hosting.md) 10), Cucumber
([testing](features/testing-strategy.md) 6), and archival and retention
([storage](features/storage-sqlite-single-file.md) 7) — that last one now has its
decision (one year, configurable, settled 2026-08-11) and needs an
archive-then-remove path that can break and rebuild the audit trigger, which is
deliberately not a general capability.

**Rows that reached `[x]` for this wave while their specs still show Todo phases.**
Seven of them, and the Wave column is what makes that legitimate: native device
transport, single-file SQLite storage, the audit trail, the bootstrap planner, the
GUI shell, the bootstrap wizard and bootstrap templates have every
wave-0 phase done and only Wave 1+ work left. The database password joined them in
0.8.0.

Camera scanning is a default feature, and on macOS it needs the bundled application:
an unbundled build refuses with an explanation rather than aborting (v0.2.2). One
release blocker stands in front of any distributed artefact — the future-incompatible
`block` 0.1.6 in `nokhwa`'s macOS bindings. The `NSCameraUsageDescription` half is
closed: `packaging/macos/verify-bundle.sh` fails the build without it. See
[`features/serial-scanning.md`](features/serial-scanning.md).

## How to read this

- **Status** is a checkbox plus a word: `[x]` Done, `[/]` In progress, `[ ]` Todo.
- **A feature is Done *for a wave* when every phase carrying that wave's number is
  done.** Each phase table in [`features/`](features/) has a **Wave** column, and
  it is what decides whether a phase gates this wave or a later one.

  That column exists because the previous rule — "Done only when every phase in
  its spec is done" — made Wave 0 unverifiable. Several Wave 0 rows have phase
  tables containing Wave 1 work by nature: device detection cannot read PIN
  retries before the applet transports exist, and the bootstrap wizard could not
  show a live run before there was an executor. Under the old rule those rows
  could never reach `[x]`, so "Wave 0 is finished" was a question with no answer.

  The consequence to expect when reading a `[x]` row: it means **done for this
  wave**, and its spec may still show Todo phases. Six rows are in exactly that
  state today, and are named in the section below so the mismatch is never a
  surprise.
- A phase marked **—** in the Wave column gates *no* wave: it is optional, or it
  is blocked on a decision that is not the implementer's (`AGENTS.md` §8). Those
  are listed under [What stands between here and a closed Wave 0](#what-stands-between-here-and-a-closed-wave-0)
  with their owner, and a wave can close with them outstanding.
- Waves are ordered. Work the current wave; if something must jump the queue,
  edit this file in the same commit and say why (see [AGENTS.md](AGENTS.md)).
- Every feature file carries its own phase table, audit events and test list.

---

## Wave 0 — Foundation

Everything needed before a single byte is written to a key.

| Status | Feature | Notes |
|---|---|---|
| `[x]` | Native device transport | [spec](features/native-device-transport.md) — `yubikey` over PC/SC reads serial + firmware from a real key today (verified against 5 NFC / fw 5.4.3, agrees with `ykman`). FIDO2 and OTP transports are Wave 1. |
| `[x]` | `ykman` fallback + parsers | [spec](features/ykman-fallback.md) — argv-only subprocess, typed errors, parsers unit-tested against recorded output of ykman 5.9.2. |
| `[/]` | Device detection | [spec](features/device-detection.md) — read-on-demand works; hot-plug polling and multi-key selection pending. |
| `[x]` | Single-file SQLite storage | [spec](features/storage-sqlite-single-file.md) — schema **v5** (per-step run rows), `user_version` migrations (v1→v5 tested), WAL locally / rollback journal on a share, `VACUUM INTO` backup **on a schedule with rotation**, `integrity_check`, **CSV import** of the spreadsheet this replaces, and the SMB share connected by the application itself. Every wave-0 phase done. Concurrency is Wave 2; archival and retention (phase 7, gating no wave) has its decision — one year, configurable — and needs the archive-then-remove path that may break and rebuild the audit trigger. |
| `[/]` | SMB share hosting | [spec](features/smb-share-hosting.md) — the application connects the share itself: the signed-in user (the default, and the whole mechanism on Windows), guest, or a named account whose password is typed and never stored. `WNetAddConnection2W` / `NetFSMountURLSync`, never a command line. An already-mounted share is used and left alone; one this session made is released on close and on quit. Reconnecting a share that drops mid-session, and Kerberos on macOS, are Todo. |
| `[x]` | Cloud-sync hosting (OneDrive) | [spec](features/cloud-sync-hosting.md) — `Location::CloudSync`: waits for the sync client, takes `<database>.lock`, refuses a second workstation by name, releases after the upload, reports sync conflict copies, **snapshots the register at open** before this session can write, and offers a **read-only** open so a second operator can look without taking the lock. Every phase done. Whether the location is *acceptable* is an ESI decision, not this feature's. |
| `[x]` | Choosing / creating the database file | [spec](features/database-selection.md) — strict `open_existing` vs `create_new` (a typo can no longer create an empty database), recent-database list, native dialogs, switch from Settings. |
| `[x]` | Optional database password | [spec](features/db-password-and-encryption.md) — `encrypted-db` wires `PRAGMA key`; the chooser prompts. **Settings → Password protection** sets, changes and removes the password by export-and-swap, never an in-place re-key, with the strength meter beside it and the floor enforced by the store rather than by the screen. The prompt slows down after three wrong passwords — a doubling delay counted down on screen, enforced in `handle_db_request` and deliberately not a lockout. Explicit KDF parameters are the one thing left, and they are **blocked on the ESI's approved cipher set**; unlocking with a YubiKey is Wave 1. |
| `[x]` | Logging | [spec](features/logging.md) — one entry point, three levels, G-002 line format, no hand-built log lines. |
| `[x]` | Audit trail | [spec](features/audit-trail.md) — SHA-256 chain, `UPDATE`/`DELETE` refused by trigger, **verification at open**, a **segregated mirror** whose divergence is an alert, and **filtering** by event, actor, target and date, plus one entry per executor step outcome. Every wave-0 phase done, and the segregation question is answered: the trail lives in the register, and the optional mirror is what satisfies the norm (2026-08-11). An external chain-head witness (phase 5) gates no wave; the signed ESI export is Wave 2. |
| `[x]` | Key inventory | [spec](features/key-inventory.md) — serial, model, firmware, form factor, FIPS flag, applications, lifecycle with guarded transitions, serial provenance (verified / scanned / typed), an editable **observation** per key, and confirmed **removal** of an intake mistake (refused once a hand-over or a run refers to the serial). |
| `[x]` | Serial from a barcode | [spec](features/serial-scanning.md) — camera decoding via `rxing` + `nokhwa`, a USB-wedge/typed path that needs no features, and provenance that only ever improves. |
| `[x]` | Holder registry | [spec](features/holder-registry.md) — minimal personal data, validated e-mail, RFC 4514 subject derivation, plus optional identification number, phone and address. |
| `[x]` | Distribution records | [spec](features/distribution-records.md) — hand-over, operator, delivery method, receipt reference, linked bootstrap run, return without rewriting history. |
| `[x]` | Bootstrap templates | [spec](features/bootstrap-templates.md) — versioned templates, `{{variable}}` rendering, two built-ins, validation, and a **Templates screen**: add, duplicate, edit (always as a new version), retire / reinstate, remove. A draft is refused unless it *plans* against sample data. Both built-ins ship at **v2** with the corrected forced-change ordering, `fido-only` deriving its version from `org-standard` so a correction cannot reach the code and stop at the database. Also: **export and import as a file** (with the canonical bytes beside it; import is a preview, and the receiving register numbers the version), **Ed25519 signatures** verified before a run — the private key never comes near this tool — with pilot mode visible on screen and audited, and a **structural diff** between two versions that reports a reordered step as *moved*. Applicability rules and the retry policy are Wave 1. |
| `[x]` | Bootstrap planner | [spec](features/bootstrap-engine.md) — plan with per-step transport (native / ykman / manual) and secret placeholders; dry runs recorded — both wave-0 phases. **The executor is Wave 1**, and is tracked as its own row there. |
| `[x]` | GUI shell | [spec](features/gui-shell.md) — eight screens, unlock screen, status bar, egui 0.36 `App::ui`, themed with `egui-elegance` (four palettes, the choice persisted) and laid out fluidly (one gutter, full-width cards, columns that split the page, tables that contain their own overflow). Search, sortable columns, window-state persistence, keyboard flow, hardware-write confirmation, the log panel and the accessibility pass are all done — every wave-0 phase. Localisation (phase 9) is closed as not needed: the interface is English (2026-08-12). |
| `[x]` | Bootstrap wizard | [spec](features/gui-bootstrap-wizard.md) — selection (the newest version of each template in use), per-step opt-out, plan review, dry run, and a link to the Templates screen — the whole of wave 0. The live run view, the secret panels and the pre-flight checks landed with the executor in 0.7.1; resume and the post-run summary are Wave 1, batch mode Wave 2. |
| `[/]` | Application icon | [spec](features/application-icon.md) — one SVG (a box truck carrying a YubiKey), `make icons` rendering the PNGs, the macOS `.icns` and the RGBA blob the binary embeds; window, dock and bundle icons done. A Windows `.ico` resource and a Linux `hicolor` install wait on there being Windows and Linux packaging. |
| `[/]` | Testing strategy | [spec](features/testing-strategy.md) — **662 tests** (679 with `--all-features`) across unit + behaviour suites, a mock device backend and a mock share connector, recorded fixtures, and tests ignored by default for what needs hardware or `openssl`; **84.5%** core line coverage. **CI enforces the gate** on every push, with a macOS/Windows/Linux build matrix. Mock write transports and the secret-leak sweep wait on Wave 1. |

### Paperwork

| Status | Feature | Notes |
|---|---|---|
| `[x]` | Consignment terms | [spec](features/consignment-terms.md) — multilingual templates keyed `(id, language, version)`, pt-BR + en built in, optional fields that omit their own line, generated from the record. A **Terms screen** edits the wording and adds languages: saving stores a new version, and terms are generated from the newest. The **wording needs its owner's review**, which the editor is what makes possible — and, since phase 7, an *Export as PDF…* that sends the reviewer the document rather than a template full of `{{variables}}`. The term now leaves as **text or PDF** from one rendering, so the copy reviewed on screen cannot disagree with the copy that is signed; `crate::pdf` writes the file with **no dependency and no TeX**, and every page names the template version that produced it. |
| `[x]` | Signed-term upload | [spec](features/signed-term-documents.md) — the scan is filed in the database with a SHA-256, verified on export, with a per-hand-over "none filed" badge. |
| `[/]` | Receipts & terms (signature tracking) | [spec](features/receipts-and-terms.md) — the term, its PDF, the versioned template and the filed signature are done, and the sealed-envelope slip shipped in 0.7.1. Left for wave 0: a **signature state machine** with an age warning (phase 4) and the **return receipt** (phase 6). Batch generation is Wave 2. |

## Wave 1 — Execute the bootstrap, natively

The point of the tool: apply the template to a key, safely, with evidence.

| Status | Feature | Notes |
|---|---|---|
| `[/]` | Bootstrap executor | [spec](features/bootstrap-engine.md) — **the engine is built and proven against mocks**: sequencing, per-step persistence, abort-on-required-failure, idempotency, resume, an unforgeable confirmation gate, and a sweep asserting no secret reaches any record. What is missing is a transport that talks to hardware, and the GUI wiring — no code path in this build can write to a key. |
| `[/]` | Step: FIDO2 PIN | [spec](features/step-fido2-pin.md) — set/change the PIN over CTAP2, minimum length policy (fw 5.7+), retry accounting, and `forcePINChange` so the holder must replace the transport PIN (custody model B). |
| `[/]` | Step: initial FIDO2 credential | [spec](features/step-fido2-credentials.md) — `authenticatorMakeCredential` with `rk=true` so the credential is resident on the key. `ykman` cannot do this at all. |
| `[ ]` | Step: OTP slot access code | [spec](features/step-otp-access-code.md) — the 6-byte code that write-protects a slot, plus optional slot programming. Needs the HID config frame (no crate covers it). |
| `[ ]` | Step: PIV PIN / PUK / management key | [spec](features/step-piv-pin-puk-management-key.md) — leave no factory default; prefer a PIN-protected random management key so nothing needs custody. **Blocked on a transport decision (ESI):** every PIV *write* in the `yubikey` crate sits behind its `untested` feature, and the worst failure here — a management key nobody holds — kills the applet. Options and a recommendation are in the spec. |
| `[ ]` | Step: PIV signing certificate | [spec](features/step-piv-signing-certificate.md) — on-device key in slot 9c, CSR **with `rfc822Name` SAN**, issued certificate imported, attestation stored. The SAN is why this step goes native. |
| `[ ]` | CA integration | [spec](features/ca-integration.md) — internal CA for pilots, BastionVault PKI, and an enterprise CA profile; SAN and EKU requirements per option. |
| `[/]` | Secrets custody | [spec](features/secrets-custody.md) — **model B decided (2026-08-10)**: transport secret + forced change, nothing retained. `CustodyModel` fixes the vocabulary, the standard template carries the forced-change step, and **the generate / show-once / zeroise machinery is built** (`crate::secret`: no `Clone`, no `Serialize`, `Debug` prints `<redacted>`, OS CSPRNG with unbiased digits). The sealed-envelope slip and the custody report are still Todo. |

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
| `[ ]` | Compliance artefacts | [spec](features/compliance.md) — classification proposal, system registration, data documentation, change/homologation records. |
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

Two, and **neither holds Wave 0** — each one's phase is marked `—` (gating no wave)
or belongs to a later wave. They were dropped from this list when the big questions
were settled on 2026-08-11, which was an error: an unanswered question that blocks
nothing today still blocks something eventually, and a list that says "None"
invites nobody to answer it.

1. **The approved cipher and KDF parameter set.** *Owner: ESI.* Holds
   [`features/db-password-and-encryption.md`](features/db-password-and-encryption.md)
   phase 4 — `PRAGMA kdf_iter` and the cipher page size, stated explicitly rather
   than inherited. Until it is answered the defaults are SQLCipher's own, which are
   reasonable and *not* the same thing as approved. Also listed as an approval gate
   in `docs/security-and-compliance.md` §7. Everything else about the password is
   built.

2. **May an already-configured key be re-bootstrapped, and by whom?** *Owner:
   operational.* Holds [`features/device-detection.md`](features/device-detection.md)
   phase 5, which is Wave 1. Under custody model B the holder is *told* to change
   the transport PIN, so a second run that silently set one would replace a PIN the
   holder chose without their knowing — which is why the executor already treats
   "already applied" as a skip. What needs deciding is whether an operator may
   override that deliberately, and whether the record should distinguish a re-issue
   from a first issue.

Every question below has an owner's answer; re-open one by moving it back up here
with the date and the reason.

### Answered

- **Is the interface English or pt-BR?** *(2026-08-12)* **English.** The screens stay
  as they are, so [`features/gui-shell.md`](features/gui-shell.md) phase 9 is closed
  as *not needed* rather than done — there is no localisation to build.

  Two consequences worth stating rather than discovering:

  1. **The holder-facing documents are a separate decision and stay multilingual.**
     The consignment term is keyed `(id, language, version)` with pt-BR and en built
     in, and the sealed-envelope slip follows the term's language. A holder signing a
     consignment of institutional property reads it in their own language; the
     operator driving the tool is one trained person. Those are different audiences,
     and this answer is about the second one.
  2. **The log line format stays as G-002 specifies it**, which is a Portuguese
     norm's format (`[dd/mm/aaaa] hh:mm:ss ; evento ; detalhes`). An English
     interface writing that format is not an inconsistency to fix: the format is a
     compliance requirement, and the event names inside it are stable identifiers
     (`db.unlocked`, `bootstrap.step.failed`) rather than prose in either language.

- **The PUK under model B.** *(2026-08-11)* **Sealed envelope — the default is
  confirmed.** The PUK travels to the holder alongside the transport PIN and
  nothing is retained. A blocked PIN with a lost PUK therefore costs a PIV applet
  reset and a new certificate; that price was accepted over holding an escrow
  store the tool would then have to protect.
- **The OTP access code under model B.** *(2026-08-11)* **In the envelope —
  which reverses the default this was built with.** Generate-and-discard froze
  the OTP slot deliberately, making any later reprogramming cost an applet reset.
  Carrying the code on the sealed slip keeps that door open, at the price of one
  more line on a slip the holder is told to destroy after use. Implemented in
  `SecretKind::goes_to_the_holder`: the management key is now the only secret
  that does not travel, because it is `--protect`ed onto the key itself.

- **The term's wording.** *(2026-08-11)* **Left as it stands, and reviewed at
  operational time rather than during development.** The built-in pt-BR and en
  consignment terms ship as they are; whoever owns the term — and the DPO, for
  the data-protection paragraph — reviews the wording when the tool is put into
  service.

  This is exactly what the Terms screen was built for. The wording is data keyed
  `(id, language, version)`, so a review is an **edit and a new version**, not a
  code change and not a release: the reviewer opens the screen, changes the text,
  and terms generated from then on use the new version while every term already
  signed still points at the version it was generated from. *Export as PDF…* is
  what sends a reviewer the document rather than a template full of
  `{{variables}}`.

  What that means for the shipped text: it is a **plausible draft, not approved
  wording**, and nothing in this repository should describe it otherwise until
  the review happens.

- **Classification level of the system.** *(2026-08-11)* **Level 2.**
  `docs/security-and-compliance.md` proposed **level 3**, and the answer is one
  level below that — recorded as given, and flagged here rather than quietly
  applied, because classification is what selects the controls the system is held
  to. Two consequences to settle when the docs are updated: which of the
  level-3 controls the compliance mapping currently assumes are no longer
  mandated, and whether any of them should be kept anyway because the system
  holds personal data and the custody record for security tokens regardless of
  what its level requires. The immutable audit trail and the encryption-at-rest
  option are both in that category — cheap to keep, awkward to argue for
  re-adding later.

- **May the register live in a cloud-sync folder at all?** *(2026-08-11)*
  **Yes.** The location is approved. What made it defensible had already landed:
  one workstation at a time by lock file, a snapshot before the session can
  write, conflict copies detected and reported — and, since this session, the
  file can be **encrypted at rest**, which was the mitigation this question was
  most waiting on. A sync folder holding an encrypted register with a
  single-writer lock is a different proposition from the plain file that prompted
  the question.

  Still true and still the operator's to manage: the folder's sharing settings
  are its access control, and the lock binds only workstations running this
  tool.

- **Where does the audit trail live?** *(2026-08-11)* **The same file.** The
  register stays one file, with the audit table immutable by trigger inside it,
  and `StoreConfig::with_audit_mirror` remains available for a deployment that
  wants a second copy on storage the operator cannot rewrite. That mirror is the
  answer to the norm's segregation requirement and is what catches a chain
  rebuilt in the database; it is optional because the single-file deployment
  model is the requirement it has to live with.

- **Retention of audit entries and logs.** *(2026-08-11)* **One year by default,
  and configurable in the application** — `AppSettings::retention`. The period is
  the organisation's to set and may differ per deployment or change after an ESI
  review, so it is a setting rather than a constant, in the same way the CA and
  the certificate SAN are. Twelve months is what a fresh install starts at.

  Recorded as decided, and with one consequence that needs deciding separately
  rather than being assumed, because it cuts against two things already built:

  1. **The audit table refuses `DELETE` by trigger.** That is deliberate — NRM
     §5.3.1 wants immutability guaranteed by the database rather than promised by
     the application — so enforcing retention means an archive-then-remove that
     drops and recreates the trigger. Any code that can delete an audit row is
     code that can be misused to delete an audit row, so it needs to be one
     narrow, audited, exported-first path rather than a general capability.
  2. **A key can be held for longer than a year.** Deleting the trail after
     twelve months would remove the record of a hand-over while the holder still
     carries the key — and "who was given serial 20423633, and when" is the
     question this register exists to answer. So retention has to be measured
     from when a record stops being live (the key is returned, retired or
     re-issued), not from when the entry was written.

  Both are implementation questions rather than reopenings of the decision:
  entries are kept for a year, and `features/storage-sqlite-single-file.md`
  phase 7 records how. Nothing is deleted until that phase is built.

- **Which CA issues the signing certificate?** *(2026-08-11)* **None of them, and
  all of them: the CA is a configured parameter.** The tool must be pointable at
  whichever CA a deployment has — an internal one for a pilot, BastionVault's PKI
  engine, an enterprise CA — without a rebuild, because the answer differs per
  deployment and changes over a deployment's life.

  What that settles, which is the part that was actually blocking: **we build the
  CSR ourselves.** "Configurable" cannot mean "assume the CA injects the SAN",
  because a CA that takes a plain CSR is one of the cases that must work. So the
  PKCS#10 request with an `rfc822Name` SAN is required, not optional, and
  `features/step-piv-signing-certificate.md` is unblocked.

  The SAN half of this already landed: `crate::san::SanPolicy` renders the value
  and `SanSource` records whether the request carries it, the CA's profile
  injects it, or it travels out of band. The CA itself now needs the same
  treatment — an issuer chosen by configuration, with **manual/offline** as the
  one that always works: export the CSR, get it signed however the unit does
  that, import the certificate. Every other issuer is an automation of that
  path, not a replacement for it.

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
| 2026-08-11 | The consignment term's **wording is reviewed in service, not in development** | It is institutional text somebody else owns, and the Terms screen exists so that review costs an edit rather than a release: the wording is data keyed `(id, language, version)`, a review produces a new version, and every term already signed keeps pointing at the version that produced it. Until then the shipped text is a draft, and is described as one |
| 2026-08-11 | System **classification: level 2** | The organisation's call. One level below the level 3 `docs/security-and-compliance.md` proposed, so the control mapping has to be revisited rather than assumed — and the controls already built that level 3 implied (immutable audit, encryption at rest) are kept regardless, because they are cheap to keep and awkward to argue for re-adding |
| 2026-08-11 | A cloud-sync folder is an **approved** location for the register | The mitigations are in place and measurable: one workstation at a time by lock file, a snapshot before the first write of a session, conflict copies reported, and the file encryptable at rest. The last of those was the missing piece — the question was asked of a plain file in OneDrive, and that is no longer the only option |
| 2026-08-11 | The audit trail stays **in the same file**, with the segregated mirror optional | The single file is the deployment: it is what an operator copies, backs up and puts on a share. Splitting the trail out would mean two things to keep, two things to move and two things to lose one of. The triggers make it immutable where it sits, and the mirror — verbatim, comparable, on storage the operator cannot rewrite — is what answers the norm's segregation requirement for a deployment that needs it |
| 2026-08-11 | Audit and log **retention defaults to one year and is configurable**, measured from when a record stops being live | A period the norm does not set, so it is the organisation's to choose — and one that may differ per deployment or change after a review, which is why it is a setting rather than a constant. Measured from the record going cold rather than from the entry being written, because a key can be held for years and deleting the hand-over while the holder still has the key would remove the answer the register exists to give. Enforcing it needs a narrow archive-then-remove path: the audit table refuses DELETE by trigger, and that guarantee is not one to weaken generally |
| 2026-08-11 | The CA is a **configured parameter**, not a compiled-in choice, and the tool therefore **builds its own CSR** | The answer differs per deployment and over time, so hard-coding one issuer makes the other two unreachable. And configurability forces the CSR question: a CA that takes a plain PKCS#10 is one of the cases that must work, so the `rfc822Name` SAN has to be something this tool can put in a request rather than something it hopes a profile will add. Manual/offline issuance is the baseline every other issuer automates — it needs no integration, no credential and no network, so it is the one that cannot be unavailable |
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
| 2026-08-11 | The application carries **no institution's name** — the organisation is the operator's setting, and the built-in procedure is `org-standard` | A tool that hard-codes one unit's name is that unit's tool; a tool whose default template says `{{org}}` is anybody's. The name also is not decoration: it reaches a PIV certificate subject and a FIDO2 relying-party id, so it has to come from the deployment. The security rules keep citing NRM and G-002 — those are requirements, and describing them without naming the organisation costs nothing. Renaming the built-in id was done by *adding* the new id, never by rewriting the id a bootstrap run recorded |
| 2026-08-11 | A bootstrap template is edited **only** by storing a new version, and its id is immutable once stored | A run records `(template_id, template_version)`, so overwriting a version would rewrite what a key was told to have applied to it, and renaming an id would orphan every run that referred to it. The number comes from the database, not from the operator's screen — two workstations editing the same template would otherwise both produce "version 2". Same rule, and now the same code, as the consignment term |
| 2026-08-11 | A template is **retired** rather than deleted once anything refers to it — and a built-in can only ever be retired | Three facts collide: a register must be correctable, a run's procedure must stay explainable, and the application re-seeds its built-ins on every open. So removal is kept for the case it is honest in (a procedure typed by mistake, nothing referring to it), and everything else is retirement: withdrawn from the wizard, kept in the database, and *not* undone by the next launch. Deleting a built-in would only have looked like it worked, which is worse than a refusal that names the operation that lasts |
| 2026-08-11 | A draft template must **plan** before it can be stored | `check()` runs a real `plan()` against a fictitious holder and key, including the steps that arrive disabled. An unknown `{{variable}}` or a missing `slot` is a refusal at the desk, by the person who typed it, instead of a failure in front of a key with a holder waiting — and because the store applies the same gate, nothing in the database can fail to plan. The cost is that the sample context has to be able to supply every documented variable, which a test asserts |
| 2026-08-10 | The layout is fluid — the page decides width, and a wide table scrolls inside its own card | A card is an `egui::Frame`, so it was as wide as whatever was inside it: the Inventory table filled the window while the Holders form stopped at two 340px columns, and no two screens lined up. Making the *page* horizontally scrollable would have been the other fix, but then every card on a screen becomes as wide as the widest table on it — so the overflow is contained in `ui::table` and the body scrolls vertically only |
| 2026-08-11 | A database in a cloud-sync folder is opened **one workstation at a time**, enforced by a lock file next to it | Warning about the location changed nothing, because the unit that hit it has nowhere else to put the register. A sync client offers no lock manager and no supported "have you finished?" API, so sequencing has to happen outside the database: wait until the file stops changing, take `<database>.lock`, refuse the second workstation *by name*, and remove the lock only after the upload — releasing it earlier would invite the next operator to start from a file still on its way. The lock is cooperative and says so: it binds workstations running this tool, not a machine that ignores it, which is why sync conflict copies are also detected and reported |
| 2026-08-11 | A lock is held by a **run**, not by a host and pid, and an abandoned one is broken only by an explicit operator action | Pids are reused, so "same host, same pid" would let a lock left by a dead run be silently adopted — the exact two-writer case the lock prevents. A per-run id makes the question exact. And staleness is deliberately 15 minutes with no automatic break: a sleeping laptop stops renewing without releasing, so only the operator can know the other machine is off rather than mid-hand-over. Taking the lock over names the previous holder in the audit trail |
| 2026-08-11 | The consignment term is set as a PDF by **code in this repository** — no PDF crate, no TeX, no subprocess | The deployment is a desktop application on a workstation that has nothing else installed, so a document pipeline the operator has to install is a document pipeline that will not be there during a hand-over. Writing the file is affordable because of one restriction: the document is monospaced text in **Courier**, a standard-fourteen font, so nothing is embedded and the font-parsing half of a PDF writer disappears. Courier is also the correct font rather than a concession — the term's clause indentation and its side-by-side signature rules are built out of spaces in the template, and a proportional font would quietly destroy the wording's own alignment. The cost is stated and handled, not hidden: CP1252 covers the Latin-script languages completely, and a character outside it is *reported to the operator before printing* rather than silently turned into `?` |
| 2026-08-11 | The text output and the PDF come from **one rendering** (`term::render_term_parts`) | The operator reviews the text on screen and the holder signs the PDF. Two substitution paths would eventually disagree, and the disagreement would be found by a holder holding a document that contradicts the register — which is the exact failure this whole feature exists to prevent |
| 2026-08-11 | Every page of the term names the **template version** that produced it, and the last page always carries at least six rows | The wording is versioned because something gets signed against a version; a signed sheet in a filing cabinet that does not say which one leaves the versioning doing no work. The six-row rule is the same argument applied to the page break: the shipped term is 62 rows against a 61-row page, so the default break put the signature rules on one sheet and the names beneath them on the next. Asking the template to mark the block instead would have made a legal document depend on its author remembering a mechanism, and forgetting would be invisible until a term came back signed |
| 2026-08-11 | In a term template a gap of **two or more spaces is a column**, kept where the template put it; a single space is spacing and is never touched | The signature block is two columns, and its author counted the spaces so the rule, the name under it and the role under that all begin at column 41. Substitution destroyed that, because a holder is not fifteen characters long like `{{holder.name}}` — so the name stopped sitting under its own rule. The template was never wrong; the renderer was discarding geometry the template had declared. The fix is layout, not logic: line omission stays the only conditional, there is no `{{name\|pad:41}}` (that is the template language this feature refuses to grow), and a gap before the first substitution on a line is untouched, which is what leaves the wrapped clauses' indentation alone. Restructuring the shipped wording into stacked blocks would have avoided the code, but the layout of an institutional document is its owner's decision — and a two-column signature block is the ordinary form, so the next unit to write one would have hit the same bug |
| 2026-08-11 | The application **connects the SMB share itself**, and the default identity is the operator already signed in | "Put the register on a real network share" was the recommendation in every storage document and the one thing the tool could not help with: it opened paths, so the share had to be mounted by somebody else first, and the answer when it was not was a question. On Windows the correct implementation of "use my credentials" is to make *no* call — a UNC path is authenticated by the session's own token, exactly as Explorer is — so the default costs nothing and needs no password. Guest and a named account exist because a NAS and a service-account share both exist, and an explicit choice is honoured exactly rather than falling back to the signed-in user: connecting as an unexpected identity is a register opened with permissions nobody reviewed, and on a share that is read-only for everyone else it looks like the writes were lost |
| 2026-08-11 | Reaching a share uses the **native API** (`WNetAddConnection2W`, `NetFSMountURLSync`), never `net use` or `mount_smbfs` | The rule that no secret may persist decides this, not taste. Both CLIs take the password in the URL or the argument vector, where every process on the workstation can read it, and the documented way around that is a credentials file — the temporary file the same rule forbids. `mount_smbfs`'s interactive prompt reads `/dev/tty`, which a windowed application does not have. The API takes a string in this process's memory, which is what a zeroed-on-drop `Secret` can supply. The same reasoning is why Linux gets a *refusal that names `mount.cifs` and `autofs`* rather than a helper that writes a credentials file |
| 2026-08-11 | A share the operating system already mounted is **used and left alone**; only a connection this session made is taken down | The probe comes before the credential, which means an operator who already has the share is never asked for a password and never has their mount pulled out from under their own work. It also fixes the ordering that matters on close: audit, close the database, *then* disconnect — and the audit entry has to be written while there is still a database to write it to |
| 2026-08-12 | The database password is **chosen** under a policy the store enforces, and **retried** under a throttle the application enforces — neither lives in the screen | The meter and the disabled button are what an operator sees, and neither is a control: paint code cannot be covered by the gate, and a rule that exists only there is one keyboard shortcut or one new call site away from being bypassed. So `create_new` and `change_password` refuse a password below the floor — the first password, the one that protects the register for the rest of its life, was previously advice the GUI happened to give — and `handle_db_request` refuses an unlock while a wait is owed, which is the one funnel every route to an open passes through. What counts as a wrong password is a single predicate (`StoreError::is_wrong_password`), so a missing file, a share that is not there, another workstation's lock, a build without SQLCipher and the application's own password-less probe at startup cannot silently earn the operator a delay |
| 2026-08-12 | Setting, changing and removing the password is **one screen and one operation**, and every refusal happens before the register is handed over to it | `Store::change_password` consumes the `Store` because after the swap the handle points at a file that is no longer the register — correct for the operation, and unforgiving for the caller: a refusal reached after that point closes the register in order to explain itself. So the build, the read-only session, the mismatch and the policy are all answered while the store is still ours, and the reopen afterwards deliberately does not go through `open_database` — that one releases what is open first, and releasing disconnects an SMB share this session connected, which on a share takes the file away before the reopen and reports a password change that worked as one that failed |
