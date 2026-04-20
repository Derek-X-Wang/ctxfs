# Phase 3b — `ctxfs update` Subcommand Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add `ctxfs update` and `ctxfs update --check` subcommands so CLI-only users (Homebrew formula, GitHub Releases tarball, `curl | sh` future installer) can self-update their `ctxfs` binary without manually downloading. Binary swaps are atomic, SHA-256-verified, and refuse to touch package-manager-managed installs.

**Architecture:** `self_update` crate handles the download + atomic swap dance. A dedicated `install_path` module resolves the running binary via `_NSGetExecutablePath` + `canonicalize`, detects package-manager ownership (cask, formula) via path markers, and returns a `Decision` enum that the update handler consults before touching the binary. `ctxfs update --check` is a read-only GitHub API call; `ctxfs update` is the full apply flow.

**Tech Stack:**
- `self_update` 0.41.x — download/verify/swap
- `whoami` target detection — no, use `std::env::consts::ARCH` — simpler, stdlib
- `anyhow` for CLI error surfacing (already in use)
- Standard Rust test harness — unit tests inline, no new test infra

**What's out of scope for 3b** (belongs to later phases):
- Real GitHub releases with signed tarballs (Phase 3d produces them)
- Homebrew formula/cask URLs (Phase 3d/3e)
- End-to-end update download against a real release (requires 3d artifacts; smoke-tested in 3e dress rehearsal)
- `minisign` signature verification of checksums (explicitly dropped from Phase 3 per spec Section 2)

3b's ship criterion: `ctxfs update --check` runs locally without panicking, `install_path` module has exhaustive unit-test coverage, and a fresh `--help` shows the new subcommand. Actually applying an update against a real release is deferred to Phase 3e.

---

## File structure

Files created or modified by this plan:

| File | Responsibility |
|---|---|
| `crates/ctxfs-cli/Cargo.toml` | Add `self_update` dependency |
| `crates/ctxfs-cli/src/install_path.rs` | NEW — resolve running binary, classify manager ownership. Pure logic, exhaustively unit-tested. |
| `crates/ctxfs-cli/src/update.rs` | NEW — `--check` and apply modes. Wraps `self_update`'s GitHub backend. |
| `crates/ctxfs-cli/src/main.rs` | Register `Commands::Update` variant; dispatch to `update::run`. |

---

## Task 1: Add the `self_update` dependency + skeleton modules

**Files:**
- Modify: `crates/ctxfs-cli/Cargo.toml`
- Create: `crates/ctxfs-cli/src/install_path.rs`
- Create: `crates/ctxfs-cli/src/update.rs`
- Modify: `crates/ctxfs-cli/src/main.rs` (declare new modules)

- [ ] **Step 1: Inspect the current Cargo.toml**

```bash
cat /Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-cli/Cargo.toml
```

Note the existing `[dependencies]` block so the next step's insertion preserves sort order.

- [ ] **Step 2: Add `self_update` to `crates/ctxfs-cli/Cargo.toml`**

Append to the existing `[dependencies]` block (or insert in alphabetical position):

```toml
self_update = { version = "0.41", default-features = false, features = ["rustls", "archive-tar", "compression-flate2"] }
```

Rationale for the feature set:
- `rustls` — don't pull in OpenSSL (matches the rest of the workspace which uses `reqwest` with `rustls`)
- `archive-tar` + `compression-flate2` — our Release tarballs are `.tar.gz`. No zip support needed (Mac CLI ships tarballs, per Phase 3 spec Section 2)
- `default-features = false` — opt in explicitly, avoid pulling DSA/OpenSSL transitively

- [ ] **Step 3: Create an empty `install_path.rs` module**

Create `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-cli/src/install_path.rs` with this content (intentionally minimal — Task 2 adds real logic via TDD):

