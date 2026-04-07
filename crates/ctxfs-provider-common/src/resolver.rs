//! Registry resolver types shared across npm, `PyPI`, and crates.io providers.

use async_trait::async_trait;
use ctxfs_core::error::CtxfsError;

/// The result of resolving a package to its GitHub source repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    pub owner: String,
    pub repo: String,
    pub git_ref: String,
    pub subpath: Option<String>,
}

/// Trait implemented by each registry resolver (npm, `PyPI`, crates.io).
#[async_trait]
pub trait RegistryResolver: Send + Sync {
    /// Resolve a package name + version to a GitHub source repo.
    /// Returns `Err(CtxfsError::NoSourceRepo { .. })` if no GitHub repo is found.
    async fn resolve(&self, name: &str, version: &str) -> Result<ResolvedSource, CtxfsError>;

    /// Resolve "latest" to an exact version string.
    async fn resolve_latest(&self, name: &str) -> Result<String, CtxfsError>;
}
