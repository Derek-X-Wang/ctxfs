//! `ctxfs update` subcommand — self-update the CLI binary from GitHub Releases.
//!
//! Two modes:
//! - `ctxfs update --check` → query GitHub API, print latest version, exit 0
//!   if up-to-date, exit 1 if newer available. For scripting.
//! - `ctxfs update` → if newer available, download, verify SHA-256, atomically
//!   swap the current binary. Refuses if the binary is package-manager-managed.

use anyhow::{bail, Context, Result};
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
        println!(
            "Updated to {}. Restart any open ctxfs shell sessions.",
            status.version()
        );
    } else {
        println!(
            "Already on the latest version ({}).",
            cargo_crate_version!()
        );
    }
    Ok(())
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

/// Resolve the Rust target-triple string for the current binary. Used by
/// `self_update` to pick the right platform-specific archive out of the
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
        Decision::RefuseHomebrewFormula => bail!("Run 'brew upgrade contextfs' instead."),
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
}
