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

Not yet done: hot-plug polling, an explicit picker for multiple keys, and reading
per-applet state (PIN retries, occupied PIV slots) for verification.

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

The native path deliberately reports an empty `form_factor` rather than guessing.

### Firmware gates

`YubiKeyRecord::supports_fido_min_pin_length()` and
`device::ykman::supports_min_pin_length()` implement the 5.7 floor. The wizard
uses them to skip optional steps automatically instead of letting them fail.

### Polling (Phase 2)

egui repaints continuously, so detection must not run per frame. Design: a
background thread polls `list_serials()` every 1.5s, publishes to a channel, and
the UI reads the latest snapshot. Polling is read-only and cheap; identification
(`info`) runs only when the serial set changes.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Read on demand, insert/refresh inventory | Done | Inventory + wizard |
| 2 | Background hot-plug polling | Todo | 1.5s, channel to the UI, never in the paint pass |
| 3 | Explicit picker when several keys are attached | Todo | show serial + model; never auto-pick |
| 4 | Per-applet state read (PIN retries, PIV slots, FIDO PIN set?) | Todo | needed by `StepKind::Verify` |
| 5 | "This key is already bootstrapped" warning | Todo | detect an occupied 9c slot or an existing FIDO PIN before re-running a template |
| 6 | Attestation read (`piv keys attest 9c`) | Todo | proves on-device generation; stored with the run |

## Audit events

| Event | When |
|---|---|
| `key.added` | A serial never seen before was read |
| `key.refreshed` | A known serial was read again (firmware/applications updated) |
| `device.detect.failed` | Detection failed, with the reason |
| `device.ambiguous` | More than one key attached; nothing was chosen |

## Tests

- `scenario_no_key_attached_is_reported_clearly` — the message names the problem.
- `scenario_two_keys_attached_is_refused_rather_than_guessed`.
- `scenario_reading_the_same_key_twice_does_not_duplicate_inventory` — upsert on
  serial, firmware updated, one row.
- `refresh_keeps_lifecycle_and_notes` — a re-read must not reset a distributed key
  to "in stock" or wipe operator notes.
- Hardware: `tests/hardware_native.rs` (ignored by default).

## Open questions and gates

- Phase 5 needs a policy: is re-bootstrapping an already-configured key allowed,
  and does it require a second operator? That is an operational decision, not a
  code one.

## References

- `src/device/mod.rs`, `src/app.rs` (`detect_keys`)
- `features/native-device-transport.md`, `features/key-inventory.md`
