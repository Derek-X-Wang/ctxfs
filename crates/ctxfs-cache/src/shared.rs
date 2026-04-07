//! Shared tree cache trait — backend-agnostic interface for distributed tree caching.

use async_trait::async_trait;

/// Trait for shared tree cache backends (Redis, HTTP, etc.).
///
/// Implementations should handle errors gracefully — a failed `get` returns `None`,
/// a failed `put` is silently dropped. The caller falls back to the GitHub API.
#[async_trait]
pub trait SharedTreeCache: Send + Sync + std::fmt::Debug {
    /// Retrieve a cached tree manifest. Returns the raw snapshot JSON bytes.
    async fn get_tree(&self, owner: &str, repo: &str, commit_sha: &str) -> Option<Vec<u8>>;

    /// Store a tree manifest. Errors are logged but not propagated.
    async fn put_tree(&self, owner: &str, repo: &str, commit_sha: &str, data: &[u8]);
}
