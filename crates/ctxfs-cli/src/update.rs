#![allow(dead_code)]

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
///
/// Returns `false` on malformed input — fail safe, no spurious update prompts.
fn is_newer(candidate: &str, current: &str) -> bool {
    // unwrap_or_default() returns false (bool::default) on parse error.
    self_update::version::bump_is_greater(current, candidate).unwrap_or_default()
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
