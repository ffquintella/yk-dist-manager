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

**Every platform builds an artefact from a tag, and every artefact is verified by asking
the binary about itself. What is left is two code-signing certificates this project does
not have.**

- **The release workflow** ([`.github/workflows/release.yml`](../.github/workflows/release.yml))
  triggers on `v*`, re-runs the whole gate against the tag, builds macOS, Linux and Windows
  from a fresh checkout, verifies each artefact, and attaches the results to a **draft**
  release. Draft, not published: somebody looks at the artefacts and presses the button —
  a release of a tool that holds a token register should not happen automatically at 03:00.
- **Which commit a build came from** is embedded by [`build.rs`](../build.rs) and reported by
  `--diagnose`, `--version`, the About box and Settings. A version number cannot show that a
  build came from a tag: `0.13.0` says the same thing built from the tag, from a branch, or
  from a tree with uncommitted changes. A build from a dirty tree says `-dirty`, and the
  verifiers **fail** on that when `YKDM_VERIFY_RELEASE=1` — which the release workflow sets.
- **Linux packages**: [`packaging/linux/package.sh`](../packaging/linux/package.sh) builds a
  tarball (always) and a `.deb` (where `dpkg-deb` exists), carrying the binary, the
  `hicolor` icons, a `.desktop` entry, the **udev rule** and install notes that name `pcscd`.
  [`verify-package.sh`](../packaging/linux/verify-package.sh) checks the layout, the rule,
  the desktop entry, the dependencies the `.deb` declares, and then runs the packaged binary
  with `--diagnose`.
- **The `block` 0.1.6 blocker is resolved** (phase 0b), by the route cargo itself recommends
  for a dependency whose maintainer cannot be waited on: a patched copy in
  [`vendor/block`](../vendor/block/README.md), four lines, with the reasoning and the way to
  revert written down beside it.
- **Windows** produces a zip of the executable rather than an MSI, deliberately: an
  *unsigned* MSI is a worse experience than an unsigned executable, not a better one, so the
  installer waits for the certificate that makes it worth having.
- **No artefact names an institution.** The 2026-08-11 decision — the application carries no
  institution's name, the organisation is the operator's setting — has to be upheld here in
  particular, because packaging is the one place a name can come back with no code changing:
  it is a build variable, not a string in the source, so nothing in the test suite sees it.
  So the macOS `NSHumanReadableCopyright` defaults to the copyright line read out of
  `LICENSE` (true of this source, and nobody's institution) and the bundle identifier to
  `org.example.yk-dist-manager`; the `.deb` is maintained by "yk-dist-manager maintainers";
  the `.desktop` entry and the udev rule carry no vendor. A unit that wants its own line sets
  `YKDM_COPYRIGHT` and `YKDM_BUNDLE_ID` at build time, which is where a deployment's identity
  belongs.

**What is blocked, and on what**: Developer ID + notarisation (phase 3b) and Authenticode
(phase 4). Both are procurement with a long lead time, not code — the workflow already has
the macOS signing step, guarded by whether the secret exists, so the day a certificate
arrives the change is a secret rather than a rewrite. Until then macOS is ad-hoc signed
(Gatekeeper blocks it, and the camera grant does not survive a rebuild) and Windows warns
through SmartScreen.

### What shipped before this

**macOS bundle shipped; signing for distribution and the other platforms are not.**

- `packaging/macos/Info.plist.in` — the plist template, with
  `NSCameraUsageDescription` as its reason for existing.
- `packaging/macos/bundle.sh` — assembles
  `target/bundle/YubiKey Distribution Manager.app`: builds the binary, writes the plist
  with the version from `Cargo.toml` (single source of truth), copies an optional
  `packaging/macos/icon.icns`, lints the plist, and code-signs. Ad-hoc by default;
  `--sign 'Developer ID Application: …'` for a real identity. `--dmg` wraps it with
  `hdiutil`.
- `packaging/macos/write-plist.sh` — the plist writer, separate so it can be
  exercised (below). `@VERSION@` and `@IDENTIFIER@` go in with `sed`; the
  copyright does not (see *Free text does not go through a substitution*).
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

That was the whole of the packaging work until this release; what it was missing is the
list above.

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
2. **`block` 0.1.6 is future-incompatible.** It was a prerequisite for a distributed
   build under NRM §5.4.3, and it is **resolved**: `[patch.crates-io]` onto
   [`vendor/block`](../vendor/block/README.md), a four-line fix to a crate with no
   maintainer, which is the remedy cargo's own warning recommends. The other three
   routes — an upstream fix, a native AVFoundation path, reverting `camera` to opt-in —
   are recorded there with why each was not taken. `cargo build` reports no future
   incompatibility now, and CI's `-D warnings` covers the patched copy (a path crate is
   not lint-capped the way a registry one is), which is why its deprecated `extern`
   declarations were spelled `extern "C"` at the same time.

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

