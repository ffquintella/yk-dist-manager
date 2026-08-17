#!/usr/bin/env bash
#
# Check that the installer package will install the thing it claims to.
#
# The .app has its own verifier (verify-bundle.sh) and this is not a second copy of
# it: what is checked here is everything the packaging step can get wrong *after*
# the bundle was correct. Each one has a real failure mode:
#
#   * a payload rooted at the wrong path installs an app nobody can find;
#   * a relocatable component installs over an old copy somewhere else on the
#     disk, so the operator's /Applications never changes and the receipt names a
#     path nobody expects;
#   * a distribution whose version disagrees with the payload makes an upgrade
#     decision on a wrong number;
#   * a distribution with no architecture restriction installs an arm64 binary on
#     an Intel Mac, where it dies on launch instead of being refused;
#   * a missing license or Read Me resource is a blank pane in the installer.
#
# Nothing is installed to verify it. The payload is extracted and interrogated in
# a temporary directory — `--diagnose` on the binary that is actually inside the
# package, which is the same question verify-bundle.sh and verify-package.sh ask.
#
# Usage: packaging/macos/verify-pkg.sh [PKG]
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

APP_NAME="YubiKey Distribution Manager"
BINARY="yk-dist-manager"

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

pass() { echo "  ok    $*"; }

PKG="${1:-}"
if [[ -z "$PKG" ]]; then
	PKG="$(ls -t target/bundle/*.pkg 2>/dev/null | head -1 || true)"
fi
[[ -n "$PKG" && -f "$PKG" ]] || fail "no package to check — run: make pkg"
echo "checking $PKG"
pass "package exists"

SCRATCH="$(mktemp -d)"
trap 'rm -rf "$SCRATCH"' EXIT

# --expand reads the product archive itself: the distribution and the component's
# PackageInfo, without unpacking the payload.
pkgutil --expand "$PKG" "$SCRATCH/expanded" || fail "the package could not be expanded"
DIST="$SCRATCH/expanded/Distribution"
[[ -f "$DIST" ]] || fail "the package carries no Distribution script"
pass "package expands"

xmllint --noout "$DIST" 2>/dev/null || fail "the Distribution script is not valid XML"
pass "Distribution is valid XML"

# xmllint --xpath rather than grep: an attribute this check depends on is easy to
# match by accident in a comment.
dist_value() {
	xmllint --xpath "string($1)" "$DIST" 2>/dev/null || true
}

CARGO_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
PKG_VERSION="$(dist_value '//pkg-ref[@version]/@version')"
[[ -n "$PKG_VERSION" ]] || fail "the Distribution declares no version"
[[ "$PKG_VERSION" == "$CARGO_VERSION" ]] ||
	fail "version drift: the package says $PKG_VERSION, Cargo.toml says $CARGO_VERSION"
pass "version matches Cargo.toml ($PKG_VERSION)"

for resource in license.txt readme.txt; do
	[[ -f "$SCRATCH/expanded/Resources/$resource" ]] ||
		fail "the installer has no $resource, so its pane will be blank"
done
# The license pane must show the licence, not a placeholder: comparing against
# LICENSE is the only way to catch a resource that was copied once and then went
# stale.
cmp -s LICENSE "$SCRATCH/expanded/Resources/license.txt" ||
	fail "the license pane does not match LICENSE"
pass "license and Read Me panes are present, and the licence matches LICENSE"

# The Read Me is the copy that is in front of somebody while they install, so it
# has to still name the two things that turn into support calls.
README="$SCRATCH/expanded/Resources/readme.txt"
grep -qi "camera permission" "$README" || fail "the Read Me does not mention camera permission"
grep -q -- "--diagnose" "$README" || fail "the Read Me does not tell the operator how to interrogate the build"
pass "the Read Me names the platform requirements"

HOST_ARCHS="$(dist_value '//options/@hostArchitectures')"
[[ -n "$HOST_ARCHS" ]] ||
	fail "the Distribution sets no hostArchitectures, so it would install on a machine that cannot run the binary"
pass "restricted to: $HOST_ARCHS"

ENABLE_ANYWHERE="$(dist_value '//domains/@enable_anywhere')"
[[ "$ENABLE_ANYWHERE" == "false" ]] ||
	fail "the Distribution allows installing anywhere; the app belongs in /Applications"
pass "installs to /Applications only"

COMPONENT="$SCRATCH/expanded/component.pkg"
[[ -d "$COMPONENT" ]] || fail "the product archive does not contain component.pkg"
PACKAGE_INFO="$COMPONENT/PackageInfo"
[[ -f "$PACKAGE_INFO" ]] || fail "component.pkg has no PackageInfo"

