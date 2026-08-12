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

- **The sealed-envelope slip** ([`features/secrets-custody.md`](features/secrets-custody.md)
  phase 5, [`features/receipts-and-terms.md`](features/receipts-and-terms.md) phase 0).
  Custody model B always hands a transport secret over, so for a posted or
  couriered key something has to travel with it, sealed. This is the only
  document the tool produces that carries a live PIN on purpose, and it is shaped
  by that: it is **never stored** in the database — a slip is a courier, not
  evidence, and filing one would turn the register into a credential store — its
  bytes are `Zeroizing`, it refuses to render from a dismissed show-once panel,
  and it carries only the secrets the holder actually needs (not the management
  key, which is protected onto the key, nor the OTP access code, which is
  discarded). The instruction wording changes with the firmware: where the key
  cannot enforce the PIN change, the sentence on the slip is the only mechanism,
  so it says so in as many words. It refuses to render a name the document
  encoding cannot set, rather than printing `?` in a document that still looks
  valid.
- **The native FIDO2 transport** ([`features/native-device-transport.md`](features/native-device-transport.md)
  phase 2), behind `native-fido`, **verified against real hardware**. This is the
  transport the native argument was always about: `ykman` can list and delete
  resident credentials but **cannot create one**, and creating the initial
  discoverable credential is a required step of the standard procedure. Every
  operation — `set_pin`, `set_min_pin_length`, `make_credential` with `rk=true`,
  and `force_pin_change` — ran against a YubiKey 5C NFC (firmware 5.7.4) through
  `examples/verify_fido2_write.rs`, the manual procedure that exists because no
  *test* may write to a key. The credential the transport created is recorded in
  `features/step-fido2-credentials.md`. Until that ran, the native transport was
  an argument; it is now a demonstrated capability.

  The run also settled a question the specs had left open: **`forcePINChange` is
  genuinely enforced by the firmware on 5.7+**, so custody model B's enforcement
  is `enforced-by-firmware` on such a key rather than only
  `instructed-on-handover`. The specs had assumed a 5.4.3 reference key, where
  only the procedural path exists.

  Two read-only hardware tests were added alongside, including one asserting the
  transport refuses a serial it was not opened for — HID exposes no serial, so
  that guard is what stands between a run and writing a PIN to the wrong key.

- **The bootstrap executor** ([`features/bootstrap-engine.md`](features/bootstrap-engine.md)
  phases 3, 8, 9, 10). A plan is now applied step by step, with the evidence
  recorded as it goes. The **default** build still cannot write to a key:
  `MockWriter` is the only implementation of the write traits compiled in, and
  `--features native-fido` is what adds a real one.
  - **The confirmation is a type, not a flag.** `Executor::run` takes a
    `Confirmation` that can only be built by naming the serial and step count
    actually shown, and re-checks it against the request. A stale confirmation —
    the operator agreed to six steps, then changed the template — does not
    authorise the new plan. "No confirmation, no writes" is now a signature
    rather than a rule to remember.
  - **Status is persisted before each write, not after the run.** An interruption
    leaves an accurate record instead of an optimistic one; schema v5's per-step
    rows are what make that cheap.
  - **A required step's failure aborts; an optional one is recorded and the run
    continues.** The difference between "the key is unusable" and "the key works,
    minus the OTP slot".
  - **Already-applied is a skip, not an overwrite.** Every step reads its applet's
    state first. Under model B the holder is *told* to change the transport PIN,
    so a second run that blindly set one would replace a PIN the holder chose,
    and they would not know.
  - **Resume, not restart.** An interrupted run continues from the first step that
    is not `Done`.
  - **A run that cannot be recorded does not touch the key.** A configured key
    with no record of what was applied is the failure this tool exists to
    prevent, so an unreachable register stops the run before the first write.
