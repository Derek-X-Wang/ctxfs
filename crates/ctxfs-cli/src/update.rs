//! `ctxfs update` subcommand — self-update the CLI binary from GitHub Releases.
//!
//! Two modes:
//! - `ctxfs update --check` → query GitHub API, print latest version, exit 0
//!   if up-to-date, exit 1 if newer available. For scripting.
//! - `ctxfs update` → if newer available, download, verify SHA-256, atomically
//!   swap the current binary. Refuses if the binary is package-manager-managed.

// Entry point is a stub wired up in subsequent tasks — suppress dead-code lint.
#![allow(dead_code)]

use anyhow::Result;

/// Top-level entry point. `check_only=true` means `--check` was passed.
/// Implementation fleshed out in Tasks 3 + 4.
pub fn run(check_only: bool) -> Result<()> {
    let _ = check_only;
    anyhow::bail!("not yet implemented")
}