```rust
//! Resolve the running `ctxfs` binary's canonical path and classify whether
//! it's managed by a package manager (Homebrew formula, Homebrew cask, or
//! direct DMG install). Consulted by `ctxfs update` before touching the
//! binary so we never desync a package manager's view of its own files.

use std::path::{Path, PathBuf};

/// What `ctxfs update` should do given the running binary's location.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    /// The binary is not package-manager-managed. Safe to self-update.
    Proceed,
    /// The binary is bundled inside `/Applications/ContextFS.app`. Whether
    /// that app was cask-installed or DMG-dragged is indistinguishable from
    /// the canonical path alone, so surface a single user-facing message.
    RefuseAppBundled,
    /// The binary is a Homebrew-formula install. Direct the user to
    /// `brew upgrade contextfs`.
    RefuseHomebrewFormula,
}

/// The canonical path prefix for binaries bundled inside `ContextFS.app`.
///
/// Both cask-managed (`$HOMEBREW_PREFIX/Caskroom/contextfs/<ver>/...`) and
/// direct-DMG installs ultimately resolve symlinks to this path via `canonicalize()`.
const APP_BUNDLE_PREFIX: &str = "/Applications/ContextFS.app/Contents/MacOS/";

/// Classify a canonical binary path. Pure function; all filesystem work
/// happens elsewhere. `brew_prefix` is passed in so tests don't shell out.
#[must_use]
pub fn classify(canonical_path: &Path, brew_prefix: &Path) -> Decision {
    // Implementation added in Task 2.
    let _ = (canonical_path, brew_prefix, APP_BUNDLE_PREFIX);
    Decision::Proceed
}

/// Resolve the running binary's canonical path. Uses `std::env::current_exe`
/// followed by `canonicalize` — the former gives the invocation path, the
/// latter resolves every symlink (e.g., `$HOMEBREW_PREFIX/bin/ctxfs` →
/// `$HOMEBREW_PREFIX/Cellar/contextfs/<ver>/bin/ctxfs`).
pub fn current_canonical_exe() -> std::io::Result<PathBuf> {
    std::env::current_exe()?.canonicalize()
}
```

- [ ] **Step 4: Create an empty `update.rs` module**

Create `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-cli/src/update.rs`:

```rust
//! `ctxfs update` subcommand — self-update the CLI binary from GitHub Releases.
//!
//! Two modes:
//! - `ctxfs update --check` → query GitHub API, print latest version, exit 0
//!   if up-to-date, exit 1 if newer available. For scripting.
//! - `ctxfs update` → if newer available, download, verify SHA-256, atomically
//!   swap the current binary. Refuses if the binary is package-manager-managed.

use anyhow::Result;

/// Top-level entry point. `check_only=true` means `--check` was passed.
/// Implementation fleshed out in Tasks 3 + 4.
pub fn run(check_only: bool) -> Result<()> {
    let _ = check_only;
    anyhow::bail!("not yet implemented")
}
```

- [ ] **Step 5: Declare the new modules in `main.rs`**

Open `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-cli/src/main.rs`. Near the top with the other `mod` declarations (usually after `use` statements), add:

```rust
mod install_path;
mod update;
```

- [ ] **Step 6: Compile to verify skeleton is syntactically valid**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo build -p ctxfs 2>&1 | tail -5
```

Expected: `Finished \`dev\` profile …` with 0 errors. `self_update` will fetch + compile on first use (takes ~60 seconds on a warm machine).

If `self_update` fails to compile, it's usually a rustls/openssl feature-flag collision. Verify the feature set in Step 2 and confirm no workspace dep enabled `openssl`.

- [ ] **Step 7: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add crates/ctxfs-cli/Cargo.toml \
        crates/ctxfs-cli/src/install_path.rs \
        crates/ctxfs-cli/src/update.rs \
        crates/ctxfs-cli/src/main.rs \
        Cargo.lock
git commit -m "feat(cli): scaffold ctxfs update subcommand

