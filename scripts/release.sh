#!/usr/bin/env bash
#
# Signal a release: tag this version under `releases/` and push the tag, which is
# what starts the release build (features/packaging-and-release.md phase 6).
#
# The norm's requirement is that every version installed anywhere is generated
# from version control and carries a tag (AGENTS.md §5). That makes the tag the
# thing the whole release rests on, and the tag is created by hand at the end of a
# long afternoon — which is exactly when a tree with an uncommitted file, a
# changelog nobody moved out of [Unreleased], or a commit that never reached the
# remote gets tagged anyway. Each of those produces an artefact somebody cannot
# rebuild, and none of them is visible in the tag.
#
# So the checks are here rather than in a checklist. Every one of them refuses
# rather than warns, because the alternative to refusing is a released build.
#
# `releases/v0.16.0` rather than `v0.16.0`: the tag namespace says what the tag is
# *for*. A repository accumulates tags — a fixture, a machine's checkpoint, a
# branch point somebody wanted to keep — and `refs/tags/releases/` is the set that
# was built and handed to somebody. `git tag -l 'releases/*'` is then the list of
# what exists in the field, which is the question asked during an incident.
# The workflow still triggers on both patterns, so the tags before this one are
# not orphaned.
#
# Usage:
#   scripts/release.sh              # tag the version in Cargo.toml and push it
#   scripts/release.sh --dry-run    # every check, then say what it would do
#
# Exit codes:
#   0  the tag was created and pushed (or the dry run passed every check)
#   1  a check refused
#
set -euo pipefail

cd "$(dirname "${BASH_SOURCE[0]}")/.."

DRY_RUN=0
[[ "${1:-}" == "--dry-run" ]] && DRY_RUN=1

fail() {
	echo "FAIL: $*" >&2
	exit 1
}

pass() { echo "  ok    $*"; }

VERSION="$(sed -n 's/^version = "\(.*\)"/\1/p' Cargo.toml | head -1)"
[[ -n "$VERSION" ]] || fail "no version in Cargo.toml"
TAG="releases/v$VERSION"

echo "Releasing $TAG"

# 1. A tag is a claim about a specific tree. An uncommitted file means the tree
#    that was tested is not the tree that will be built.
[[ -z "$(git status --porcelain)" ]] ||
	fail "the working tree has uncommitted changes — commit them before tagging"
pass "working tree is clean"

# 2. Not already released. Locally *and* on the remote: a tag that exists only on
#    the remote is the case where somebody released from another machine, and
#    pushing over it would silently change what a released version means.
! git rev-parse -q --verify "refs/tags/$TAG" >/dev/null ||
	fail "$TAG already exists locally — a released version is never re-tagged"
if git ls-remote --exit-code --tags origin "refs/tags/$TAG" >/dev/null 2>&1; then
	fail "$TAG already exists on origin — bump the version instead of retagging"
fi
pass "$TAG does not exist yet"

# 3. The changelog has a section for this version, and it agrees with Cargo.toml.
#    release-notes.sh refuses on either count, and it is the same command the
#    workflow runs, so a release that would fail in CI fails here instead.
NOTES="$(scripts/release-notes.sh "$TAG")" ||
	fail "release notes could not be generated — see the message above"
pass "CHANGELOG.md has a section for $VERSION"

# 4. The commit is on the remote. The workflow checks the tag out of origin, so a
#    commit that only exists here produces a build nobody else can reproduce —
#    and `git push <tag>` would carry the commit without moving any branch, which
#    is how a release ends up on no branch at all.
COMMIT="$(git rev-parse HEAD)"
if ! git branch -r --contains "$COMMIT" 2>/dev/null | grep -q .; then
	fail "HEAD ($(git rev-parse --short HEAD)) is on no remote branch — push it first"
fi
pass "HEAD is on a branch origin knows about"

if [[ "$DRY_RUN" == 1 ]]; then
	echo
	echo "Dry run: every check passed. This would have run"
	echo "    git tag -a $TAG -m '<the changelog section>'"
	echo "    git push origin $TAG"
	exit 0
fi

# The annotated tag carries the notes, so the release exists in the repository
# even for somebody working from a clone with no network.
git tag -a "$TAG" -m "$VERSION" -m "$NOTES"
pass "created $TAG at $(git rev-parse --short HEAD)"

git push origin "$TAG"
pass "pushed $TAG — the release build has started"

REMOTE="$(git remote get-url origin 2>/dev/null || true)"
SLUG="${REMOTE#*github.com[:/]}"
SLUG="${SLUG%.git}"
echo
echo "The workflow builds macOS, Linux and Windows and drafts a release."
echo "It never publishes: somebody looks at the artefacts and presses the button."
[[ -n "$SLUG" ]] && echo "  https://github.com/$SLUG/actions/workflows/release.yml"