- **The secrets a bootstrap sets** ([`features/secrets-custody.md`](features/secrets-custody.md)
  phase 3). `crate::secret` produces them from the OS CSPRNG, shows them once and
  wipes them. `Secret` has no `Clone`, no `Serialize` and no `Display`, and its
  `Debug` prints `<redacted>` — so a panic message, a stray `dbg!` or a
  mis-pointed `tracing` field prints the redaction rather than the value.
  Generated PINs use rejection sampling, because `byte % 10` would make the
  digits 0–5 about 1.02× likelier than 6–9. A behaviour scenario greps every
  persisted record and audit entry of a complete run against every value it
  generated.
- **Per-applet write traits and a mock** ([`features/testing-strategy.md`](features/testing-strategy.md)
  phase 7), kept apart from the read-only `YubiKeyBackend` so a screen holding
  only a reader cannot write to a key by accident. The mock records that a call
  *carried* a secret and never which, so the leak sweep needs no exception for it.
- **Audit coverage for every executor outcome**
  ([`features/audit-trail.md`](features/audit-trail.md) phase 3):
  `bootstrap.started`, `.step.done`, `.step.failed`, `.step.skipped`,
  `.finished`, `.aborted`, `.resumed`, `.incomplete`, plus `secret.generated`
  (kind and length, never the value) and `secret.change_enforcement`.

### Changed

- **The PIV transport is blocked on a decision, and the analysis is written down**
  ([`features/step-piv-pin-puk-management-key.md`](features/step-piv-pin-puk-management-key.md)).
  Implementing it against the `yubikey` crate stopped on the discovery that every
  mutating PIV operation — `change_pin`, `change_puk`, and all three management-key
  setters — sits behind that crate's `untested` Cargo feature. The read paths are
  not gated; it is precisely the operations whose failure mode is worst that
  upstream declines to vouch for, and a management key set to a value nobody holds
  leaves the applet administratively dead. Three options, their costs and a
  recommendation (use the `ykman` fallback for these writes in the first
  production release) are recorded in the spec. It is an architecture premise, so
  it is the ESI's call rather than the implementer's.

### Fixed

- **The build is warning-free again on Linux and under Rust 1.97.** Two unrelated
  breakages, both of which CI treats as failures because `RUSTFLAGS: -D warnings`.
  The SMB password reader is called only by the Windows and macOS connectors, so on
  Linux — where [`store::smb::system`](src/store/smb/system.rs) refuses to mount
  rather than hand a password to `mount.cifs` — `Secret::expose`,
  `Credential::password` and the field behind them are dead *by design*; they now
  carry an `allow(dead_code)` gated on exactly the platforms that lack a caller,
  rather than the whole module going quiet. Separately, 1.97 flags a redundant
  reference in a `format!`/`assert!` argument, which four call sites had. The
  deref in the envelope's assertions was kept: `text` is `Zeroizing`, and only the
  `&` in front of it was redundant.
- **The standard procedure ships as `org-standard` v2, so a register seeded by an
  older build gets the corrected ordering.** Fixing the constructor was not
  enough: seeding deliberately never overwrites a stored `(id, version)`, so a
  database written by an earlier build kept the broken v1 and the operator met
  the refusal in the template editor. v2 is added beside it. v1 stays on
  record — a run may have recorded it, and rewriting what a version *said* would
  rewrite what a key was told to have applied to it — and the wizard offers v2,
  because it takes the newest version that is not retired.
