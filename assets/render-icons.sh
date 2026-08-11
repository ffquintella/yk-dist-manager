#!/usr/bin/env bash
#
# Render every raster icon from assets/logo.svg.
#
# `assets/logo.svg` is the only hand-edited artwork in the repository. Everything
# this script writes is generated, and is committed anyway: a build on a machine
# without a rasteriser still has to produce a bundle with an icon, and the window
# icon is compiled into the binary. Re-run after touching the SVG, and commit the
# SVG and the output together.
#
# Outputs:
#   assets/icons/icon-<n>.png      16 … 1024, for docs, Linux hicolor and Windows
#   assets/icons/icon-256.rgba     straight RGBA8, what src/branding.rs embeds
#   packaging/macos/icon.icns      picked up by packaging/macos/bundle.sh
#
# Requires rsvg-convert (librsvg) and ImageMagick. On macOS:
#   brew install librsvg imagemagick
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

SVG="assets/logo.svg"
OUT="assets/icons"
SIDE=256 # the window icon; egui wants a square, ideally 256×256

for tool in rsvg-convert magick; do
	if ! command -v "$tool" >/dev/null 2>&1; then
		echo "$tool is required (brew install librsvg imagemagick)" >&2
		exit 1
	fi
done

mkdir -p "$OUT"

echo "==> rendering $SVG"
for n in 16 32 64 128 256 512 1024; do
	rsvg-convert -w "$n" -h "$n" "$SVG" -o "$OUT/icon-$n.png"
	echo "    $OUT/icon-$n.png"
done

# The window icon is a raw pixel blob rather than a PNG so that the binary needs
# no image decoder: `image` is an optional dependency behind the barcode feature,
# and the icon must be there in every build.
echo "==> writing the window icon blob"
magick "$OUT/icon-$SIDE.png" -depth 8 "RGBA:$OUT/icon-$SIDE.rgba"
EXPECTED=$((SIDE * SIDE * 4))
ACTUAL=$(wc -c <"$OUT/icon-$SIDE.rgba" | tr -d ' ')
if [[ "$ACTUAL" != "$EXPECTED" ]]; then
	echo "icon-$SIDE.rgba is $ACTUAL bytes, expected $EXPECTED" >&2
	exit 1
fi
echo "    $OUT/icon-$SIDE.rgba ($ACTUAL bytes)"

# macOS wants the retina pairs under exactly these names or iconutil refuses.
echo "==> building packaging/macos/icon.icns"
ICONSET="$(mktemp -d)/icon.iconset"
mkdir -p "$ICONSET"
for pair in "16 16x16" "32 16x16@2x" "32 32x32" "64 32x32@2x" \
	"128 128x128" "256 128x128@2x" "256 256x256" "512 256x256@2x" \
	"512 512x512" "1024 512x512@2x"; do
	read -r px name <<<"$pair"
	cp "$OUT/icon-$px.png" "$ICONSET/icon_$name.png"
done
iconutil -c icns "$ICONSET" -o packaging/macos/icon.icns
rm -rf "$(dirname "$ICONSET")"
echo "    packaging/macos/icon.icns"

echo
echo "done. Commit assets/logo.svg together with everything above."
