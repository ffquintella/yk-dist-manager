# Feature: Device detection

## Summary

Identify the key in front of the operator — serial, model, firmware, form factor,
enabled applications — and turn that into an inventory record without typing.

## Motivation

Every record in this tool is keyed on the **serial number**. Typing a serial from
the engraving is the single most likely way to corrupt the dataset: the digits are
small, and a transposed pair silently attributes a key to the wrong person. The
serial must come from the hardware.

Detection also gates the template: a firmware 5.4 key cannot take a minimum-PIN-length
policy, and a key with PIV disabled cannot take a signing certificate. Reading the
device first turns those into greyed-out steps instead of mid-run failures.

## Current state

**Read-on-demand works.** "Read attached key" on the Inventory screen and
"read attached key" in the wizard both call `YubiKeyBackend::info(None)`, then
insert or refresh the inventory record and pre-fill the wizard's serial field.

- Exactly one key attached → identified.
- No key → `DeviceError::NoDevice`, shown in the status bar.
- More than one key → `DeviceError::Ambiguous(n)`, refused rather than guessed.
  Picking one at random and writing a PIN to it is the worst possible outcome.

**Done for Wave 0, and phases 4–6 have now landed for Wave 1.** The applet state is
read before a run, an already-configured key is refused, and a generated key carries
its attestation into the record.

Worth being precise about what phase 4 changed, because it is the kind of gap that
hides: the three state reads already existed on the *write* traits, and the pre-flight
said `AppletSnapshot::default()` with a comment explaining that reading one needed a
write-capable transport. So every check that depended on applet state — including the
"already configured" one — was written and **never fired**. `device::applets` is the
read side, callable from a screen, and it is what turned those checks on.

The one thing phase 4 cannot answer is whether an OTP slot carries an **access code**:
neither the HID status frame nor `ykman otp info` reports it, and the only way to find
out is to attempt a write and be rejected. So no read ever claims one, and it is the
record rather than the key that says whether this tool set one.

- **Keys are noticed as they are plugged in** ([`device::watch`](../src/device/watch.rs)):
  a background thread enumerates on a tick, identifies only when the set of serials
  changes, and publishes a snapshot the GUI reads once per frame. The status bar
  carries it, so "which key is this application about to act on" is answerable from
  any screen that is watching.
- **Several attached is a choice, not a guess.** The picker lists them; nothing is
  selected on the operator's behalf; and a selection is dropped the moment that key
  is unplugged, rather than leaving the wizard aimed at a serial nobody can see.
- **The watch never overlaps a run.** It is stopped — and its thread joined — before
  the first write, and restarted by the next frame.

## Design

### What is read, and from where

| Field | Native source | Fallback |
|---|---|---|
| Serial | `YubiKey::serial()` (PIV applet) | `ykman list --serials` |
| Firmware | `YubiKey::version()` | `ykman info` |
| Model / marketing name | — (reader name only) | `ykman info` → `Device type` |
| Form factor | — (management applet, not covered) | `ykman info` → `Form factor` |
| Enabled applications | — (management applet) | `ykman info` applications table |
| FIPS | inferred from the model string | same |

The native path deliberately reports an empty `form_factor` **and an empty application
list** rather than guessing, and the second one is load-bearing rather than tidy: an
empty list means *not read*, and the pre-flight reads it that way — no reason to skip a
step. It once reported `["PIV"]`, the applet it had just spoken to, and that one word was
a claim that FIDO2 and OTP were disabled: five of the eleven steps of the standard
procedure skipped themselves on a key that had all three enabled. The rule is the same
one `from_serial` follows — a field nobody read stays empty.

Because that leaves a real gap, the pre-flight now *names* it: an applet whose enablement
is neither listed nor readable raises a `Warning` per step, saying the step will be
attempted and will fail there rather than skip if the application turns out to be off.
Closing the gap properly means the management applet (CCID `00 1D`), which
`features/ykman-abstraction.md` still owns.

### Firmware gates

`YubiKeyRecord::supports_fido_min_pin_length()` and
`device::ykman::supports_min_pin_length()` implement the 5.7 floor. The wizard
uses them to skip optional steps automatically instead of letting them fail.