- **The standard bootstrap procedure could never have completed on a real key.**
  It marked the key with `forcePINChange` at step 3 and then tried to create the
  resident credential at step 5 — but a key marked that way refuses its PIN for
  everything except changing it, so the credential step was always going to fail
  with "PIN not accepted". Found by running the procedure against a YubiKey
  5.7.4; `ykman fido info` says the same thing from the other side ("The FIDO PIN
  is disabled and must be changed before it can be used"). Every test had passed,
  because `MockWriter` did not model the rule. Fixed in three places so it cannot
  return: the mock now locks the PIN, `BootstrapTemplate::validate` refuses the
  ordering as `TemplateError::PinLockedBeforeUse` before a template can be
  stored, and the built-in procedure puts the forced change last. The refusal
  names both steps, because a template author needs to know which pair conflicts.
- **A run missing a required step is no longer reported as `Completed`.**
  `settle()` counts a skip as neither failure nor pending, so a required step
  that was skipped produced a run claiming success. That is not hypothetical:
  `piv-cert-import` is required by the standard procedure and skips on every run
  today, because the issuing CA is an open question — so a key with no signing
  certificate would have been recorded as a completed bootstrap. The executor now
  emits `bootstrap.incomplete` naming the steps, and the run is not `Completed`.

## [0.7.0] - 2026-08-11

Reaching the register where it actually lives, and handing the holder a document
they can sign. The MINOR slot because both add a capability, and because the
build gains platform-specific dependencies (`windows-sys`, macOS NetFS).

### Added

- **The register can live on an SMB share, and the application connects the share
  itself** ([`features/smb-share-hosting.md`](features/smb-share-hosting.md)). Every
  storage document here ended with "a real network share is still the recommendation",
  and until now nothing could act on it: the tool could only open a path somebody else
  had mounted, and the symptom of that not having happened was
  `… is not reachable — is the share mounted?` — a message that names the problem and
  offers nothing.
  - **Three identities, and the chosen one is used exactly.** *The account I am signed
    in with* is the default and, on **Windows**, the whole mechanism: a UNC path is
    authenticated by this session's own token, the same way Explorer's is, so the right
    implementation is to make no API call at all. *Guest* is anonymous access, chosen
    deliberately. *A named account* takes a `DOMAIN\user` and a password typed at the
    chooser. The signed-in user is **never** a silent fallback under a named account —
    connecting as an unexpected identity is a register opened with permissions nobody
    reviewed, and on a share that is read-only for everyone else it looks like lost
    writes.
  - **The password is never durable.** It is held in a `Secret` that keeps bytes,
    zeroes them on drop and prints as `Secret(********)`, so no `{:?}`, no `tracing`
    field and no panic message can carry it; it is readable only inside the crate, so
    no test can assert on one and no widget can echo one. It never reaches an argument
    vector either, which is precisely why the backends are native APIs
    (`WNetAddConnection2W` on Windows, `NetFSMountURLSync` on macOS) rather than
    `net use` and `mount_smbfs`: a password on a command line is readable by every
    process on the workstation, and a credentials file is the temporary file the
    security rules forbid.
  - **A share that is already mounted is used and left alone.** Every connector probes
    first, so a share mounted by Finder or a login script needs no credential — and is
    not unmounted when the register closes. A connection *this* session made is
    disconnected on every path that stops using the database: the Close button,
    switching database, and quitting the application.
  - **No drive letters.** Windows connections are deviceless, so the UNC path simply
    starts working; `Z:` meaning a different share on the next workstation is exactly
    the drift the register's location cannot have.
  - **The location does not have to be spelled one way.** `smb://server/share/…`,
    `cifs://…`, `\\server\share\…` and `//server/share/…` all parse, separators may be
    mixed, and `smb://DOMAIN%5Cuser@server/share` carries its user out. A `..` segment,
    an empty host or share, a control character or an over-long location is refused
    before it reaches a system call — a traversal would put the register somewhere on
    the file server nobody named.
  - **Audited**: `db.share.connected` names the share and the identity when the
    register opens on a freshly connected share; `db.share.disconnected` is written
    *before* the close, while there is still a database to write it to. Neither ever
    carries a secret. A refused connection cannot be audited — there is no open
    database — so it is logged and shown on the chooser, the same rule as a refused
    open.
  - **Remembered without its password**: the settings file keeps the share, the access
    mode and the user name, and the chooser offers them back with the password field
    empty. `--diagnose` reports how this build reaches a share and which shares this
    workstation has used.
  - **On Linux the gap is stated rather than papered over**: an unprivileged process
    cannot mount CIFS, and the alternatives are a `setuid` helper or a credentials
    file, so the refusal names `mount.cifs`, `/etc/fstab` and `autofs` — and a share
    that is already mounted is found and used normally.
  - `StoreConfig::with_location` lets a caller state the location instead of having it
    guessed from the path, because a share this application just connected *is* on a
    network filesystem whatever mount point the operating system chose. Guessing wrong
    would put a shared file in WAL mode, whose shared-memory sidecar cannot cross a
    network filesystem at all.
- **The consignment term exports as a PDF** — the sheet that is printed, signed and
  filed ([`features/consignment-terms.md`](features/consignment-terms.md) phase 7).
  *Export as PDF…* sits next to *Save as text…* in the term panel, and the Terms
  editor's preview exports too, so the wording can be circulated for the review it
  needs in the form the holder will actually read.
  - **No new dependency, and no TeX on a workstation.** `src/pdf.rs` writes the file:
    A4, Courier from the standard fourteen fonts so nothing is embedded, uncompressed
    and entirely ASCII so a filed artefact stays greppable. Courier is also the right
    font rather than a compromise — the term's numbered clauses and side-by-side
    signature rules are built out of spaces, and only a fixed-width font keeps them.
  - **The text and the PDF are the same document by construction**: both go through
    `term::render_term_parts`, so the copy reviewed on screen cannot disagree with the
    copy that gets signed. Line omission for an absent optional field applies to both.
  - **Every page's footer names the wording that produced it**
    (`consignment@2 (pt-BR) · #20423633 · TERM-2026-001`), so a signed sheet in a
    filing cabinet is traceable back to the exact template version in the database.
    A draft out of the editor says `@draft`.
  - **The signature block is never split across a page break.** A term one line too
    long for A4 would otherwise put the rules on page 1 and the names beneath them on
    page 2 — a holder signing a sheet that does not say what they are signing.
  - **A character the font cannot set is reported, not silently mangled.** The
    encoding is CP1252, which covers Portuguese, Spanish, English, French, German and
    Italian; a term in a language it cannot set warns the operator, before printing,
    exactly which characters would come out as `?` — and the text output still carries
    them correctly.
  - No personal data in the PDF metadata: a file's `/Title` and `/Subject` travel with
    it into mail clients and search indexes, and the body already says everything the
    document needs to say.
### Changed

- `term.saved` now records **which format left the tool** (`format=pdf path=…`), because
  the two outputs are filed differently — a signed PDF comes back as a scan, a text copy
  goes into a ticket — and "a term was written" was not enough to reconstruct what
  happened.

## [0.6.0] - 2026-08-11

The storage, cloud-sync, audit and CI halves of Wave 0. The MINOR slot because
the database schema moves to **v5** with a migration: a bootstrap run's steps
become rows.

### Added

- **The audit trail can be mirrored to segregated storage, and a divergence is an
  alert.** `StoreConfig::with_audit_mirror` points at a second location — ideally
  a share with an append-only ACL — and every entry is copied there *verbatim*:
  the same sequence, timestamp and hashes, not a second chain about the same
  events. That distinction is the feature. The database's triggers stop every
  ordinary edit, and a chain **rebuilt** consistently still verifies against
  itself — so the only thing that shows it changed is a copy the operator cannot
  rewrite. `Store::mirror_status()` compares the two and names the entry where
  they part. A mirror failure is logged at `error` and surfaced, never `let _ =`,
  and never fails the mutation: undoing a hand-over because a second file was
  unreachable would lose the fact being recorded.
  ([`features/audit-trail.md`](features/audit-trail.md) phase 2)
- **The audit chain is verified when the register is opened**, not only when
  somebody presses Verify — a broken chain found by chance is found too late.
  Bounded at 20 000 entries so a register with a year of history does not delay
  the first frame; past that the status reports *not checked* rather than
  implying it passed. ([`features/audit-trail.md`](features/audit-trail.md) phase 7)
- **The audit trail can be filtered** by event, actor, target and date range.
  "Everything that touched serial 20423633" and "every template change in June"
  are the questions an audit actually asks, and a flat list of the newest 500
  entries answers neither. The row limit applies *after* the filter, so a key
  whose events are old is still found. A filtered view says that it is filtered,
  because one that looks like the whole trail is how somebody concludes an event
  never happened. ([`features/audit-trail.md`](features/audit-trail.md) phase 6)
- **A second operator can read a locked register instead of being turned away.**
  `Store::open_read_only` opens with `SQLITE_OPEN_READ_ONLY` and takes no lock, so
  "who holds serial 20423633?" is answerable while somebody else is mid-hand-over.
  The refusal to write comes from SQLite rather than from a check in each of the
  twenty-odd methods that write — the same reasoning as the audit table's
  triggers, and one that cannot be forgotten by the next mutation added. A
  register needing a migration will not open this way, because migrating is a
  write: it is refused, naming both versions, rather than opened and misread.
  ([`features/cloud-sync-hosting.md`](features/cloud-sync-hosting.md) phase 8)
- **CI runs on every push and pull request**, and the coverage gate now gates.
  fmt, clippy with `-D warnings`, the no-default-features build, the full suite,
  and `cargo llvm-cov` against the 80% floor; a second job compiles on macOS,
  Windows and Linux including the native PC/SC and HID transports. It cannot
  *run* the hardware tests — a hosted runner has no reader — and the workflow says
  so, so a green matrix is not misread as hardware having been exercised.
  ([`features/testing-strategy.md`](features/testing-strategy.md) phases 5 and 10)
- **The tool takes its own backups, on a schedule, and rotates them.** Every
  storage document in this repository ended with "and a scheduled backup is still
  required", which was advice to an operator rather than something the tool did.
  Now `store::backup` takes a `VACUUM INTO` copy — daily by default, keeping the
  newest seven — named `<stem>.<YYYYMMDD-HHMMSS>.backup.sqlite3`, the shape the
  manual backup in Settings already used and which the sync-conflict detector
  already knew was ours. Rotation only ever deletes a filename it can parse as
  one of our backups: the folder next to the register also holds the register,
  its journal, its lock file and a sync client's conflict copies, and deleting
  the wrong one of those is unrecoverable.
  ([`features/storage-sqlite-single-file.md`](features/storage-sqlite-single-file.md) phase 6)
- **A register in a cloud-sync folder is copied before the session can write to
  it.** The cheapest answer to a fork that has already happened: if a sync client
  resolved a clash by keeping both copies, the side this workstation is about to
  overwrite otherwise has no copy at all. Taken at open rather than at the first
  write — earlier, and with no bookkeeping to get wrong about whether this
  session has written yet. A failure is logged loudly and never stops the
  register opening.
  ([`features/cloud-sync-hosting.md`](features/cloud-sync-hosting.md) phase 7)
- **The spreadsheet this tool replaces can be imported.** Preview first, always:
  `store::import::plan` reads the file, decides what each row would do and returns
  it as data with nothing written, so the operator sees "12 new keys, 3 already
  known, 1 refused: `ABC123` is not a serial number" before agreeing to anything.
  Reads what a spreadsheet actually produces — semicolon separators from a
  decimal-comma locale, a UTF-8 BOM, accented and spaced headers in Portuguese or
  English, quoted cells containing the separator, and serials decorated with a
  leading apostrophe or thousands separators. An imported serial is
  `manual-entry` provenance, because nobody has touched that key, and it never
  downgrades one already read from hardware. Distributions are deliberately not
  imported — a hand-over needs a date, a method and an operator that a
  spreadsheet rarely records, and inventing them would fabricate custody
  evidence. A file with no unit column imports its keys and none of its people,
  said once rather than repeated per row, because a holder's unit reaches the
  `OU=` of a signing certificate and cannot be guessed.
  ([`features/storage-sqlite-single-file.md`](features/storage-sqlite-single-file.md) phase 8)
- **The database can live in a OneDrive folder, and two operators can share it —
  one at a time.** A synchronising folder is the worst place for a SQLite file, and it
  is where a real installation keeps the register, because it is the shared folder that
  unit has. Detecting it and warning (v0.4.0) managed nothing. So such a path is now its
  own location, `Location::CloudSync`, with a cooperative **single-writer lock**
  ([`features/cloud-sync-hosting.md`](features/cloud-sync-hosting.md)):
  - opening **waits for the sync client** to stop changing the file, so the connection
    is never opened on a half-downloaded database (bounded, reported, and tunable with
    `$YKDM_SYNC_QUIET_MS` / `$YKDM_SYNC_TIMEOUT_MS`);
  - a **`<database>.lock`** file next to the database records who has it — operator,
    workstation, pid, run, and when it was taken and last refreshed. A second
    workstation is **refused by name** rather than allowed to write to a copy the sync
    client will resolve by keeping both;
  - the lock is refreshed every minute while the session lives, and a session that
    finds the lock taken from it **closes the database** instead of writing;
  - closing releases in the right order: audit entry, connection closed, **wait for the
    upload**, then remove the lock — so the next workstation cannot start from a file
    still on its way;
  - the pragmas are the network share's (rollback journal, `synchronous=FULL`), because
    WAL's `-wal`/`-shm` sidecars cannot survive a sync client.
- **A sync conflict is reported instead of going unnoticed.** Copies a client left
  because it could not merge (`keys (1).sqlite3`, `…conflicted copy…`) are found next to
  the database and surfaced in the status line, in Settings, in `--diagnose` and in the
  audit trail (`db.sync.conflict_copies`) — the register may have forked, which is the
  failure this location is dangerous for. Our own backups and lock file are not mistaken
  for them.
- **Taking over an abandoned lock**, deliberately. A lock unrefreshed for fifteen
  minutes is *still* refused: only the operator can know the other machine is switched
  off rather than mid-hand-over. The chooser then offers *Take the lock over*, names who
  was holding it, and records the break in the audit trail (`db.lock.taken_over`).
- `--diagnose` gains a **`database lock:`** line, read without ever taking a lock of its
  own, plus the conflict-copy alarm. Settings shows the lock, the holder and the lock
  file; the status bar says `db: cloud-sync (locked)`.

### Fixed

- **The signature block of a term now lines up for any holder's name.** A gap of two or
  more spaces in a term template is a *column* — the shipped wording puts the rule, the
  name under it and the role under that all at column 41 — and substituting a name of
  any length other than the fifteen characters of `{{holder.name}}` slid the second
  column with it. The template was never wrong; the renderer was throwing away the
  geometry the template declared. A gap that follows a substitution is now resized to
  put what comes after it back where the template put it, never shrinking below one
  space. Applies to both outputs, since both come from one rendering. Line omission
  remains the only *logic* in a term: this is layout, and it makes the spaces an author
  already typed mean what they look like.

### Changed

- **`make coverage-core` is now actually a gate.** It was labelled "THE GATE" and
  documented as the 80% floor, but it only printed a summary and exited 0 — so a
  change that dropped core coverage below the floor passed `make release-check`
  unnoticed. It now passes `--fail-under-lines`, and the build agrees with
  AGENTS.md §4 that such a change is not ready.
- **A bootstrap run's steps are rows, not a JSON blob** (schema **v5**,
  `bootstrap_run_steps`, with a migration). The blob was the right shape while a
  run was written once and read back whole, and the wrong one for what comes
  next: step-level reporting becomes a `GROUP BY` instead of a parse of every
  run; the Wave 1 executor writes one step outcome at a time, and rewriting a
  blob per step means an interrupted run loses the steps that had already
  succeeded; and the rows store the same readable strings as the rest of the
  schema (`fido2-pin`, `done`) rather than serde's variant names, so the file
  stays answerable from a SQL console. The backfill runs in Rust rather than SQL
  so the mapping is `StepKind::slug` itself and cannot drift; a step list that
  cannot be parsed leaves that run with no steps and an `error` log line, because
  refusing to open the register over one historical run would trade a partial
  record for no record.
