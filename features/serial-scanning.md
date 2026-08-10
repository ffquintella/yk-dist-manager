# Feature: Reading a serial from a barcode

## Summary

Get a serial into the inventory without plugging the key in: decode the barcode on
the packaging with a camera, or let a USB barcode scanner type it. Either way the
record is marked as **not yet verified**, and a device read upgrades it.

## Motivation

Receiving a shipment means getting dozens of serials into the inventory. Plugging in
each key is accurate and slow, and it happens at the wrong time — the keys are not
being bootstrapped yet, they are being *received*. The packaging carries the serial
as a barcode, so scanning the labels records the shipment in minutes.

The thing that makes this safe rather than sloppy is refusing to pretend: a serial
from a label is a *claim* about a key nobody has touched. Recording provenance keeps
the two apart, so nothing downstream treats a scanned serial as a confirmed key.

## Current state

**Shipped; the camera is on by default.**

> **`camera` is a default feature.** That was a deliberate choice — an operator
> should not need a special build to point a webcam at a box label. It moves two
> problems onto every build, and neither is solved: the macOS camera-usage
> declaration, and the future-incompatible `block` 0.1.6 in `nokhwa`'s macOS
> bindings. Both are release blockers now; see *Open questions and gates*.
> `--no-default-features --features file-dialog` builds without any camera code.

- `SerialSource` (`Device` / `ScannedLabel` / `ManualEntry`) on every inventory
  record, stored in `keys.serial_source` (schema v2). Provenance only ever improves:
  a device read upgrades a scanned record, and a later scan never downgrades a
  verified one — enforced in SQL, not by the caller.
- `scan::parse_serial` — pulls a serial out of decoded text, tolerating prefixes
  (`S/N: 20423633`), refusing a payload with two different candidates, and refusing
  to truncate a long id into a serial.
- `scan::BarcodeDecoder` over `scan::LumaFrame` (grayscale, no image crate), so the
  reduction logic is testable with a stub.
- `scan::RxingDecoder` (`barcode` feature) — `rxing`, the Rust ZXing port: 1D and 2D
  formats, pure Rust, no system library. Also decodes a photo from disk
  (`decode_image_file`), which is a phone-photo path that needs no camera.
- `scan::camera::CameraScanner` (`camera` feature) — `nokhwa` capture on its own
  thread, publishing the latest frame and any decoded serial through a mutex. The
  camera is opened *on* that thread because `nokhwa::Camera` is not `Send`, and the
  open result comes back over a channel with a 10s bound, so a wedged backend or a
  denied permission cannot hang the GUI.
- `scan::preflight` — the guard that makes the camera path safe to *attempt*. See
  below; without it, an unbundled macOS build aborted the process.
- Inventory screen: a scan panel with a typed field (which is what a USB wedge types
  into), camera controls, a preview, and confirm/discard for the decoded serial.

## Design

### The keyboard wedge is the recommended option

A USB barcode scanner presents itself as a keyboard: it types the serial and presses
Enter. That needs no camera, no permissions and no decoding, and it is what a busy
receiving desk actually wants. The typed field is therefore not a fallback — it is
the primary path, and the camera exists for the operator who has a laptop and no
scanner. The docs say so rather than selling the camera.

### Ambiguity is refused, never guessed

Two labels in shot showing different serials, or one payload containing two
plausible numbers, is an error. Choosing one would attribute a credential to
whichever key was closer to the lens.

### Provenance, and what it protects

| Source | Means | Consequence |
|---|---|---|
| `Device` | Read over PC/SC or `ykman` | Verified: the key exists and is reachable |
| `ScannedLabel` | Decoded from packaging | A claim; model/firmware unknown |
| `ManualEntry` | Typed | A claim, with a typo risk |

A scanned record is deliberately incomplete: no model, no firmware, no application
list — empty rather than guessed. The bootstrap wizard's firmware gates therefore
cannot run against a scanned key, which is correct: it has to be read first.

### On macOS, a bad camera call aborts the process

This is the sharpest edge in the feature, found by running the default build:

```text
thread 'camera-scan' panicked at core/src/panicking.rs:225:5:
panic in a function that cannot unwind
thread caused non-unwinding panic. aborting.
```

AVFoundation raises an **Objective-C exception**, which crosses an `extern "C"`
boundary and becomes a *non-unwinding* panic. `catch_unwind` cannot recover from it,
`Result` never gets a chance, and the operator loses the application — mid-hand-over,
potentially. The only defence is to not make the call, so `scan::preflight` checks
every precondition first:

| Check | Why |
|---|---|
| `nokhwa_initialize()` was called | nokhwa's docs: the caller's responsibility "before anything else" on macOS. Runs in `main`, on the main thread, so the permission prompt appears while the operator is there |
| Authorisation granted (`nokhwa_check()`) | An unauthorised open raises the exception |
| Running inside a `.app` bundle | A bare binary has no `Info.plist`, so nothing declares `NSCameraUsageDescription` and TCC has no identity to attribute a grant to |
| The requested device index exists | Opening a device that is not there |

Each failure is a `ScanError::Camera` whose text names the cause **and** the way
forward — including that a USB barcode reader needs no camera at all.
`YKDM_ALLOW_UNBUNDLED_CAMERA=1` forces an attempt for someone who has arranged
access another way; it is documented as "may abort", because that is the truth.

Consequence worth stating plainly: **camera scanning does not work under
`cargo run` on macOS.** It needs the bundled application — which now exists:

```bash
make bundle && make verify-bundle && make run-bundled
```

`verify-bundle` confirms the fix by asking the bundled binary itself: the camera verdict
moves from *"not running from an .app bundle"* to *"not yet authorised"*, which the
permission prompt resolves. The typed/wedge path still works in every build, which is one
more reason it is the recommended one.

One wrinkle to know about: macOS remembers the camera grant against the **code
signature**, so an ad-hoc signed bundle re-prompts after every rebuild. A stable
Developer ID identity is therefore a requirement for the camera feature, not only for
distribution (`features/packaging-and-release.md` Phase 3b).

### Other camera realities

Documented in `src/scan/camera.rs` because they will generate support questions: a
laptop camera is fixed-focus and struggles closer than ~20cm; Linux needs the `video`
group or a udev rule; a device that matches no requested format is tried three ways
(highest frame rate, highest resolution, anything) before being reported as
unusable.

## Phases

| # | Phase | State | Notes |
|---|---|---|---|
| 1 | Serial parsing and ambiguity rules | Done | 12 unit tests |
| 2 | `SerialSource` + schema v2, provenance never downgraded | Done | enforced in the upsert |
| 3 | `rxing` decoder over a luminance frame | Done | tested against a rendered Code 128 |
| 4 | Camera capture on a thread, with preview | Done | `camera` feature |
| 4b | `preflight` guard so a camera problem is an error, not a process abort | Done | regression test calls the same entry point the button does |
| 5 | Typed / wedge entry in the inventory panel | Done | Enter submits |
| 6 | Decode a photo from disk in the GUI | Todo | `decode_image_file` exists; no button yet |
| 7 | Camera selection when several are attached | Todo | `available_cameras()` exists |
| 8 | Batch scanning: keep the camera open and queue serials | Todo | with `features/bulk-enrollment.md` |
| 9 | "Unverified keys" report | Todo | scanned records never followed by a device read |
| 10 | Resolve the `block` 0.1.6 chain (patch, upstream fix, or a native AVFoundation path) | **Todo — release blocker** | now applies to the default build |
| 11 | `NSCameraUsageDescription` in the macOS bundle | **Done** | `packaging/macos/`; verified by `make verify-bundle` |

## Audit events

| Event | Detail |
|---|---|
| `key.added` | `source=scanned-label\|manual-entry\|device verified=false\|true` |
| `key.refreshed` | A device read; provenance upgraded |
| `camera.started` / `camera.stopped` | With the device name |
| `camera.start.failed` | Reason (permission, in use, no camera) |

## Tests

- `src/scan/mod.rs` — 13 unit tests: bare, prefixed and decorated serials; repeated
  vs conflicting candidates; no digits vs implausible digits; frame validation; RGB
  to luma; a stub decoder end to end.
- `src/scan/rxing_decoder.rs` — 5 tests against **rendered Code 128 barcodes**
  produced by rxing's own encoder, so the real decoder is exercised: a bare serial, a
  prefixed serial, a blank frame, and a barcode that decodes but is not a serial.
- `tests/unit_store.rs` — `provenance_is_upgraded_by_a_device_read_but_never_downgraded`.
- `src/scan/preflight.rs` — 8 unit tests over pure predicates: the unbundled-macOS
  refusal and its message, the override spellings, missing and out-of-range devices.
  The predicates take their inputs as parameters rather than reading the environment,
  so nothing races when tests share a process.
- `tests/camera_guard.rs` — calls `CameraScanner::start`, the exact entry point the
  *Start camera* button calls. Before the guard, this aborted the test binary; now it
  must return an `Err`. Also asserts that enumerating cameras is safe before
  authorisation, since the preflight depends on counting devices.
- The capture loop itself has no automated test (it needs a camera); its contract —
  never block the GUI, bounded open — is enforced by construction.

## Open questions and gates

- Do the unit's actual YubiKey boxes carry a barcode with the serial? Yubico's retail
  packaging and bulk labels generally do, but a batch could arrive with a
  purchase-order barcode instead. Worth confirming with one real box before relying
  on the camera path; the wedge and typed paths are unaffected.
- macOS packaging must add the camera usage description, or the feature silently
  fails in a bundled app (`features/packaging-and-release.md`).
- **Release blocker — `block` 0.1.6.** `nokhwa`'s macOS bindings depend on it, and
  cargo reports it as future-incompatible: `static of uninhabited type`, which a later
  rustc will reject outright. Since `camera` is now a default feature, **every** build
  carries it. NRM §5.4.3 forbids shipping a component without maintainer support, so
  one of these has to happen before a tagged build is distributed:

  1. `nokhwa` updates its `objc`/`block` chain (upstream issue worth filing);
  2. pin a patched `block` through `[patch.crates-io]`;
  3. write the macOS capture path directly against AVFoundation, which removes the
     dependency entirely and is the durable answer;
  4. take `camera` back out of the default set and ship it opt-in.

  Recorded here rather than in a build log, because a default feature is the kind of
  decision that stops being visible once it works.

- **Release blocker — macOS camera permission.** A bundled `.app` without
  `NSCameraUsageDescription` is terminated by the OS the first time it opens the
  camera. It is a one-line `Info.plist` entry, but with `camera` on by default it
  applies to every macOS build, not just a special one.

## References

- `src/scan/`, `src/domain/key.rs`, `src/ui/inventory.rs`
- `features/key-inventory.md`, `features/device-detection.md`