### Free text does not go through a substitution

Three values are written into `Info.plist`: the version, the bundle identifier and
the copyright. Only one of them is free text — `YKDM_COPYRIGHT`, whatever the
operator running the build types — and it was the one going in through a `sed`
replacement, which is two escaping layers it was never escaped for:

| Character | Through `sed` | Result |
|---|---|---|
| `&` | the whole match, in a replacement | `"Foo & Bar"` became `Foo @COPYRIGHT@ Bar` — wrong, and silent |
| `\|` | the delimiter (chosen because a copyright may contain a slash) | the `s///` ends early; the build fails talking about a sed script |
| `&` `<` `>` | passed through to the XML | invalid plist, caught by `plutil -lint` several steps from the cause |

So the copyright is **not substituted**. The plist is written from the template
first, and then `plutil -replace NSHumanReadableCopyright -string "$COPYRIGHT"`
sets the key with the value as an argument: plutil takes it as data and escapes it
for XML itself. `PlistBuddy -c "Set …"` is *not* an alternative, though it is the
idiom used two lines away for `CFBundleIconFile` — PlistBuddy re-parses its command
string, so it eats double quotes without a word and fails on an apostrophe, which a
copyright line can easily carry.

The version and the identifier stay with `sed`, because a semantic version and a
reverse-DNS name have no such character. `write-plist.sh` **enforces** that rather
than assuming it: anything outside `A-Za-z0-9._+-` is refused, with a message that
names the value. An unenforced assumption is the same bug one variable over.

That is also why the writer is a script of its own: a bundle built with an ordinary
copyright proves nothing about a hostile one, so `verify-bundle.sh` runs the real
writer against `Fundação & Cia | <Tech> "Q" O'Brien 1/2 \ x` and requires it back
byte for byte.

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
| 0b | Resolve the `block` 0.1.6 chain | **Done** | `[patch.crates-io]` onto [`vendor/block`](../vendor/block/README.md) — four lines, replacing an `extern` static of an uninhabited type with a zero-sized inhabited one. The three alternatives and why they were not taken are recorded there: there is no upstream fix (no release since 2020, and `block2` is not API-compatible with what `nokhwa` calls), a native AVFoundation path is weeks of platform code, and making `camera` opt-in reverses the decision of 2026-08-10, which is the owner's to reverse. `cargo build` now reports no future incompatibility |
| 1 | CI build matrix (macOS / Windows / Linux) with `native-device` | **Done** — landed as `features/testing-strategy.md` phase 10 | [`.github/workflows/ci.yml`](../.github/workflows/ci.yml)'s `cross-platform` job builds all three, including the native transports (default since 0.12.0) and the `ykman`-only build. It does not *run* the hardware tests: a hosted runner has no reader and no USB key |
| 2 | Version + commit hash embedded and shown in Settings | **Done** | [`build.rs`](../build.rs) → `crate::COMMIT` / `crate::build_id()`, reported by `--version`, `--diagnose`, the About box and the Settings footer, and checked by both verifiers. `unknown` when there is no `git` (a source tarball), `-dirty` from a tree with uncommitted changes; a release build fails the verifier on either |
| 3 | macOS `.app` + `.dmg` | **Done** | `bundle.sh [--release] [--dmg]`; the plist writer is `write-plist.sh`, and free text no longer goes through a substitution |
| 3b | Developer ID signing + notarisation | **Blocked on a certificate** | The workflow's macOS step signs with `secrets.MACOS_SIGN_IDENTITY` when it exists and warns loudly when it does not, so this is a secret away rather than a rewrite. Until then: Gatekeeper blocks the bundle, and the camera grant does not survive a rebuild because macOS remembers it against the signature. **Procurement, not code** — see the gate below |
| 4 | Windows MSI + Authenticode | **Partly done; the installer is blocked on a certificate** | The workflow builds Windows from the tag, interrogates the binary with `--diagnose` and ships a zip. No MSI yet, deliberately: an unsigned MSI is a worse experience than an unsigned executable, so the installer waits for the Authenticode certificate that makes it worth having |
| 5 | Linux packages + udev rule + `pcscd` dependency documented | **Done** | [`package.sh`](../packaging/linux/package.sh) (tarball + `.deb`), the `uaccess` udev rule, the `.desktop` entry, the `hicolor` icons, install notes that travel with the artefact, and [`verify-package.sh`](../packaging/linux/verify-package.sh), which checks all of it and then runs the packaged binary |
| 6 | Tagged release workflow with artefacts attached | **Done** | [`release.yml`](../.github/workflows/release.yml): gate → three platforms → verify → **draft** release with generated notes. Publishing stays a human action |
| 7 | Install/upgrade documentation per platform | **Done** | `docs/operations.md`, "Installing", plus `README.install` inside the Linux artefact — the copy that is there when somebody is installing it |
| 8 | Upgrade note automation: warn when a schema bump is in the release | **Done** | [`scripts/release-notes.sh`](../scripts/release-notes.sh) compares `store::SCHEMA_VERSION` against the previous tag and appends the upgrade note when it moved. It also refuses to produce notes at all when the changelog has no section for the version, which is the other thing that gets forgotten |