- **Closing a database is now a protocol, not a drop.** Every path that stops using one
  — the Close button, switching database, creating another — writes its audit entry
  while the connection is still open, then closes the connection and releases the lock.
  `Store::close` returns what the wait for the sync client achieved.
- A lock is identified by the **run** that took it, not by host and pid: pids are reused,
  so a lock left behind by a dead session could otherwise have been silently adopted by
  a later one — the exact two-writer case the lock exists to prevent.

### Added

- **An application icon.** A box truck whose cargo panel is a YubiKey — keyring
  slot, gold touch contact, USB contacts — drawn once in
  [`assets/logo.svg`](assets/logo.svg) and rendered by `make icons` into the window
  and dock icon, the macOS `.icns` the bundle already looked for, and the PNGs the
  documentation uses. The mark carries no text, so it needs no translation, and it
  is deliberately generic: replacing it with an institution-issued asset is one SVG and one
  command. Until now `cargo run` and the bundle both showed the platform
  placeholder, and an application you identify by reading its title bar is one you
  mis-click during a hand-over. See
  [`features/application-icon.md`](features/application-icon.md).
- **`make icons`** — `assets/render-icons.sh` renders every raster size from the
  SVG (PNG 16–1024, the macOS `.icns`, and the RGBA blob the binary embeds) and
  fails if the blob is not the byte count it should be. The generated files are
  committed on purpose: `make bundle` has to produce an icon on a machine with no
  rasteriser, and `include_bytes!` needs the blob at compile time. Edit the SVG,
  run the target, commit both.

