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
cargo run                                  # or the packaged application
```

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
writing, you will see a busy message rather than a corruption. Do not put the file on a
filesystem that does not support locking (some cloud-sync folders do not — a synced folder is
the one place *not* to put it, since sync conflicts produce duplicate files, not merges).

**With a password:** run a build with `--features encrypted-db`; the app prompts at startup.
Note that everyone shares the one password — it is confidentiality at rest, not per-operator
access control.

---

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

**Settings → Backup next to the database** runs `VACUUM INTO`, producing a consistent copy
even while others are connected. The copy is a complete, standalone database.

Recommended: a backup before any upgrade that changes the schema, before a password change,
and on whatever periodic schedule the unit's share backup already provides. An encrypted
database's backup needs the same password — do not lose the password and keep the backups
thinking you are covered.

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
