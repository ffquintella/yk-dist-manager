# Feature: Native device transport

## Summary

Talk to the YubiKey from Rust, in process, instead of shelling out to `ykman`:
`yubikey` for PIV over PC/SC, `ctap-hid-fido2` for FIDO2/CTAP2 over USB HID, and
`hidapi` for the Yubico OTP slots.

## Motivation

A subprocess transport has four concrete problems for this tool:

1. **Secrets on a command line.** `ykman fido access change-pin --new-pin 123456`
   puts the PIN in an argv vector visible to `ps` and, on some systems, in shell
   history. The native path passes it as a parameter of a function call.
2. **Errors are strings.** Deciding "wrong PIN" from "no device" from "applet
   locked" means matching English text that changes between versions. CTAP and PIV
   return status words; the crates surface them as typed errors.
3. **Coverage.** `ykman` *cannot* create a FIDO2 credential (only list and delete
   them) and *cannot* put an e-mail SAN in a CSR. Both are core requirements. See
   `docs/yubikey-reference.md`.
4. **Deployment.** A workstation needs Python and `ykman` on `PATH`, at a
   compatible version, forever.

## Current state

**Phase 1 shipped.** `src/device/native.rs` implements `YubiKeyBackend` over the
`yubikey` crate:

- `Context::open()` → iterate readers → `reader.open()` → `serial()`, `version()`.
- A reader that cannot be opened (another process holds it) is logged and skipped
  rather than failing the whole enumeration.
- Verified against real hardware: `tests/hardware_native.rs` reads serial
  `20423633`, firmware `5.4.3` from a YubiKey 5 NFC and asserts the native read
  agrees with the `ykman` read.
- Behind the `native-piv` feature, because `pcsc` links a system library
  (`PCSC.framework`, `libpcsclite`, `WinSCard`).

Not yet done: the FIDO and OTP transports, and the management applet.

## Design

### Feature flags

| Feature | Enables | Links against |
|---|---|---|
| `native-piv` | `yubikey` crate, PIV applet | PC/SC |
| `native-fido` | `ctap-hid-fido2`, FIDO2 applet | USB HID |
| `native-otp` | `hidapi`, OTP slots | USB HID |
| `native-device` | all three | — |

Default build has none of them: it compiles anywhere and uses the `ykman`
fallback. Distributed builds enable `native-device`.

### The gap the crates do not close

The **management applet** (`00 1D`) is what reports form factor, per-application
enable flags over USB and NFC, and FIPS status. No crate exposes it, so
`ykman info` remains the source for those fields until we implement the APDU
ourselves (Phase 5). The inventory record therefore accepts an empty
`form_factor` from the native path.

### Backend selection

`YkDistApp` holds a `Box<dyn YubiKeyBackend>`. Selection order at startup:
native if compiled in and a reader responds, else `ykman` if on `PATH`, else a
disabled state that tells the operator what is missing. Both are always
available to the plan, so a step can name its transport per step rather than per
session.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | PIV identification over PC/SC | Done | serial + firmware, hardware-verified |
| 2 | FIDO2 transport (`get_info`, PIN, credential) | **Done** | [`src/device/native_fido.rs`](../src/device/native_fido.rs) — **hardware-verified on a 5.7.4 key**, reads and writes, including the resident credential `ykman` cannot create |
| 3 | PIV write operations (PIN/PUK/mgmt key, keygen, cert import, attest) | Todo | `features/step-piv-*.md` |
| 4 | OTP slot HID config frames | Todo | `features/step-otp-access-code.md` |
| 5 | Management applet APDU (form factor, capabilities, FIPS) | Todo | removes the last read-only dependency on `ykman` |
| 6 | Backend auto-selection + Settings override | Todo | with a visible indicator of which transport is live |
| 7 | Cross-check mode | Todo | run both transports on read paths and log divergence during the migration |

## Audit events

| Event | When |
|---|---|
| `device.detected` | A key was read successfully |
| `device.detect.failed` | Enumeration or identification failed (reason, no secret) |
| `device.transport.selected` | Backend chosen at startup or changed in Settings |
| `device.transport.divergence` | Phase 7: the two transports disagree on a read |

## Tests

- `tests/hardware_native.rs` — ignored by default; read-only; asserts native and
  `ykman` agree.
- `tests/behaviour_bootstrap.rs` — no-key and two-keys-attached cases through
  `MockBackend`.
- Unit tests for every parser on the fallback path (`tests/unit_ykman_parse.rs`).
- Phase 2+: each write operation gets a behaviour test against a mock transport;
  **no test ever writes to a real key**.

## Open questions and gates

- Windows PC/SC service and driver behaviour needs testing on a managed
  workstation image before Phase 3 ships.
- Linux needs `pcscd` plus a udev rule for HID access; document per platform in
  `docs/operations.md`.
- ESI gate: using a hardware transport in process is an architecture premise
  change relative to "drive the vendor CLI"; flag it when the architecture is
  reviewed.

## References

- `src/device/native.rs`, `src/device/mod.rs`
- `docs/yubikey-reference.md`
- [`yubikey` crate](https://docs.rs/yubikey), [`ctap-hid-fido2`](https://docs.rs/ctap-hid-fido2)
