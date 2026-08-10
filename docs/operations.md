# Operations

Runbooks for the people using the tool.

## Install and first run

Requirements per platform (the native transports link against system libraries):

| Platform | Needs |
|---|---|
| macOS | Nothing extra — PC/SC is a system framework |
| Windows | The **Smart Card** service running |
| Linux | `pcscd` running, `libpcsclite`, and a udev rule granting your user access to the YubiKey HID device |

For the `ykman` fallback, `ykman` 5.x must be on `PATH`
(`brew install ykman`, or your distribution's package).

First run:

```bash
cargo run                                  # or the packaged application
```

Set the operator name and organisation in **Settings**. Check that the status bar shows the
database path and whether it is local or on a share.

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
