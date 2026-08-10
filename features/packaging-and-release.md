# Feature: Packaging and release

## Summary

Ship the tool: macOS, Windows and Linux builds, produced from a tag, with the platform
requirements the native transports impose, and semantic versions that mean something.

## Motivation

The tool is used at a desk by an operator who should not need a Rust toolchain. It also
links against platform smartcard and HID libraries, so "it builds on my machine" is a
weaker statement here than usual: PC/SC is a framework on macOS, a service on Windows,
and a daemon plus udev rules on Linux.

The norm adds a hard requirement: every version installed anywhere is generated from
version control and carries a tag. No hand-built binaries.

## Current state

**Not started.** `cargo build` produces a local binary; there is no bundle, no signing,
no release workflow.

## Design

### Per-platform requirements

| Platform | Bundle | PC/SC | HID | Signing |
|---|---|---|---|---|
| macOS | `.app` in a `.dmg` | `PCSC.framework`, built in | IOKit; no entitlement needed for HID, but a hardened-runtime app needs the right entitlements for smartcard access | Developer ID + notarisation, or the app is blocked by Gatekeeper |
| Windows | MSI or a signed `.exe` | `WinSCard`, plus the *Smart Card* service running | HID via `hid.dll` | Authenticode, or SmartScreen warns |
| Linux | `.deb`/`.rpm` or an AppImage | `libpcsclite` + `pcscd` running | `hidraw` plus a udev rule granting the user access to the YubiKey | Repository signing |

Each of those is a support call waiting to happen, so the packaging phase is as much
documentation as it is build configuration — `docs/operations.md` gets an install section
per platform.

### Feature flags in a release build

Release builds enable `native-device`. `encrypted-db` is the interesting decision: it
vendors and builds OpenSSL, so either every release carries it (simpler support, longer
builds) or there are two artefacts (confusing). **Recommendation: one artefact with
`native-device,encrypted-db`**, because an operator cannot rebuild to get password
support, and telling them "you have the wrong build" is worse than a slower CI.

### Versioning

Semantic versioning, per `AGENTS.md` §5. While `0.y.z`, the MINOR slot carries breaking
changes. A release is:

1. `CHANGELOG.md`: `[Unreleased]` → `[x.y.z] - YYYY-MM-DD`.
2. Bump `Cargo.toml`, commit.
3. Tag `vX.Y.Z`.
4. CI builds the three platforms from the tag and attaches the artefacts.

Any schema change bumps `SCHEMA_VERSION`, ships a migration, and is called out in the
changelog — because an operator on a share may run an older build against a newer file,
which the store refuses (`StoreError::SchemaTooNew`). That refusal is only useful if the
release notes tell people to upgrade together.

### Reproducibility

`Cargo.lock` is committed (it is a binary, not a library). The build records its version
and commit hash, and the app shows them in Settings — so an operator's screenshot
identifies the exact build.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | CI build matrix (macOS / Windows / Linux) with `native-device` | Todo | proves the transports compile everywhere |
| 2 | Version + commit hash embedded and shown in Settings | Todo | `VERSION` exists; add the hash |
| 3 | macOS `.app` + `.dmg`, Developer ID signing, notarisation | Todo | Gatekeeper blocks otherwise |
| 4 | Windows MSI + Authenticode | Todo | SmartScreen otherwise |
| 5 | Linux packages + udev rule + `pcscd` dependency documented | Todo | |
| 6 | Tagged release workflow with artefacts attached | Todo | norm: every installed build comes from a tag |
| 7 | Install/upgrade documentation per platform | Todo | `docs/operations.md` |
| 8 | Upgrade note automation: warn when a schema bump is in the release | Todo | avoids the mixed-version surprise on a share |

## Audit events

None from the app itself. Release actions are recorded in Git and CI, and the norm's
change-document requirement is satisfied by the changelog plus the FGV change record
(`features/fgv-compliance.md`).

## Tests

- CI compiles every feature combination that will ship, on every platform.
- A smoke test per platform: launch headless-ish (open the store, run migrations, exit) so
  a broken bundle fails in CI rather than at a desk.
- Verify the artefact's version string matches the tag.

## Open questions and gates

- **Code-signing certificates**: does the unit have a Developer ID and an Authenticode
  certificate? Without them, macOS and Windows will fight every installation. This is a
  procurement question with a long lead time — worth raising early.
- Distribution channel: a share, an internal package repository, or GitHub releases? A
  security tool downloaded over HTTP from an unsigned source is its own problem.
- Whether `encrypted-db` is in the default artefact (recommendation above).

## References

- `AGENTS.md` §5, `CHANGELOG.md`
- `features/native-device-transport.md` (platform libraries), `docs/operations.md`