INSTALL_LOCATION="$(xmllint --xpath "string(//pkg-info/@install-location)" "$PACKAGE_INFO" 2>/dev/null || true)"
[[ "$INSTALL_LOCATION" == "/Applications" ]] ||
	fail "the payload installs to '$INSTALL_LOCATION', not /Applications"
pass "payload install-location: $INSTALL_LOCATION"

# Relocation, which is the failure mode with the least visible symptom: with it on,
# the installer looks for an existing copy of the bundle identifier anywhere on the
# disk and installs over *that*, so /Applications never changes and the receipt
# names a path nobody expects.
#
# Two things say it is off, and both are checked because neither alone is
# conclusive: `pkgbuild` writes `relocatable="false"` on pkg-info, but it emits an
# empty `<relocate/>` element either way — what distinguishes the two is whether
# that element lists a bundle to search for.
RELOCATABLE="$(xmllint --xpath "string(//pkg-info/@relocatable)" "$PACKAGE_INFO" 2>/dev/null || true)"
[[ "$RELOCATABLE" == "false" ]] ||
	fail "the payload declares relocatable='$RELOCATABLE': an upgrade would install over an existing copy elsewhere on the disk instead of into /Applications"
RELOCATE_TARGETS="$(xmllint --xpath "count(//relocate/bundle)" "$PACKAGE_INFO" 2>/dev/null || echo "?")"
[[ "$RELOCATE_TARGETS" == "0" ]] ||
	fail "the payload names $RELOCATE_TARGETS bundle(s) to relocate onto; it should name none"
pass "payload is not relocatable"

# The signature. Blocked on a Developer ID Installer certificate
# (features/packaging-and-release.md phase 3c), so this reports rather than fails
# — including for a release, because failing here would stop every release until
# procurement finishes.
SIGNATURE="$(pkgutil --check-signature "$PKG" 2>&1 || true)"
if grep -q "signed by a developer certificate issued by Apple" <<<"$SIGNATURE"; then
	pass "signed with a Developer ID Installer certificate"
elif grep -q "signed by an untrusted certificate" <<<"$SIGNATURE"; then
	echo "  warn  signed, but by a certificate this machine does not trust"
else
	echo "  warn  unsigned — Gatekeeper refuses it on first open, and a management tool may refuse it outright"
fi

# The decisive check: extract the payload and ask the binary that is actually
# inside the package about itself.
echo
echo "==> extracting the payload"
pkgutil --expand-full "$PKG" "$SCRATCH/full" >/dev/null ||
	fail "the payload could not be extracted"

APP="$SCRATCH/full/component.pkg/Payload/$APP_NAME.app"
[[ -d "$APP" ]] ||
	fail "the payload does not contain '$APP_NAME.app' at its root — it would not land in /Applications"
pass "payload root is $APP_NAME.app"

EXECUTABLE="$APP/Contents/MacOS/$BINARY"
[[ -x "$EXECUTABLE" ]] || fail "the packaged app has no executable at Contents/MacOS/$BINARY"
pass "packaged executable is present and executable"

codesign --verify --strict "$APP" 2>/dev/null ||
	fail "the app inside the package is not validly signed — packaging lost the signature"
pass "the packaged app's code signature survived packaging"

echo
echo "  --diagnose, from the app inside the package:"
REPORT="$("$EXECUTABLE" --diagnose)"
echo "$REPORT" | sed 's/^/    /'
echo

grep -q "^bundle:            yes" <<<"$REPORT" ||
	fail "the packaged binary does not consider itself bundled"
pass "the packaged binary sees itself as bundled"

grep -q "^camera usage key:  yes" <<<"$REPORT" ||
	fail "the packaged binary cannot find NSCameraUsageDescription in its own bundle"
pass "the packaged binary can read its usage description"

# Which commit this came from — a warning locally, a failure for a release, the
# same rule verify-bundle.sh and verify-package.sh apply: the norm requires every
# installed build to come from a tag.
COMMIT="$(grep '^commit:' <<<"$REPORT" | sed 's/^commit: *//')"
[[ -n "$COMMIT" ]] || fail "the packaged binary reports no commit"
case "$COMMIT" in
unknown | *-dirty)
	if [[ "${YKDM_VERIFY_RELEASE:-0}" == "1" ]]; then
		fail "this package reports commit '$COMMIT', so it cannot be traced to a tag"
	fi
	echo "  warn  commit $COMMIT — fine for a local build, never for one that is installed"
	;;
*)
	pass "built from commit $COMMIT"
	;;
esac

echo
echo "package verified: $PKG"
