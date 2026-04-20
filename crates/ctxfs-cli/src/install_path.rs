//! Resolve the running `ctxfs` binary's canonical path and classify whether
//! it's managed by a package manager (Homebrew formula, Homebrew cask, or
//! direct DMG install). Consulted by `ctxfs update` before touching the
//! binary so we never desync a package manager's view of its own files.

// Interfaces are stubs used by subsequent tasks — suppress dead-code lints.
#![allow(dead_code)]

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

/// Resolve the running binary's canonical path. Uses `std::env::current_exe`
/// followed by `canonicalize` — the former gives the invocation path, the
/// latter resolves every symlink (e.g., `$HOMEBREW_PREFIX/bin/ctxfs` →
/// `$HOMEBREW_PREFIX/Cellar/contextfs/<ver>/bin/ctxfs`).
pub fn current_canonical_exe() -> std::io::Result<PathBuf> {
    std::env::current_exe()?.canonicalize()
}

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
