//! The application's visual identity: the window and dock icon.
//!
//! The artwork is [`assets/logo.svg`](../../../assets/logo.svg) — a box truck
//! whose cargo panel is a YubiKey. `assets/render-icons.sh` (`make icons`)
//! renders it to every raster size the platforms ask for.
//!
//! # Why a raw pixel blob and not a PNG
//!
//! The icon is embedded as straight (non-premultiplied) RGBA8 rather than as a
//! PNG, because decoding a PNG would mean the `image` crate, which is an
//! *optional* dependency behind the `barcode` feature. The icon has to be
//! present in every build — including `--no-default-features` — so it cannot
//! depend on a decoder that may not be compiled in. 256 KiB of the binary buys
//! that independence.
//!
//! macOS takes its icon from the bundle (`CFBundleIconFile` →
//! `packaging/macos/icon.icns`), not from here; this is what Windows and Linux
//! use, and what an unbundled `cargo run` shows on any platform.

/// Side of the embedded icon, in pixels. Square, and a multiple of four, which
/// is what `egui::IconData` asks for.
pub const ICON_SIDE: u32 = 256;

/// The icon as straight RGBA8, row-major, `ICON_SIDE` × `ICON_SIDE`.
///
/// Generated — never edit by hand. Change `assets/logo.svg` and run
/// `make icons`.
const ICON_RGBA: &[u8] = include_bytes!("../assets/icons/icon-256.rgba");

/// Bytes per pixel in [`ICON_RGBA`].
const CHANNELS: u32 = 4;

/// The window icon, ready for `ViewportBuilder::with_icon`.
///
/// Returns `None` if the embedded blob is not the size the dimensions claim,
/// which can only happen if a generated asset was committed half-written. The
/// caller then runs without an icon rather than shipping a window full of
/// garbage pixels — a missing icon is cosmetic, and the operator has work to do.
pub fn window_icon() -> Option<egui::IconData> {
    let expected = (ICON_SIDE * ICON_SIDE * CHANNELS) as usize;
    if ICON_RGBA.len() != expected {
        tracing::error!(
            event = "branding.icon_malformed",
            expected,
            actual = ICON_RGBA.len(),
            "embedded window icon has the wrong length; run `make icons`"
        );
        return None;
    }

    Some(egui::IconData {
        rgba: ICON_RGBA.to_vec(),
        width: ICON_SIDE,
        height: ICON_SIDE,
    })
}

/// The same blob as an [`egui::ColorImage`], for drawing the mark *inside* the
/// application (`features/application-icon.md` phase 7).
///
/// One source for the window icon and for every on-screen use, which is the point:
/// an About box showing a different picture from the dock is an About box that
/// cannot be used to confirm which build is running.
///
/// `from_rgba_unmultiplied` because that is what the blob is — the same reason
/// `egui::IconData` accepts it directly. Feeding premultiplied bytes to this
/// constructor would darken every semi-transparent edge pixel of the rounded
/// backdrop, which is exactly the artefact nobody notices in review.
///
/// `None` for a malformed blob, for the same reason [`window_icon`] returns
/// `Option`: a missing picture is cosmetic, and the operator has work to do.
pub fn icon_image() -> Option<egui::ColorImage> {
    let expected = (ICON_SIDE * ICON_SIDE * CHANNELS) as usize;
    if ICON_RGBA.len() != expected {
        // Not logged again here: `window_icon` already says it once at startup,
        // and this is called from paint code — a log line per frame is how a real
        // error becomes invisible.
        return None;
    }
    Some(egui::ColorImage::from_rgba_unmultiplied(
        [ICON_SIDE as usize, ICON_SIDE as usize],
        ICON_RGBA,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_embedded_icon_is_the_size_it_claims() {
        let icon = window_icon().expect("the committed blob must match ICON_SIDE");
        assert_eq!(icon.width, ICON_SIDE);
        assert_eq!(icon.height, ICON_SIDE);
        assert_eq!(icon.rgba.len(), (ICON_SIDE * ICON_SIDE * CHANNELS) as usize);
    }

    #[test]
    fn the_centre_of_the_icon_is_opaque() {
        // The mark fills the middle of the canvas; a fully transparent centre
        // would mean the blob was rendered from the wrong file or byte order.
        let icon = window_icon().expect("icon");
        let middle = ((ICON_SIDE / 2 * ICON_SIDE + ICON_SIDE / 2) * CHANNELS) as usize;
        assert_eq!(icon.rgba[middle + 3], 255, "alpha at the centre");
    }

    #[test]
    fn the_corner_of_the_icon_is_transparent() {
        // The backdrop is a rounded square, so pixel (0,0) falls outside it.
        let icon = window_icon().expect("icon");
        assert_eq!(icon.rgba[3], 0, "alpha at the top-left corner");
    }

    #[test]
    fn the_on_screen_image_is_the_same_mark_as_the_window_icon() {
        // One source, two consumers. An About box showing a different picture from
        // the dock is an About box nobody can use to confirm which build is running,
        // so this asserts they come from the same bytes rather than trusting that
        // two functions in one file stay in step.
        let icon = window_icon().expect("icon");
        let image = icon_image().expect("image");

        assert_eq!(image.size, [ICON_SIDE as usize, ICON_SIDE as usize]);
        assert_eq!(image.pixels.len(), icon.rgba.len() / CHANNELS as usize);

        // The centre is opaque in both, and the corner transparent in both — the
        // two ends of the alpha channel, which is what a wrong byte order or a
        // premultiplied blob would break.
        let middle = (ICON_SIDE / 2 * ICON_SIDE + ICON_SIDE / 2) as usize;
        assert_eq!(image.pixels[middle].a(), 255, "alpha at the centre");
        assert_eq!(image.pixels[0].a(), 0, "alpha at the top-left corner");
    }
}