### Changed

- The window icon is embedded as a raw RGBA blob rather than a PNG, because
  decoding a PNG would mean the `image` crate — an *optional* dependency behind the
  `barcode` feature — and the icon has to be present in every build, including
  `--no-default-features`. A blob whose length disagrees with its declared size
  costs a generic icon and an `error` log line, never the launch.
- `packaging/macos/bundle.sh` no longer explains why there is no logo. The icon is
  still optional there: a bundle without the file is valid, just generic.

## [0.5.0] - 2026-08-11

### Added

- **A Templates screen: the bootstrap procedure is now editable in the
  application.** Templates have always been data
  ([`features/bootstrap-templates.md`](features/bootstrap-templates.md) phase 2), but
  the only way to change one was to edit Rust. The new screen lists every template
  version on record and lets an operator **add** a template (from nothing or by
  duplicating an existing procedure), change its name, description and steps —
  including which steps are enabled, which are required, their order, their ids and
  their `name = value` parameters — and **withdraw** one. Adding a step fills in the
  parameters that step kind reads, so a hand-built template plans on the first try.
  The Bootstrap screen gains a *Manage templates…* button that opens this screen on
  the template the wizard has selected.
- **A live verdict on the draft.** The editor shows `plans` or the exact refusal,
  because `BootstrapTemplate::check` runs a real `plan()` against a fictitious
  holder and key (`RenderContext::sample`). An unknown `{{variable}}`, a step
  missing a parameter it needs, a duplicate step id or a template with nothing
  enabled is refused **at the desk**, and the same gate guards the database — so a
  procedure that cannot be planned cannot be stored. Steps that arrive disabled are
  checked too: the wizard can enable an optional step on any run.
