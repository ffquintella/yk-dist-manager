#!/usr/bin/env bash
#
# Wrap the assembled .app in an installer package.
#
# The .dmg stays: it is the right artefact for an operator who administers their
# own workstation — mount it, drag the app across, done. The .pkg is for the case
# the .dmg cannot serve, which is the one a unit with managed machines is in: a
# .pkg can be pushed by a management tool (Jamf, Intune, `installer -pkg`) with no
# person at the keyboard, and it leaves a receipt saying which version went on.
# A .dmg can do neither. Two artefacts, two audiences, and the choice is documented
# in docs/operations.md rather than left for somebody to guess.
#
# This script does not build anything. It takes the bundle bundle.sh assembled and
# wraps it, so there is one place that knows how to build the app and one that
# knows how to package it — and so `make pkg` cannot quietly ship a bundle that
# `make verify-bundle` never saw.
#
# Two certificates, not one. macOS signs an application with a *Developer ID
# Application* identity (bundle.sh --sign) and an installer package with a
# *Developer ID Installer* identity (--sign-installer here). They are different
# certificates from the same programme, and a package signed with the wrong one is
# rejected with a message that does not say so.
#
# Usage:
#   packaging/macos/pkg.sh [--sign-installer IDENTITY]
#
# Environment:
#   YKDM_BUNDLE_ID       bundle identifier (default: org.example.yk-dist-manager)
#   YKDM_NOTARY_PROFILE  a `xcrun notarytool store-credentials` profile name. When
#                        set, the package is submitted for notarisation and
#                        stapled. The credentials stay in the keychain: nothing
#                        here takes an Apple ID or a password, so no secret can
#                        reach a command line, a log or this repository (AGENTS.md
#                        §2).
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

SIGN_IDENTITY=""

while [[ $# -gt 0 ]]; do
	case "$1" in
	--sign-installer)
		SIGN_IDENTITY="${2:?--sign-installer needs an identity}"
		shift 2
		;;
	*)
		echo "unknown argument: $1" >&2
		exit 2
		;;
	esac
done

if [[ "$(uname -s)" != "Darwin" ]]; then
	echo "This script builds a macOS installer package and only runs on macOS." >&2
	exit 1
fi

APP_NAME="YubiKey Distribution Manager"
BINARY="yk-dist-manager"
IDENTIFIER="${YKDM_BUNDLE_ID:-org.example.yk-dist-manager}"

OUT_DIR="target/bundle"
APP="$OUT_DIR/$APP_NAME.app"

if [[ ! -d "$APP" ]]; then
	echo "no bundle at $APP" >&2
	echo "build it first:  make bundle-release" >&2
	exit 1
fi

# The version comes from the bundle rather than from Cargo.toml on purpose: the
# package must describe the app it actually contains. If the two disagree, the
# bundle is stale, and saying so here beats shipping a package whose receipt lies
# about its own payload.
VERSION="$(/usr/libexec/PlistBuddy -c "Print :CFBundleShortVersionString" "$APP/Contents/Info.plist" 2>/dev/null || true)"
CARGO_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
if [[ -z "$VERSION" ]]; then
	echo "the bundle's Info.plist has no CFBundleShortVersionString" >&2
	exit 1
fi
if [[ "$VERSION" != "$CARGO_VERSION" ]]; then
	echo "version drift: the bundle says $VERSION, Cargo.toml says $CARGO_VERSION" >&2
	echo "re-assemble it:  make bundle-release" >&2
	exit 1
fi

# The architecture the binary was actually built for, which the distribution then
# refuses to install on anything else. `lipo -archs` reads the Mach-O rather than
# asking the machine, so a cross-build is described correctly too.
ARCH="$(lipo -archs "$APP/Contents/MacOS/$BINARY" 2>/dev/null || true)"
if [[ -z "$ARCH" ]]; then
	echo "could not read the architecture of $APP/Contents/MacOS/$BINARY" >&2
	exit 1
fi
# A universal binary reports both, space separated; the distribution wants them
# comma separated.
ARCH="${ARCH// /,}"

# The same rule write-plist.sh enforces, for the same reason: these three go into
# the distribution through `sed`, and a value that means something to sed or to XML
# would corrupt it quietly. Enforced rather than assumed.
for constrained in "$VERSION" "$IDENTIFIER" "$ARCH"; do
	case "$constrained" in
	*[^A-Za-z0-9._+,-]*)
		echo "refusing to substitute '$constrained': limited to letters, digits and . _ + , -" >&2
		exit 1
		;;
	esac
done

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT
ROOT="$WORK/root"
RESOURCES="$WORK/resources"
PACKAGES="$WORK/packages"
mkdir -p "$ROOT" "$RESOURCES" "$PACKAGES"

