# Feature: `ykman` fallback and output parsers

## Summary

A `YubiKeyBackend` implementation that drives the `ykman` CLI as a subprocess,
used where no Rust crate covers the operation yet, and as a cross-check while the
native transport is validated.

## Motivation

`ykman` is the vendor tool: it is correct, it is maintained, and it covers
everything. It is a poor *primary* transport (see
`features/native-device-transport.md`), but it is the right thing to have
underneath while the native path grows, and for two applets where nothing else
exists today (OTP slot configuration, management-applet metadata).

## Current state

**Done.** `src/device/ykman.rs`:

- `run(&[&str])` builds an argv vector — never a shell string, so no user value
  can be interpreted as a command.
- `std::io::ErrorKind::NotFound` becomes `DeviceError::ToolMissing` with the
  binary name, so the operator is told to install `ykman` instead of seeing a
  raw OS error.
- Non-zero exit becomes `DeviceError::Command` with stderr, trimmed.
- `parse_serials` and `parse_info` are pure functions, unit-tested against output
  recorded from **ykman 5.9.2** in `tests/fixtures/`.
- The binary path is configurable (Homebrew on Apple silicon, portable Windows
  builds).

## Design

### Parsed surfaces

| Command | Parsed into | Notes |
|---|---|---|
| `ykman list --serials` | `Vec<u32>` | one decimal per line; non-numeric lines (warnings) ignored |
| `ykman --device <serial> info` | `DeviceInfo` | `Key: value` lines plus the applications table |

The applications table is tab-separated in real output but space-aligned in some
terminals, so `parse_app_row` handles both. Only applications whose USB column
reads `Enabled` are recorded.

Parsing rules that matter:

- A missing serial number is an **error**, not a zero. A record keyed on serial
  `0` would silently merge unrelated keys.
- A non-numeric serial is an error, not a default.
- Unknown `Key: value` lines are ignored, so a new ykman version adding a line
  does not break detection.

### Rules for anything added here

- Every new invocation gets a fixture and a unit test with the ykman version
  recorded in the test module docs.
- Never pass a secret as an argument. If a step needs a PIN and no native path
  exists yet, the plan shows it as `<PIN>` and the executor must use `ykman`'s
  interactive prompt (`-` sentinel) rather than argv.
- Version drift is detected, not assumed: Phase 3 records `ykman --version` on
  every run and warns when it changes.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | Backend + argv invocation + typed errors | 0 | Done | `src/device/ykman.rs` |
| 2 | `list --serials` and `info` parsers with recorded fixtures | 0 | Done | `tests/unit_ykman_parse.rs`, 8 tests |
| 3 | Version detection and compatibility warning | 2 | Todo | store `ykman --version` on the bootstrap run |
| 4 | Interactive-prompt path for secret-carrying steps | 0 | **Done** | `YkmanBackend::run_with_stdin` — the prompt form (`--new-access-code -`) with the value on **stdin**, because an argument vector is readable by every process on the workstation. Its one caller is the OTP access code ([`step-otp-access-code.md`](step-otp-access-code.md) phase 2), which is the only secret-carrying step with no native path; the arguments are logged and the input never is |
| 5 | `piv info` / `fido info` / `otp info` parsers for verification steps | — | **Closed — not needed** | `Verify` reads the applets natively and reads the certificate back off the card. `parse_otp_info` is the one that earned its keep and exists; writing the other two would be parsing text to learn what the transport already answers in types |
| 6 | Retire the parsers that Phase 5 of the native transport replaces | — | **Closed — not needed** | the management applet read (`native-device-transport.md` phase 5) gives the native transport everything `ykman info` was parsed for, but `ykman` remains a **supported transport** that `device::select` may choose at runtime — so its parsers are not dead code, they are the other transport's implementation |

## Audit events

Fallback usage is visible, not silent:

| Event | When |
|---|---|
| `device.fallback.used` | A step ran through `ykman` because no native path exists |
| `device.tool.missing` | `ykman` was needed and not found |
| `device.tool.version` | Version recorded at the start of a run |

## Tests

- `tests/unit_ykman_parse.rs` — 8 unit tests: serial list, noise tolerance, empty
  list, full `info`, disabled applications not listed, missing serial rejected,
  non-numeric serial rejected, firmware gate.
- `tests/fixtures/ykman_info_5nfc.txt` — real capture, YubiKey 5 NFC fw 5.4.3.
- `tests/fixtures/ykman_info_5c_partial.txt` — a FIPS key on fw 5.7.1 with several
  applications disabled and no NFC line.
- `tests/hardware_native.rs` — asserts the fallback and native reads agree.

## References

- `src/device/ykman.rs`
- `docs/yubikey-reference.md` — command surface with exact flags for ykman 5.9.2