### Polling (Phase 2)

egui repaints continuously, so detection must not run per frame: a read costs tens
of milliseconds over PC/SC and about a second through the `ykman` subprocess, and
either would be paid sixty times a second. So a background thread does the looking
and publishes an [`Attached`](../src/device/watch.rs) snapshot the GUI clones.

Four properties, each of which is a test:

1. **Identification runs only when the set of serials changes.** A key sitting in a
   port costs one enumeration per tick and nothing else — which is what keeps the
   subprocess transport tolerable: one fork per tick, not one per key per tick.
2. **The interval follows the transport.** 1.5s natively, as the original design
   said; **4s** when every poll forks a Python process. At 1.5s that would be 40
   processes a minute for as long as a screen is open, and an operator cannot tell
   the two apart while walking a key from a box to a port.
3. **It runs only while a screen that shows attached keys is open** (Inventory,
   Bootstrap), and stops on the way out. Polling for a screen nobody is looking at
   is pure cost.
4. **It never overlaps a run.** `execute_run` stops the watch first, and stopping
   joins the thread, so no enumeration is in flight when the first write goes out.
   Enumerating readers while another handle holds an exclusive transaction is not
   something to discover halfway through setting a PIN.

**What it does not do: write to the register.** Plugging a key in fills a list and a
field. Recording it stays a click, because a tool that added inventory rows because
somebody plugged something in would be making records nobody asked for — the same
rule as `docs/security-and-compliance.md`'s "nothing mutates as a side effect of
opening a screen".

A transport that is *missing* stops the watch instead of forking a doomed subprocess
every few seconds for the rest of the session; a transport that is merely *busy*
keeps it running and shows the reason.

### The picker (Phase 3)

With more than one key attached, `info(None)` still refuses — that refusal is the
point, and it predates the picker. What the picker adds is a way to *choose*: a row
per key with its serial, model, firmware and applications, and *Use this one*.

- **Nothing is chosen for the operator.** Writing a PIN to whichever key a transport
  happened to list first is the worst outcome this feature has, and it is the one
  thing the code must never do to be helpful.
- **The list is ordered by serial**, not by enumeration order. A list that reshuffles
  itself between polls is one where somebody clicks the wrong row.
- **A key that enumerates but will not describe itself is shown as itself**, not
  hidden and not counted as absent: that is usually a driver or a permission, and
  "no key attached" would send the operator after a cable.
- **A selection dies with its key.** Unplug the chosen one and the selection goes,
  with the status line saying so.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Read on demand, insert/refresh inventory | 0 | Done | Inventory + wizard |
| 2 | Background hot-plug polling | 0 | **Done** | [`device::watch`](../src/device/watch.rs) — a thread enumerates on a tick and publishes a snapshot the GUI clones; identification runs **only when the set of serials changes**. 1.5s with a native transport, 4s when every poll is a `ykman` subprocess — chosen from the *live* transport, not the compiled one — and only while a screen that shows attached keys is open |
| 3 | Explicit picker when several keys are attached | 0 | **Done** | serial, model, firmware and applications per row, with *Use this one*; on the Inventory screen and above the wizard's serial field. Nothing is chosen for the operator, `device.selected` records which one was, and a selection is dropped when that key is unplugged |
| 4 | Per-applet state read (PIN retries, PIV slots, FIDO PIN set?) | 1 | **Done** | [`device::applets`](../src/device/applets.rs) — read-only, one call per applet, failures recorded as reasons rather than dropped. PIV slots + management-key/PIN-default flags + **PIN retries** natively; FIDO2 via CTAP2 `get_info`; OTP slots via `ykman otp info`, a labelled fallback because no crate exposes the OTP HID status frame. Audited as `device.applets.read` |
| 5 | "This key is already bootstrapped" **refusal** | 1 | **Done** | `Preflight::check_already_configured` — a `Blocking` finding, before any per-step check, naming the evidence *and* the reset. **No override**, per the decision of 2026-08-13. A changed PIV management key is deliberately not evidence: a fleet-management tool may have set it without ever bootstrapping the key. Neither is **PIV slot `f9`**: it is the factory attestation slot, present on every key since firmware 4.3 and not cleared by a reset, and counting it made the refusal fire on the first factory-fresh key it ever saw — `PivState::configured_slots` is what the refusal rests on, while `occupied_slots` stays the raw read a description shows |
| 6 | Attestation read (`piv keys attest 9c`) | 1 | **Built** (not hardware-verified) | `PivWriter::attest`, plus `KeygenEvidence::attestation_pem` read *at generation time* and stored in the step's detail; the verification step re-reads it. A firmware with no attestation is recorded as `NO attestation — unproven`, never omitted. **No key was attached when this was written**, so the APDU path is unexercised |