- **Retiring a template**, and reinstating it. A version a bootstrap run recorded
  cannot be deleted — a run saying it applied `org-standard v1` with no
  `org-standard v1` to look up is not a record — so it is *retired* instead:
  withdrawn from the wizard, kept in the database, and **not resurrected** by the
  built-in seeding that runs on every open. New audit events `template.created`,
  `template.changed`, `template.retired`, `template.reinstated`,
  `template.removed`; each entry carries the id, version, previous version, step
  count and run count, never the procedure text.
- **Removing a template version outright**, behind a confirmation, for a procedure
  typed by mistake. `Store::delete_template` refuses a version any run recorded and
  a version this build ships (that one would come back on the next open), and both
  refusals name retirement instead (`StoreError::TemplateInUse`). The Remove button
  is disabled with the reason on it rather than offering an action that will be
  refused.

### Changed

- **An edit of a template stores a new version**, numbered by the database, exactly
  as the Terms screen already did for the consignment wording: the version a run
  recorded is never overwritten, and two workstations editing the same template
  cannot both produce "version 2". The numbering rule now lives in one place,
  `versioning::next_version`, shared by terms and templates.
- **The bootstrap wizard offers the newest version of each template**, rather than
  every stored version. Older versions stay in the database because runs refer to
  them; offering one for a *new* run would be offering a superseded procedure. A
  database with no template at all still falls back to the built-ins — but a
  database where every template has been *retired* now offers none, because that
  was a deliberate decision.