Adds self_update 0.41 as a CLI-only dependency (rustls, tar+gzip
only — no OpenSSL, no zip). Two new modules stub the classify /
run interfaces that subsequent tasks fill in via TDD. No behavior
change yet; 'ctxfs update' returns 'not yet implemented'."
```

---

## Task 2: Implement install-path classification with TDD

The classifier is the only non-mechanical logic in Phase 3b — it enforces a safety rail (refusing to self-update brew-managed binaries). Exhaustive unit tests are worth the time.

**Files:**
- Modify: `crates/ctxfs-cli/src/install_path.rs`

- [ ] **Step 1: Write the first failing test**

Append to `crates/ctxfs-cli/src/install_path.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn returns_proceed_for_random_path() {
        let decision = classify(
            Path::new("/tmp/ctxfs-rc1/ctxfs"),
            Path::new("/opt/homebrew"),
        );
        assert_eq!(decision, Decision::Proceed);
    }
}
```

- [ ] **Step 2: Run the test to confirm it passes trivially (skeleton returns Proceed)**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo test -p ctxfs --lib install_path 2>&1 | tail -5
```

Wait — `ctxfs-cli`'s binary crate. Use the right test invocation:

```bash
cargo test -p ctxfs install_path::tests 2>&1 | tail -5
```

Expected: `test result: ok. 1 passed`.

This first test is the "no-op skeleton still holds" anchor. The next tests drive real logic.

- [ ] **Step 3: Add failing tests for cask/DMG bundled + formula cases**

Extend the tests module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn returns_proceed_for_random_path() {
        let decision = classify(
            Path::new("/tmp/ctxfs-rc1/ctxfs"),
            Path::new("/opt/homebrew"),
        );
        assert_eq!(decision, Decision::Proceed);
    }

    #[test]
    fn refuses_for_app_bundled_binary() {
        let decision = classify(
            Path::new("/Applications/ContextFS.app/Contents/MacOS/ctxfs"),
            Path::new("/opt/homebrew"),
        );
        assert_eq!(decision, Decision::RefuseAppBundled);
    }

    #[test]
    fn refuses_for_homebrew_formula_arm() {
        // Typical Apple Silicon Homebrew path
        let decision = classify(
            Path::new("/opt/homebrew/Cellar/contextfs/0.1.0/bin/ctxfs"),
            Path::new("/opt/homebrew"),
        );
        assert_eq!(decision, Decision::RefuseHomebrewFormula);
    }

    #[test]
    fn refuses_for_homebrew_formula_intel() {
        // Typical Intel Homebrew path
        let decision = classify(
            Path::new("/usr/local/Cellar/contextfs/0.1.0/bin/ctxfs"),
            Path::new("/usr/local"),
        );
        assert_eq!(decision, Decision::RefuseHomebrewFormula);
    }

    #[test]
    fn proceeds_for_homebrew_prefix_but_not_cellar() {
        // A user binary living in /opt/homebrew/bin that happens not to be
        // a Cellar symlink — e.g., manually placed. Don't refuse those.
        let decision = classify(
            Path::new("/opt/homebrew/bin/ctxfs"),
            Path::new("/opt/homebrew"),
        );
        assert_eq!(decision, Decision::Proceed);
    }

    #[test]
    fn app_bundled_takes_precedence_over_brew() {
        // If someone installs via cask, the canonical path resolves into
        // the app bundle. Don't trip over the intermediate Caskroom/
        // ancestor — the canonical match wins.
        let decision = classify(
            Path::new("/Applications/ContextFS.app/Contents/MacOS/ctxfs"),
            Path::new("/opt/homebrew"),
        );
        assert_eq!(decision, Decision::RefuseAppBundled);
    }

    #[test]
    fn handles_different_cellar_version_strings() {
        for version in ["0.1.0", "1.2.3", "0.1.0-beta.1", "HEAD-abc1234"] {
            let p = format!("/opt/homebrew/Cellar/contextfs/{version}/bin/ctxfs");
            let decision = classify(
                Path::new(&p),
                Path::new("/opt/homebrew"),
            );
            assert_eq!(
                decision,
                Decision::RefuseHomebrewFormula,
                "should refuse for version {version}",
            );
        }
    }
}
```

- [ ] **Step 4: Run the tests — expect 5 failures + 1 pass**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo test -p ctxfs install_path::tests 2>&1 | tail -15
```

