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
}
