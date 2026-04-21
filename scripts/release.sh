#!/usr/bin/env bash
# Stamp a new version across VERSION, Cargo.toml, the Swift Xcode project,
# and create a commit + tag. Does NOT push — Derek reviews manually.
#
# Usage: scripts/release.sh X.Y.Z
# Example: scripts/release.sh 0.1.0
#
# Precondition: .github/release-notes/vX.Y.Z.md exists and is non-empty.

set -euo pipefail

# ---- Argument validation ---------------------------------------------------

if [ "$#" -ne 1 ]; then
    echo "usage: $(basename "$0") X.Y.Z" >&2
    exit 64  # EX_USAGE
fi

VERSION="$1"

# Plain-text semver check (no suffixes for Phase 3 — no v0.1.0-rc.1 etc.)
if ! [[ "$VERSION" =~ ^([0-9]+)\.([0-9]+)\.([0-9]+)$ ]]; then
    echo "error: version must be X.Y.Z (plain semver), got: $VERSION" >&2
    exit 64
fi

TAG="v$VERSION"
NOTES_FILE=".github/release-notes/${TAG}.md"

# ---- Cwd: repo root --------------------------------------------------------

SCRIPT_DIR="$(cd "$(dirname "$0")" && pwd)"
REPO_ROOT="$(cd "$SCRIPT_DIR/.." && pwd)"
cd "$REPO_ROOT"

# ---- Preconditions ---------------------------------------------------------

# Ignore untracked files — local-tool state (.claude/, .vscode/, etc.) is
# routinely present and shouldn't block a release. We *do* want to block on
# modified or staged tracked files, which the `-uno` flag still reports.
if [ -n "$(git status --porcelain --untracked-files=no)" ]; then
    echo "error: working tree has uncommitted changes to tracked files." >&2
    echo "       Commit or stash before releasing." >&2
    git status --short --untracked-files=no >&2
    exit 65  # EX_DATAERR
fi

if [ ! -s "$NOTES_FILE" ]; then
    echo "error: $NOTES_FILE is missing or empty." >&2
    echo "       Write release notes first, commit them, then re-run." >&2
    exit 65
fi

if git rev-parse -q --verify "$TAG" >/dev/null; then
    echo "error: tag $TAG already exists locally." >&2
    exit 65
fi

echo "==> Releasing $TAG"
echo "    Notes file: $NOTES_FILE ($(wc -l <"$NOTES_FILE") lines)"

# ---- Stamp VERSION ---------------------------------------------------------

echo "$VERSION" > VERSION

# ---- Stamp root Cargo.toml ([workspace.package].version) -------------------

# Only touches the line inside the [workspace.package] table. BSD/macOS sed
# with a range address: between the header '[workspace.package]' and the next
# empty line, replace the 'version = "…"' line.
sed -i '' -e "/^\[workspace\.package\]/,/^$/ s/^version = \".*\"$/version = \"$VERSION\"/" Cargo.toml

# ---- Stamp Swift Xcode project --------------------------------------------

PBXPROJ="swift/ContextFS/ContextFS.xcodeproj/project.pbxproj"

# MARKETING_VERSION appears multiple times (one per build configuration /
# target); stamp every occurrence.
sed -i '' -e "s/MARKETING_VERSION = [^;]*;/MARKETING_VERSION = $VERSION;/g" "$PBXPROJ"

# CURRENT_PROJECT_VERSION is a monotonic build number, not the semver.
# We compute this *before* the release commit exists, so add 1 — by the
# time the tag points at the release commit, `git rev-list --count HEAD`
# on that commit will equal this number.
BUILD_NUMBER="$(( $(git rev-list --count HEAD) + 1 ))"
sed -i '' -e "s/CURRENT_PROJECT_VERSION = [^;]*;/CURRENT_PROJECT_VERSION = $BUILD_NUMBER;/g" "$PBXPROJ"

# ---- Refresh Cargo.lock ---------------------------------------------------

# --offline avoids a network round-trip; if offline fails (e.g., a new
# workspace-inherited field forced resolution), retry online with --locked=false.
if ! cargo generate-lockfile --offline 2>/dev/null; then
    echo "    cargo generate-lockfile offline failed, retrying online..."
    cargo generate-lockfile
fi

# ---- Stage + commit + tag -------------------------------------------------

# Explicit list. -am would miss the newly-added release-notes file on *first*
# release, but since Derek writes+commits notes separately per the flow, this
# list covers only files the script itself touched. If it turns out something
# was missed, `git status` after the commit will show it.
#
# Cargo.lock is conditionally staged — this workspace gitignores it (binaries
# only; no libs publish). `git ls-files --error-unmatch` returns non-zero if
# the file isn't tracked, in which case we skip it.
git add VERSION Cargo.toml "$PBXPROJ"
if git ls-files --error-unmatch Cargo.lock >/dev/null 2>&1; then
    git add Cargo.lock
fi

git commit -m "chore: release $TAG"
git tag "$TAG"

echo ""
echo "==> Done. Review:"
echo "    git show HEAD"
echo "    git log -1 --stat"
echo ""
echo "==> If everything looks right, push:"
echo "    git push && git push --tags"
