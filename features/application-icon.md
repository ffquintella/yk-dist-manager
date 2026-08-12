# Feature: Application icon

## Summary

One hand-drawn mark — `assets/logo.svg` — rendered by script into every raster
size the platforms ask for: the window/dock icon compiled into the binary, the
macOS `.icns` the bundle carries, and the PNGs the documentation uses.

## Motivation

Requested by the operator who will run the tool. Before this, `cargo run` and
the bundle both showed the platform's generic placeholder, and
`packaging/macos/bundle.sh` carried a comment saying no logo would be invented
because "a made-up logo on an institutional tool is worse than the generic one".
That reasoning held while nobody had asked for one; the request settles it.

It is not only decoration. The tool is used alongside other windows during a
hand-over — the register, the term to sign, a terminal — and an application the
operator has to identify by reading its title bar is one they mis-click. A dock
icon is how you tell two windows apart at a glance.

## Current state

**Done.** The mark reads at 32 px and up; below that it degrades to a light
shape with a gold dot, which is still distinguishable from every other icon on
the dock.

- `assets/logo.svg` — the only hand-edited artwork in the repository.
- `assets/render-icons.sh` (`make icons`) renders everything else.
- `src/branding.rs` — `window_icon()`, embedding the 256×256 blob.
- `src/main.rs` — `ViewportBuilder::with_icon`, skipped if the blob is bad.
- `packaging/macos/icon.icns` — picked up by the existing bundle script.

## Design

### The mark

A box truck in side profile, facing right. Its cargo panel **is** a YubiKey seen
face-on: keyring slot at the top, gold touch contact in the middle, USB contact
block at the foot. Three motion strokes behind it.

Two decisions worth writing down, because both were the second attempt:

- **The key is the cargo, not the whole vehicle.** At 16–32 px the only YubiKey
  cue that survives is *gold ring on a dark slab*, and the only distribution cue
  that survives is *truck silhouette*. Putting the ring on the side of the box
  keeps both at every size. A key stylised into the shape of the truck loses
  both.
- **The key panel is slim, and the ring nearly spans its width.** The first
  draft used a wide panel with a small ring and a round keyring hole, and read
  unmistakably as a *loudspeaker*. Narrowing the panel and swapping the round
  hole for a keyring slot fixed it.

Palette: slate backdrop (`#26374D`→`#0C131C`), near-white bodywork, the dark of
the key doubling as the windscreen glass, and one accent — Yubico-ish gold
(`#FFD469`→`#E5A017`) on the touch contact and the headlamp. Two colours and a
metal, so the gold is the only place the eye is pulled.

The mark carries **no text**, so it needs no translation — the same reason the
consignment term is data and the icon is not.

### Why the icon lives on a backdrop

The rounded square is part of the mark, not a platform affordance. A white truck
floating free would vanish on a light desktop, and the alternative — shipping a
light and a dark variant — is two files to keep in step for no gain on a desktop
tool.

### Why the window icon is a raw pixel blob

`src/branding.rs` embeds straight (non-premultiplied) RGBA8, not a PNG, because
decoding a PNG would mean the `image` crate — which is an **optional**
dependency behind the `barcode` feature. The icon must exist in every build,
including `--no-default-features`, so it cannot depend on a decoder that may not
be compiled in. The cost is 256 KiB of binary; the alternative is either a new
mandatory dependency or an icon that disappears in some feature combinations.

`window_icon()` returns `Option`: a blob whose length disagrees with
`ICON_SIDE` can only mean a generated asset was committed half-written, and the
right response is a generic icon plus an `error` log, not a window full of
garbage or a refused launch.

### Why the generated files are committed

They are build output, and they are in Git anyway. `make bundle` must produce a
bundle with an icon on a machine that has neither librsvg nor ImageMagick, and
`include_bytes!` needs the blob present at compile time. The rule is therefore:
**edit the SVG, run `make icons`, commit both together.**

### Platform routing

| Platform | Source |
|---|---|
| macOS, bundled | `CFBundleIconFile` → `packaging/macos/icon.icns` |
| macOS, `cargo run` | the embedded blob |
| Windows, Linux | the embedded blob |
| Documentation | `assets/icons/icon-*.png` |

A Windows `.ico` resource and a Linux `hicolor` install are Todo; both are
packaging work that has no packaging to attach to yet.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | The mark, as SVG | 0 | Done | `assets/logo.svg` |
| 2 | Render script + `make icons` | 0 | Done | PNGs 16–1024, RGBA blob, `.icns` |
| 3 | Window / dock icon | 0 | Done | `src/branding.rs`, wired in `src/main.rs` |
| 4 | macOS bundle icon | 0 | Done | the bundle script already looked for the file |
| 5 | Windows `.ico` resource | 3 | Todo | needs a `build.rs` resource step; no Windows packaging exists yet |
| 6 | Linux `hicolor` + `.desktop` entry | 3 | Todo | same: no Linux packaging exists yet |
| 7 | In-application use | 0 | Todo | the icon on the unlock screen and in an About box |

## Audit events

**None, and that is correct.** The icon is not a state change: nothing is
written, no record is touched, and no secret is near it. `window_icon()`
logs once at `error` if the embedded blob is malformed, which is an operational
fault, not accountability (see `features/logging.md` on the split).

## Tests

Unit tests in `src/branding.rs`:

- `the_embedded_icon_is_the_size_it_claims` — the committed blob is exactly
  `ICON_SIDE² × 4` bytes, so a half-written asset fails the suite rather than
  the launch.
- `the_centre_of_the_icon_is_opaque` — catches a blob rendered from the wrong
  file, or with the channels in the wrong order.
- `the_corner_of_the_icon_is_transparent` — the backdrop is a rounded square, so
  pixel (0,0) falls outside it. Together with the previous test this pins the
  alpha channel down at both ends.

`assets/render-icons.sh` checks the blob's byte count itself and exits non-zero
on a mismatch, so a bad asset never reaches a commit in the first place.

No behaviour test: there is no workflow here to exercise end to end.

## Open questions and gates

- **Does the organisation running the tool want its own identity here instead?**
  The mark is deliberately generic — no institutional logo, no colours claimed
  from a brand manual — for the same reason `org-standard` and
  `UNSET-ORGANISATION` are placeholders since v0.5.0: this build belongs to no
  one institution. Swapping the mark for an issued asset is replacing one SVG
  and re-running `make icons`, and that choice is not the author's to make.
- Phases 5 and 6 wait on there being Windows and Linux packaging to attach to.

## References

- `assets/logo.svg`, `assets/render-icons.sh`
- `src/branding.rs`, `src/main.rs`
- `packaging/macos/bundle.sh` (`CFBundleIconFile`)
- `features/gui-shell.md` — where phase 7 would land
