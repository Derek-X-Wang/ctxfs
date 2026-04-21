# Phase 3c — Versioning + `release.sh` Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create a single-source-of-truth version pipeline so Derek can run `scripts/release.sh 0.1.0`, review the result, and push a `v0.1.0` tag — CI then takes over in Phase 3d. Stamps 16 `ctxfs-*` Rust crates + the Swift app + `VERSION` file with one version string; keeps vendored `fskit-rs` on its own upstream-derived version track; asserts that `.github/release-notes/vX.Y.Z.md` exists before committing, so no un-annotated releases ever reach GitHub.

**Architecture:**
- Root `VERSION` file is the authoritative version string (no `v` prefix, just `X.Y.Z`).
- Root `Cargo.toml` gets a `[workspace.package]` block with `version = "X.Y.Z"`.
- All 16 `ctxfs-*` crates migrate from `version = "0.0.0"` to `version.workspace = true`.
- `crates/fskit-rs` is explicitly excluded — it's our vendored fork of a third-party crate with its own upstream-derived version track (currently `0.1.0` inherited).
- `scripts/release.sh` is the only piece that edits version strings. It asserts the release-notes file exists, stamps every location, refreshes `Cargo.lock`, stages explicit paths, commits, and tags — but **does not push** (Derek reviews + pushes manually).

**Tech Stack:**
- Bash 3.2+ (macOS default shell compatibility — no Bash 4 features)
- `sed -i '' ...` syntax (macOS/BSD `sed`, not GNU)
- `cargo` for `generate-lockfile`
- `git` for staging + commit + tag
- `shellcheck` (optional — run manually in CI later)

**What's out of scope for 3c** (belongs to later phases):
- Pushing the tag (Derek does this manually — last safety net)
- CI reading `VERSION` (Phase 3d)
- Release-notes auto-generation (spec explicit NO — Derek writes manually)
- `.app` bundle signature verification (Phase 3d)
- `fskit-rs` version bumping (intentionally out — tracks upstream separately)

3c's ship criterion: running `scripts/release.sh 0.1.0` on a clean checkout produces one commit + one tag with all version strings updated, no stray `0.0.0` references, and fails loudly if the release-notes file is missing or malformed.

---

## File structure

Files created or modified by this plan:

| File | Responsibility |
|---|---|
| `VERSION` | NEW — authoritative version string, one line, no `v` prefix |
| `Cargo.toml` (root) | Add `[workspace.package]` block with `version`, `edition`, and `publish = false` |
| `crates/ctxfs-*/Cargo.toml` (16 crates) | Migrate `version = "0.0.0"` → `version.workspace = true` |
| `crates/fskit-rs/Cargo.toml` | UNCHANGED — vendored fork stays on upstream-derived version |
| `.github/release-notes/.gitkeep` | NEW — ensure directory survives empty state |
| `.github/release-notes/v0.1.0.md` | NEW — first release's notes template (Derek expands before cutting the tag) |
| `scripts/release.sh` | NEW — version-stamp + stage + commit + tag. ~80 lines of Bash. |
| `scripts/README.md` | NEW — one-page doc on release.sh usage + safety properties |

---

## Task 1: Introduce workspace-level version

**Files:**
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/VERSION`
- Modify: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/Cargo.toml`

- [ ] **Step 1: Create the `VERSION` file**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/VERSION` with exactly these contents (no `v` prefix, trailing newline):

```
0.1.0
```

Verify:
```bash
cat /Users/derekxwang/Development/incubator/ContextFS/ctxfs/VERSION
wc -c /Users/derekxwang/Development/incubator/ContextFS/ctxfs/VERSION
```

Expected: prints `0.1.0` then `6` (5 chars + newline).

- [ ] **Step 2: Add `[workspace.package]` to root Cargo.toml**

Open `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/Cargo.toml`. Find the `[workspace]` block (starts line 1) and the `[workspace.lints.rust]` block (around line 28 based on current file). Insert a new `[workspace.package]` block between them — so the structure becomes:

```toml
[workspace]
members = [
    "crates/ctxfs-core",
    # ... existing members ...
]
exclude = [
    # existing excludes
]
resolver = "2"

[workspace.package]
version = "0.1.0"
edition = "2021"
publish = false

