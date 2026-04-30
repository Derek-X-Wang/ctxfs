//! `ProviderContext` centralizes the daemon-owned `Arc`s and configuration that
//! every Phase-4-shaped provider needs, so `GitHubProvider::new` shrinks to
//! `(token, ctx)`.
//!
//! Lives in `ctxfs-provider-git` (not `provider-common`) because
//! `provider-common` cannot depend on `ctxfs-cache` without inverting the
//! existing dep direction (`cache → provider-common`). Future native-CDN
//! providers (npm/PyPI/crates.io) get their own context type adapted to
//! their auth/cache/network needs; the shared structural call (duplicate,
//! extract to a new crate, or migrate `ctxfs-cache` under `provider-common`)
//! is best made with a second concrete consumer in hand — Phase 6 work.

use ctxfs_cache::reservation::MountCacheView;
use ctxfs_cache::{BlobCache, SharedTreeCache, TreeCache};
use ctxfs_provider_common::fetcher::TarballSingleflightMap;
use ctxfs_provider_common::observability::Observability;
use std::sync::Arc;

/// Daemon-owned context bundled into a single value so `GitHubProvider::new`
/// shrinks from 7 arguments to 2 (`token`, `ctx`).
///
/// `Clone` clones all `Arc`s by reference count — the underlying resources are
/// shared, not copied.
#[derive(Clone)]
pub struct ProviderContext {
    /// API host (e.g. `api.github.com`, or a `http://127.0.0.1:PORT` test
    /// override). Used for both REST URL composition and codeload-host
    /// derivation.
    pub api_host: String,
    /// Daemon-side observability registry (gauges + per-mount counters).
    pub observability: Arc<Observability>,
    /// Blob cache (content-addressable).
    pub cache: Arc<BlobCache>,
    /// Local tree cache (per-commit manifests).
    pub tree_cache: Option<Arc<TreeCache>>,
    /// Optional shared tree cache (Redis-backed cross-process).
    pub shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    /// Singleflight registry for in-flight tarball prefetches.
    pub singleflight: Arc<TarballSingleflightMap>,
    /// Per-mount cache view that pins a [`RepoKey`](ctxfs_cache::RepoKey) for
    /// B5 ownership tracking. `None` for paths that don't need ownership
    /// tracking (NFS test helpers, FSKit shared-cache paths). Wired up by
    /// the daemon in T3b's `prepare_mount`.
    pub mount_cache: Option<Arc<MountCacheView>>,
}

impl std::fmt::Debug for ProviderContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderContext")
            .field("api_host", &self.api_host)
            .field("observability", &"<Arc<Observability>>")
            .field("cache", &"<Arc<BlobCache>>")
            .field("tree_cache", &self.tree_cache.is_some())
            .field("shared_tree_cache", &self.shared_tree_cache.is_some())
            .field("singleflight_len", &self.singleflight.len())
            .field("mount_cache", &self.mount_cache.is_some())
            .finish()
    }
}

impl ProviderContext {
    /// Minimal context for callers that need only a host and a cache. Tree
    /// caching and singleflight are disabled (each call gets a fresh registry).
    ///
    /// Intended for integration tests and one-off CLI callers. Daemon code uses
    /// the full struct literal so all shared resources are wired up explicitly.
    #[must_use]
    pub fn minimal(api_host: impl Into<String>, cache: Arc<BlobCache>) -> Self {
        Self {
            api_host: api_host.into(),
            observability: Arc::new(Observability::new()),
            cache,
            tree_cache: None,
            shared_tree_cache: None,
            singleflight: Arc::new(TarballSingleflightMap::new()),
            mount_cache: None,
        }
    }
}

/// Minimal [`ProviderContext`] for unit tests. Shared across `context`,
/// `github`, and any other in-crate test module that needs a provider context
/// without making real network calls.
///
/// Returns `(ProviderContext, TempDir)` — the caller must hold `TempDir` for
/// the lifetime of the provider; dropping it deletes the cache directory.
///
/// Exposed as `pub(crate)` so sibling modules (e.g. `github::tests`) can call
/// `crate::context::make_test_provider_context()` instead of duplicating the
/// construction.
#[cfg(test)]
pub(crate) fn make_test_provider_context() -> (ProviderContext, tempfile::TempDir) {
    use std::sync::Arc;
    let dir = tempfile::tempdir().expect("tempdir");
    let cache =
        Arc::new(BlobCache::new(dir.path().to_path_buf(), 1024 * 1024).expect("BlobCache::new"));
    // mount_cache is None for test contexts — ownership tracking and
    // reservation enforcement require a daemon-provided RepoKey (T3b).
    let ctx = ProviderContext::minimal("api.github.com", cache);
    (ctx, dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    #[test]
    fn provider_context_clones_arcs_correctly() {
        let (ctx, _tmp) = make_test_provider_context();
        let cloned = ctx.clone();
        assert!(Arc::ptr_eq(&ctx.cache, &cloned.cache));
        assert!(Arc::ptr_eq(&ctx.observability, &cloned.observability));
        assert!(Arc::ptr_eq(&ctx.singleflight, &cloned.singleflight));
    }

    #[test]
    fn provider_context_debug_redacts_arc_contents() {
        let (ctx, _tmp) = make_test_provider_context();
        let dbg = format!("{ctx:?}");
        assert!(dbg.contains("api_host"));
        assert!(dbg.contains("<Arc<Observability>>"));
        assert!(dbg.contains("<Arc<BlobCache>>"));
        assert!(dbg.contains("singleflight_len"));
        // Sanity: no token stored on ProviderContext (tokens live on GitHubProvider)
        assert!(
            !dbg.contains("token"),
            "token must not appear in ProviderContext debug output"
        );
    }
}
