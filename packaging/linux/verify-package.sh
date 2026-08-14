#!/usr/bin/env bash
#
# Check that a Linux artefact contains what an operator needs
# (features/packaging-and-release.md phase 5).
#
# Same argument as packaging/macos/verify-bundle.sh: packaging fails in ways that
# only show up at somebody's desk, so the artefact is verified rather than
# assumed. Every check here is something that has a specific failure mode:
#
#   * a missing udev rule means FIDO2 and the OTP slots report "no device" on a
#     key that is plugged in — a permission problem that looks like a broken key;
#   * a missing .desktop entry means the tool can only be started from a terminal;
#   * a binary that will not run at all is caught by asking it about itself, which
#     is also the report an operator would paste into a ticket;
#   * a version drift between the artefact's name and the binary makes a support
#     request unanswerable.
#
# Usage:
#   packaging/linux/verify-package.sh target/linux/yk-dist-manager-0.13.0-amd64.tar.gz
#   packaging/linux/verify-package.sh target/linux/yk-dist-manager_0.13.0_amd64.deb
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

ARTEFACT="${1:?usage: verify-package.sh <tarball|deb>}"
[[ -f "$ARTEFACT" ]] || {
	echo "FAIL: no artefact at $ARTEFACT — run: packaging/linux/package.sh" >&2
	exit 1
}

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

pass() { echo "  ok    $*"; }

WORK="$(mktemp -d)"
trap 'rm -rf "$WORK"' EXIT

case "$ARTEFACT" in
*.tar.gz)
	tar -C "$WORK" -xzf "$ARTEFACT"
	;;
*.deb)
	command -v dpkg-deb >/dev/null 2>&1 || fail "dpkg-deb is needed to check a .deb"
	dpkg-deb --extract "$ARTEFACT" "$WORK"
	# The control file is the half a .deb adds over the tarball, so it is the half
	# worth checking: a package that does not depend on pcscd installs cleanly and
	# then cannot read a PIV applet.
	CONTROL="$(dpkg-deb --field "$ARTEFACT")"
	grep -q '^Depends:.*libpcsclite1' <<<"$CONTROL" || fail "the package does not depend on libpcsclite1"
	grep -q '^Depends:.*pcscd' <<<"$CONTROL" || fail "the package does not depend on pcscd"
	pass "the package declares the PC/SC dependencies"
	;;
*)
	fail "unrecognised artefact: $ARTEFACT (expected .tar.gz or .deb)"
	;;
esac

BINARY="$WORK/usr/bin/yk-dist-manager"
[[ -x "$BINARY" ]] || fail "no executable at usr/bin/yk-dist-manager"
pass "layout"

RULES="$WORK/usr/lib/udev/rules.d/70-yk-dist-manager.rules"
[[ -f "$RULES" ]] || fail "the udev rule is missing — FIDO2 and OTP would be unreachable"
grep -q 'ATTRS{idVendor}=="1050"' "$RULES" || fail "the udev rule does not match Yubico's vendor id"
grep -q 'uaccess' "$RULES" || fail "the udev rule grants nothing to the logged-in seat"
pass "udev rule present and matches a YubiKey"

DESKTOP="$WORK/usr/share/applications/yk-dist-manager.desktop"
[[ -f "$DESKTOP" ]] || fail "the .desktop entry is missing"
grep -q '^Exec=yk-dist-manager' "$DESKTOP" || fail "the .desktop entry does not launch the binary"
if command -v desktop-file-validate >/dev/null 2>&1; then
	desktop-file-validate "$DESKTOP" || fail ".desktop entry is not valid"
	pass ".desktop entry is valid"
else
	pass ".desktop entry present (desktop-file-validate not installed)"
fi

[[ -f "$WORK/usr/share/icons/hicolor/256x256/apps/yk-dist-manager.png" ]] ||
	fail "the hicolor icon is missing — the launcher would show a placeholder"
pass "hicolor icons"

[[ -f "$WORK/usr/share/doc/yk-dist-manager/README.install" ]] ||
	fail "the install notes are missing, so nothing tells the operator about pcscd"
pass "install notes travel with the artefact"

# The decisive check, and the same one the macOS verifier makes: ask the binary
# about itself. This runs the packaged executable, so it also proves it links.
echo
echo "  --diagnose, from the packaged binary:"
REPORT="$("$BINARY" --diagnose)"
echo "$REPORT" | sed 's/^/    /'
echo

CARGO_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
grep -q "^yk-dist-manager $CARGO_VERSION" <<<"$REPORT" ||
	fail "version drift: the binary does not report $CARGO_VERSION"
pass "version matches Cargo.toml ($CARGO_VERSION)"

# Which commit this came from, and — when this is checking something about to be
# distributed — whether that commit is one anybody can look up. `unknown` and
# `-dirty` are warnings by default because a developer's own build is legitimately
# both; the release workflow sets YKDM_VERIFY_RELEASE=1, where they are failures,
# because the norm requires every installed build to come from a tag.
COMMIT="$(grep '^commit:' <<<"$REPORT" | sed 's/^commit: *//')"
[[ -n "$COMMIT" ]] || fail "the binary reports no commit"
case "$COMMIT" in
unknown | *-dirty)
	if [[ "${YKDM_VERIFY_RELEASE:-0}" == "1" ]]; then
		fail "this build reports commit '$COMMIT', so it cannot be traced to a tag — build from a clean checkout of one"
	fi
	echo "  warn  commit $COMMIT — fine for a local build, never for one that is installed"
	;;
*)
	pass "built from commit $COMMIT"
	;;
esac

echo
echo "artefact verified: $ARTEFACT"
