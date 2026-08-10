#!/usr/bin/env bash
#
# Assemble the macOS application bundle.
#
# The bundle exists for one hard reason: macOS will not grant camera access to a
# bare binary. Without an Info.plist declaring NSCameraUsageDescription there is
# nothing to show the operator and no identity to attribute the grant to, and
# AVFoundation aborts the process instead of returning an error. See
# features/serial-scanning.md.
#
# No bundling tool is used. The layout is a handful of directories and one plist,
# and assembling it here means the whole thing is reviewable, works in CI, and does
# not depend on a crate that goes unmaintained.
#
# Usage:
#   packaging/macos/bundle.sh [--release] [--dmg] [--sign IDENTITY]
#
# Environment:
#   YKDM_BUNDLE_ID   bundle identifier (default: br.fgv.yk-dist-manager)
#   YKDM_FEATURES    cargo features (default: native-device,encrypted-db + defaults)
#   YKDM_COPYRIGHT   NSHumanReadableCopyright
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

PROFILE="debug"
CARGO_PROFILE_ARGS=()
MAKE_DMG=0
SIGN_IDENTITY="-" # ad-hoc; a Developer ID is passed with --sign

while [[ $# -gt 0 ]]; do
	case "$1" in
	--release)
		PROFILE="release"
		CARGO_PROFILE_ARGS=(--release)
		shift
		;;
	--dmg)
		MAKE_DMG=1
		shift
		;;
	--sign)
		SIGN_IDENTITY="${2:?--sign needs an identity}"
		shift 2
		;;
	*)
		echo "unknown argument: $1" >&2
		exit 2
		;;
	esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
	echo "This script builds a macOS bundle and only runs on macOS." >&2
	exit 1
fi

APP_NAME="YubiKey Distribution Manager"
BINARY="yk-dist-manager"
IDENTIFIER="${YKDM_BUNDLE_ID:-br.fgv.yk-dist-manager}"
FEATURES="${YKDM_FEATURES:-native-device,encrypted-db}"
COPYRIGHT="${YKDM_COPYRIGHT:-Fundação Getulio Vargas}"

# Single source of truth for the version: the manifest.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
if [[ -z "$VERSION" ]]; then
	echo "could not read the version from Cargo.toml" >&2
	exit 1
fi

OUT_DIR="target/bundle"
APP="$OUT_DIR/$APP_NAME.app"

echo "==> building $BINARY $VERSION ($PROFILE, features: $FEATURES)"
# `${array[@]}` on an empty array trips `set -u` under bash 3.2, which is what macOS
# ships as /bin/bash — so expand with a default.
cargo build ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"} --features "$FEATURES"

echo "==> assembling $APP"
rm -rf "$APP"
mkdir -p "$APP/Contents/MacOS" "$APP/Contents/Resources"

cp "target/$PROFILE/$BINARY" "$APP/Contents/MacOS/$BINARY"

# Info.plist from the template. `|` as the sed delimiter, since the copyright may
# contain a slash.
sed -e "s|@VERSION@|$VERSION|g" \
	-e "s|@IDENTIFIER@|$IDENTIFIER|g" \
	-e "s|@COPYRIGHT@|$COPYRIGHT|g" \
	packaging/macos/Info.plist.in >"$APP/Contents/Info.plist"

# `APPL????` — the classic package-type marker. Harmless, and some tools still
# look for it.
printf 'APPL????' >"$APP/Contents/PkgInfo"

# An icon is optional: drop an .icns at packaging/macos/icon.icns and it is picked
# up. No placeholder is invented — a made-up logo on an institutional tool is
# worse than the generic one.
if [[ -f packaging/macos/icon.icns ]]; then
	cp packaging/macos/icon.icns "$APP/Contents/Resources/icon.icns"
	/usr/libexec/PlistBuddy -c "Add :CFBundleIconFile string icon" \
		"$APP/Contents/Info.plist" >/dev/null
	echo "    icon: packaging/macos/icon.icns"
else
	echo "    icon: none (drop packaging/macos/icon.icns to add one)"
fi

echo "==> validating Info.plist"
plutil -lint "$APP/Contents/Info.plist"
for key in CFBundleIdentifier CFBundleExecutable NSCameraUsageDescription; do
	value="$(/usr/libexec/PlistBuddy -c "Print :$key" "$APP/Contents/Info.plist" 2>/dev/null || true)"
	if [[ -z "$value" ]]; then
		echo "Info.plist is missing $key" >&2
		exit 1
	fi
done

# Signing matters for more than Gatekeeper: the camera grant is remembered against
# the code signature, so an unsigned bundle re-prompts constantly. Ad-hoc is enough
# for local use; a Developer ID (via --sign) is required for distribution, along
# with notarisation.
echo "==> signing (identity: $SIGN_IDENTITY)"
codesign --force --deep --options runtime --sign "$SIGN_IDENTITY" "$APP"
codesign --verify --verbose=2 "$APP" 2>&1 | sed 's/^/    /'

if [[ "$SIGN_IDENTITY" == "-" ]]; then
	echo "    note: ad-hoc signature. Rebuilding changes the signature, so macOS will"
	echo "          ask for camera permission again. Use --sign 'Developer ID Application: …'"
	echo "          plus notarisation for anything distributed."
fi

if [[ "$MAKE_DMG" == "1" ]]; then
	DMG="$OUT_DIR/$APP_NAME $VERSION.dmg"
	echo "==> building $DMG"
	rm -f "$DMG"
	hdiutil create -quiet -volname "$APP_NAME" -srcfolder "$APP" -ov -format UDZO "$DMG"
	echo "    $DMG"
fi

echo
echo "built: $APP"
echo "check it with:  make verify-bundle"
echo "run it with:    open '$APP'"
