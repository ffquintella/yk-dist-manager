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

The native transports are compiled in by default. `ykman` is the **fallback** — install
`ykman` 5.x on `PATH` (`brew install ykman`, or your distribution's package) so a
workstation whose reader does not answer still has a path to the hardware, and so the
management-applet fields (form factor, capabilities, FIPS state) can be read at all.

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

**The same report is in the application**: click the version badge beside the product
name in the top bar. *Copy the report* puts it on the clipboard, which is the quickest
way to answer "which build, on what machine?" without asking somebody to open a
terminal.

Set the operator name and organisation in **Settings**. Check that the status bar shows the
database path and whether it is local or on a share.

### Which transport is reading the hardware

The status bar says: `via: native` reads keys in process, `via: ykman` shells out to the
command, and `via: none` (amber) means nothing on this machine can reach hardware —
the register still works, and keys can still be recorded by serial from a barcode or by
hand.

The choice is made once at startup, by *asking* rather than by inspecting the build:
the native transports are compiled in by default, and a build that has them still falls
back to `ykman` when no reader answers, because PC/SC exists on machines where the
service is stopped. The fallback says which service to check.

`ykman` therefore matters as a fallback rather than as the normal path. A build that
deliberately excludes the native transports — for a workstation with no smartcard
service, or no HID permission — is still supported:

```bash
cargo build --no-default-features --features file-dialog,camera
```

**Settings → Device transport** overrides it. Reach for it in two situations:

* the transports disagree about the same key and you are working out which is right;
* something else on the workstation is holding the reader.

The override is honoured even when the probe disagrees — an application that quietly
overrules the person diagnosing it is an application that cannot be diagnosed — but the
card reports what is *actually* in use, so a forced choice that cannot work reads as
forced and failing rather than as working. Changing it restarts device detection and is
recorded as `device.transport.selected`; the trail therefore says which transport was
live when a key was prepared.

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

### Connecting the share from the application (SMB)

The share does not have to be mounted before you start. The chooser has an
**Open from a network share (SMB)** card:

1. **Say where the register is**, including the file inside the share:
   `smb://fileserver/ti-share/yubikeys/yk-dist-manager.sqlite3`. Windows UNC
   (`\\fileserver\ti-share\yubikeys\yk-dist-manager.sqlite3`) and
   `//fileserver/ti-share/…` are accepted too — paste whichever form your platform gave
   you. A share on its own is refused: it names no register.
2. **Choose the identity.**
   - *The account I am signed in with* — the default. On **Windows** this is the whole
     mechanism: the share is opened with this session's own credentials, exactly as
     Explorer does, and no password is asked for or sent. On **macOS** it uses the
     Keychain entry macOS already holds for that server.
   - *Guest* — for a NAS share that allows anonymous access. Deliberate, and the status
     line says `guest` for as long as the register is open.
   - *A named account* — `DOMAIN\user` plus a password. **The password is typed every
     time.** The share and the user name are remembered so only the password has to be
     retyped; the password itself is never written to the settings file, the database, a
     log or the audit trail.
3. **Connect and open**, or **Connect and create** for a new register. The two stay
   separate for the same reason they do for a local file: a mistyped file name must be a
   refusal, not a second empty register that looks like total data loss.

Notes worth knowing at the desk:

- **A share that is already mounted is used as it is** — and is *not* unmounted when you
  close the register. If you mounted it in Finder for your own work, it stays.
- **A share this application connected is disconnected when you close the register**, or
  switch database, or quit. Settings shows which share is held, as whom, and offers
  *Close and disconnect the share*.
- If the identity is refused you are told which of three things happened: the user name
  or password was wrong, the account was accepted but may not use the share, or the
  server or share name could not be reached. They need three different actions.
- On Windows, `ERROR_SESSION_CREDENTIAL_CONFLICT` means Windows already has a connection
  to that server as somebody else. Close it first (`net use \\server\share /delete`), or
  use the signed-in account.
- **On Linux** an unprivileged process cannot mount a CIFS share, so the application does
  not try: ask for it in `/etc/fstab` (`mount.cifs`) or from an `autofs` map, and the
  application will find and use the mount. It looks in the mount table, and under `/mnt`,
  `/media` and `/net`.
- `--diagnose` reports how this build reaches a share, and which shares this workstation
  has used (never a password — none is stored).

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
   A lock that is **still being refreshed** can be taken too, and the card asks for more
   before it lets you: first *Try again*, then a tick confirming that nobody is working
   in the register, and only then the red button. Use it for a window you cannot get back
   to — a workstation left on a locked screen, or this application no longer responding
   in another window of your own machine — not for a colleague you have not asked. The
   session that loses the lock finds out within a minute and closes the register rather
   than writing to it, so nothing is corrupted; what it cannot undo is a hand-over it was
   halfway through recording. The audit entry says the lock was live when it was taken.
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
better home for the register — and the share can now be reached from the chooser
(above), which is what makes that a recommendation somebody can act on.

**With a password:** run a build with `--features encrypted-db`; the app prompts at startup.
Note that everyone shares the one password — it is confidentiality at rest, not per-operator
access control.

### Runbook: set, change or remove the database password

**Settings → Password protection**, in a build with `--features encrypted-db`. The
same operation covers all three: the register is exported into a new file under the
new key, the copy is verified, and only then does it take the original's place.
Nothing is ever re-keyed in place, so an interruption leaves the register as it was.

1. **Write the password down where the unit keeps its passwords, first.** There is
   no recovery, no reset and no administrator. A password nobody can produce is a
   register nobody can read.
2. Type it twice. It is graded as you type: at least 12 characters, and a
   passphrase of several unrelated words beats a short password with symbols in it
   — the threat is a copied file attacked offline, where length is the only thing
   that buys time.
3. The register is backed up before it is rewritten, and reopened under the new
   password afterwards. Both are automatic.
4. **Older backups keep the password they were taken with.** A password change does
   not reach copies already on disk — label them, or take a fresh backup after the
   change and prune the old ones deliberately.

Removing the password asks for a separate confirmation, because what it does is
make the whole register — every serial, every holder's name, e-mail and unit, and
the audit trail — readable to anybody who can read the file.

If the change itself fails, the register is untouched and the application returns
to the chooser: reopen with the password it had. That is not a lost register, it is
this session having handed its connection over to the swap.

**Wrong passwords at the prompt** are slowed down: three free attempts, then a
delay that doubles up to thirty seconds, counted down on screen. It is not a
lockout — there is nobody to unlock it, and there is no limit on attempts. The
delay only makes guessing at the prompt pointless; it does nothing about a copied
file, which is what the length floor is for.

---

## Runbook: share a bootstrap procedure with another unit

**Templates → Share a procedure with another unit.** A procedure crosses between two
installations as one readable JSON file instead of being retyped, which is the one
transfer method guaranteed to introduce a difference nobody notices.

**Sending.** *Export* on the catalogue row writes two files:

| File | What it is |
|---|---|
| `org-standard-v2.json` | the procedure, pretty-printed and reviewable |
| `org-standard-v2.canonical` | the exact bytes a signature is made over |

The export is audited, and the notice gives the procedure's **fingerprint** (16 hex
characters). Read that out to the receiving unit: if their import shows the same
fingerprint, the two of you have the same procedure, and neither of you had to read
the file down the phone.

