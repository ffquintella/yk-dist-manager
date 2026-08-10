#!/usr/bin/env bash
#
# Check that the assembled bundle is the thing macOS needs it to be.
#
# Packaging is easy to get subtly wrong in ways that only show up as a crash at an
# operator's desk, so the bundle is verified rather than assumed. Every check here
# corresponds to something that has actually gone wrong:
#
#   * a missing NSCameraUsageDescription aborts the process on first camera use;
#   * a binary outside Contents/MacOS is not seen as bundled, so the app refuses
#     the camera it should now be able to use;
#   * a version drift between the plist and the binary makes a support ticket
#     unanswerable;
#   * an unsigned bundle re-prompts for camera permission on every launch.
#
# The binary is asked about itself with `--diagnose`, which is what an operator
# would paste into a ticket, so this checks the same thing they would report.
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

APP_NAME="YubiKey Distribution Manager"
APP="target/bundle/$APP_NAME.app"
BINARY="$APP/Contents/MacOS/yk-dist-manager"

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

pass() { echo "  ok    $*"; }

[[ -d "$APP" ]] || fail "no bundle at $APP — run: make bundle"
[[ -x "$BINARY" ]] || fail "no executable at $BINARY"
pass "bundle layout"

PLIST="$APP/Contents/Info.plist"
plutil -lint "$PLIST" >/dev/null || fail "Info.plist is not valid"
pass "Info.plist is valid"

plist_value() {
	/usr/libexec/PlistBuddy -c "Print :$1" "$PLIST" 2>/dev/null || true
}

CAMERA_USAGE="$(plist_value NSCameraUsageDescription)"
[[ -n "$CAMERA_USAGE" ]] || fail "NSCameraUsageDescription is missing — the camera will abort the app"
# A vague string is a real problem: it is the whole text the operator sees when
# deciding whether to allow access.
[[ ${#CAMERA_USAGE} -ge 40 ]] || fail "NSCameraUsageDescription is too vague to be useful: '$CAMERA_USAGE'"
pass "NSCameraUsageDescription present and specific"

IDENTIFIER="$(plist_value CFBundleIdentifier)"
[[ -n "$IDENTIFIER" ]] || fail "CFBundleIdentifier is missing"
pass "bundle identifier: $IDENTIFIER"

EXECUTABLE="$(plist_value CFBundleExecutable)"
[[ -x "$APP/Contents/MacOS/$EXECUTABLE" ]] || fail "CFBundleExecutable '$EXECUTABLE' is not there"
pass "executable matches the plist"

PLIST_VERSION="$(plist_value CFBundleShortVersionString)"
CARGO_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[[ "$PLIST_VERSION" == "$CARGO_VERSION" ]] ||
	fail "version drift: plist says $PLIST_VERSION, Cargo.toml says $CARGO_VERSION"
pass "version matches Cargo.toml ($PLIST_VERSION)"

codesign --verify --strict "$APP" 2>/dev/null || fail "the bundle is not validly signed"
pass "code signature verifies"

# The decisive check: the binary, running from inside the bundle, must report that
# macOS sees it as bundled and that camera scanning is no longer refused for that
# reason.
echo
echo "  --diagnose, from inside the bundle:"
REPORT="$("$BINARY" --diagnose)"
echo "$REPORT" | sed 's/^/    /'
echo

grep -q "^bundle:            yes" <<<"$REPORT" ||
	fail "the binary does not consider itself bundled"
pass "the binary sees itself as bundled"

grep -q "^camera usage key:  yes" <<<"$REPORT" ||
	fail "the binary cannot find NSCameraUsageDescription in its own bundle"
pass "the binary can read its usage description"

VERDICT="$(grep '^camera scanning:' <<<"$REPORT" | sed 's/^camera scanning: *//')"
case "$VERDICT" in
*"not running from an .app bundle"* | *"does not declare NSCameraUsageDescription"*)
	fail "the bundle did not fix the camera refusal: $VERDICT"
	;;
"ready")
	pass "camera scanning: ready"
	;;
*"not yet authorised"*)
	pass "camera scanning: waiting for the permission prompt (expected before first use)"
	;;
*)
	pass "camera scanning: $VERDICT"
	;;
esac

echo
echo "bundle verified: $APP"