[workspace.lints.rust]
# existing lints ...
```

Exact insertion: add the 4-line `[workspace.package]` block (plus a trailing blank line) immediately after the existing `resolver = "2"` line and immediately before `[workspace.lints.rust]`.

- [ ] **Step 3: Verify the workspace still parses**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo metadata --no-deps --format-version 1 > /dev/null && echo "metadata OK"
```

Expected: `metadata OK`. No crates have migrated to `version.workspace = true` yet — the workspace-level version exists but is unused. That's fine.

- [ ] **Step 4: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add VERSION Cargo.toml
git commit -m "build(workspace): introduce VERSION file + workspace.package

VERSION is the single source of truth for the project's version
string (no 'v' prefix, one line). Phase 3d's CI reads it and asserts
it matches the tag before signing/notarizing.

Root Cargo.toml now has a [workspace.package] block with version=0.1.0,
edition=2021, publish=false. Per-crate Cargo.toml files migrate to
version.workspace = true in the next task — they still carry
version=0.0.0 inline until then, which cargo tolerates."
```

---

## Task 2: Migrate 16 `ctxfs-*` crates to workspace-inherited version

**Files:**
- Modify: `crates/ctxfs-app-helper/Cargo.toml`
- Modify: `crates/ctxfs-cache/Cargo.toml`
- Modify: `crates/ctxfs-cache-redis/Cargo.toml`
- Modify: `crates/ctxfs-cli/Cargo.toml`
- Modify: `crates/ctxfs-core/Cargo.toml`
- Modify: `crates/ctxfs-daemon/Cargo.toml`
- Modify: `crates/ctxfs-fskit/Cargo.toml`
- Modify: `crates/ctxfs-ipc/Cargo.toml`
- Modify: `crates/ctxfs-manifest/Cargo.toml`
- Modify: `crates/ctxfs-nfs/Cargo.toml`
- Modify: `crates/ctxfs-provider-common/Cargo.toml`
- Modify: `crates/ctxfs-provider-crate/Cargo.toml`
- Modify: `crates/ctxfs-provider-git/Cargo.toml`
- Modify: `crates/ctxfs-provider-npm/Cargo.toml`
- Modify: `crates/ctxfs-provider-pypi/Cargo.toml`
- Modify: `crates/ctxfs-vfs/Cargo.toml`
- **Do NOT modify** `crates/fskit-rs/Cargo.toml` — vendored fork with its own version track

- [ ] **Step 1: Pre-check that every target crate has the expected line**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
for c in crates/ctxfs-*/Cargo.toml; do
  line=$(grep -n '^version = "0.0.0"' "$c")
  if [ -z "$line" ]; then
    echo "UNEXPECTED in $c — need manual handling"
  else
    echo "$c: $line"
  fi
done
```

Expected: 16 lines, each showing `version = "0.0.0"` on the same line number across crates (probably line 3). If any crate doesn't show up, investigate before running the migration — the line we're replacing must be unique within each file.

- [ ] **Step 2: Run the migration using `sed`**

Running against the full glob because the line is the same across all 16 crates. Use BSD/macOS `sed -i ''` syntax (no `-i.bak` — we rely on git for backup):

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
for c in crates/ctxfs-*/Cargo.toml; do
  sed -i '' 's/^version = "0.0.0"$/version.workspace = true/' "$c"
done
```

- [ ] **Step 3: Verify every crate now inherits the workspace version**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
grep -l '^version\.workspace = true$' crates/ctxfs-*/Cargo.toml | wc -l
echo "---no-0.0.0-left-in-ctxfs-crates---"
grep -l '^version = "0.0.0"' crates/ctxfs-*/Cargo.toml 2>&1
echo "---fskit-rs unchanged---"
grep '^version = ' crates/fskit-rs/Cargo.toml | head -1
```

Expected:
- First line: `16`
- Second section: empty grep result (no `0.0.0` left in ctxfs-* crates)
- Third section: `version = "0.1.0"` (fskit-rs unchanged)

- [ ] **Step 4: Build the workspace to confirm versions resolve**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo build --workspace 2>&1 | tail -5
```

Expected: `Finished 'dev' profile [unoptimized + debuginfo] target(s) in …`. All 17 crates (16 ctxfs + 1 fskit-rs) compile. If Cargo rejects the migration, it typically prints a clear error pointing at the offending crate.

- [ ] **Step 5: Run the test suite to verify no regressions**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
CTXFS_BACKEND=nfs cargo test --workspace -- --test-threads=1 2>&1 | grep -E "^test result:" | tail -20
```