- Schema **v4**: `templates.retired_at`, with a migration. A template id is
  restricted to lower-case letters, digits and hyphens (`template::check_id`), since
  it is what a bootstrap run records.

### Removed

- **The application is no longer branded to one institution.** Every occurrence of
  the previous organisation's name is gone from the code, the tests, the packaging
  and the documentation:
  - The built-in procedure is now **`org-standard` / "Organisation standard
    bootstrap"** (`BootstrapTemplate::org_standard`), and its description
    interpolates `{{org}}` instead of naming an institution. **On upgrade:** the new
    id is seeded beside the previously shipped one, so a database opened by an older
    build shows both in the Templates screen — retire the older entry, which keeps
    the runs that recorded it explainable. Nothing is renamed under an existing run's
    feet.
  - The default organisation is now the placeholder `UNSET-ORGANISATION`
    (`app::DEFAULT_ORG`) and Settings warns while it is unset, because `{{org}}`
    reaches the PIV certificate subject and the FIDO2 relying-party id — that value
    belongs to the unit running the tool, not to this build.
  - Sample and test data use `example.org`, "Example Organisation" and unit `IT`;
    the macOS bundle identifier is `org.example.yk-dist-manager` (override with
    `YKDM_BUNDLE_ID`).
  - The security rules are unchanged and still cite **NRM** and **G-002**; they are
    now described as the institutional norm and its secure-systems guide rather than
    named to an organisation. The compliance spec is now
    [`features/compliance.md`](features/compliance.md).