**Receiving.** Put the path in the field and *Read this file*. Nothing is stored yet
— the preview shows what the file contains, whether its signature verifies **on this
workstation**, and what it would change against the newest version this register
already holds. *Store as a new version* then stores it.

Three things the import deliberately does:

- **This register assigns the version number.** The file's version is information.
  Two units both calling their procedure "version 2" is the normal case, so honouring
  an incoming number would silently redefine what "v2" means here.
- **Importing the same file twice stores nothing.** You will do this — once from the
  mail, once from the share. The second attempt says "already on record as version N".
- **A procedure that cannot be planned never reaches the register**, whether it
  arrived by file or was typed in the editor.

## Runbook: have a procedure signed, and require signatures

A template decides what is written to a security key, so an unauthorised edit of one
is an attack rather than a data-quality problem. Signing is the control; the audit
trail is only the record.

**This application verifies signatures and cannot make them.** It holds no private
key — the same rule that keeps PINs and access codes out of every file it writes. So
the signing step belongs to whoever holds the organisation's key, and it happens
outside the tool.

**Once, to set up a key.** On the machine that will hold it — ideally not the
workstation that edits templates, or the signature is only as good as that machine:

```bash
openssl genpkey -algorithm ed25519 -out template-key.pem
```

Publish the public half as hex, and give it to every workstation:

```bash
openssl pkey -in template-key.pem -pubout -outform DER | tail -c 32 | xxd -p -c 32
```

**Settings → Template signatures → Trust this key**: the key id (the label
signatures will carry, e.g. `esi-templates-2026`), that hex, and a note saying whose
it is. A malformed key is refused as it is typed — a trust store with a broken key in
it reports every template as *altered*, which sends you after the wrong problem.

**Per procedure.** Export it, sign the `.canonical` file, and hex-encode the result:

```bash
openssl pkeyutl -sign -inkey template-key.pem -rawin -in org-standard-v2.canonical -out sig.bin
xxd -p -c 64 sig.bin
```

Add the signature to the exported JSON, beside `"steps"`, and import the file:

```json
  "signature": {
    "key_id": "esi-templates-2026",
    "algorithm": "ed25519",
    "signature": "a6d66509…04f06"
  }
```

The import preview says immediately whether it verifies, so a mistake here is caught
by the tool rather than discovered later.

**Then turn the control on:** Settings → *Refuse to run a bootstrap from a template
whose signature does not verify*. Until you do, the Templates screen shows a **pilot
mode** banner, the run confirmation says the procedure is unverified, and every such
run is recorded as `template.unsigned_used`.

Two consequences worth knowing before you switch it on:

- **The procedures shipped with the application are unsigned**, deliberately — a
  signature from the tool's author would say something untrue about who approved the
  procedure for your deployment. Export them, have them signed, import them back.
- **Editing a template produces an unsigned version.** Changing a step changes the
  bytes the signature was made over; there is no way round that, and a tool that
  could re-sign would be a tool holding the key.

| Badge on a version | What it means | What to do |
|---|---|---|
| `signed` | verifies against a key you trust | nothing |
| `unsigned` | no signature | pilot mode, or get it signed |
| `signed by an unknown key` | you do not have that key | add it **if you trust it**; a signature nobody can check is not one |
| `signature does not match` | **altered since it was signed** | do not run it; get the procedure again from whoever signed it |
| `unknown signature algorithm` | a newer scheme than this build | upgrade the workstation |

## Runbook: what changed between two versions of a procedure

*Compare* on a catalogue row, or the two version pickers in the compare card. The
comparison is structural rather than a text diff, so a step that changed places reads
as **moved** — which is the fact that matters, because the order of the steps is the
order of execution, and `org-standard` v1 could not complete on real hardware for
exactly that reason.

Use it when a key comes back and the register says it was prepared with a version the
wizard no longer offers: the diff is the difference between what that key got and what
a key gets today.

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

## Runbook: the share went away while you were working

The application checks every five seconds that a share-hosted register is still
reachable. When the file server goes — a dropped VPN, a laptop that changed network, a
server rebooted — you get a card saying so instead of an SQLite error on your next
click.

**The register is intact.** It is on the file server, not on your workstation, and
everything you recorded before the share dropped was committed. Nothing was written
while it was gone.

What happens next depends on how the share was reached:

- **The signed-in account, or guest** — nothing needs typing, so the application tries
  to reconnect immediately. If the share is back, you are working again and the status
  line says so.
- **A named account** — the password was used for one connection and dropped, which is
  why it cannot be retried for you. Type it again on the card and press *Reconnect and
  reopen*.

Two messages worth telling apart:

| What it says | What it means |
|---|---|
| *…is still not reachable* | the share is not back; try again when it is |
| *…answered, but the register is not reachable on it yet* | the mount is back and the file is not — usually a link that has flapped. Wait a moment and try again |

If you would rather not wait, **Work on another database** clears the card and leaves
the chooser as usual. The reconnection is recorded in the audit trail
(`db.share.reconnected`) on the register that came back; the gap itself has no entry,
because there was no register to write one to.

## Runbook: two keys are plugged in

The Inventory and Bootstrap screens watch for keys while they are open, so plugging one in
shows it without pressing anything. With **one** attached, it is named and every operation
acts on it. With **more than one**, nothing is chosen for you — deliberately:

1. **Choose one** — *Use this one* on the Inventory row, or the button per key above the
   wizard's serial field. The status bar carries the choice from then on, and *which* key
   was picked out of how many goes into the audit trail.
2. Until you do, operations that need a key are refused. Reading on demand refuses too,
   with "more than one YubiKey is connected". That refusal is the feature: writing a PIN to
   whichever key the reader happened to list first is not something anybody can undo.
3. **Unplugging the chosen key drops the choice**, with a line saying so. Choose again
   rather than assuming the wizard is still aimed where you left it.

A device that appears in the list as **could not be read** enumerated but would not describe
itself. That is a driver or a permission, not a missing key — on Linux check the udev rules,
on macOS and Windows check that nothing else has the reader open.

Two things worth knowing about the watching itself: it runs only while one of those two
screens is open (a poll is cheap with the native transport and a subprocess without it, so polling for a
screen nobody is looking at is pure cost), and it is **stopped for the duration of a
bootstrap run** — nothing else touches the key while a run is writing to it.

## Runbook: distribute a key

1. **Inventory → Read attached key.** Confirm the serial matches the engraving.
2. **Holders** — register the person if they are not there. The corporate e-mail matters:
   it goes into the signing certificate.
3. **Bootstrap** — pick the key, the holder and the template; **Build plan**; read the plan.
   Check the transport column and any notes (a firmware gate may skip a step).
4. Read the **pre-flight**. It reads the key's applets first — PIV slots and PIN retries,
   the FIDO2 state, which OTP slots are programmed — and turns problems into lines on the
   screen instead of failures halfway through a key. Three things to look for:
   * **"already been through a procedure"** — the run is refused; see the runbook below.
   * **"was not read"** — an applet did not answer. Not the same as an empty applet: the
     checks that depend on it produced nothing, so this pre-flight looked at less than it
     appears to have.
   * a **low PIN retry count** — a key one wrong PIN from needing its PUK.
5. Run it *(Wave 1; today: **Record dry run**)*.
5. **Distribution** — select the key and holder, set the delivery method, put the signed
   term's reference in *Receipt*, leave *Attach the most recent bootstrap run* ticked, and
   **Record distribution**.
6. Confirm the Distribution table shows the hand-over with what was applied.

If the key status refuses to advance ("illegal status transition"), the key was never marked
bootstrapped — that is the guard working, not a bug.

## Runbook: a key that is already configured

The pre-flight refuses a key that shows any sign of a previous procedure — a certificate in
a PIV slot, a FIDO2 PIN already set, or a programmed OTP slot. There is **no override**, and
that is a decision rather than a missing feature: overwriting the credential a holder is
currently relying on cannot be undone from this tool.

What to do:

1. **Establish whether the key is in service.** Check Inventory and Distribution for that
   serial. If it was handed over, the holder is relying on what is on it — start the
   return, do not reset it.
2. If it is genuinely a key to reissue, **reset it to factory default**. That is the system
   operator's action, and it destroys the credentials on the key — see the runbook below.
3. Read the key again and start the run.

A changed PIV **management key** on its own is not treated as evidence and will not refuse
the run: a fleet-management tool may have set it without the key ever having been
bootstrapped.

If the refusal seems wrong, the pre-flight names the evidence it found — quote that line.
An applet reported as *not read* is worth checking too: the refusal only speaks for the
applets that answered.