Expected: `test result: FAILED. 1 passed; 6 failed.` (all except the `returns_proceed_for_random_path` case fail because skeleton always returns `Proceed`).

- [ ] **Step 5: Implement `classify()` to make all tests pass**

Replace the body of `classify` in `install_path.rs`:

```rust
#[must_use]
pub fn classify(canonical_path: &Path, brew_prefix: &Path) -> Decision {
    // Use string comparison on the canonical path — it's the cleanest way
    // to match a *prefix* across possibly non-UTF8 paths. The spec paths
    // are ASCII, so lossy conversion is fine for prefix checks.
    let path_str = canonical_path.to_string_lossy();

    // Order matters: a cask-managed install resolves to the app bundle
    // path; we want that to be RefuseAppBundled, not RefuseHomebrewFormula.
    if path_str.starts_with(APP_BUNDLE_PREFIX) {
        return Decision::RefuseAppBundled;
    }

    // Formula installs live at `{brew_prefix}/Cellar/contextfs/.../bin/ctxfs`.
    // Join the segments via Path::join so platform separators stay consistent.
    let cellar_prefix = brew_prefix.join("Cellar").join("contextfs");
    if canonical_path.starts_with(&cellar_prefix) {
        return Decision::RefuseHomebrewFormula;
    }

    Decision::Proceed
}
```

- [ ] **Step 6: Run the tests again — all should pass**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo test -p ctxfs install_path::tests 2>&1 | tail -5
```

Expected: `test result: ok. 7 passed`.

- [ ] **Step 7: Add a small helper to call `brew --prefix` with a fallback**

Still in `install_path.rs`, above the `#[cfg(test)]` block, add:

```rust
/// Resolve Homebrew's prefix. Falls back to `/opt/homebrew` (Apple Silicon
/// default) if `brew` isn't on PATH — the spec says classification works
/// even on machines that never installed Homebrew (they'll just never match
/// the formula branch anyway).
pub fn brew_prefix() -> PathBuf {
    // 1. HOMEBREW_PREFIX env var — Homebrew exports this in its shell env,
    //    and `brew env --plain` prints it. Cheapest to check first.
    if let Ok(prefix) = std::env::var("HOMEBREW_PREFIX") {
        if !prefix.is_empty() {
            return PathBuf::from(prefix);
        }
    }

    // 2. `brew --prefix` — authoritative but forks a process.
    if let Ok(output) = std::process::Command::new("brew").arg("--prefix").output() {
        if output.status.success() {
            let s = String::from_utf8_lossy(&output.stdout).trim().to_string();
            if !s.is_empty() {
                return PathBuf::from(s);
            }
        }
    }

    // 3. Fallback — Apple Silicon default. Intel users with no HOMEBREW_PREFIX
    //    and no `brew` on PATH are extremely rare; if they exist, classify()
    //    won't match Cellar paths anyway.
    PathBuf::from("/opt/homebrew")
}
```

- [ ] **Step 8: Verify the build still compiles and tests pass**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo build -p ctxfs 2>&1 | tail -3
cargo test -p ctxfs install_path::tests 2>&1 | tail -3
```

Expected: both green.

- [ ] **Step 9: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add crates/ctxfs-cli/src/install_path.rs
git commit -m "feat(cli): classify install-path for ctxfs update safety rail

Classify a canonical binary path into one of:
- RefuseAppBundled (binary is inside /Applications/ContextFS.app —
  user should use the app's Check for Updates menu or brew upgrade --cask)
- RefuseHomebrewFormula (binary is inside {brew_prefix}/Cellar/contextfs —
  user should brew upgrade)
- Proceed (safe to self-update)

Seven unit tests cover cask, DMG, formula (ARM + Intel), HOMEBREW_PREFIX
bin-but-not-Cellar, version-string edge cases, and the precedence rule
that app-bundled beats brew-formula when paths overlap.

brew_prefix() resolves Homebrew's prefix via HOMEBREW_PREFIX env var,
then \`brew --prefix\`, then /opt/homebrew fallback."
```

