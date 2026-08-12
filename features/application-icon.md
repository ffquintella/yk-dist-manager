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

**Done for Wave 0.** The mark reads at 32 px and up; below that it degrades to a
light shape with a gold dot, which is still distinguishable from every other icon
on the dock. What is left — a Windows `.ico` and a Linux `hicolor` install — is
**Wave 3**, with the packaging it attaches to.

- `assets/logo.svg` — the only hand-edited artwork in the repository.
- `assets/render-icons.sh` (`make icons`) renders everything else.
- `src/branding.rs` — `window_icon()` for the platform, `icon_image()` for the
  screens, both from the same embedded 256×256 blob.
- `src/main.rs` — `ViewportBuilder::with_icon`, skipped if the blob is bad.
- `packaging/macos/icon.icns` — picked up by the existing bundle script.
- `src/ui/mod.rs` — `app_icon()`, the one helper every screen draws through.
- Three placements in the application, and an About box (phase 7, below).

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

### Inside the application (Phase 7)

Three placements, each answering a different question:

| Where | Size | What it is for |
|---|---|---|
| Top bar, beside the name | ~26 px | telling this window from the register, the term and a terminal during a hand-over — the reason the icon exists |
| Database chooser | 96 px | the one screen that is nothing but the application introducing itself, and the screen an operator is looking at while a password prompt waits |
| About box | 88 px | confirming *which build* is running |

**The About box is opened from the version badge**, not from a menu item. The badge
is already the thing somebody points at when asked which version they are running,
and a separate *About* button would be a second place to look for the same answer.

**It carries the `--diagnose` report**, which `docs/operations.md` already calls the
first thing to attach to a support request — reused rather than reimplemented,
because an About box listing the version and features by hand would be a second
answer to the same question and the two would drift. Selectable, with a *Copy the
report* button: the point of showing it is that somebody sends it on, and a retyped
diagnostic is worse than none.

The report is gathered **when the box opens**, not per frame. Gathering reads the
filesystem and enumerates the cameras; doing that sixty times a second for a panel
nobody is touching would be absurd, and a support report is a snapshot of the moment
somebody asked for it anyway.

### Two mistakes worth recording, both found by running it

**`icon_image()` must not be premultiplied.** `ColorImage::from_rgba_unmultiplied`
matches what the blob is — the same reason `egui::IconData` takes it directly.
Feeding premultiplied bytes to that constructor darkens every semi-transparent edge
pixel of the rounded backdrop, which is precisely the artefact nobody catches in
review.

**The texture cache must not upload from inside `data_mut`.** The first version of
`app_icon` looked up the handle and, on a miss, called `load_texture` *inside* the
closure — which takes egui's context lock while it is already held, and hangs the
frame. Not a slow path: a deadlock, and one that only happens on the first frame the
icon is drawn. The lookup and the insert each take the lock briefly and the upload
happens between them, with nothing held.

## Phases

| # | Phase | Wave | State | Notes |
|---|---|---|---|---|
| 1 | The mark, as SVG | 0 | Done | `assets/logo.svg` |
| 2 | Render script + `make icons` | 0 | Done | PNGs 16–1024, RGBA blob, `.icns` |
| 3 | Window / dock icon | 0 | Done | `src/branding.rs`, wired in `src/main.rs` |
| 4 | macOS bundle icon | 0 | Done | the bundle script already looked for the file |
| 5 | Windows `.ico` resource | 3 | Todo | needs a `build.rs` resource step; no Windows packaging exists yet |
| 6 | Linux `hicolor` + `.desktop` entry | 3 | Todo | same: no Linux packaging exists yet |
| 7 | In-application use | 0 | **Done** | three placements: the top bar beside the name, the database chooser at 96 px, and an **About box** opened from the version badge — which carries the `--diagnose` report, copyable |

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
- `the_on_screen_image_is_the_same_mark_as_the_window_icon` — one source, two
  consumers, asserted rather than assumed: an About box showing a different picture
  from the dock is one nobody can use to confirm which build is running. It checks
  both ends of the alpha channel in the `ColorImage` too, which is what a wrong byte
  order or a premultiplied blob would break.

`assets/render-icons.sh` checks the blob's byte count itself and exits non-zero
on a mismatch, so a bad asset never reaches a commit in the first place.

No behaviour test: there is no workflow here to exercise end to end.

**Phase 7 was verified by running the application**, because paint code is outside
the coverage gate and the two failure modes it had were both invisible to a unit
test: the deadlock above, and whether the modal paints at all. The build was
launched against a scratch register (`YKDM_DB`, `YKDM_SETTINGS` and `YKDM_DATA_DIR`
pointed at a temporary directory, so the operator's own register was never opened),
once normally and once with the About box forced open, and left painting for several
seconds each time. The `--diagnose` output was checked separately, since it is the
box's contents.

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
- `src/branding.rs` (`window_icon`, `icon_image`), `src/main.rs`
- `src/ui/mod.rs` (`app_icon`), `src/ui/database.rs`, `src/app.rs` (`about_box`)
- `src/diagnostics.rs` — the report the About box shows
- `packaging/macos/bundle.sh` (`CFBundleIconFile`)
- `features/gui-shell.md` — where phase 7 would land
