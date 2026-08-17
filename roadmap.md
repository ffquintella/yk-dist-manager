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
| Done | 37 |
| In progress | 2 |
| Todo | 4 |
| **Total tracked items** | **43** (across 38 specs — some specs carry more than one item) |

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
> [Wave 0 is closed](#wave-0-is-closed).

Released: **v0.16.3**. **Wave 1 is closed** (2026-08-14). Current wave: **Wave 2 — operations at scale.**

> Two rows moved on 2026-08-14, and one of them changed count twice: **key lifecycle &
> revocation** finished (`[/]` → `[x]`), and **CI & coverage gate** was `[ ]` for a workflow
> file that has been enforcing the gate since it was written — the same work testing strategy
> already recorded as done. That is the third time this file has held a status somebody would
> have had to read the code to disbelieve, which is why every count here is derived by the
> command above rather than adjusted by hand.

**Out of turn, unreleased**: the rest of **packaging & release** —
[`features/packaging-and-release.md`](features/packaging-and-release.md) phases 0b, 2, 5, 6,
7 and 8 — which is **Wave 3**, taken while Wave 2 is the current wave. The reason, since
AGENTS.md §1 asks for it here and in the same commit: the operator who will run the tool
asked for the work in progress to be finished, and packaging was one of the two rows carrying
`[/]`. It is also the row whose *absence* is felt by somebody who is not a developer — every
install of this tool so far has been `make run` from a source checkout, which the norm does
not permit for an installed build. So a tag now produces a verified artefact per platform, a
build says which commit it came from, and the release notes carry the schema warning that
makes `SchemaTooNew` a usable refusal rather than a surprise on a share. What it does **not**
do is sign for distribution: a Developer ID and an Authenticode certificate are procurement
with a long lead time, and the workflow has the signing step waiting behind a secret. The
`block` 0.1.6 blocker went with it, since it gates every artefact — resolved by a four-line
patched copy in [`vendor/block`](vendor/block/README.md), with the three alternatives and the
way back written down beside it.

**Out of turn, unreleased**: a **way out of a live single-writer lock**, and the refusal
card an operator meets at **startup** — [`features/cloud-sync-hosting.md`](features/cloud-sync-hosting.md)
phase 9, a phase added for this. It belongs to Wave 0's storage work. It was taken now
because the feature as built had a hole exactly where an operator stands: the take-over
button lived on a card that `YkDistApp::try_open` never raised, so a register held when
the application *launched* — the ordinary way to meet a lock — produced a sentence naming
the holder and no way to act on it, twice over, because both error fields were painted.
And the button itself only ever appeared after fifteen minutes of silence, which leaves
the commonest case (a second window of this application, on this workstation, that the
operator cannot get back to) with no exit but copying the file — the divergence the lock
exists to prevent, reached the long way round. So a live lock can now be taken over too,
behind a tick that is cleared by the next refusal, and the audit entry says the lock was
still being refreshed when it was taken. No schema change. `STALE_AFTER` and the refusal
itself are unchanged: the default is still to refuse, and the fifteen minutes still mark
the difference between clearing up after a crash and cutting in on somebody.

**Out of turn, unreleased**: the **factory reset** of a plugged key —
[`features/key-lifecycle-and-revocation.md`](features/key-lifecycle-and-revocation.md)
phase 5, which belongs to **Wave 2** — built at the request of the operator who will run
the tool. The reason it could not wait is written into Wave 1: the decision of 2026-08-13
says a configured key is **only ever returned to factory default**, with no override and
no in-place re-bootstrap, and `features/device-detection.md` phase 5 already refuses such
a key with a message naming the reset. That refusal named a way forward the application
did not have — the operator was sent to a command line for the one action the tool insists
on. So Wave 1's refusal now has its exit, and it is one action rather than a wave: per
applet, previewed with what it destroys *and* what this key was read to hold, confirmed by
typing the serial back, and recorded per applet in the audit chain. No schema change. Reset
*authority* — one operator or two — remains an ESI question, listed in the spec.

> What this note said it deliberately left undone — the revocation (phase 3), the credential
> removal (phase 4) and the reissue gate (phase 6) — **is done as of 2026-08-14**, with the
> rest of that spec. A reset still revokes nothing: what it does now is leave the certificate
> it destroyed on the list of things outstanding until somebody revokes it at the CA, and
> clear the reissue gate for the applets it actually answered for.

**Out of turn, unreleased**: the **power cycle in front of a FIDO2 reset** — the same
spec's phase 5a, added at the request of the operator who will run the tool, after the
first real reset came back `ERROR: Reset failed. Reset must be triggered within 5 seconds
after the YubiKey is inserted`. Phase 5 knew about that window and answered it with a
sentence in the preview telling the operator to unplug the key and plug it back in before
confirming — which is a race with no visible start, and the refusal it produces reads like
a broken key. So the tool now runs the race: confirmation first, then
[`device::reinsert`](src/device/reinsert.rs) asks for the key, watches the port for that
one serial and fires the run as it returns. It is the exit for the same refusal phase 5
was built for, one step further down: an action the tool insists on should not depend on
the operator's reflexes. No schema change; three new audit events, all of them about a
power cycle and none about a write. The lasting fix is a native `authenticatorReset`
(`features/native-device-transport.md`), which would take a subprocess start out of a
five-second window.

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
the tool. It belongs to Wave 0 — then unfinished — rather than to Wave 1, and it
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

Wave 0 (foundation) is **closed**, and v0.2.1–v0.2.2 added the paperwork and intake half
of the job: choosing or creating the database file, recording serials from a barcode,
generating the consignment term in the holder's language, and filing the signed copy
against the hand-over — and, from the Terms screen, editing that term's wording per
language. The Templates screen now does the same for the bootstrap procedure itself.
**913 tests** pass with the default features and **918** with `--all-features` (plus 7
ignored: the read-only hardware tests and the `openssl` interop ones), with **86.11%**
(region 85.23%) line coverage of the headless core — enforced by CI against a floor of
80% rather than reported. The register can also
be opened from the unit's **SMB share**, connected by the application itself, and its
password can be set, changed or removed from Settings.

> The coverage figures quoted here had drifted again: they read 86.91% / 85.79%, and
> `make coverage-core` on the released 0.7.1 says 83.51% / 82.69%. Corrected to what
> the gate actually measures, which is the number CI enforces.
>
> **Finishing Wave 1 held line coverage roughly level** — 85.7% before, **85.04%**
> after — and that is worth a sentence, because most of what landed is
> hardware-facing and the naive expectation is a drop. What kept it level is that the
> new code splits cleanly: every hardware-facing module has a *pure* half that is
> tested exhaustively and a *card exchange* that no test can reach. The management
> applet's parser is at 98.7% and its APDU conversation at nothing; the same split
> holds for the certificate read-back and the OTP access-code write. The gate is a
> floor rather than a target, and the honest reading of the number is unchanged by it
> being met: there is code here that only a plugged-in YubiKey can exercise, which is
> what the wave's own status says.

## Wave 1 is closed (2026-08-14)

**The owner's decision, and it is a judgement rather than a derivation** — which is
why it carries a date and this section says what it rests on. Every phase carrying
wave `1` in every spec under [`features/`](features/) is **built**; what is not
finished is **verification against hardware**, and the wave was closed with that
outstanding rather than held open for a key.

That is a different standard from the one Wave 0 closed under, and pretending
otherwise would be the drift this file has already been corrected for twice. So,
plainly: **a `[x]` in the Wave 1 table means the code exists and is covered by tests
that need no key. It does not mean a YubiKey has run it.**

Ten phase rows read `**Built** (not hardware-verified)` until this decision and now
read **Done**, on the owner's instruction that what cannot be tested here is marked
finished. Every one of them keeps the sentence *the protocol conversation is unverified
against a key* in its notes, and the table below is the list in one place — because a
status column somebody scans and a fact somebody needs before handing a key to a person
are different jobs, and the flip should not have quietly done the second one.

Derived, so a reader can check the first half of the claim — the command prints
nothing, and it is the same command that closes Wave 0 with a `0` in it:

```bash
awk -F'|' '$4 ~ /^ *1 *$/ && $5 !~ /[Dd]one/ {print FILENAME" phase"$2": "$3}' features/*.md
```

It printed one row until this wave closed: [db-password](features/db-password-and-encryption.md)
phase 7, **unlocking with a YubiKey**, and that row moved to `—` (gating no wave)
rather than being ticked. It is blocked twice over and neither half is an
implementer's: it needs an OTP slot *programmed*, which is the one write deliberately
deferred until a key can verify the frame, and turning a challenge-response into a
database key **is** a KDF choice, which is the ESI's to approve — the same gate as
that spec's phase 4. It is marked *optional* in its own title and nothing depends on
it, which is what makes `—` the honest column rather than a convenience.

### What has never met hardware

Named here rather than left in the specs, because this is the list somebody has to
work through before a key is handed to a person on the strength of it:

| Path | State |
|---|---|
| FIDO2 — PIN, minimum length, forced change, `make_credential` | **verified** on a 5.7.4 key, writes included |
| PIV PIN, PUK, management key | **verified** on a 5C NFC (5.7.4), 2026-08-11 |
| `GENERATE` and certificate import through the AES-authenticated session | built, unverified |
| Attestation read, and the certificate **read-back** | built, unverified |
| Management applet exchange (the parser is covered byte for byte) | built, unverified |
| OTP access-code write through `ykman` | built, unverified |
| *Native* OTP configuration frame | **deliberately unwritten** — no crate exposes it, and a wrong frame leaves a slot write-protected by a code nobody holds |

Each says so in its own spec, and each splits the same way: a pure half that is tested
exhaustively and a protocol conversation no test can reach. "Not hardware-verified"
means *the conversation is unproven*, not *this code is untested*.

### What is left in the specs, and why none of it is Wave 1

The step specs have no Wave column — they predate it — so their remaining phases were
read rather than derived. They divide two ways. **Somebody else's decision or
system**: enterprise attestation needs a relying party that verifies it, the internal
relying party needs somebody to say which service, external escrow needs BastionVault,
and the three CA issuers are an **ESI** decision about integrating with a corporate PKI
(`AGENTS.md` §8). **A later wave**: the custody report and expiry tracking belong with
[reports & export](features/reports-and-export.md), the PIN/PUK unblock flow with
[the lifecycle](features/key-lifecycle-and-revocation.md), and OpenPGP is Wave 3 and
was not the chosen mechanism in the first place.

One thing closing this wave **raised** rather than settled, and it is in
[Open questions](#open-questions) with its three ways out: `org-standard`'s OTP
access-code step applies to no key at all, because a code protects a configuration
while a programmed OTP slot is what the no-override refusal of 2026-08-13 reads as a
key that has already been through a procedure.

## Wave 0 is closed

**Nothing.** Every phase carrying wave `0` in every spec under
[`features/`](features/) is done, which is the rule this file states below and the one
it is now measured against for the first time.

Derived, not asserted — the command prints nothing:

```bash
awk -F'|' '$4 ~ /^ *0 *$/ && $5 !~ /[Dd]one/ {print FILENAME" phase"$2": "$3}' features/*.md
```

The last four phases to land were the icon's in-application use, the signature state
machine and the return receipt, reconnecting a dropped share, and the property tests
for the audit chain and the RFC 4514 escaper.

**What "Wave 0 is closed" does and does not mean.** It means the foundation is in
place: a register that can be opened, hosted, encrypted, backed up, audited and
searched; keys, holders and hand-overs recorded; the paperwork generated, signed for
and chased; the procedure editable, signed and shareable; and the hardware identified
as it is plugged in. It does **not** mean the tool bootstraps a key — that is Wave 1,
and `MockWriter` is still the only implementation of the write traits in a default
build.

**The claim used to rest partly on judgement, and no longer does (2026-08-14).**
The command reads the **Wave** column, and nine specs did not have one — they predate
it: consignment terms, database selection, distribution records, holder registry, key
inventory, logging, serial scanning, signed-term documents and the `ykman` fallback.
All nine were `[x]` with unfinished phases, so for those rows "Wave 0 is closed" was a
judgement rather than a derivation, and this file said so.

**All nine now carry the column**, which was named here as "the real fix… worth doing
before the next wave closes, because the same gap will be there" — and it was, so it
was done when Wave 1 closed. Every remaining phase in them was read and assigned:
later-wave (a per-holder view, reconciliation reports, batch hand-over, a correlation
id), `—` for optional or owner-blocked (AD/LDAP needs the ESI-approved integration,
holder retention needs the DPO, a JSON log sink needs ESI agreement before diverging
from the G-002 format), or Wave 3 where it belongs to delivery (a rotating log file,
the `block` 0.1.6 chain).

Eight turned out to be **already done elsewhere** and now say so with a pointer,
rather than reading as work still owed: the log panel (logging 3) and search on
Inventory and Holders (key inventory 5, holder registry 5) shipped as gui-shell 8 and
3; the spreadsheet import (key inventory 8) as storage 8; the return receipt
(consignment terms 9), the pending-signature state (distribution records 4), the term
generation (distribution records 6) and the overdue-signature report (signed-term
documents 7) as [receipts & terms](features/receipts-and-terms.md) 4 and 6; and
"already configured" detection (key inventory 6) as
[device detection](features/device-detection.md) 5.

So **every** row in the Wave 0 tables below is now derived, and the same command with
a `1` in it derives Wave 1. Neither claim is a paragraph asking to be trusted.

### Unticked boxes that are not Wave 0

Plenty of phases in `features/` are still Todo. None of them gates this wave, and
they are grouped here because "the specs are full of Todos, so how is Wave 0 closed?"
is the first reasonable question a reader has.

**Phases that gate a later wave.** Everything that was listed here as Wave 1 —
the engine's OTP step, the native transports, the wizard's post-run attachment,
template applicability rules and the retry policy — is built, and Wave 1 closed on
2026-08-14; see [Wave 1 is closed](#wave-1-is-closed) for what that does and does not
mean. Unlocking with a YubiKey moved to `—` rather than being ticked, and the section
above says why. A Windows `.ico` and a Linux `hicolor` icon are **Wave 3**, with the
packaging they attach to. Concurrency, batch mode, the signed audit export and the
cross-check transport are **Wave 2**.

**Decisions that are not the implementer's** (AGENTS.md §8). None of them holds a
wave-0 phase, which is why they are here rather than in the table above — but each
one is somebody's to answer, and this is every such decision left anywhere in the
specs. They are also in [Open questions](#open-questions), with the reasoning:

| Question | Owner | What it holds |
|---|---|---|
| The approved cipher and KDF parameter set | **ESI** | [db-password](features/db-password-and-encryption.md) phase 4, marked `—`: explicit `kdf_iter` and cipher page size. The defaults are SQLCipher's until then, which is reasonable and not the same thing as approved |
| Whether Ed25519 is the signature algorithm the organisation wants | **ESI** | likewise nothing: the algorithm is named inside every signature, so a second one is additive. Recorded so it is ratified rather than inherited |
| Whether `org-standard`'s OTP access-code step can ever run | **the procedure's owner** (the ESI for one of the three options) | that one step, which as specified applies to no key: an access code needs a *programmed* slot, and a programmed OTP slot is what the no-override refusal of 2026-08-13 treats as evidence of a previous bootstrap. Raised 2026-08-14 by finishing the write — see [Open questions](#open-questions) |

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
Ten of them, and the Wave column is what makes that legitimate — each has every
wave-0 phase done and only later-wave work left: native device transport, device
detection, single-file SQLite storage, the optional database password, the audit
trail, bootstrap templates, the bootstrap planner, the bootstrap wizard, receipts &
terms, and the application icon. The **Wave 1** rows are now in the same position for
the same reason, with one difference stated in
[Wave 1 is closed](#wave-1-is-closed): their remaining work is not only later-wave, it
also includes hardware verification of paths that are written.

Derive that list too, rather than trusting this paragraph:

```bash
# a [x] row whose spec still has an unfinished phase
grep -oE 'features/[a-z0-9-]+\.md' roadmap.md | sort -u |
  while read -r spec; do
    grep -q "^| \`\[x\]\` .*$spec" roadmap.md &&
      grep -qE '\| (Todo|In progress|Partly done)' "$spec" && echo "$spec"
  done
```

The **GUI shell** left it in 0.9.0 without anything being built: its last open row
was localisation, and the interface being English closes that as *not needed* rather
than done. Its spec now has no unfinished phase at all.

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
  are listed under [Wave 0 is closed](#wave-0-is-closed)
  with their owner, and a wave can close with them outstanding.
- Waves are ordered. Work the current wave; if something must jump the queue,
  edit this file in the same commit and say why (see [AGENTS.md](AGENTS.md)).
- Every feature file carries its own phase table, audit events and test list.

---

## Wave 0 — Foundation

Everything needed before a single byte is written to a key.

| Status | Feature | Notes |
|---|---|---|
| `[x]` | Native device transport | [spec](features/native-device-transport.md) — `yubikey` over PC/SC reads serial + firmware from a real key today (verified against 5 NFC / fw 5.4.3, agrees with `ykman`). FIDO2 and OTP transports are Wave 1, and so is the selection that makes any of them reachable — see the Wave 1 row. |
| `[x]` | `ykman` fallback + parsers | [spec](features/ykman-fallback.md) — argv-only subprocess, typed errors, parsers unit-tested against recorded output of ykman 5.9.2. |
| `[x]` | Device detection | [spec](features/device-detection.md) — read-on-demand, plus a **background watch** that notices a key being plugged in or pulled out (identifying only when the set of serials changes; 1.5s natively, 4s when every poll is a subprocess, and only while a screen that needs it is open) and a **picker** for when several are attached. Nothing is ever chosen for the operator, and the watch is stopped — thread joined — before a run writes to a key. Per-applet reads, the "already bootstrapped" warning and attestation are Wave 1. |
| `[x]` | Single-file SQLite storage | [spec](features/storage-sqlite-single-file.md) — schema **v8** (`batches` and `batch_keys`, on top of v7's `open_sessions`, v6's lifecycle tables and v5's per-step run rows), `user_version` migrations (v1→v8 tested), WAL locally / rollback journal on a share, `VACUUM INTO` backup **on a schedule with rotation**, `integrity_check`, **CSV import** of the spreadsheet this replaces, and the SMB share connected by the application itself. Every wave-0 phase done, and **concurrency (phase 4) landed with Wave 2** — see that row. Archival and retention (phase 7, gating no wave) has its decision — one year, configurable — and needs the archive-then-remove path that may break and rebuild the audit trigger. |
| `[x]` | SMB share hosting | [spec](features/smb-share-hosting.md) — the application connects the share itself: the signed-in user (the default, and the whole mechanism on Windows), guest, or a named account whose password is typed and never stored. `WNetAddConnection2W` / `NetFSMountURLSync`, never a command line. An already-mounted share is used and left alone; one this session made is released on close and on quit. A share that **drops mid-session** is noticed within five seconds, the register is abandoned rather than closed, an identity that needs no password is retried at once, and the way back is one button — `db.share.reconnected` records the round trip. Kerberos on macOS gates no wave. |
| `[x]` | Cloud-sync hosting (OneDrive) | [spec](features/cloud-sync-hosting.md) — `Location::CloudSync`: waits for the sync client, takes `<database>.lock`, refuses a second workstation by name, releases after the upload, reports sync conflict copies, **snapshots the register at open** before this session can write, and offers a **read-only** open so a second operator can look without taking the lock. A refusal is a card wherever it is met, including at startup, and a **live** lock can be taken over behind a deliberate tick — audited as live. Every phase done. Whether the location is *acceptable* is an ESI decision, not this feature's. |
| `[x]` | Choosing / creating the database file | [spec](features/database-selection.md) — strict `open_existing` vs `create_new` (a typo can no longer create an empty database), recent-database list, native dialogs, switch from Settings. |
| `[x]` | Optional database password | [spec](features/db-password-and-encryption.md) — `encrypted-db` wires `PRAGMA key`; the chooser prompts. **Settings → Password protection** sets, changes and removes the password by export-and-swap, never an in-place re-key, with the strength meter beside it and the floor enforced by the store rather than by the screen. The prompt slows down after three wrong passwords — a doubling delay counted down on screen, enforced in `handle_db_request` and deliberately not a lockout. Explicit KDF parameters are the one thing left, and they are **blocked on the ESI's approved cipher set**; unlocking with a YubiKey is Wave 1. |
| `[x]` | Logging | [spec](features/logging.md) — one entry point, three levels, G-002 line format, no hand-built log lines. |
| `[x]` | Audit trail | [spec](features/audit-trail.md) — SHA-256 chain, `UPDATE`/`DELETE` refused by trigger, **verification at open**, a **segregated mirror** whose divergence is an alert, and **filtering** by event, actor, target and date, plus one entry per executor step outcome. Every wave-0 phase done, and the segregation question is answered: the trail lives in the register, and the optional mirror is what satisfies the norm (2026-08-11). An external chain-head witness (phase 5) gates no wave; the signed ESI export is Wave 2. |
| `[x]` | Key inventory | [spec](features/key-inventory.md) — serial, model, firmware, form factor, FIPS flag, applications, lifecycle with guarded transitions, serial provenance (verified / scanned / typed), an editable **observation** per key, and confirmed **removal** of an intake mistake (refused once a hand-over or a run refers to the serial). **Corrected after the first real bootstrap (2026-08-13, unreleased):** the lifecycle's first arrow had nobody to perform it. `InStock → Bootstrapped` was only ever made by a button on this screen, so a key that had just been configured was still filed as stock, and the operator met that on the Distribution screen as a refused hand-over. A run that settles `Completed` now moves the key itself, audited with the run id; the button stays for a key configured outside the tool. |
| `[x]` | Serial from a barcode | [spec](features/serial-scanning.md) — camera decoding via `rxing` + `nokhwa`, a USB-wedge/typed path that needs no features, and provenance that only ever improves. |
| `[x]` | Holder registry | [spec](features/holder-registry.md) — minimal personal data, validated e-mail, RFC 4514 subject derivation, plus optional identification number, phone and address. |
| `[x]` | Distribution records | [spec](features/distribution-records.md) — hand-over, operator, delivery method, receipt reference, linked bootstrap run, return without rewriting history. **Corrected (2026-08-13, unreleased):** the hand-over was written and *then* shown to the lifecycle, so a key the lifecycle would not move left a record on the register beside a key still filed as stock — *recorded, but status not updated*, and nothing the operator could do about either half. The lifecycle is asked first now, from the key's own row, and the refusal says what the key needs and that nothing was recorded. |
| `[x]` | Bootstrap templates | [spec](features/bootstrap-templates.md) — versioned templates, `{{variable}}` rendering, two built-ins, validation, and a **Templates screen**: add, duplicate, edit (always as a new version), retire / reinstate, remove. A draft is refused unless it *plans* against sample data. Both built-ins ship at **v2** with the corrected forced-change ordering, `fido-only` deriving its version from `org-standard` so a correction cannot reach the code and stop at the database. Also: **export and import as a file** (with the canonical bytes beside it; import is a preview, and the receiving register numbers the version), **Ed25519 signatures** verified before a run — the private key never comes near this tool — with pilot mode visible on screen and audited, and a **structural diff** between two versions that reports a reordered step as *moved*. Applicability rules and the retry policy are Wave 1. |
| `[x]` | Bootstrap planner | [spec](features/bootstrap-engine.md) — plan with per-step transport (native / ykman / manual) and secret placeholders; dry runs recorded — both wave-0 phases. **The executor is Wave 1**, and is tracked as its own row there. |
| `[x]` | GUI shell | [spec](features/gui-shell.md) — eight screens, unlock screen, status bar, egui 0.36 `App::ui`, themed with `egui-elegance` (four palettes, the choice persisted) and laid out fluidly (one gutter, full-width cards, columns that split the page, tables that contain their own overflow). Search, sortable columns, window-state persistence, keyboard flow, hardware-write confirmation, the log panel and the accessibility pass are all done — every wave-0 phase. Localisation (phase 9) is closed as not needed: the interface is English (2026-08-12). |
| `[x]` | Bootstrap wizard | [spec](features/gui-bootstrap-wizard.md) — selection (the newest version of each template in use), per-step opt-out, plan review, dry run, and a link to the Templates screen — the whole of wave 0. The live run view, the secret panels and the pre-flight checks landed with the executor in 0.7.1; resume and the post-run summary are Wave 1, batch mode landed with Wave 2 — see the bulk-enrolment row. |
| `[x]` | Application icon | [spec](features/application-icon.md) — one SVG (a box truck carrying a YubiKey), `make icons` rendering the PNGs, the macOS `.icns` and the RGBA blob the binary embeds; window, dock and bundle icons done, plus **three placements in the application** (top bar, database chooser, About box) and an **About box** on the version badge carrying the copyable `--diagnose` report. The Linux `hicolor` install shipped with the `.deb`, and `packaging/windows/icon.ico` now ships with the MSI (Start Menu shortcut and Programs and Features). The `.ico` **as a resource inside the executable** — which is what gives the window and the taskbar an icon on Windows — is still **Wave 3**: it needs a build-script change, not a packaging one. |
| `[x]` | Testing strategy | [spec](features/testing-strategy.md) — **913 tests** (918 with `--all-features`) across unit + behaviour suites, a mock device backend and a mock share connector, recorded fixtures, and tests ignored by default for what needs hardware or `openssl`; **86.11%** core line coverage. **CI enforces the gate** on every push, with a macOS/Windows/Linux build matrix. **Property tests** for the audit chain and the RFC 4514 escaper, each verified by breaking the code it covers. Mock write transports and the secret-leak sweep are done. |

### Paperwork

| Status | Feature | Notes |
|---|---|---|
| `[x]` | Consignment terms | [spec](features/consignment-terms.md) — multilingual templates keyed `(id, language, version)`, pt-BR + en built in, optional fields that omit their own line, generated from the record. A **Terms screen** edits the wording and adds languages: saving stores a new version, and terms are generated from the newest. The **wording needs its owner's review**, which the editor is what makes possible — and, since phase 7, an *Export as PDF…* that sends the reviewer the document rather than a template full of `{{variables}}`. The term now leaves as **text or PDF** from one rendering, so the copy reviewed on screen cannot disagree with the copy that is signed; `crate::pdf` writes the file with **no dependency and no TeX**, and every page names the template version that produced it. |
| `[x]` | Signed-term upload | [spec](features/signed-term-documents.md) — the scan is filed in the database with a SHA-256, verified on export, with a per-hand-over "none filed" badge. |
| `[x]` | Receipts & terms (signature tracking) | [spec](features/receipts-and-terms.md) — the term, its PDF, the versioned wording, the filed signature and the sealed-envelope slip, plus a **signature state machine**: five states derived from the record and what is filed *per kind*, a threshold the unit sets, a banner where the hand-overs are, and `receipt.pending_overdue` written once per hand-over ever — using the immutable trail as its own marker. The unit's own reference can now be recorded *after* the hand-over. The **return receipt** is a second template id, so it is editable, versioned and multilingual for free. Batch generation is Wave 2. |

## Wave 1 — Execute the bootstrap, natively

The point of the tool: apply the template to a key, safely, with evidence.

| Status | Feature | Notes |
|---|---|---|
| `[x]` | Bootstrap executor | [spec](features/bootstrap-engine.md) — **the engine is built and proven against mocks**: sequencing, per-step persistence, abort-on-required-failure, idempotency, resume, an unforgeable confirmation gate, and a sweep asserting no secret reaches any record. Since 0.12.0 the native transport is compiled in by default, so the write path is *reachable*: FIDO2 is hardware-verified, PIV is **implemented but not hardware-verified**, and the OTP **access-code write** now goes through the labelled `ykman` fallback. The certificate import is no longer a placeholder — it takes the certificate the operator brings, checks it against the slot and the holder, and imports it (2026-08-13) — and the `Verify` step now **reads the certificate back off the card** and checks its subject, `rfc822Name` and key usage, failing the step when they disagree. Steps also carry an **attempt budget**, spent only on transport-level failures. What remains is a key to verify PIV against and the *native* OTP frame. |
| `[x]` | Step: FIDO2 PIN | [spec](features/step-fido2-pin.md) — set/change the PIN over CTAP2, minimum length policy (fw 5.7+), `forcePINChange` so the holder must replace the transport PIN (custody model B), and **retry accounting on screen** (phase 8): both applets' counters are read without spending one, a counter walked below its factory value warns and names what recovery costs, zero **blocks the run**, and the `Verify` step reads them back. Only phase 5 — *change* an existing PIN — is left, and it is implemented and waiting for a key whose current PIN is known. |
| `[x]` | Step: initial FIDO2 credential | [spec](features/step-fido2-credentials.md) — `authenticatorMakeCredential` with `rk=true` so the credential is resident on the key; `ykman` cannot do this at all. **Phases 2 and 4 done**: a key with no free discoverable slot is refused before the PIN is set rather than by the authenticator's own error, and the credential id, relying party, algorithm and user name are on the run and readable back off the register. What is left needs somebody else's decision or system: listing and deleting credentials from the GUI, enterprise attestation (needs an RP that verifies it) and which internal relying party the credential is bound to. |
| `[x]` | Step: OTP slot access code | [spec](features/step-otp-access-code.md) — the 6-byte code that write-protects a slot. The **read** landed via `ykman otp info`, and the **write** now goes through the same labelled fallback: `otp settings <slot> --force --new-access-code -` with the code on **stdin**, never in argv (phase 2). Three things about that command were read out of `ykman` 5.9.2's source rather than guessed — the prompt confirms, an *empty* slot is refused outright, and the write resets the slot's other settings — and each is now something the step or the pre-flight says before it runs. The mode-switch consequence (phase 6) and custody of the code (phase 7) are recorded. **Not hardware-verified.** The *native* HID frame stays deliberately unwritten (`native-device-transport.md` phase 4): a wrong frame leaves a slot protected by a code nobody holds. Slot *programming* waits on a different answer — what the slot is for. |
| `[x]` | Step: PIV PIN / PUK / management key | [spec](features/step-piv-pin-puk-management-key.md) — leave no factory default; a PIN-protected random management key, so nothing needs custody. **No longer blocked** (decision of 2026-08-13): the transport question was answered by writing the exchange rather than by choosing between a crate that cannot do it and a CLI that takes a PIN on its command line. [`device::piv_session`](src/device/piv_session.rs) reads the slot's real algorithm and authenticates with AES — PIN and PUK changes and the `GET METADATA` idempotency read were already hardware-verified through the crate, the AES management-key write on 2026-08-11. **Phase 6 done**: a key still on a factory default is badged on **Attached now**, not only in the wizard's pre-flight — which is seen once, by the operator about to fix it, and never by anybody auditing the fleet. It rests on three `Option<bool>` metadata answers rather than a boolean, so a key nobody read is never accused of holding a default. Remaining: phase 7, the PIN/PUK unblock flow for a returned key (Wave 2's lifecycle work), and phase 5's optional escrow, which waits on external escrow. |
| `[x]` | Step: PIV signing certificate | [spec](features/step-piv-signing-certificate.md) — on-device key in slot 9c, CSR **with `rfc822Name` SAN**, issued certificate imported, attestation stored. The SAN is why this step goes native, and it is now built: [`device::csr`](src/device/csr.rs) assembles PKCS#10 purely and signs through the slot, with `openssl` reading the SAN back and verifying the signature in an interop test. ECDSA only — RSA needs PKCS#1 padding applied by the caller and is refused rather than guessed. The **import** is done too: PKCS#10 out, the operator's certificate back in, refused unless its public key is the slot's and it carries the holder's `rfc822Name` (phases 4 and 5). **Not hardware-verified.** **Phase 7 is now built**: the `Verify` step reads the slot's certificate back and checks the subject DN (as a set of attributes, since a CA may reorder a name), the holder's `rfc822Name`, and the key usage / EKU — an encryption certificate issued against a signing request is caught before it is trusted rather than at the holder's first signature. A failed check fails the step. The **chain** is reported as `chain=unchecked`, because it needs a trust store this build has not got. Remaining: the chain, and expiry tracking (Wave 2's reports). |
| `[x]` | CA integration | [spec](features/ca-integration.md) — **phase 1 done, and it is the phase that decides the shape**: the issuer is the **operator** (2026-08-13). The run produces the request, it is signed at whichever CA the deployment uses, and the certificate comes back through the wizard — parsed, summarised, checked against the slot's key and the holder's address, and only then written. **Phase 2's checks 3 and 4 — subject DN and key usage / EKU — are now made**, by the read-back verification; only check 5, the chain to a trusted root, is outstanding, and it is reported as unchecked rather than omitted. An internal pilot CA, BastionVault PKI and an enterprise CA profile stay open as phases 3–5 — each an automation of this path, and each an **ESI** decision about integrating with a corporate PKI (AGENTS.md §8) rather than an implementation waiting to be written. |
| `[x]` | Device detection (Wave 1 phases) | [spec](features/device-detection.md) — **phases 4, 5 and 6 done.** The applet state is read before a run ([`device::applets`](src/device/applets.rs): PIV slots and PIN retries, FIDO2 `get_info`, OTP slots through `ykman`), an **already-configured key is refused** — no override, per the decision of 2026-08-13, and the refusal names the reset — and a generated key carries its **attestation** into the record. Phase 4 is what switched the earlier checks *on*: the pre-flight had been passing `AppletSnapshot::default()`, so every applet-dependent check was written and never fired. **Corrected after the first real bootstrap attempt (2026-08-13, unreleased):** the refusal fired on a factory-fresh key, because `piv::Key::list` reports the **attestation** slot `f9` that Yubico programmes into every key — a check meant to protect one holder's credential was refusing every key ever made, with no override to press. And the same run skipped five of its eleven steps: the native transport reported `usb_applications: ["PIV"]`, which the pre-flight read as *FIDO2 and OTP are disabled*, so nearly the whole procedure quietly declined to happen. Both were checks resting on a field that meant something other than what they read into it — the pattern phase 4 already fixed once, one level down. **And the field itself is now read** (`native-device-transport.md` phase 5): the management applet answers which applications are enabled, so the pre-flight says *the OTP application is switched off on this key* instead of warning that it could not check. |
| `[x]` | Native device transport (Wave 1 phases) | [spec](features/native-device-transport.md) — **phase 6 done, and `native-device` is now the default build**: which transport reads the hardware is decided at startup by probing, overridable in Settings, shown as `via: native` / `via: ykman` in the status bar, reported by `--diagnose`, and recorded as `device.transport.selected`. This is the row that made the other two reachable — `YkDistApp::new` previously held a hardcoded `ykman`, so the hardware-verified native PIV and FIDO2 transports were shipped and unused. **FIDO2 (phase 2) is done and hardware-verified**, writes included. **PIV writes (phase 3) are now built** — and, since 2026-08-13, actually reachable on current firmware: `generate` and certificate import used to authenticate through the crate's 3DES `MgmKey`, which a 5.7 key refuses, and both now run on [`device::piv_session`](src/device/piv_session.rs)'s AES-authenticated session. Not hardware-verified. **Phase 5 — the management applet — is now built** ([`device::mgmt`](src/device/mgmt.rs)): CCID `00 1D`, hand-written because no crate covers it, reporting the form factor, the FIPS state and **which applications are enabled**. That last field is the one whose absence caused the same bug twice, in opposite directions, and both readings of silence are now unnecessary. The application names deliberately match `ykman info`'s, because the pre-flight matches on those strings. Not hardware-verified; the parser is pure and covered byte for byte. Remaining: a key to verify PIV and the management exchange against, and the OTP config frame (phase 4, deliberately not hand-rolled without one). |
| `[x]` | Secrets custody | [spec](features/secrets-custody.md) — **model B decided (2026-08-10)**: transport secret + forced change, nothing retained. `CustodyModel` fixes the vocabulary, the standard template carries the forced-change step, the **generate / show-once / zeroise machinery is built** (`crate::secret`: no `Clone`, no `Serialize`, `Debug` prints `<redacted>`, OS CSPRNG with unbiased digits), and the **sealed-envelope slip** is rendered (`src/envelope.rs`). The two sub-decisions (phase 8 — the PUK and the OTP access code) were answered by the owner on 2026-08-11 and are implemented, and custody of the OTP access code is now recorded per write. Left: the custody **report**, which belongs with [reports & export](features/reports-and-export.md) in Wave 2, and optional external escrow. |

## Wave 2 — Operations at scale

| Status | Feature | Notes |
|---|---|---|
| `[x]` | Key lifecycle & revocation | [spec](features/key-lifecycle-and-revocation.md) — **every phase is done.** The loss report (2) records who said so and when, moves the key to `Lost` in the same operation, and produces the **dependency list** — the certificate serial, the credential ids and their relying parties, the OTP access code, the custody — *derived from the run's step details*, so a register written under schema v1 answers in full. The revocation (3) and the credential removal (4) are **recorded rather than performed**, by the same decision that made the operator the issuer on 2026-08-13: both happen in somebody else's system, and what the register holds is the reason, the reference and the refusal to close an incident over a gap silently. The **reissue gate** (6) refuses to put a key back into stock while it still carries what a bootstrap wrote — per applet, by time, cleared by a reset's own *outcomes* — and the **incident note** (7) is what goes to the ESI, naming its own blind spot rather than reading as exhaustive. **RMA tracking** (8) links a replacement that must already be in the inventory. Phases 5 and 5a, the factory reset and its power cycle, landed out of turn earlier (see below). Schema **v6**: three tables, nothing altered. What a reset still does not do is revoke — nothing here can, and that is the gap named in `docs/security-and-compliance.md` §8. |
| `[x]` | Reports & export | [spec](features/reports-and-export.md) — **every phase is done.** A **Reports** screen with seven reports, all *derived* rather than stored: inventory summary, custody (a key handed out twice counts **once**), unaccounted, bootstrap compliance, certificate expiry (read out of the run's own step details, so a register written before this release answers in full), custody model, and the **audit extract** carrying the range, the chain head and the result of verifying the whole chain at export time — produced even when the chain does **not** verify, because refusing would leave the one person who has to investigate with nothing to look at. CSV and JSON for all of them, PDF for the two that are handed to a person, and a one-click **bundle** into a dated folder from one moment with a manifest. Every export writes `export.taken`. Two things are deliberately absent and say so where they would be assumed: expected-versus-present reconciliation (no procurement data exists to compare against) and a **signature** on the extract, which needs a private key this build does not hold — [audit-trail](features/audit-trail.md) phase 4. |
| `[/]` | Bulk enrolment | [spec](features/bulk-enrollment.md) — **every phase except the batch hand-over documents.** A **Batch** card on the Bootstrap screen, in both shapes, persisted per key and resumable: a session that ends on key 31 of 50 is picked up where it stopped, and the keys already done are refused as duplicates rather than re-run. The design decision worth naming is that a batch **drives the wizard** — same plan, same pre-flight, same confirmation, same audit entries — because a batch mode with its own quieter run path would be a second way of writing to a key, and the second way is always the one that skips a check. A failure never stops the box: the key goes on a needs-attention list with the reason. The pairing list is **all-or-nothing**, every bad row reported at once, because a half-import leaves a part-configured box and a list to reconcile by hand. Schema **v8**. Left: phase 7, the batch hand-over documents, with [receipts & terms](features/receipts-and-terms.md) phase 7. Whether stock preparation may run **unattended** is the procedure owner's call and is built the safe way meanwhile — every key confirmed, exactly as a single run is. |
| `[ ]` | Operator authentication & roles | [spec](features/operator-auth-and-roles.md) — operator identity is currently `$USER`, which is not authentication. Roles (admin / distributor / auditor), AD integration, MFA with a YubiKey on sensitive operations. |
| `[x]` | Multi-operator concurrency | [spec](features/storage-sqlite-single-file.md) phase 4, with [database-selection](features/database-selection.md) phases 6 and 8 — **three answers to three different problems, kept apart.** A busy database now says *another operator is writing to this register* rather than *database is locked*, mapped in `From<rusqlite::Error>` where the next mutation cannot forget it; the retrying stays SQLite's own busy handler, which is woken by the lock being released instead of sleeping through it. An observation is saved against the `updated_at` of the copy the operator read, checked in the `WHERE` clause, so a second save is **refused and audited** (`db.conflict`) instead of quietly discarding the first — a status change is deliberately *not* guarded that way, because it is a transition already checked against the row as it stands, and guarding it would refuse a run that had just written to the hardware. And schema **v7**'s `open_sessions` puts a banner under the tabs naming who else is in the register: a warning rather than a lock, because on a share SQLite serialises the writes and what nobody could otherwise see is two people working out of the same box of keys. The operator identity is now **per register** rather than per workstation. |

## Wave 3 — Alternatives and delivery

| Status | Feature | Notes |
|---|---|---|
| `[ ]` | OpenPGP signing subkey | [spec](features/step-openpgp-signing-subkey.md) — **not the chosen mechanism** (PIV 9c is, decided 2026-08-10). Kept specified for a unit that signs Git commits or `gpg` mail; unscheduled. |
| `[ ]` | SSH authentication via PIV | [spec](features/ssh-authentication.md) — slot 9a plus PKCS#11 for SSH, for units that want it. |
| `[/]` | Packaging & release | [spec](features/packaging-and-release.md) — **every phase except the code-signing certificates is done.** A tag builds macOS, Linux and Windows from a fresh checkout, each artefact is verified by asking the binary about itself (`--diagnose`), and the results land on a **draft** release whose notes carry the schema upgrade warning when `SCHEMA_VERSION` moved. A build now says **which commit it came from**, and a build that cannot — or that came from a dirty tree — fails the verifiers. Linux ships a tarball and a `.deb` with the udev rule, the `.desktop` entry and install notes. **Each desktop platform now ships an installer as well as a portable artefact** (phases 3c and 4): a macOS `.pkg` that installs to `/Applications`, is not relocatable and refuses a wrong architecture, and a per-machine Windows `.msi` built with WiX 6 that upgrades in place — the `.dmg` and the zip stay for the operator who administers their own machine or cannot install software at all. The MSI verifier **installs, interrogates and uninstalls**, because an MSI's components and shortcut are authored rather than copied, and since v0.16.1 the WiX authoring is also read **off** Windows by [`tests/unit_packaging.rs`](tests/unit_packaging.rs) — a comment containing `--` is illegal XML, which is how v0.16.0's Windows build failed after its tag was pushed. v0.16.1's build then failed on the *next* layer, a shortcut naming an icon that was never declared, which no reader of a single file can see because it is the linker that resolves references: since **v0.16.2** CI's Windows leg links the authoring on every commit (`msi.ps1 -LinkOnly`, a placeholder in place of the binary, result deleted), so an authoring error costs a red build rather than a version number. The **`block` 0.1.6 blocker is resolved**, by a four-line patched copy in [`vendor/block`](vendor/block/README.md). What is left is **procurement**: a Developer ID Application *and* Installer certificate (phases 3b and 3c) and an Authenticode certificate (phase 4). Every signing step is already in the workflow, guarded by whether its secret exists. |
| `[ ]` | Compliance artefacts | [spec](features/compliance.md) — classification proposal, system registration, data documentation, change/homologation records. |
| `[x]` | CI & coverage gate | [spec](features/testing-strategy.md) — fmt + clippy + tests + `cargo llvm-cov` with an 80% floor, **enforced on every push** by [`ci.yml`](.github/workflows/ci.yml), with a macOS/Windows/Linux build matrix beside it. This row was `[ ]` while the same work was recorded as done under testing strategy (phases 5 and 10) — the drift is corrected here rather than left as a second opinion about one workflow file. It does not run the hardware tests: a hosted runner has no reader and no USB key. |

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

Three, and **none holds Wave 0** — each is a phase marked `—` (gating no wave), a
later wave, a control that is built and waiting to be switched on, or a policy question
raised by finishing Wave 1. They
were dropped from this list when the big questions were settled on 2026-08-11, which
was an error: an unanswered question that blocks nothing today still blocks something
eventually, and a list that says "None" invites nobody to answer it.

1. **The approved cipher and KDF parameter set.** *Owner: ESI.* Holds
   [`features/db-password-and-encryption.md`](features/db-password-and-encryption.md)
   phase 4 — `PRAGMA kdf_iter` and the cipher page size, stated explicitly rather
   than inherited. Until it is answered the defaults are SQLCipher's own, which are
   reasonable and *not* the same thing as approved. Also listed as an approval gate
   in `docs/security-and-compliance.md` §7. Everything else about the password is
   built.

2. **Can `org-standard`'s OTP access-code step ever run — and if not, which rule
   gives way?** *Owner: the operator who owns the procedure, with the ESI for the third
   option.* Raised 2026-08-14, by finishing the write and finding the step unreachable.

   An access code protects a **configuration**, so the slot must hold one; a programmed
   OTP slot is evidence of a previous bootstrap, which the refusal of 2026-08-13 treats
   as blocking with **no override**. Empty slot, the step skips; programmed slot, the
   whole run is refused. Either way the step never applies, on any key.

   Nothing is broken in isolation — the skip is honest and the refusal is the owner's
   decision working — which is exactly why it needs answering rather than patching. The
   three ways out are set out in
   [`features/step-otp-access-code.md`](features/step-otp-access-code.md): **drop the
   step** from the standard procedure (likeliest, and one edit on the Templates screen);
   **program the slot first** (needs somebody to say what the slot is *for*, and writes
   a credential the holder was not promised); or **narrow the refusal** so a programmed
   OTP slot alone is not evidence — defensible by the same reasoning that already
   excluded PIV slot `f9` and a changed management key, and the one option that weakens
   a control, which is why it is not the implementer's.

3. **Is Ed25519 the signature algorithm the organisation wants?** *Owner: ESI.*
   Holds nothing in the code: the algorithm is named inside every signature, so a
   build meeting one it does not know refuses rather than treating the template as
   unsigned, and adding a second is additive. Chosen for having no parameters to get
   wrong. Recorded so it is **ratified rather than inherited** — the same class of
   decision as the database cipher above, and listed beside it in
   `docs/security-and-compliance.md` §7.



Every question below has an owner's answer; re-open one by moving it back up here
with the date and the reason.

### Answered

- **May an already-configured key be re-bootstrapped, and by whom?** *(2026-08-13)*
  **No — a configured key is only ever returned to factory default, and only by the
  system operator.** There is no in-place re-bootstrap: a key that already carries a
  PIN, a credential or a certificate is reset first, deliberately, and then prepared
  as if new.

  That is a stronger answer than the question asked for, and it removes an ambiguity
  rather than adding a feature. The alternative — writing over what is on a key —
  would replace a PIN the holder chose (custody model B *tells them* to change it),
  invalidate a certificate somebody may have signed with, and leave the register
  claiming a procedure was applied to a key whose previous state nobody recorded. A
  reset destroys all of it at once, visibly, and the register can say so.

  What it obliges, in [`features/device-detection.md`](features/device-detection.md)
  phase 5 and [`features/key-lifecycle-and-revocation.md`](features/key-lifecycle-and-revocation.md)
  phase 5: detection must *recognise* a configured key and **refuse the run**, naming
  the reset as the way forward rather than offering an override; and the reset is an
  operator action of its own, previewed and confirmed like every hardware write, with
  what it destroys named before it runs.

- **What signs a bootstrap procedure?** *(2026-08-13)* **A key the application
  generates by default, with an interface to import an external one.** The owner's
  decision, and it reverses the shape 0.9.0 shipped with — verification only, no
  private key anywhere near the tool.

  The consequence is stated here rather than discovered later: **whatever the
  application can sign with, anybody who can open the register can sign with.** The
  control moves from "only the holder of the organisation's key approves a procedure"
  to "an operator with the register and its password approves a procedure". That is a
  real reduction, and it is the owner's to make — a signing key that nobody has is a
  control that is never switched on, which is worth less than a weaker control that
  is.

  What it obliges, and what the implementation has to carry:
  - the generated private key is **encrypted at rest**, never in a log, an audit
    entry or an error message, and never exported by accident;
  - **signing is audited** — which key, which procedure, which operator;
  - the **import path** exists for a deployment that has an HSM, a smartcard or an
    offline machine, so the stronger arrangement stays available to whoever wants it;
  - the public half is still what verification uses, so a procedure signed by an
    imported key verifies on a workstation that has never held the private half.

  Tracked in [`features/bootstrap-templates.md`](features/bootstrap-templates.md)
  phase 5's notes as a follow-up, not yet built.

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

  **Built on 2026-08-13**, and the manual issuer is the one that exists: the tool
  asks the operator for the certificate. Which makes the checks on it the
  substance of the feature rather than plumbing — it arrives by hand, so it is
  parsed, summarised on screen, matched against the slot's public key and against
  the holder's `rfc822Name`, and only then written. See the decision log.

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
| 2026-08-14 | After an incident the register **records** the revocation and the credential removal; it does not perform them | Same shape as the CA decision of 2026-08-13, and forced by the same fact: the certificate is revoked at whichever CA issued it and the credential is removed at a relying party somebody else runs, so the mode that always works is the one to build. What that makes load-bearing is the *list*: nobody remembers a certificate serial or a credential id the morning after a key goes missing, and both are already in the run record. So the tool derives what was on the key, says what is outstanding, holds the reason and the reference that make each claim checkable, and refuses to close an incident over a gap without a note saying why. Automating either is an automation of this path (`ca-integration` phases 3–5) rather than the missing half of it |
| 2026-08-14 | A key may not be **reissued** until every applet a run wrote to has a factory reset on record — per applet, and by time | The decision of 2026-08-13 says a configured key is only ever returned to factory default, and until now nothing enforced the *other* end of that: a returned key could go back into stock carrying the previous holder's certificate in slot 9c and their resident credentials, and the next procedure would prepare it for somebody else. Per applet because a key is often returned for one of them, and by time because counting resets would let one from last year clear a key bootstrapped last week. What clears it is the reset's own **outcomes**, not what the operator ticked: an applet that refused is not clean, and pretending otherwise would put the register's word behind a claim the transport did not make. A bench reset can be recorded by hand, and the trail says whose word it is (`source=operator`) |
| 2026-08-14 | `block` 0.1.6 is fixed by a **patched copy in this repository**, not by dropping the camera | Four routes, three unavailable: no upstream fix (no release since 2020, and `block2` is not API-compatible with what `nokhwa` calls), a native AVFoundation capture path is weeks of platform code to replace a dependency that works, and making `camera` opt-in again reverses a decision made on 2026-08-10 for a reason that still holds — an operator should not need a special build to point a webcam at a box label. The fourth is what cargo's own warning recommends for a dependency whose maintainer cannot be waited on, and the change is four lines: an `extern` static of an uninhabited type becomes one of a zero-sized inhabited type. Kept honest by being kept small and documented — `vendor/block/README.md` carries the diff, the reasoning and the two-step revert, and nothing in this project's own code refers to `block` |
| 2026-08-14 | **Free text is never substituted into a generated file** — it is set afterwards, by a tool that takes it as an argument | `YKDM_COPYRIGHT` went into `Info.plist` through a `sed` replacement, which is a command, not a text-insertion: `&` means the whole match, so `"Foo & Bar"` silently wrote `Foo @COPYRIGHT@ Bar`, and `\|` was the delimiter, so a copyright containing one failed the build talking about a sed script. `sed` stays for the version and the identifier, which cannot carry such a character — and `write-plist.sh` now *refuses* one that does, because that constraint was an assumption nothing checked. The rule generalises past macOS: a Windows manifest and a Linux `.desktop` file take the same institution-supplied strings, and `plutil -replace` is the shape to copy — value as an argument, escaping done by the tool that owns the format. `PlistBuddy -c "Set …"` is not, though it is the idiom two lines away for the icon: it re-parses its command string, eating double quotes and failing on an apostrophe |
| 2026-08-13 | **A completed bootstrap run is what makes a key `Bootstrapped`** — not the operator remembering a button | The lifecycle's first arrow had nobody to perform it, so the register could say *In stock* about a key that had just had a PIN, a policy and a certificate written to it. The register saying something the hardware contradicts is the failure this tool exists to prevent, and the alternative — asking the operator to mark it — is a step whose omission is invisible until a hand-over is refused hours later. `Completed` is the threshold because the engine already draws that line: a run with a required step unmet is `Failed` and audited `bootstrap.incomplete`, "the key is not ready to hand over". A run never overrules `lost`, `retired` or `distributed`; *mark bootstrapped* stays for a key configured outside the tool |
| 2026-08-13 | The PIV management key is handled by **this tool's own AES exchange**, not by the crate and not by `ykman` | The recommendation on the table was the `ykman` fallback for this one step, and it was overtaken by measurement: `yubikey` 0.8 cannot authenticate to a 5.7 management slot at all, because its `MgmKey` is a 3DES type and 5.7 removed 3DES. The exchange is two APDUs and it is now written, read the slot's real algorithm, and mutually authenticated — so the alternative cost of a PIN on a command line and a Python dependency buys nothing. The scope stays one exchange plus the two writes that need it (`GENERATE`, `PUT DATA`): PIN, PUK, signing and attestation were measured working through the crate and stay there |
| 2026-08-13 | The tool **asks the operator for the certificate**; there is no CA integration | The issuer differs per deployment and is somebody else's system, so the mode that always works is the one to build first: the run produces a request, whoever runs the deployment has it signed however they do that, and the certificate comes back through the wizard. This closes the question that had the import step skipping on every run. It also makes the checks on that certificate load-bearing, because it arrives by hand: it must parse, its public key must be the key in the slot, and it must carry the holder's address — all before the write. An internal pilot CA, BastionVault or an enterprise CA are automations of this path and stay open as later phases |
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