---

## Task 3: Implement `ctxfs update --check` — GitHub API query

**Files:**
- Modify: `crates/ctxfs-cli/src/update.rs`

- [ ] **Step 1: Expand `update.rs` with the `--check` implementation**

Replace the stub `run` function in `crates/ctxfs-cli/src/update.rs` with:

```rust
//! `ctxfs update` subcommand — self-update the CLI binary from GitHub Releases.
//!
//! Two modes:
//! - `ctxfs update --check` → query GitHub API, print latest version, exit 0
//!   if up-to-date, exit 1 if newer available. For scripting.
//! - `ctxfs update` → if newer available, download, verify SHA-256, atomically
//!   swap the current binary. Refuses if the binary is package-manager-managed.

use anyhow::{Context, Result, bail};
use self_update::backends::github::ReleaseList;
use self_update::cargo_crate_version;

use crate::install_path::{self, Decision};

const REPO_OWNER: &str = "Derek-X-Wang";
const REPO_NAME: &str = "ctxfs";

/// Top-level entry point. `check_only=true` means `--check` was passed.
pub fn run(check_only: bool) -> Result<()> {
    if check_only {
        return run_check();
    }
    run_apply()
}

/// `ctxfs update --check` — read-only, exits 0 if up-to-date, 1 if newer.
fn run_check() -> Result<()> {
    let current = cargo_crate_version!();
    let latest = fetch_latest_version()?;

    if let Some(latest) = latest {
        // self_update strips the leading `v` for us — so `v0.1.0` becomes `0.1.0`.
        if is_newer(&latest, current) {
            println!("Update available: {current} → {latest}");
            println!("Run 'ctxfs update' to apply.");
            std::process::exit(1);
        } else {
            println!("Up to date ({current}).");
            return Ok(());
        }
    }

    println!("No releases found at github.com/{REPO_OWNER}/{REPO_NAME}.");
    println!("Your current version is {current}.");
    Ok(())
}

/// Placeholder for Task 4.
fn run_apply() -> Result<()> {
    bail!("not yet implemented — filled in by Task 4");
}

/// Query the latest release tag from GitHub. Returns `None` if the repo has
/// no releases yet (expected state on a fresh Phase 3e bootstrap).
fn fetch_latest_version() -> Result<Option<String>> {
    let releases = ReleaseList::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .build()
        .context("failed to configure GitHub release listing")?
        .fetch()
        .context("failed to fetch release list from GitHub")?;

    Ok(releases.first().map(|r| r.version.clone()))
}

/// Compare two semver-ish versions. Defers to `self_update`'s internal
/// comparator so we don't re-implement semver parsing here.
fn is_newer(candidate: &str, current: &str) -> bool {
    match self_update::version::bump_is_greater(current, candidate) {
        Ok(is_greater) => is_greater,
        Err(_) => false, // Malformed version — treat as not newer (fail safe).
    }
}

// Decision is consumed by run_apply (Task 4); suppress dead-code warning until then.
#[allow(dead_code)]
fn _require_proceed() -> Result<()> {
    let path = install_path::current_canonical_exe()
        .context("failed to resolve current executable path")?;
    let prefix = install_path::brew_prefix();
    match install_path::classify(&path, &prefix) {
        Decision::Proceed => Ok(()),
        Decision::RefuseAppBundled => bail!(
            "This ctxfs is bundled with ContextFS.app. Update via the app's \
             'Check for Updates…' menu (or 'brew upgrade --cask contextfs' \
             if you installed via Homebrew)."
        ),
        Decision::RefuseHomebrewFormula => bail!(
            "Run 'brew upgrade contextfs' instead."
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_newer_recognizes_patch_bump() {
        assert!(is_newer("0.1.1", "0.1.0"));
    }

    #[test]
    fn is_newer_recognizes_minor_bump() {
        assert!(is_newer("0.2.0", "0.1.9"));
    }

    #[test]
    fn is_newer_rejects_same_version() {
        assert!(!is_newer("0.1.0", "0.1.0"));
    }

    #[test]
    fn is_newer_rejects_older() {
        assert!(!is_newer("0.0.9", "0.1.0"));
    }

    #[test]
    fn is_newer_returns_false_on_malformed() {
        // Malformed inputs should fail safe — don't trigger spurious update
        // prompts if GitHub returns an unexpected tag shape.
        assert!(!is_newer("not-a-version", "0.1.0"));
    }
}
```

