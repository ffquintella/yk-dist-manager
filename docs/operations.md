# Operations

Runbooks for the people using the tool.

## Install and first run

Requirements per platform (the native transports link against system libraries):

| Platform | Needs |
|---|---|
| macOS | Nothing extra for smartcards — PC/SC is a system framework. Camera scanning needs camera permission; a bundled app must declare `NSCameraUsageDescription` |
| Windows | The **Smart Card** service running |
| Linux | `pcscd` running, `libpcsclite`, a udev rule granting your user access to the YubiKey HID device, and — for camera scanning — read access to the V4L2 device (usually the `video` group) |

Camera scanning is compiled in by default. For a build with no camera code at all:

```bash
cargo build --no-default-features --features file-dialog
```

For the `ykman` fallback, `ykman` 5.x must be on `PATH`
(`brew install ykman`, or your distribution's package).

First run:

```bash
make run          # from a source checkout
```

On macOS, build and launch the **bundled application** if you want camera scanning:

```bash
make bundle           # assembles target/bundle/YubiKey Distribution Manager.app
make verify-bundle    # checks the bundle is what macOS needs
make run-bundled      # launches it
```

To see what a build is and what it can reach — the first thing to attach to a support
request:

```bash
make diagnose         # or: yk-dist-manager --diagnose
```

It prints the version, the features compiled in, whether macOS sees an app bundle,
whether the camera is authorised and which cameras exist, the database and settings
paths, and whether `ykman` is on `PATH`.

Set the operator name and organisation in **Settings**. Check that the status bar shows the
database path and whether it is local or on a share.

## Choosing, creating and switching databases

The database screen appears whenever nothing is open. From it you can:

- **open a recent database** — the list marks each entry *available* or *not reachable*,
  so an unmounted share is obvious;
- **type or paste a path** — the way to reach a UNC path (`\\server\ti\keys.sqlite3`)
  or a share the dialog will not browse;
- **Open** an existing file, or **Create** a new one. These are separate on purpose:
  opening a path that does not exist is an error, and creating over an existing file is
  refused. A mistyped share path can no longer produce an empty database that looks like
  every record having vanished;
- **Choose file… / New file…** for the native dialog (needs the `file-dialog` feature,
  on by default).

Once open, **Settings → Switch database…** closes the current one and brings the chooser
back. The last database used is reopened at the next start, unless `$YKDM_DB` says
otherwise.

The recent list and the operator identity live in `settings.json` in the per-user data
directory (`$YKDM_SETTINGS` overrides it). **It never holds the database password** — it
sits next to the database, so storing one there would defeat encrypting it.

## Choosing where the database lives

```bash
# A shared file for the whole unit
YKDM_DB=/Volumes/ti-share/yubikeys/yk-dist-manager.sqlite3 cargo run

# A local file (the default)
#   macOS:   ~/Library/Application Support/yk-dist-manager/yk-dist-manager.sqlite3
#   Linux:   ~/.local/share/yk-dist-manager/yk-dist-manager.sqlite3
#   Windows: %APPDATA%\yk-dist-manager\yk-dist-manager.sqlite3
```

A path under `/Volumes/`, `/mnt/`, `/net/`, `/media/`, or a Windows UNC path (`\\server\…`)
is detected as a share and opened in rollback-journal mode with `synchronous=FULL` and a
20-second busy timeout. Settings shows which mode is active — check it once when you set up a
new location, because the heuristic can be wrong.

**On a share:** two operators can use the file, but they are serialised. If someone else is
writing, you will see a busy message rather than a corruption.

### In a cloud-sync folder (OneDrive, Dropbox, Google Drive, iCloud)

Not the recommended place, and the place several units actually have. Two things make it
dangerous:

- the sync client copies the file while a writer holds it open;
- a conflict is resolved by keeping **both** copies rather than merging — so the failure
  mode is two divergent registers of who holds which key.

The application recognises those paths, opens them with the conservative journal mode
(never WAL), and — because a sync client offers no locking of its own — **serialises
access with a lock file**. What that means at the desk:

1. Opening the database waits a moment for OneDrive to finish downloading it, then
   creates `yk-dist-manager.sqlite3.lock` beside it.
2. While you have it open, **another computer cannot open it**. That operator sees your
   name, your workstation and when you took it, instead of silently working on a copy.
3. **Close the database (or the application) when you are done** — Settings → *Switch
   database…*, or just quit. Closing waits for the upload to finish and then removes the
   lock. Leaving the application open all afternoon keeps everybody else out.
4. If a computer crashed or was switched off with the database open, its lock stays
   behind. After fifteen minutes without a refresh the chooser offers **Take the lock
   over** — use it only when you know nobody is working in the register, because two
   people writing is exactly what produces two divergent copies. Who was holding it goes
   into the audit trail.
5. Settings and `--diagnose` show the lock state (`database lock:`). If a sync conflict
   copy appears next to the file (`yk-dist-manager (1).sqlite3`, `…conflicted copy…`),
   both places report it as an alarm: the register may have forked, and the copies have
   to be compared before either is trusted.

On a slow link the wait can be tuned: `$YKDM_SYNC_QUIET_MS` (how long the file must be
unchanged before it counts as downloaded, default 1500) and `$YKDM_SYNC_TIMEOUT_MS` (how
long to wait before carrying on and saying so, default 15000).

The lock binds workstations running this tool. It cannot bind a machine that ignores it,
and neither operator can see anything while offline. It removes the common accident, not
the risk: a real network share, or a local file with a scheduled backup, is still the
better home for the register.

**With a password:** run a build with `--features encrypted-db`; the app prompts at startup.
Note that everyone shares the one password — it is confidentiality at rest, not per-operator
access control.

---

## Runbook: import the spreadsheet you keep today

Most units already have a register: a spreadsheet with a serial column, a name
column and an e-mail column. It can be imported rather than retyped.

1. Export it as CSV. A semicolon-separated export from a pt-BR locale is fine, as
   is a UTF-8 file with a byte-order mark.
2. **Preview first — always.** The import reads the file and reports what it
   *would* do without writing anything: "12 new keys, 3 already known, 1 refused:
   `ABC123` is not a serial number", with the line number a spreadsheet shows.
3. Read the refusals, fix the spreadsheet, preview again. Importing twice is safe:
   serials and e-mail addresses are unique, so a second pass refreshes rather than
   duplicates.

Recognised headers, matched ignoring case, accents and separators:

| Field | Accepted names |
|---|---|
| Serial | `serial`, `serial number`, `SN`, `número de série`, `nº série` |
| Name | `name`, `full name`, `holder`, `nome`, `portador`, `responsável` |
| E-mail | `email`, `e-mail`, `mail`, `correio` |
| Unit | `unit`, `department`, `unidade`, `setor`, `lotação` |
| Model | `model`, `modelo`, `type`, `tipo` |
| Notes | `notes`, `observação`, `observações` |

Anything else is listed as ignored rather than silently dropped.

Two deliberate limits:

* **A file with no unit column imports its keys and none of its people.** A
  holder's unit reaches the `OU=` of the signing certificate this tool puts on the
  key, and a guessed unit means a certificate naming a department the person is
  not in.
* **Hand-overs are not imported.** A distribution record needs a date, a delivery
  method and the operator who performed it, and a spreadsheet rarely has all
  three. Importing one anyway would fabricate custody evidence. Record who holds
  what through the Distribution screen, which asks for the facts a hand-over
  needs.

An imported serial is marked **manual entry** — nobody has held that key, so it is
a claim, not a fact. Reading the key later upgrades the provenance, and an import
never downgrades a serial already read from hardware.

## Runbook: receive a shipment

For getting serials in without unboxing every key:

1. **Inventory → Add by serial / scan…**
2. With a **USB barcode scanner** (recommended): click into the field, scan the box
   label, and the scanner types the serial and presses Enter. Repeat. Nothing else to
   configure.
3. With the **camera**: *Start camera*, hold the label about 20cm away filling the frame
   width, and confirm the decoded serial. A laptop camera is fixed-focus and will
   struggle closer than that.

   **On macOS this needs the bundled application.** Run from `cargo run` or a bare
   binary, the panel refuses with an explanation: there is no `Info.plist`, so nothing
   declares `NSCameraUsageDescription`, and attempting the capture would abort the
   process rather than fail. Use the wedge or type the serial until the bundle exists
   (`features/packaging-and-release.md`). `YKDM_ALLOW_UNBUNDLED_CAMERA=1` forces an
   attempt if you have arranged access another way — it may abort.
4. Or just type the serial.

Keys recorded this way are marked **not verified**: no model, no firmware, no application
list, because nobody has plugged them in. Reading the key later — during bootstrap —
upgrades the record and fills in the hardware detail. A serial that was verified is never
downgraded by a later scan.

If the tool refuses a scan: two different serials in shot are rejected rather than guessed
(scan one label at a time), and a barcode that is not a serial says so.

## Runbook: distribute a key

1. **Inventory → Read attached key.** Confirm the serial matches the engraving.
2. **Holders** — register the person if they are not there. The corporate e-mail matters:
   it goes into the signing certificate.
3. **Bootstrap** — pick the key, the holder and the template; **Build plan**; read the plan.
   Check the transport column and any notes (a firmware gate may skip a step).
4. Run it *(Wave 1; today: **Record dry run**)*.
5. **Distribution** — select the key and holder, set the delivery method, put the signed
   term's reference in *Receipt*, leave *Attach the most recent bootstrap run* ticked, and
   **Record distribution**.
6. Confirm the Distribution table shows the hand-over with what was applied.

If the key status refuses to advance ("illegal status transition"), the key was never marked
bootstrapped — that is the guard working, not a bug.

## Runbook: the consignment term

1. **Distribution** → the row → **term**.
2. Pick the language. pt-BR and en ship; a unit can add others. If the requested language
   has no template, the panel says which one it used instead.
3. Review the rendered term. It is built from the record: the holder's name and
   identification number, the key's serial, what the bootstrap applied, the custody
   statement, the delivery method and the operator. Optional fields the holder did not
   give (phone, address) take their whole line with them rather than printing an empty
   label.
4. **Save as text…**, print it, and have it signed.
5. When the signed copy comes back: **Upload signed term…** (or *upload* on the row).
   PDF, PNG, JPEG or TIFF, up to 8 MiB. It is stored **in the database** with a SHA-256,
   so copying the database copies the evidence.
6. The row's badge turns from `none filed` (amber) to `n filed` (green). *export* writes a
   filed document back out, verifying the digest first and refusing on a mismatch.

The built-in term wording is a **draft**: it needs review by whoever owns the term at your
institution, and the data-protection paragraph needs the DPO. Templates are data, so that
review is an edit rather than a code change.

## Runbook: record a return

1. **Distribution** → find the open record → **record return**.
2. The key moves to `Returned`. It is **not** ready for reuse: the previous holder's
   certificate is still valid and their FIDO credentials are still on the key.
3. Before reissuing: revoke the old certificate, reset the applets you are reusing, and
   record it. (Automated in
   [`../features/key-lifecycle-and-revocation.md`](../features/key-lifecycle-and-revocation.md);
   until then, do it manually and note it.)

## Runbook: a key is lost or stolen

Treat it as a possible credential compromise, not an inventory problem.

1. **Inventory → mark lost.** Record when and who reported it.
2. Find what was on it: **Distribution** → the record → its bootstrap run. That tells you the
   certificate to revoke and the credentials to remove.
3. **Revoke the PIV certificate** at the issuing CA with reason `keyCompromise`.
4. **Remove the FIDO2 credential(s)** from the relying party.
5. Check for other dependencies: SSH authorised keys, a challenge-response slot used to
   unlock something.
6. **Report to the ESI.** A possible credential compromise is an incident under NRM §5.4.4.
7. Keep the record. A lost key is never deleted from the inventory.

## Runbook: backup

**The application takes backups itself**, and has done since the automatic-backup
work landed. `VACUUM INTO` produces a consistent copy even while others are
connected, and the copy is a complete, standalone database.

| | Default | Where |
|---|---|---|
| How often | daily | `BackupPolicy::every` |
| How many kept | 7 | `BackupPolicy::keep` |
| Extra copy before a session writes | on, for cloud-sync folders only | `BackupPolicy::before_first_write` |

Copies are named `<stem>.<YYYYMMDD-HHMMSS>.backup.sqlite3` next to the register.
Rotation deletes **only** filenames it can parse as one of ours: the same folder
holds the register, its journal, its lock file and any conflict copies a sync
client left, and deleting the wrong one of those is unrecoverable.

A register in a **cloud-sync folder** is also copied when it is opened, before
this session can write anything. If a sync client has already resolved a clash by
keeping both copies, the side this workstation is about to overwrite would
otherwise have no copy at all. Comparing two divergent registers is still a human
job — the tool will not merge them.

**Settings → Backup next to the database** still takes one on demand.

Recommended in addition: a backup before any upgrade that changes the schema, and
before a password change. An encrypted database's backup needs the same password —
do not lose the password and keep the backups thinking you are covered.

Restore is a file copy. Afterwards, open the copy and run **Audit → Verify chain** plus
**Settings → Integrity check**.

## Runbook: the database will not open

| Symptom | Cause | Action |
|---|---|---|
| Password prompt on a file you thought was plain | It is encrypted, or corrupt | Try the password; if none exists, restore a backup |
| "this build has no encryption support" | Plain build, encrypted file | Use a build with `--features encrypted-db` |
| "schema version N is newer than this build supports" | Someone upgraded first | Upgrade this workstation; do **not** try to force it open |
| "database is locked" / busy | Another operator is writing | Wait; if it persists, check for a stale lock or a crashed session |
| `integrity_check` reports anything but `ok` | Real corruption | Restore the most recent backup, then verify the audit chain |
| "no database file at …" | The path does not exist (typo, or an unmounted share) | Fix the path or mount the share; use **Create** only if you really want a new, empty database |
| "a file already exists at …" | **Create** was used on an existing file | Use **Open** instead |

## Runbook: no key detected

1. Is exactly one key plugged in? Two attached keys are refused deliberately.
2. Is the smartcard service running (Windows *Smart Card*, Linux `pcscd`)?
3. Does `ykman list --serials` see it? If `ykman` sees it and the tool does not, that is a
   transport bug worth reporting with the log.
4. On Linux, check the udev rule for HID access.
5. Is another application holding the reader exclusively (a browser mid-WebAuthn, GnuPG's
   scdaemon)? Close it and retry.

## Health checks worth doing periodically

- **Audit → Verify chain.** It should report every entry verified. Anything else is
  investigated immediately, not next week.
- Hand-overs with **no signed term filed** — the badge on the distribution table.
- Keys still marked **not verified** — recorded from a label and never read.
- **Settings → Integrity check** → `ok`.
- Open distributions with no attached bootstrap run: keys handed out without recorded
  evidence.
- Keys in `Distributed` whose holder has left.
- Certificates approaching expiry (report planned in
  [`../features/reports-and-export.md`](../features/reports-and-export.md)).

## Logs

Currently written to stderr, so launch from a terminal when diagnosing:

```bash
YKDM_LOG=debug cargo run 2> ~/ykdm.log
```

Format: `[dd/mm/aaaa] hh:mm:ss ; evento ; nivel=… detalhes`. A file sink and an in-app log
panel are planned ([`../features/logging.md`](../features/logging.md)).
