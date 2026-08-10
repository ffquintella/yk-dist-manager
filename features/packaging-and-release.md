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

**macOS bundle shipped; signing for distribution and the other platforms are not.**

- `packaging/macos/Info.plist.in` — the plist template, with
  `NSCameraUsageDescription` as its reason for existing.
- `packaging/macos/bundle.sh` — assembles
  `target/bundle/YubiKey Distribution Manager.app`: builds the binary, substitutes the
  version from `Cargo.toml` (single source of truth), copies an optional
  `packaging/macos/icon.icns`, lints the plist, and code-signs. Ad-hoc by default;
  `--sign 'Developer ID Application: …'` for a real identity. `--dmg` wraps it with
  `hdiutil`.
- `packaging/macos/verify-bundle.sh` — checks the layout, the plist, the version
  against `Cargo.toml`, the signature, and then asks **the bundled binary itself**
  (`--diagnose`) whether macOS sees it as bundled and whether camera scanning is still
  refused. Packaging is verified rather than assumed.
- `--diagnose` / `--version` / `--help` on the binary: build, paths, features,
  bundle state, camera verdict, database and settings locations, `ykman` presence.
  What an operator pastes into a ticket, and what the verifier interrogates.
- `make bundle`, `make bundle-release`, `make verify-bundle`, `make run-bundled`,
  `make dmg`, `make diagnose`.

No bundling crate is used: the layout is a few directories and one plist, so
assembling it in a reviewable script beats a dependency that can go unmaintained.

Still to do: Developer ID signing and notarisation, Windows, Linux, CI, and the
`block` 0.1.6 resolution.

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

Defaults today are `file-dialog` and `camera`; release builds add `native-device`.
`encrypted-db` is the interesting decision: it vendors and builds OpenSSL, so either
every release carries it (simpler support, longer builds) or there are two artefacts
(confusing). **Recommendation: one artefact with `native-device,encrypted-db`**,
because an operator cannot rebuild to get password support, and telling them "you have
the wrong build" is worse than a slower CI.

**Two blockers come from `camera` being a default feature** (see
`features/serial-scanning.md`):

1. **macOS `Info.plist` must declare `NSCameraUsageDescription`.** Without it the OS
   terminates the app the first time it opens the camera — a crash, not a refusal.
   Add it to the bundle template in Phase 3, and smoke-test the camera on a signed,
   notarised build rather than only under `cargo run`.
2. **`block` 0.1.6 is future-incompatible.** Resolving it (upstream fix, a
   `[patch.crates-io]` pin, a native AVFoundation path, or reverting `camera` to
   opt-in) is a prerequisite for a distributed build under NRM §5.4.3.

Linux adds a third: the V4L2 capture path needs a camera device the user can read
(`video` group or a udev rule), which belongs in the install documentation next to the
`pcscd` requirement.

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

### Signing is not only about Gatekeeper

macOS remembers a camera grant against the **code signature**. An ad-hoc signature
changes whenever the binary is rebuilt, so a developer gets the permission prompt again
after every `make bundle`. That is tolerable locally and unacceptable for an operator,
which makes a stable Developer ID identity a requirement for the camera feature and not
just for installation.

### Reproducibility

`Cargo.lock` is committed (it is a binary, not a library). The build records its version
and commit hash, and the app shows them in Settings — so an operator's screenshot
identifies the exact build.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 0a | `NSCameraUsageDescription` in a real bundle | **Done** | `make bundle` + `make verify-bundle`; the camera refusal is now "not yet authorised", which the prompt resolves |
| 0b | Resolve the `block` 0.1.6 chain | **Todo — still gates every artefact** | upstream fix, `[patch.crates-io]`, a native AVFoundation path, or `camera` back to opt-in |
| 1 | CI build matrix (macOS / Windows / Linux) with `native-device` | Todo | proves the transports compile everywhere; the default build now needs a V4L2-capable Linux image |
| 2 | Version + commit hash embedded and shown in Settings | Partial | the version is in the plist and in `--diagnose`; the commit hash is not |
| 3 | macOS `.app` + `.dmg` | **Done** | `bundle.sh [--release] [--dmg]` |
| 3b | Developer ID signing + notarisation | Todo | ad-hoc signing works locally but re-prompts for the camera after every rebuild, and Gatekeeper blocks distribution |
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