## Audit events

| Event | When |
|---|---|
| `key.added` | A serial never seen before was read |
| `key.refreshed` | A known serial was read again (firmware/applications updated) |
| `device.detect.failed` | Detection failed, with the reason |
| `device.ambiguous` | More than one key attached; nothing was chosen. Written when the watch first sees that arrangement, and when a read-on-demand is refused for it |
| `device.selected` | The operator chose one of several attached keys. With several keys in front of somebody, *which one they picked* is part of the story of everything written afterwards |
| `device.applets.read` | The per-applet state was read before a run. Recorded because the phase-5 refusal rests on it, and "what did the tool actually see" is the first question when one is disputed. States, slots and counts only — never a secret |

## Tests

- `scenario_no_key_attached_is_reported_clearly` — the message names the problem.
- `scenario_two_keys_attached_is_refused_rather_than_guessed`.
- `scenario_reading_the_same_key_twice_does_not_duplicate_inventory` — upsert on
  serial, firmware updated, one row.
- `refresh_keeps_lifecycle_and_notes` — a re-read must not reset a distributed key
  to "in stock" or wipe operator notes.
- Hardware: `tests/hardware_native.rs` (ignored by default).

### Phases 2 and 3

`src/device/watch.rs` (8 unit tests): a key plugged in appears and one pulled out
disappears; identification runs only when the serial set changes (asserted by
counting calls, which is the cost control); two keys are reported as two and
`only_key()` stays `None`; a key that enumerates but cannot be read is reported as
itself; a transient failure keeps the watch running while a **missing transport**
stops it; and **dropping the watch stops the thread promptly** — tested with a
30-second interval, because that drop is what gets the hardware out of the way
before a run.

`tests/behaviour_app_device_watch.rs` drives the application: the watch runs on
Inventory and not on Audit, syncing every frame does not restart it, a key arriving
fills the wizard's field **and records nothing**, a second key makes the arrangement
ambiguous, unplugging the chosen key drops the selection and says so, choosing one is
audited with the model and the count, and `execute_run` leaves `watch: None` — the
interlock, asserted rather than assumed.

## Open questions and gates

- ~~Phase 5 needs a policy: is re-bootstrapping an already-configured key allowed?~~
  **Answered 2026-08-13: no.** A configured key is only ever returned to factory
  default, and only by the system operator; then it is prepared as if new.

  So phase 5 is a **refusal**, not a warning with an override — which is a simpler
  thing to build and a stronger thing to have. Writing over what is on a key would
  replace a PIN the holder chose (model B *tells* them to change it), invalidate a
  certificate somebody may have signed with, and leave the register claiming a
  procedure was applied to a key whose previous state nobody recorded. A reset
  destroys all of it at once, visibly, and the register can say so.

  Two obligations follow. Detection has to *recognise* a configured key — which is
  phase 4, the per-applet read — so this phase cannot land before it. And the reset
  itself is an operator action of its own
  (`features/key-lifecycle-and-revocation.md` phase 5), previewed and confirmed like
  every hardware write, naming what it destroys before it runs.

## References

- `src/device/mod.rs`, `src/device/watch.rs`, `src/app.rs` (`detect_keys`,
  `sync_device_watch`, `poll_device_watch`, `target_serial`, `select_key`)
- `src/ui/inventory.rs` (`attached`), `src/ui/bootstrap.rs` (`attached_keys`)
- `features/native-device-transport.md`, `features/key-inventory.md`