## [0.4.0] - 2026-08-11

### Added

- **An observation on every key.** The `notes` column has existed since schema v1
  with nothing in the GUI to fill it; the Inventory screen now writes it. The
  intake panel carries an *Observation (optional)* field that is stored with the
  serial and **kept for the next serial added**, so a whole box shares one note,
  and every row has an `observation…` action that opens an editor for a key already
  on record. The observation is what no device can supply — the shipment it arrived
  in, a bent connector, why a key is being held back — is bounded by
  `domain::MAX_NOTE`, survives a device re-read untouched, and is never a place for
  a secret. New audit event `key.note_changed`; `key.added` now also records
  `note_chars=<n>`.
- **Removing a registered key, behind a confirmation.** A row action asks, and a
  panel says what goes (the inventory row and its observation), what stays (the
  audit trail, which no code path can edit), and what the alternative is
  (retirement, which keeps the record) before anything is deleted. Removal is for a
  mistake at intake — a mis-typed serial, a label scanned twice — so
  `Store::delete_key` **refuses** a serial that any hand-over or bootstrap run
  refers to (`StoreError::HasHistory`), naming the counts and pointing at
  retirement; the Inventory panel shows that refusal before the operator clicks
  rather than after. New audit event `key.removed`, carrying the status, provenance,
  model and observation length of the row that was removed.

### Changed

- One spelling for a status and a provenance, stored and audited:
  `KeyStatus::audit_name` / `SerialSource::audit_name` are now the source of the
  snake_case names, and `store::key_status_str` / `store::serial_source_str`
  delegate to them. The third copy of that mapping (in `app.rs`) is gone.

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
  rendering, structural validation, and two built-ins (`org-standard`,
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

[Unreleased]: https://github.com/ffquintella/yk-dist-manager/compare/v0.7.0...HEAD
[0.7.0]: https://github.com/ffquintella/yk-dist-manager/compare/v0.6.0...v0.7.0
[0.6.0]: https://github.com/ffquintella/yk-dist-manager/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/ffquintella/yk-dist-manager/compare/v0.4.0...v0.5.0
[0.4.0]: https://github.com/ffquintella/yk-dist-manager/compare/v0.3.0...v0.4.0
[0.3.0]: https://github.com/ffquintella/yk-dist-manager/compare/v0.2.2...v0.3.0
[0.2.2]: https://github.com/ffquintella/yk-dist-manager/compare/v0.2.1...v0.2.2
[0.2.1]: https://github.com/ffquintella/yk-dist-manager/compare/cdea137...v0.2.1