## Runbook: returning a key to factory default

The action for a key coming back into stock, a key that refuses a run because it is already
configured, and a key nobody can account for. **It destroys what is on the key and nothing
that is on file**: the inventory row, the hand-overs, the runs and the reset itself all
stay in the register.

Before you start, be sure this key is not in service. Check Inventory and Distribution for
the serial — if it is out with a holder, start the return instead. A reset does **not**
revoke the certificate that was on it; that is still a manual step with the issuing CA
(`features/ca-integration.md`).

1. **Inventory** → *Attached now* → the row for the key → **factory reset…**. Nothing is
   written by opening the panel; it reads the applets and shows what it found.
2. Read the two lists per applet: what a reset of it destroys, and what this key is
   holding. Untick any applet you want to keep. An applet reported as *not read* is an
   applet whose contents are unknown — the reset still destroys them.
3. If FIDO2 is ticked, **unplug the key and plug it back in now**, and be ready to touch
   it. CTAP refuses a reset that does not arrive within a few seconds of power-up, which is
   the usual reason a FIDO2 reset fails.
4. Type the serial into the confirmation field, then **Reset serial N to factory
   default**.
5. Read the result table. Each applet says *reset*, *nothing to do*, or *refused* with the
   transport's own words, and the applets are read again afterwards so the panel shows the
   key as it now is.
6. If an applet refused: a FIDO2 timing failure is retried by starting again at step 1 with
   only FIDO2 ticked. A protected **OTP** slot cannot be cleared without its access code,
   which this tool records custody of and never stores — that slot stays as it is, and the
   key is reusable for everything except reprogramming it.
7. **Read attached key** to refresh the inventory row before bootstrapping it again.

Everything is recorded: `key.reset.started`, one `key.applet_reset` per applet that was
actually reset, `key.reset.failed` for each refusal, and `key.reset.finished` with the
counts. There is no undo, and nothing here can be restored from a backup — the register's
backups hold the register, and what a reset destroys lived only on the key.

## Runbook: the consignment term

1. **Distribution** → the row → **term**.
2. Pick the language. pt-BR and en ship; a unit can add others. If the requested language
   has no template, the panel says which one it used instead.
3. Review the rendered term. It is built from the record: the holder's name and
   identification number, the key's serial, what the bootstrap applied, the custody
   statement, the delivery method and the operator. Optional fields the holder did not
   give (phone, address) take their whole line with them rather than printing an empty
   label.
4. **Export as PDF…** — the sheet to print and have signed. A4, one page for the
   built-in wording plus a second for the signatures, and a footer on every page
   naming the wording that produced it (`consignment@2 (pt-BR) · #20423633 ·
   TERM-2026-001`), so the signed sheet in the folder is traceable back to the exact
   template version in the database. **Save as text…** gives the same document as
   plain text, for a ticket.
   - If the panel warns that the PDF font cannot set some characters, that is real:
     the PDF is set in Courier with CP1252, which covers the Latin-script languages
     but not, say, Japanese. Those characters print as `?` — use the text output for
     that language, and see the open question in the feature file.
5. When the signed copy comes back: **Upload signed term…** (or *upload* on the row).
   PDF, PNG, JPEG or TIFF, up to 8 MiB. It is stored **in the database** with a SHA-256,
   so copying the database copies the evidence.
   - **Or record your own reference** instead, in the same panel: a unit that files paper
     in a process system types the number there (`processo 2026/114`) and the hand-over
     counts as signed. Either answers "where is the signed term"; nothing demands both.
6. The row's badge in the **Term** column says where the signature stands, with the age:
   `awaiting signature · 3d`, `overdue · 30d`, `signed`, `returned unsigned · 50d`.
   *export* writes a filed document back out, verifying the digest first and refusing on
   a mismatch.

## Runbook: chasing the terms that never came back

The **Distribution** screen carries one line at the top when there is something
outstanding, and nothing at all when there is not. What the states mean:

| Badge | What it means | What to do |
|---|---|---|
| `awaiting signature · Nd` | handed over, not signed, inside your limit | nothing yet — a posted key is normally here |
| `overdue · Nd` | unsigned past your limit | file the scan, or record your reference |
| `signed` | the scan is filed, or a reference was recorded | nothing |
| `returned unsigned · Nd` | the key came back and no term was ever signed | nothing to chase; the gap stays on record |
| `term not used` | your unit has terms turned off | nothing |

**Set the limit to what your slowest channel takes.** *Settings → Responsibility terms*:
14 days by default. A unit that hands keys across a desk can drop it to two; one that
posts them internationally should raise it. There is deliberately **one** limit rather
than one per delivery method — two would mean deciding which applies to the row in front
of you, which is how a warning stops being read. The same card turns terms off entirely,
for an internal pilot or a batch of test keys.

Two things worth knowing:

- **An overdue term is recorded in the audit trail once, and only once.** Not once per
  session, not once per screen paint — the trail is checked before anything is written.
  The entry stays after the term is signed, because it happened.
- **A returned key with no signed term is a permanent gap**, and stays counted. The term
  was evidence of custody while the key was held; that window has closed, so nobody
  should chase it, and the register does not tidy it away either.

## Runbook: a key comes back

1. **Distribution** → the row → **record return**. The key's status becomes *Returned*
   and the hand-over closes.
   - If no signed term was ever filed for it, the status line says so once. That gap is
     now permanent — worth knowing, not worth chasing.
2. **return receipt** on the same row produces the mirror document: the holder's details,
   the key, **both** dates (handed over on…, returned on…), who received it, and the
   undertaking to revoke the certificates and remove the credentials. Export it as a PDF
   the same way as the term.
3. Have it signed, then **Upload signed receipt…**. Until you do, the row shows
   `no receipt` — a return the holder did not sign for is a return only the unit is
   asserting.
4. **Then actually revoke.** The receipt says the credentials will be revoked; the
   document is not the revocation. A returned key whose certificate is still valid is a
   credential in a drawer.

The receipt's wording is editable like the term's: **Terms** → *Document* → *return* →
the language. Saving stores a new version, and the receipts already issued keep pointing
at the version that produced them.

The built-in term wording is a **draft**: it needs review by whoever owns the term at your
institution, and the data-protection paragraph needs the DPO. Templates are data, so that
review is an edit rather than a code change. To send it for review: **Terms** → the
language → **Preview** → **Export as PDF…**, which produces the document as it will be
printed, filled with obviously fictitious values and footed `@draft`. Nothing is stored
and no hand-over is involved.

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
| "no database file at …" | The path does not exist (typo, or an unmounted share) | Fix the path, or connect the share from the SMB card; use **Create** only if you really want a new, empty database |
| "a file already exists at …" | **Create** was used on an existing file | Use **Open** instead |
| "… refused …: the user name or password was refused" | Wrong credentials for the share | Retype them; check whether the account needs its domain (`DOMAIN\user`) |
| "… may not use this share" | The account is valid but has no access | Ask whoever owns the share; nothing on this workstation will fix it |
| "… the server or the share name is wrong" | Typo, or the wrong network | Check the name, and that this workstation is on the network the server is on |
| "Windows already has a connection to this server as another user" | An existing mapping conflicts | `net use \\server\share /delete`, or choose the signed-in account |
| "… has to be mounted by the system on this platform" | Linux: CIFS needs privilege | Have it mounted from `/etc/fstab` or `autofs`, then open it by path |

## Runbook: no key detected

1. Is exactly one key plugged in? Two attached keys are refused deliberately.
2. Is the smartcard service running (Windows *Smart Card*, Linux `pcscd`)?
3. Does `ykman list --serials` see it? If `ykman` sees it and the tool does not, that is a
   transport bug worth reporting with the log. Say which transport was live — the status
   bar's `via:` item — because "native does not see it and `ykman` does" and "neither
   sees it" are different faults. Switching transport in **Settings → Device transport**
   is the fastest way to tell them apart.
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