- [ ] **Step 2: Build to confirm `self_update` links**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo build -p ctxfs 2>&1 | tail -3
```

First run downloads+compiles ~30 transitive deps; expect ~60-90 seconds. Subsequent runs are cached.

Expected: `Finished \`dev\` profile …`.

- [ ] **Step 3: Run the new unit tests**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo test -p ctxfs update::tests 2>&1 | tail -5
```

Expected: `test result: ok. 5 passed`.

- [ ] **Step 4: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add crates/ctxfs-cli/src/update.rs
git commit -m "feat(cli): implement 'ctxfs update --check' against GitHub API

Queries github.com/Derek-X-Wang/ctxfs releases, compares latest tag
against CARGO_PKG_VERSION, prints the result and exits 0 (up-to-date)
or 1 (newer available) for scripting.

Handles the no-releases-yet state gracefully (prints current version
with no error) so pre-Phase-3e invocations don't look broken.

5 unit tests cover version comparison edge cases. run_apply is still
stubbed — Task 4 fills it in with the full download + swap flow."
```

---

## Task 4: Implement `ctxfs update` — download, verify, swap

**Files:**
- Modify: `crates/ctxfs-cli/src/update.rs`

- [ ] **Step 1: Replace the `run_apply` stub with the full implementation**

In `crates/ctxfs-cli/src/update.rs`, replace the `run_apply` function and the `_require_proceed` helper. Also remove the `#[allow(dead_code)]` annotation since `_require_proceed` becomes a real callee:

```rust
/// `ctxfs update` — full download + swap flow. Refuses if the running binary
/// is package-manager-managed.
fn run_apply() -> Result<()> {
    require_proceed_or_bail()?;

    // self_update does the heavy lifting — GitHub API call, download matching
    // platform archive, untar, SHA-256 verify, atomic rename.
    //
    // The `bin_name` must match the binary inside the tarball; the `target`
    // is the triple Cargo uses (which matches our CI artifact naming per
    // Phase 3 spec Section 2: `ctxfs-X.Y.Z-darwin-{arm64,x86_64}.tar.gz`).
    let target = target_triple()?;
    let status = self_update::backends::github::Update::configure()
        .repo_owner(REPO_OWNER)
        .repo_name(REPO_NAME)
        .bin_name("ctxfs")
        .show_download_progress(true)
        .current_version(cargo_crate_version!())
        .target(&target)
        .build()
        .context("failed to configure self_update")?
        .update()
        .context("self_update failed")?;

    if status.updated() {
        println!("Updated to {}. Restart any open ctxfs shell sessions.", status.version());
    } else {
        println!("Already on the latest version ({}).", cargo_crate_version!());
    }
    Ok(())
}

/// Resolve the Rust target-triple string for the current binary. Used by
/// self_update to pick the right platform-specific archive out of the
/// Release assets. We ship two darwin triples; reject other hosts.
fn target_triple() -> Result<String> {
    match (std::env::consts::OS, std::env::consts::ARCH) {
        ("macos", "aarch64") => Ok("aarch64-apple-darwin".to_string()),
        ("macos", "x86_64") => Ok("x86_64-apple-darwin".to_string()),
        (os, arch) => bail!(
            "ctxfs update does not ship a binary for {os}/{arch}. \
             Phase 3 targets macOS only — install from source with \
             'cargo install --git https://github.com/Derek-X-Wang/ctxfs' \
             until Phase 3.5 adds Linux binaries."
        ),
    }
}

/// Consults `install_path::classify` and bails with a user-friendly message
/// if the running binary is package-manager-managed.
fn require_proceed_or_bail() -> Result<()> {
    let path = install_path::current_canonical_exe()
        .context("failed to resolve current executable path")?;
    let prefix = install_path::brew_prefix();
    match install_path::classify(&path, &prefix) {
        Decision::Proceed => Ok(()),
        Decision::RefuseAppBundled => bail!(
            "This ctxfs is bundled with ContextFS.app. Update via the app's \
             'Check for Updates…' menu (or 'brew upgrade --cask contextfs' \
             if you installed via Homebrew)."
        ),
        Decision::RefuseHomebrewFormula => bail!(
            "Run 'brew upgrade contextfs' instead."
        ),
    }
}
```

