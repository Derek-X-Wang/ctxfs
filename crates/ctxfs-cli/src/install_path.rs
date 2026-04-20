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