## Audit events

None from the app itself. Release actions are recorded in Git and CI, and the norm's
change-document requirement is satisfied by the changelog plus the change record
(`features/compliance.md`).

## Tests

- CI compiles every feature combination that will ship, on every platform (`ci.yml`).
- A smoke test per platform, and it is the same interrogation on all three: the packaged
  binary is run with `--diagnose`, which opens no database and writes nothing but proves the
  artefact links and starts. A bundle or package that cannot start fails in CI rather than
  at a desk.
- The artefact's version matches `Cargo.toml`, and its commit is neither `unknown` nor
  `-dirty` — checked by `verify-bundle.sh` and `verify-package.sh` with
  `YKDM_VERIFY_RELEASE=1`.
- `scripts/release-notes.sh` fails when the changelog has no section for the version being
  released, so the release stops before an artefact is built rather than after.
- The bundle's copyright line matches what this tree says it should be — `YKDM_COPYRIGHT`
  where a unit set one, the `LICENSE` line otherwise. A warning locally, where the bundle may
  have been built in a different shell, and a failure under `YKDM_VERIFY_RELEASE=1`, where
  the build and the check share one environment. It is a shell check rather than a Rust test
  because the value never reaches the binary: it exists only in the assembled plist.
- `verify-bundle.sh` also exercises the plist **writer** directly, since there is no Rust to
  test here: a copyright carrying `&`, `|`, `<`, `>`, a double quote, an apostrophe and a
  backslash must come back unchanged and leave a plist that lints, and a version or
  identifier carrying a metacharacter must be refused rather than substituted.

## Open questions and gates

- **Code-signing certificates**: does the unit have a Developer ID and an Authenticode
  certificate? Without them, macOS and Windows will fight every installation. This is a
  procurement question with a long lead time — worth raising early. **It is now the only
  thing between this feature and done**: phases 3b and 4 are a secret and a signing step
  away, and everything either of them needs is already in the workflow.
- Distribution channel: a share, an internal package repository, or GitHub releases? A
  security tool downloaded over HTTP from an unsigned source is its own problem.
- Whether `encrypted-db` is in the default artefact (recommendation above).

## References

- `AGENTS.md` §5, `CHANGELOG.md`
- `features/native-device-transport.md` (platform libraries), `docs/operations.md`