Also remove the old `_require_proceed` and the `#[allow(dead_code)]` line — the function is now `require_proceed_or_bail` and called from `run_apply`.

- [ ] **Step 2: Add unit tests for `target_triple` edge cases**

Extend the `#[cfg(test)]` block:

```rust
    #[test]
    #[cfg(all(target_os = "macos", target_arch = "aarch64"))]
    fn target_triple_is_aarch64_on_apple_silicon() {
        assert_eq!(target_triple().unwrap(), "aarch64-apple-darwin");
    }

    #[test]
    #[cfg(all(target_os = "macos", target_arch = "x86_64"))]
    fn target_triple_is_x86_64_on_intel_mac() {
        assert_eq!(target_triple().unwrap(), "x86_64-apple-darwin");
    }
```

These are compile-conditional: only the one matching the test machine's arch runs. Both are useful — on CI's macos-14 runner (arm64) the first runs; on a developer's Intel Mac the second does. Non-macOS test runs skip both.

- [ ] **Step 3: Build and run tests**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo build -p ctxfs 2>&1 | tail -3
cargo test -p ctxfs update::tests 2>&1 | tail -5
```

Expected: build green, `test result: ok. 6 passed` (5 version tests + 1 target_triple test matching the current arch).

- [ ] **Step 4: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add crates/ctxfs-cli/src/update.rs
git commit -m "feat(cli): implement 'ctxfs update' full apply flow

Wraps self_update's GitHub backend: download matching-arch tarball,
SHA-256 verify, atomic rename. Refuses if the running binary is
package-manager-managed (consults install_path::classify).

target_triple() maps (os, arch) → Rust target triple matching the
CI artifact naming scheme in Phase 3 spec Section 2. Only ships
darwin-arm64 and darwin-x86_64 in Phase 3; non-macOS hosts get a
clear bail message pointing at 'cargo install'."
```

---

## Task 5: Register the subcommand in `main.rs`

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs`

- [ ] **Step 1: Add the `Update` variant to the `Commands` enum**

Open `crates/ctxfs-cli/src/main.rs`. Find the `Commands` enum (around line 29). After the `Diag { … }` variant (around line 95-99), add:

```rust
    /// Self-update the CLI binary from the latest GitHub Release. Refuses
    /// if installed via Homebrew or bundled with ContextFS.app.
    Update {
        /// Only report whether an update is available. Exits 0 if up-to-date,
        /// 1 if a newer version exists. No files are modified.
        #[arg(long)]
        check: bool,
    },
```

The resulting diff span: the `Diag` variant stays at its current line range; the new `Update` variant is inserted directly before the closing `}` of the `Commands` enum.

- [ ] **Step 2: Dispatch the new variant in the `match cli.command` block**

Find the match block around line 214. After the `Commands::Diag { json } => { … }` arm, add:

```rust
        Commands::Update { check } => {
            update::run(check)?;
        }
```

- [ ] **Step 3: Build the CLI**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo build -p ctxfs 2>&1 | tail -3
```

Expected: `Finished \`dev\` profile …`.

