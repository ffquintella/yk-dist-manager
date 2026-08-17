#!/usr/bin/env bash
#
# Release notes for a tag, and the **schema warning** that has to be in them
# (features/packaging-and-release.md phase 8).
#
# Why this is automated rather than remembered: a register written by a newer
# build is refused by an older one (`StoreError::SchemaTooNew`), which is the
# correct behaviour and a confusing afternoon for a unit where two workstations
# share a file on a network share. The refusal is only useful if the release notes
# said "upgrade every workstation together" — and that sentence is exactly the one
# that gets left out, because whoever bumps SCHEMA_VERSION is three weeks away
# from whoever writes the notes.
#
# So the notes are generated from two things that cannot drift: the changelog
# section for the version, and `store::SCHEMA_VERSION` as it stands in this
# checkout compared with the previous tag.
#
# Usage:
#   scripts/release-notes.sh [releases/vX.Y.Z]   # defaults to the Cargo.toml version
#
# Exit codes:
#   0  notes written to stdout
#   1  something is missing (no changelog section for the version)
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

CARGO_VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
TAG="${1:-releases/v$CARGO_VERSION}"
# `releases/v0.16.0` since scripts/release.sh, `v0.15.0` and earlier before it.
# Both reduce to the version, so notes can still be generated for a tag from
# before the namespace existed.
VERSION="${TAG##*/}"
VERSION="${VERSION#v}"

if [[ "$VERSION" != "$CARGO_VERSION" ]]; then
	echo "tag $TAG does not match the version in Cargo.toml ($CARGO_VERSION)" >&2
	exit 1
fi

# The changelog section for this version, from its heading to the next one.
NOTES="$(awk -v version="$VERSION" '
	$0 ~ "^## \\[" version "\\]" { printing = 1; next }
	printing && /^## \[/ { exit }
	printing { print }
' CHANGELOG.md)"

if [[ -z "$(tr -d '[:space:]' <<<"$NOTES")" ]]; then
	echo "CHANGELOG.md has no section for [$VERSION] — move [Unreleased] into it first (AGENTS.md §5)" >&2
	exit 1
fi

schema_version_at() {
	# `git show` of the store module at a ref, or the working tree when no ref is
	# given. Printing 0 for "cannot tell" is deliberate: a missing previous value
	# reads as a bump, and a spurious upgrade warning is a far better failure than
	# a missing one.
	local ref="${1:-}"
	local source
	if [[ -z "$ref" ]]; then
		source="$(cat src/store/mod.rs)"
	else
		source="$(git show "$ref:src/store/mod.rs" 2>/dev/null || true)"
	fi
	sed -n 's/^pub const SCHEMA_VERSION: i64 = \([0-9]*\);/\1/p' <<<"$source" | head -1
}

CURRENT_SCHEMA="$(schema_version_at)"
# Both namespaces, so the release after the change still finds the one before it.
PREVIOUS_TAG="$(git describe --tags --abbrev=0 --match 'releases/v*' --match 'v*' "$TAG^" 2>/dev/null ||
	git describe --tags --abbrev=0 --match 'releases/v*' --match 'v*' 2>/dev/null || true)"
PREVIOUS_SCHEMA="$(schema_version_at "$PREVIOUS_TAG")"
PREVIOUS_SCHEMA="${PREVIOUS_SCHEMA:-0}"

printf '%s\n' "$NOTES"

if [[ "$CURRENT_SCHEMA" != "$PREVIOUS_SCHEMA" ]]; then
	cat <<EOF

### Upgrade note — the database schema changed

This release moves the register from schema v$PREVIOUS_SCHEMA to
**v$CURRENT_SCHEMA**, and migrates the file the first time it is opened.

* **Upgrade every workstation that shares the register.** Once one of them has
  opened the file, an older build refuses it — deliberately, rather than working
  against a schema it does not understand.
* **Take a backup first** if the register is on a share or in a synchronising
  folder. The migration is one-way: there is no downgrade.
* The migration runs on open and needs **write access**, so a read-only session
  cannot perform it.
EOF
fi

if [[ -n "$PREVIOUS_TAG" ]]; then
	printf '\n---\n\nCommits since %s: `git log --oneline %s..%s`\n' \
		"$PREVIOUS_TAG" "$PREVIOUS_TAG" "$TAG"
fi