echo "==> staging $APP_NAME $VERSION ($ARCH)"
# -R preserves the signature; cp -r on a bundle does not reliably preserve the
# extended attributes a signature lives in.
ditto "$APP" "$ROOT/$APP_NAME.app"

echo "==> installer resources"
cp LICENSE "$RESOURCES/license.txt"

# What the Read Me pane shows. This is the Linux artefact's README.install lesson
# applied to macOS: the platform requirements have to be in front of somebody at
# the moment they are installing it, not only in docs/operations.md.
cat >"$RESOURCES/readme.txt" <<EOF
YubiKey Distribution Manager $VERSION — what this installs, and what it needs

Installs
    /Applications/$APP_NAME.app     (nothing else, and nothing outside it)

Smartcards (the PIV applet)
    Nothing to install. PC/SC is a system framework on macOS.

USB HID (FIDO2 and the OTP slots)
    Nothing to install, and no permission to grant.

Camera scanning (optional)
    Reading a serial from a barcode with the built-in camera asks for camera
    permission the first time. A USB barcode scanner needs nothing: it types
    into the field.

Gatekeeper
    If this package is not signed with a Developer ID Installer certificate,
    macOS refuses it on first open: right-click the .pkg -> Open -> Open. The
    same applies to the app itself, and the camera permission is remembered
    against the code signature, so an unsigned build asks again after every
    upgrade.

The register itself
    One SQLite file that you choose or create. It can sit on an SMB share, which
    this tool can connect for you, or in a synchronising folder. Nothing is
    written outside your home directory and the file you picked.

Check what this build can reach on this machine
    "/Applications/$APP_NAME.app/Contents/MacOS/$BINARY" --diagnose
EOF

# The component package: the payload and its receipt. Relocation is turned off
# deliberately — by default the installer looks for an existing copy of the same
# bundle identifier anywhere on the disk and installs *over that* instead of into
# /Applications, so an operator who once unzipped a copy into ~/Downloads gets
# every upgrade delivered there. The receipt then names a path nobody expects.
echo "==> component package"
COMPONENT_PLIST="$WORK/component.plist"
pkgbuild --analyze --root "$ROOT" "$COMPONENT_PLIST" >/dev/null
# A literal `false`, not free text, so PlistBuddy's re-parsing of its command
# string cannot bite (the reason write-plist.sh avoids it for the copyright).
/usr/libexec/PlistBuddy -c "Set :0:BundleIsRelocatable false" "$COMPONENT_PLIST"

pkgbuild --quiet \
	--root "$ROOT" \
	--component-plist "$COMPONENT_PLIST" \
	--identifier "$IDENTIFIER" \
	--version "$VERSION" \
	--install-location /Applications \
	"$PACKAGES/component.pkg"

echo "==> distribution"
DISTRIBUTION="$WORK/Distribution.xml"
sed -e "s|@VERSION@|$VERSION|g" \
	-e "s|@IDENTIFIER@|$IDENTIFIER|g" \
	-e "s|@ARCH@|$ARCH|g" \
	packaging/macos/Distribution.xml.in >"$DISTRIBUTION"

PKG="$OUT_DIR/$APP_NAME $VERSION $ARCH.pkg"
rm -f "$PKG"

PRODUCTBUILD_ARGS=(
	--distribution "$DISTRIBUTION"
	--resources "$RESOURCES"
	--package-path "$PACKAGES"
)
if [[ -n "$SIGN_IDENTITY" ]]; then
	echo "==> building and signing (identity: $SIGN_IDENTITY)"
	PRODUCTBUILD_ARGS+=(--sign "$SIGN_IDENTITY")
else
	echo "==> building (unsigned)"
fi

productbuild "${PRODUCTBUILD_ARGS[@]}" "$PKG"

if [[ -z "$SIGN_IDENTITY" ]]; then
	echo "    note: unsigned. Gatekeeper refuses an unsigned package on first open,"
	echo "          and a management tool that requires a signed package will refuse"
	echo "          it outright. Pass --sign-installer 'Developer ID Installer: …'"
	echo "          — a different certificate from the one bundle.sh --sign takes."
fi

# Notarisation, which is separate from signing: Apple's service has to have seen
# the package before Gatekeeper will accept it on a machine that has never met it.
# Only runs when a keychain profile names the credentials, so nothing here holds or
# echoes one.
if [[ -n "${YKDM_NOTARY_PROFILE:-}" ]]; then
	echo "==> notarising (keychain profile: $YKDM_NOTARY_PROFILE)"
	xcrun notarytool submit "$PKG" \
		--keychain-profile "$YKDM_NOTARY_PROFILE" --wait
	xcrun stapler staple "$PKG"
	xcrun stapler validate "$PKG"
else
	echo "    note: not notarised (set YKDM_NOTARY_PROFILE to a notarytool keychain profile)"
fi

echo
echo "built: $PKG"
echo "check it with:  make verify-pkg"
