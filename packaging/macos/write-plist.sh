#!/usr/bin/env bash
#
# Write Contents/Info.plist from packaging/macos/Info.plist.in.
#
# Its own script, rather than four lines inside bundle.sh, because one of the
# three values is free text from the environment and free text is the thing that
# breaks a template substitution. Separating it lets `verify-bundle.sh` run the
# real writer against a hostile copyright instead of trusting it.
#
# The copyright used to go in through `sed` like the other two, which was wrong
# twice over:
#
#   * `&` in a sed *replacement* is the whole match, so YKDM_COPYRIGHT="Foo & Bar"
#     wrote "Foo @COPYRIGHT@ Bar" into the plist — a corrupted value, silently;
#   * `|` was the sed delimiter, so a copyright containing one ended the s///
#     expression and sed failed with a syntax error about the script;
#   * and `&`, `<` and `>` are XML, so they reached the plist raw. `plutil -lint`
#     caught the invalid XML that resulted, several steps away from the cause.
#
# So the copyright is not substituted at all: the plist is written first, then
# `plutil -replace` sets the key with the value passed as an *argument*. plutil
# takes it as data, escapes it for XML itself, and has no metacharacters of its
# own. (`PlistBuddy -c "Set …"` is not an alternative: it re-parses its command
# string, so it silently eats double quotes and fails on an apostrophe.)
#
# Usage: write-plist.sh TEMPLATE DEST VERSION IDENTIFIER COPYRIGHT
#
set -euo pipefail

USAGE="usage: write-plist.sh TEMPLATE DEST VERSION IDENTIFIER COPYRIGHT"
TEMPLATE="${1:?$USAGE}"
DEST="${2:?destination path — $USAGE}"
VERSION="${3:?version — $USAGE}"
IDENTIFIER="${4:?bundle identifier — $USAGE}"
COPYRIGHT="${5:?copyright — $USAGE}"

# @VERSION@ and @IDENTIFIER@ stay with sed, because both are narrow: a semantic
# version out of Cargo.toml and a reverse-DNS identifier. That is an assumption,
# so it is enforced here rather than hoped for — a value that could mean
# something to sed or to XML is refused, loudly, instead of quietly corrupting
# the plist the way the copyright did.
for constrained in "$VERSION" "$IDENTIFIER"; do
	case "$constrained" in
	*[^A-Za-z0-9._+-]*)
		echo "refusing to substitute '$constrained': a version or bundle identifier is limited to letters, digits and . _ + -" >&2
		exit 1
		;;
	esac
done

sed -e "s|@VERSION@|$VERSION|g" \
	-e "s|@IDENTIFIER@|$IDENTIFIER|g" \
	"$TEMPLATE" >"$DEST"

# The copyright, as data. plutil rewrites the file — it sorts the keys and drops
# the template's comments, which the PlistBuddy call that adds CFBundleIconFile
# already did anyway. The generated plist is a build artefact; the comments live
# in the template.
plutil -replace NSHumanReadableCopyright -string "$COPYRIGHT" "$DEST"
