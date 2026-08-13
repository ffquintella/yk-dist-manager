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

**Phases 1, 2 and 6 shipped.** Phase 6 is the one that matters most for the other
two: until it landed, `YkDistApp::new` held a hardcoded `YkmanBackend::default()`, so
a build compiled with `--features native-device` — whose FIDO2 transport is
hardware-verified and whose PIV read agrees with `ykman` on a real key — still shelled
out to a Python subprocess for every enumeration. The code was shipped and
unreachable. It is now selected at startup by probe, overridable in Settings, shown as
`via: native` / `via: ykman` in the status bar, and recorded as
`device.transport.selected`. Measured on the developer's machine: a
`--features native-device` build now decides *"native — a reader answered, and this
build talks to it in process"*; the default build decides `ykman` and says why.

Phase 1: `src/device/native.rs` implements `YubiKeyBackend` over the
`yubikey` crate:

- `Context::open()` → iterate readers → `reader.open()` → `serial()`, `version()`.
- A reader that cannot be opened (another process holds it) is logged and skipped
  rather than failing the whole enumeration.
- Verified against real hardware: `tests/hardware_native.rs` reads serial
  `20423633`, firmware `5.4.3` from a YubiKey 5 NFC and asserts the native read
  agrees with the `ykman` read.
- Behind the `native-piv` feature, because `pcsc` links a system library
  (`PCSC.framework`, `libpcsclite`, `WinSCard`).

Phase 2: `src/device/native_fido.rs`, hardware-verified on a 5.7.4 key for both
reads and writes — including the resident credential `ykman` cannot create.

Not yet done: PIV writes (phase 3 — the remaining gap is a PKCS#10 CSR builder
carrying an `rfc822Name` SAN; the write path currently returns
`WriteError::Unsupported`), the OTP slot config frames (phase 4), and the management
applet (phase 5), which is what still forces `ykman` for form factor, capabilities
and FIPS state.

## Design

### Feature flags

| Feature | Enables | Links against |
|---|---|---|
| `native-piv` | `yubikey` crate, PIV applet | PC/SC |
| `native-fido` | `ctap-hid-fido2`, FIDO2 applet | USB HID |
| `native-otp` | `hidapi`, OTP slots | USB HID |
| `native-device` | all three | — |

**`native-device` is on by default as of 0.12.0.** It was opt-in while the transports
were being written, and keeping it opt-in after they worked meant the build people
actually ran shelled out to a Python subprocess for every read while the in-process
transport sat compiled out. The flag was protecting against a case `decide` already
handles — no reader, so demote to `ykman` — at the cost of making the good path the one
nobody got.

The `ykman`-only build stays supported, because a workstation with no PC/SC service or
no HID permission is a real deployment:

```bash
cargo build --no-default-features --features file-dialog,camera
```

CI compiles it on all three platforms for exactly that reason: in a native-by-default
world, nothing else would notice it had stopped building.

**What the default now carries, stated rather than discovered.** `native-piv` enables
`yubikey/untested`, which is upstream's own name for the feature gating every mutating
PIV call — `change_pin`, `change_puk`, and the three management-key setters. Making
`native-device` a default feature therefore puts those calls in the **shipped** build,
not only in an opt-in one.

Today that is latent rather than live: no code path in this build writes to a key
(`MockWriter` is the only implementation of the write traits, and the PIV write path
returns `WriteError::Unsupported`), so what ships is unreachable code. It stops being
latent when **phase 3** lands. The gate recorded in
[`features/step-piv-pin-puk-management-key.md`](step-piv-pin-puk-management-key.md)
therefore applies to the *default* build from then on, and the worst failure it guards
against — a management key nobody holds, leaving the applet administratively dead — is
now a failure the default build would be able to cause. Phase 3 must not ship on the
strength of "it was already compiled in".

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

Implemented in [`src/device/select.rs`](../src/device/select.rs), split in two so
the decision is testable without a reader: `probe()` asks the machine the two
questions, and `decide()` is a pure function from `(requested, Availability)` to a
`Choice`. Four properties are deliberate:

* **The probe decides, not the feature flag.** A flag says what was compiled; it
  cannot say whether `pcscd` is running, whether the Smart Card service was
  disabled by policy, or whether another process holds the reader.
* **An empty reader list counts as reachable.** The question is whether PC/SC
  answers, not whether a key is plugged in — otherwise an operator who opens the
  application before reaching for a key is demoted to the subprocess for the whole
  session.
* **The probe may demote; nothing silently promotes.** Choosing `ykman` when native
  would have worked costs a subprocess per read. Choosing native when PC/SC is dead
  costs every read failing until the application is restarted.
* **An override is honoured, and reported.** A forced transport that cannot work is
  described as forced *and* failing, because the operator using the override is
  usually the person diagnosing the machine, and an application that quietly
  overrules them makes the diagnosis impossible.

Nothing available is a **state, not a panic**: the register opens, keys are recorded
by serial from a barcode or by hand, and the screens say what is missing. A tool that
refused to start without a reader would be useless for the half of this job that is
paperwork.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | PIV identification over PC/SC | 0 | Done | serial + firmware, hardware-verified |
| 2 | FIDO2 transport (`get_info`, PIN, credential) | 1 | **Done** | [`src/device/native_fido.rs`](../src/device/native_fido.rs) — **hardware-verified on a 5.7.4 key**, reads and writes, including the resident credential `ykman` cannot create |
| 3 | PIV write operations (PIN/PUK/mgmt key, keygen, cert import, attest) | 1 | Todo | `features/step-piv-*.md` |
| 4 | OTP slot HID config frames | 1 | Todo | `features/step-otp-access-code.md` |
| 5 | Management applet APDU (form factor, capabilities, FIPS) | 1 | Todo | removes the last read-only dependency on `ykman` |
| 6 | Backend auto-selection + Settings override | 1 | **Done** | [`src/device/select.rs`](../src/device/select.rs); `via: …` in the status bar, override in Settings, `device.transport.selected` audited. **This is what made phases 1–2 reachable** — until it landed, `YkDistApp::new` said `YkmanBackend::default()` and no build flag or setting could change it |
| 7 | Cross-check mode | 2 | Todo | run both transports on read paths and log divergence during the migration |

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
- Phase 6: `src/device/select.rs` unit tests cover every branch of `decide` —
  including the two that are only reachable on a machine this developer does not
  have (native compiled with a dead reader; nothing available at all) — and
  `tests/behaviour_app_transport.rs` covers the wiring: persisted, audited, the
  watch stopped so it cannot poll a transport the status bar is not naming, and a
  no-op change that does not fill the trail. Run in **both** feature
  configurations, because the interesting assertions differ.
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