Expected: every line `test result: ok.`. The `CTXFS_BACKEND=nfs` override prevents the known auto-detect brittleness we documented in prior phases.

- [ ] **Step 6: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add crates/ctxfs-*/Cargo.toml
git commit -m "build(workspace): 16 ctxfs-* crates inherit workspace version

Migrates 'version = \"0.0.0\"' → 'version.workspace = true' across
ctxfs-app-helper, ctxfs-cache, ctxfs-cache-redis, ctxfs-cli,
ctxfs-core, ctxfs-daemon, ctxfs-fskit, ctxfs-ipc, ctxfs-manifest,
ctxfs-nfs, ctxfs-provider-common, ctxfs-provider-crate,
ctxfs-provider-git, ctxfs-provider-npm, ctxfs-provider-pypi,
ctxfs-vfs.

crates/fskit-rs is intentionally excluded — it's our vendored fork
of the upstream fskit-rs crate, tracking upstream's version scheme
(currently 0.1.0). When we upstream our changes eventually, keeping
its version separate simplifies the PR diff.

Workspace builds and tests both clean."
```

---

## Task 3: Create the release-notes directory + first release's template

**Files:**
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/release-notes/.gitkeep`
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/release-notes/v0.1.0.md`

- [ ] **Step 1: Create the directory + .gitkeep**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
mkdir -p .github/release-notes
touch .github/release-notes/.gitkeep
```

- [ ] **Step 2: Create the v0.1.0 notes template**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/.github/release-notes/v0.1.0.md` with this content. Derek fills in the real details before cutting the actual tag in Phase 3e — this file is a *placeholder* that lets us test the release script; it will be edited manually before it ships publicly:

```markdown
# ContextFS 0.1.0

First public release of ContextFS — a read-only, mountable filesystem for
cloning-free access to Git repos, npm packages, PyPI packages, and crates.io
crates.

## Install

**Mac app (recommended for most users)**
```bash
brew install --cask contextfs
```

**CLI only (headless / CI)**
```bash
brew install contextfs
```

Or download the DMG / tarball directly from the GitHub Releases page.

## Requirements

- macOS 26.0 or later (FSKit backend)
- macOS 15.4 or later (NFS backend fallback)
- Apple Silicon or Intel Mac

## What's in the box

- `ctxfs` CLI — mount, list, unmount, diag, update
- `ContextFS.app` — menu bar companion, onboarding wizard, preferences window
- FSKit extension (macOS 26+) — no sudo, no Full Disk Access required
- NFS v3 loopback fallback for older macOS

## Known limitations

- macOS only (Linux CLI is Phase 3.5)
- Extension binary changes require a reboot (Apple FSKit constraint)
- Read-only mounts only — write support is post-1.0

## Getting help

- Docs: https://github.com/Derek-X-Wang/ctxfs/blob/main/README.md
- Issues: https://github.com/Derek-X-Wang/ctxfs/issues
```

- [ ] **Step 3: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add .github/release-notes/
git commit -m "docs(release): seed .github/release-notes/ + v0.1.0 template

.github/release-notes/vX.Y.Z.md is the file CI uploads as the
GitHub Release body. It's committed alongside the version bump so
nothing un-annotated ever ships.

v0.1.0.md is a first-release template; Derek expands it with
actual changelog items before cutting the tag in Phase 3e."
```

---

## Task 4: Write `scripts/release.sh`

**Files:**
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/release.sh`
- Create: `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/README.md`

The script does six things in order:

1. Validate argument format (`X.Y.Z` semver)
2. Assert working tree is clean (no dirty commits to accidentally stage)
3. Assert `.github/release-notes/vX.Y.Z.md` exists and is non-empty
4. Stamp the version in `VERSION`, root `Cargo.toml`, and the Swift project `project.pbxproj`
5. Refresh `Cargo.lock` via `cargo generate-lockfile --offline` with a fallback
6. Stage the explicit files + create the commit + tag (no push)

Bash 3.2 compatible — no `mapfile`, `[[ … =~ … ]]` with BASH_REMATCH is OK (supported since 3.2).

- [ ] **Step 1: Create the script**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/release.sh` with this exact content:

```bash
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

if [ -n "$(git status --porcelain)" ]; then
    echo "error: working tree is dirty. Commit or stash before releasing." >&2
    git status --short >&2
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
BUILD_NUMBER="$(git rev-list --count HEAD)"
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
git add \
    VERSION \
    Cargo.toml \
    Cargo.lock \
    "$PBXPROJ"

git commit -m "chore: release $TAG"
git tag "$TAG"

echo ""
echo "==> Done. Review:"
echo "    git show HEAD"
echo "    git log -1 --stat"
echo ""
echo "==> If everything looks right, push:"
echo "    git push && git push --tags"
```

- [ ] **Step 2: Make the script executable**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
chmod +x scripts/release.sh
```

- [ ] **Step 3: Create the scripts README**

Write `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/scripts/README.md`:

```markdown
# scripts/

Repository release tooling.

## `release.sh`

Stamp a new version everywhere + create a commit and tag. Does **not** push.

```bash
scripts/release.sh 0.1.0
```

### What it does

1. Validates the argument is plain semver `X.Y.Z` (no `-rc`, no `+build` — Phase 3 doesn't use suffixes).
2. Asserts `git status` is clean (no stray dirty changes get included in the release commit).
3. Asserts `.github/release-notes/vX.Y.Z.md` exists and is non-empty (no un-annotated releases).
4. Asserts the tag doesn't already exist locally.
5. Writes `X.Y.Z` to `VERSION`.
6. Stamps `version = "X.Y.Z"` inside `[workspace.package]` of the root `Cargo.toml`.
7. Stamps `MARKETING_VERSION = X.Y.Z;` across `project.pbxproj` (every build config / target).
8. Stamps `CURRENT_PROJECT_VERSION` across `project.pbxproj` with `$(git rev-list --count HEAD)` — monotonic build number.
9. Runs `cargo generate-lockfile --offline` (falls back to online if needed) to refresh `Cargo.lock`.
10. `git add`s the explicit list, creates a `chore: release vX.Y.Z` commit, creates the `vX.Y.Z` tag.

### What it doesn't do

- **Doesn't push.** That's on you — last safety net before CI runs. Review the commit and tag first:
  ```bash
  git show HEAD
  git log -1 --stat
  # then, if happy:
  git push && git push --tags
  ```
- **Doesn't create `.github/release-notes/vX.Y.Z.md`.** Write it by hand and commit it *before* running this script.
- **Doesn't bump the `fskit-rs` crate version.** That's a vendored fork on its own version track.
- **Doesn't run tests or clippy.** CI does. Run `cargo test` + `cargo clippy` locally before bumping if you want extra confidence.

### Failure modes

| Exit code | Reason |
|---|---|
| 64 | Bad argument (missing or non-semver) |
| 65 | Precondition failed (dirty tree, missing notes, tag already exists) |
| Other | Cargo / sed / git failure — read stderr |

### Undoing a release cut

If you ran the script and something's wrong:

```bash
git tag -d vX.Y.Z
git reset --hard HEAD~1
```

Safe to do as long as you haven't pushed.
```

- [ ] **Step 4: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add scripts/release.sh scripts/README.md
git commit -m "feat(release): scripts/release.sh stamps version + tags

Single source-of-truth release tooling. Asserts preconditions
(clean tree, release-notes file non-empty, tag doesn't exist),
stamps VERSION + Cargo.toml + project.pbxproj, refreshes
Cargo.lock, commits, tags. Does NOT push — Derek reviews the
commit and pushes manually.

Explicit git add of every touched path per Codex R4 feedback:
'-am' would miss new files in the release-notes directory.

Bash 3.2 / BSD sed compatible for macOS out of the box.
scripts/README.md documents usage + failure modes + undo."
```

---

## Task 5: Smoke-test `scripts/release.sh`

**Files:** none — runs the script, then resets.

This is the integration test: actually run the script against the repo, verify the commit + tag are well-formed, then reset to not pollute main.

- [ ] **Step 1: Confirm we're on main with a clean tree**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git status --short
git rev-parse --abbrev-ref HEAD
```

Expected: empty porcelain output; `main` as the current branch.

- [ ] **Step 2: Capture the current HEAD sha for the post-test reset**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
ORIGINAL_HEAD=$(git rev-parse HEAD)
echo "Will reset to: $ORIGINAL_HEAD"
```

- [ ] **Step 3: Run the release script with a dry version (use the actual v0.1.0 that already has a notes file)**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
./scripts/release.sh 0.1.0
```

Expected output:
```
==> Releasing v0.1.0
    Notes file: .github/release-notes/v0.1.0.md (X lines)
==> Done. Review:
    ...
==> If everything looks right, push:
    git push && git push --tags
```

Exit code: 0.

- [ ] **Step 4: Verify the commit + tag + stamping happened**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git log -1 --oneline
git tag --list 'v0.1.0'
git show HEAD --stat | head -20
echo "---VERSION---"
cat VERSION
echo "---Cargo.toml workspace.package---"
sed -n '/^\[workspace.package\]/,/^$/p' Cargo.toml
echo "---Xcode MARKETING_VERSION---"
grep MARKETING_VERSION swift/ContextFS/ContextFS.xcodeproj/project.pbxproj | head -3
echo "---Xcode CURRENT_PROJECT_VERSION---"
grep CURRENT_PROJECT_VERSION swift/ContextFS/ContextFS.xcodeproj/project.pbxproj | head -3
```

Expected:
- Commit message: `chore: release v0.1.0`
- Tag: `v0.1.0`
- VERSION: `0.1.0`
- `[workspace.package]` contains `version = "0.1.0"`
- MARKETING_VERSION lines show `0.1.0` (not `0.1`)
- CURRENT_PROJECT_VERSION lines show an integer (the `git rev-list --count HEAD` value)

- [ ] **Step 5: Test the precondition checks**

Delete the tag + reset first so we can re-test:
```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git tag -d v0.1.0
git reset --hard "$ORIGINAL_HEAD"
```

Now verify each failure mode:

**Bad argument:**
```bash
./scripts/release.sh not-a-version 2>&1 | head -3; echo "exit: $?"
```
Expected: `error: version must be X.Y.Z...` exit 64.

**Missing release notes:**
```bash
./scripts/release.sh 99.99.99 2>&1 | head -3; echo "exit: $?"
```
Expected: `error: .github/release-notes/v99.99.99.md is missing or empty.` exit 65.

**Dirty tree:**
```bash
echo "scratch" > /tmp/ctxfs-dirty-marker
# Create a dirty file in the repo for the test
touch dirty.tmp
./scripts/release.sh 0.1.0 2>&1 | head -5; echo "exit: $?"
rm dirty.tmp  # cleanup
```
Expected: `error: working tree is dirty.` exit 65.

- [ ] **Step 6: Final reset so main stays exactly as it was pre-smoke-test**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git tag -d v0.1.0 2>/dev/null || true
git reset --hard "$ORIGINAL_HEAD"
git status --short
git log --oneline -3
```

Expected: working tree clean, HEAD matches what it was at Step 2. No `v0.1.0` tag exists.

This task does NOT produce a commit — its purpose is to validate that the script works, then leave the tree in the pre-release state so Phase 3e can cut the real v0.1.0.

---

## Self-review checklist

**Spec coverage:** Plan 3c covers spec Section 4 (Versioning) fully. `VERSION` file, `[workspace.package]`, per-crate inheritance, `release.sh` with the six precondition checks (arg format, clean tree, notes exist, tag doesn't exist, stamping, Cargo.lock refresh), explicit `git add`, no push — all specified and implemented.

**Placeholder scan:** No "TBD" / "TODO" / "fill in later" text. Every code block is runnable as written.

**Type consistency:** `VERSION` file format is `X.Y.Z` plain (no `v`). Tag and release-notes file use `vX.Y.Z` (with `v`). The script bridges them via `TAG="v$VERSION"`. Consistent across all tasks.

**Known edges the plan does NOT solve** (all deferred with explicit citations):
- Pushing the tag — explicit user action (spec + script both document this as last safety net).
- `fskit-rs` is intentionally excluded from workspace versioning — the plan flags this explicitly and the script's Cargo.toml stamp only touches `[workspace.package]`, not the fskit-rs Cargo.toml.
- Running tests + clippy before releasing — the spec says CI does this post-tag; local runs are optional (README documents this).
- RC / beta versions — Phase 3 doesn't use them; plain `X.Y.Z` only. If 3.5+ adds them, release.sh's regex needs extending.
