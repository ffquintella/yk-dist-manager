#!/usr/bin/env bash
#
# Assemble the Linux artefacts (features/packaging-and-release.md phase 5).
#
# Two outputs, and the tarball is the one that always exists:
#
#   * a **tarball** with an install script — works on any distribution, needs no
#     packaging tool on the build machine, and is what CI can always produce;
#   * a **.deb**, when `dpkg-deb` is available. Debian and Ubuntu are what the
#     workstations in front of this tool actually run, and a package is what puts
#     the udev rule in place without an operator being told to copy a file.
#
# No packaging crate and no `fpm`: the layout is four files in known places, and
# assembling it here means the whole thing is reviewable and cannot go
# unmaintained — the same argument packaging/macos/bundle.sh is built on.
#
# What this deliberately does NOT do is declare the runtime dependencies for
# every distribution. The .deb names the Debian package names; the tarball's
# README names the requirement in words, because `libpcsclite1` is called
# something else on Fedora and guessing would produce a package that refuses to
# install for the wrong reason.
#
# Usage:
#   packaging/linux/package.sh [--release] [--deb] [--features LIST]
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/../.."

PROFILE="debug"
CARGO_PROFILE_ARGS=()
TARGET_DIR="target/debug"
WANT_DEB=0
FEATURES="${YKDM_FEATURES:-native-device,encrypted-db}"

while [[ $# -gt 0 ]]; do
	case "$1" in
	--release)
		PROFILE="release"
		CARGO_PROFILE_ARGS=(--release)
		TARGET_DIR="target/release"
		shift
		;;
	--deb)
		WANT_DEB=1
		shift
		;;
	--features)
		FEATURES="${2:?--features needs a list}"
		shift 2
		;;
	*)
		echo "unknown argument: $1" >&2
		exit 2
		;;
	esac
done

if [[ "$(uname -s)" != "Linux" ]]; then
	echo "This script builds the Linux artefacts and only runs on Linux." >&2
	exit 1
fi

BINARY="yk-dist-manager"
# Single source of truth for the version: the manifest.
VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
if [[ -z "$VERSION" ]]; then
	echo "could not read the version from Cargo.toml" >&2
	exit 1
fi
ARCH="$(dpkg --print-architecture 2>/dev/null || uname -m)"
OUT_DIR="target/linux"
STAGE="$OUT_DIR/stage"

echo "==> building $BINARY $VERSION ($PROFILE, features: $FEATURES)"
cargo build ${CARGO_PROFILE_ARGS[@]+"${CARGO_PROFILE_ARGS[@]}"} --features "$FEATURES"

echo "==> staging"
rm -rf "$STAGE"
install -d "$STAGE/usr/bin"
install -d "$STAGE/usr/share/applications"
install -d "$STAGE/usr/lib/udev/rules.d"
install -d "$STAGE/usr/share/doc/$BINARY"
for size in 16 32 64 128 256 512; do
	install -d "$STAGE/usr/share/icons/hicolor/${size}x${size}/apps"
	install -m 644 "assets/icons/icon-${size}.png" \
		"$STAGE/usr/share/icons/hicolor/${size}x${size}/apps/$BINARY.png"
done

install -m 755 "$TARGET_DIR/$BINARY" "$STAGE/usr/bin/$BINARY"
install -m 644 packaging/linux/yk-dist-manager.desktop \
	"$STAGE/usr/share/applications/$BINARY.desktop"
install -m 644 packaging/linux/70-yk-dist-manager.rules \
	"$STAGE/usr/lib/udev/rules.d/70-yk-dist-manager.rules"
install -m 644 LICENSE "$STAGE/usr/share/doc/$BINARY/LICENSE"
install -m 644 CHANGELOG.md "$STAGE/usr/share/doc/$BINARY/CHANGELOG.md"

# What an operator has to know that the files cannot say for themselves. Kept
# here rather than in the docs directory because it travels with the artefact.
cat >"$STAGE/usr/share/doc/$BINARY/README.install" <<'EOF'
yk-dist-manager — what this needs to work on Linux

1. PC/SC, for the PIV applet:
     the `pcscd` daemon must be installed and running.
       Debian/Ubuntu:  apt install pcscd libpcsclite1
       Fedora/RHEL:    dnf install pcsc-lite
       Arch:           pacman -S pcsclite
     systemctl enable --now pcscd

2. USB HID, for FIDO2 and the OTP slots:
     the device node has to be readable by your session. The udev rule installed
     with this package does that with `uaccess`; after installing it by hand run
       sudo udevadm control --reload-rules && sudo udevadm trigger
     and re-plug the key. Yubico's own `libu2f-udev` package does the same job.

3. Camera scanning (optional):
     reading a serial from a barcode with a webcam needs read access to the V4L2
     device — usually membership of the `video` group. A USB barcode scanner
     needs nothing: it types into the field.

4. The register itself:
     one SQLite file that you choose or create; it can sit on an SMB share, which
     this tool can connect for you, or in a synchronising folder. Nothing else is
     installed and nothing is written outside your home directory and the file you
     picked.

Check what this build can reach on this machine:
     yk-dist-manager --diagnose
EOF

echo "==> tarball"
TARBALL="$OUT_DIR/$BINARY-$VERSION-$ARCH.tar.gz"
tar -C "$STAGE" -czf "$TARBALL" .
echo "    $TARBALL"

if [[ "$WANT_DEB" -eq 1 ]]; then
	if ! command -v dpkg-deb >/dev/null 2>&1; then
		echo "dpkg-deb is not on PATH — the tarball above is the artefact" >&2
		exit 1
	fi
	echo "==> .deb"
	install -d "$STAGE/DEBIAN"
	# `Depends` names the Debian packages: pcscd is the daemon the PIV path talks
	# to, and libpcsclite1 is what the binary links against. GTK and the X/Wayland
	# libraries come from eframe.
	cat >"$STAGE/DEBIAN/control" <<EOF
Package: $BINARY
Version: $VERSION
Section: utils
Priority: optional
Architecture: $ARCH
Depends: libc6, libpcsclite1, pcscd, libgtk-3-0, libudev1
Recommends: yubikey-manager
Maintainer: yk-dist-manager maintainers
Description: YubiKey distribution register and bootstrap tool
 Tracks which security token went to which person, when, by whom and what was
 applied to it, and applies a versioned bootstrap procedure to each key: a FIDO2
 PIN and resident credential, an OTP access code, and a PIV signing certificate
 carrying the holder's e-mail address.
 .
 Needs pcscd running for the PIV applet and a udev rule for USB HID access; both
 are installed by this package.
EOF
	cat >"$STAGE/DEBIAN/postinst" <<'EOF'
#!/bin/sh
set -e
# The rule is only in force once udev has read it, and the key has to be
# re-enumerated to pick up the tag.
if command -v udevadm >/dev/null 2>&1; then
	udevadm control --reload-rules || true
	udevadm trigger --subsystem-match=usb --subsystem-match=hidraw || true
fi
EOF
	chmod 755 "$STAGE/DEBIAN/postinst"
	DEB="$OUT_DIR/${BINARY}_${VERSION}_${ARCH}.deb"
	dpkg-deb --root-owner-group --build "$STAGE" "$DEB"
	echo "    $DEB"
fi

echo
echo "==> done. Verify the artefact before shipping it:"
echo "    packaging/linux/verify-package.sh $TARBALL"