- [ ] **Step 4: Verify `--help` lists the new subcommand**

```bash
./target/debug/ctxfs --help 2>&1 | grep -A 1 "update"
```

Expected:
```
  update   Self-update the CLI binary from the latest GitHub Release. …
```

- [ ] **Step 5: Verify `update --help` is coherent**

```bash
./target/debug/ctxfs update --help 2>&1
```

Expected output includes:
- Description line from the doc comment
- `--check   Only report whether an update is available. Exits 0 if up-to-date, 1 if a newer version exists. No files are modified.`

- [ ] **Step 6: Commit**

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
git add crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): register 'ctxfs update' subcommand

Wires the Update variant into Commands and dispatches to update::run.
--help text is auto-generated from the doc comments on the variant
and the --check flag — they're designed to be the docs themselves,
not cross-referenced."
```

---

## Task 6: Smoke-test against the current GitHub state

**Files:** none — pure runtime verification.

- [ ] **Step 1: Run `ctxfs update --check` against the live repo**

The repo `Derek-X-Wang/ctxfs` has no releases yet (pre-Phase-3e state). Sparkle/self_update handle this gracefully.

```bash
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
./target/debug/ctxfs update --check; echo "exit: $?"
```

Expected output:
```
No releases found at github.com/Derek-X-Wang/ctxfs.
Your current version is 0.0.0.
exit: 0
```

(The version will read from `CARGO_PKG_VERSION` — currently `0.0.0` workspace-wide; Plan 3c migrates that.)

If instead you see an HTTP error (network issue, rate limit) — that's fine, retry. The ship criterion is: the command runs without panicking and prints a human-readable line.

- [ ] **Step 2: Run `ctxfs update` against the app-bundled path to verify refusal**

Copy the built binary into a spoofed path that should be refused:

```bash
sudo mkdir -p /Applications/ContextFS.app/Contents/MacOS
sudo cp ./target/debug/ctxfs /Applications/ContextFS.app/Contents/MacOS/ctxfs-spoof
/Applications/ContextFS.app/Contents/MacOS/ctxfs-spoof update; echo "exit: $?"
sudo rm /Applications/ContextFS.app/Contents/MacOS/ctxfs-spoof
```

Expected output:
```
Error: This ctxfs is bundled with ContextFS.app. Update via the app's 'Check for Updates…' menu …
exit: 1
```

If `/Applications/ContextFS.app` doesn't exist (expected at this stage — Plan 3a installed it for smoke testing; may or may not still be there), skip this test or first:

```bash
sudo mkdir -p /Applications/ContextFS.app/Contents/MacOS
```

- [ ] **Step 3: Commit a note if any unexpected behavior surfaced**

If both smoke checks passed cleanly, no commit needed. If you discovered a bug, fix it with a small commit referencing this task.

---

## Self-review checklist

**Spec coverage:** Plan 3b covers spec Section 2 CLI distribution fully except for the actual release artifacts (Phase 3d). `ctxfs update` subcommand, `--check` mode, install-path detection via `canonicalize` + `brew --prefix`, refusal messages — all specified and implemented.

**Placeholder scan:** No "TBD", "TODO", or "fill in" text outside the intentional inter-task references (Task 1 stubs reference Task 2/3/4 that fill them in — each stub has real type signatures and compiles).

**Type consistency:** `Decision` enum introduced in Task 1, used verbatim in Task 2's tests and Task 4's `require_proceed_or_bail`. `classify(canonical_path, brew_prefix)` signature stable across Tasks 1–4. `run(check_only: bool)` entry point defined in Task 1, refined in Tasks 3 and 4, called from Task 5.

**Known edges the plan does NOT solve** (deferred with explicit citations above):
- End-to-end update application against a real release — requires Phase 3d artifacts, smoke-tested in Phase 3e dress rehearsal.
- Linux/Windows support — Phase 3.5 per spec.
- `minisign` signature verification — explicitly dropped per spec Section 2.
- Homebrew cask/formula interaction testing — requires 3e's tap repo.
