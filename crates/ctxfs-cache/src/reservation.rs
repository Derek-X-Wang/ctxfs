//! Per-repo cache reservation primitives (B5).
//!
//! - [`RepoKey`] `{ host, owner, repo }` identifies a logical repo
//!   independent of commit. Two mounts of the same repo at different
//!   commits share one reservation.
//! - [`ReservationEntry`] tracks reserved bytes, whether the value was
//!   explicitly user-supplied (never touched by default rebalance), and a
//!   refcount of active mounts.
//! - [`MountCacheView`] is a thin handle over `(Arc<BlobCache>, RepoKey)`
//!   used by providers; the *primary* ownership signal is
//!   `BlobCache::register_mount(key, reservation_bytes, manifest_digests)`,
//!   which records ownership for every blob the manifest names. The
//!   view's `put`/`record_ownership_after_finalize` cover late additions
//!   (truncated-tree fallbacks, fresh fetches outside the snapshot path).

use crate::BlobCache;
use ctxfs_core::error::CtxfsError;
use ctxfs_core::Digest;
use std::sync::Arc;

/// Identifies a logical repo across all its mounts and commits.
///
/// Two concurrent mounts of the same repo at different commits share one
/// `RepoKey` and therefore one reservation — the reservation budget covers
/// the repo's total working-set footprint, not a single commit's blobs.
#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RepoKey {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

impl RepoKey {
    #[must_use]
    pub fn new(host: impl Into<String>, owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            owner: owner.into(),
            repo: repo.into(),
        }
    }
}

/// Per-repo reservation budget held inside [`CacheState`](crate::CacheState).
///
/// `pub(crate)` because only `BlobCache` internals need to construct or
/// inspect `ReservationEntry` directly.
#[derive(Debug, Clone)]
pub(crate) struct ReservationEntry {
    /// Currently-effective reservation in bytes. T3b's default-rebalance
    /// logic adjusts this for non-explicit entries on register/unregister.
    pub(crate) reserved_bytes: u64,
    /// `true` iff the user supplied `--cache-reservation` for this mount;
    /// such entries are **never** touched by default rebalance.
    ///
    /// Not read in T3a; used by T3b's rebalance logic.
    #[allow(dead_code)]
    pub(crate) is_explicit_override: bool,
    /// Number of currently active mounts for this [`RepoKey`]. Same repo at
    /// two commits means `refcount = 2`; only on `refcount → 0` does the
    /// entry disappear from the reservations table.
    ///
    /// Not read in T3a; used by T3b's register/unregister logic.
    #[allow(dead_code)]
    pub(crate) refcount: u32,
}

/// A thin handle over `(Arc<BlobCache>, RepoKey)` used by providers.
///
/// The *primary* ownership signal is `BlobCache::register_mount(key,
/// reservation_bytes, manifest_digests)`, called by the daemon after the
/// snapshot is built. This view's [`put`](MountCacheView::put) and
/// [`record_ownership_after_finalize`](MountCacheView::record_ownership_after_finalize)
/// cover late additions (truncated-tree fallbacks, fresh fetches that arrive
/// after `register_mount` completes).
#[derive(Clone)]
pub struct MountCacheView {
    cache: Arc<BlobCache>,
    repo_key: RepoKey,
}

impl std::fmt::Debug for MountCacheView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MountCacheView")
            .field("cache", &self.cache)
            .field("repo_key", &self.repo_key)
            .finish()
    }
}

impl MountCacheView {
    #[must_use]
    pub fn new(cache: Arc<BlobCache>, repo_key: RepoKey) -> Self {
        Self { cache, repo_key }
    }

    #[must_use]
    pub fn cache(&self) -> &Arc<BlobCache> {
        &self.cache
    }

    #[must_use]
    pub fn repo_key(&self) -> &RepoKey {
        &self.repo_key
    }

    /// Put a blob and record ownership for this view's [`RepoKey`].
    ///
    /// Used for late additions outside the snapshot's manifest (e.g.,
    /// truncated-tree fallback fetches discovered after `register_mount`).
    pub fn put(&self, digest: &Digest, data: &[u8]) -> Result<(), CtxfsError> {
        self.cache.put_for(&self.repo_key, digest, data)
    }

    /// Record ownership for an already-finalized blob without writing data.
    ///
    /// Called after the streaming tarball commit path
    /// (`BlobTempWriter::finalize`) for blobs that don't go through
    /// [`put`](MountCacheView::put). Idempotent — calling twice for the same
    /// `(key, digest)` is safe.
    pub fn record_ownership_after_finalize(&self, digest: &Digest) {
        self.cache.add_owner(&self.repo_key, &digest.hex);
    }

    #[must_use]
    pub fn get(&self, digest: &Digest) -> Option<Vec<u8>> {
        self.cache.get(digest)
    }

    #[must_use]
    pub fn contains(&self, digest: &Digest) -> bool {
        self.cache.contains(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxfs_core::Digest;
    use std::sync::Arc;

    fn key(repo: &str) -> RepoKey {
        RepoKey::new("api.github.com", "owner", repo)
    }

    #[test]
    fn repo_key_eq_and_hash() {
        let k1 = key("foo");
        let k2 = key("foo");
        let k3 = key("bar");
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn repo_key_ord() {
        // Ord is derived; just ensure it compiles and is consistent.
        let k1 = RepoKey::new("gh.com", "a", "repo");
        let k2 = RepoKey::new("gh.com", "b", "repo");
        assert!(k1 < k2);
    }

    #[test]
    fn add_owner_and_put_for_record_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 1 << 20).unwrap());
        let view_a = MountCacheView::new(cache.clone(), key("repo-a"));
        let view_b = MountCacheView::new(cache.clone(), key("repo-b"));

        let d1 = Digest::from_sha1_hex("aaaa000000000000000000000000000000000000");
        let d2 = Digest::from_sha1_hex("bbbb000000000000000000000000000000000000");

        // put_for records ownership.
        view_a.put(&d1, b"shared").unwrap();
        view_b.put(&d1, b"shared").unwrap(); // same blob, different mount
        view_a.put(&d2, b"a-only").unwrap();

        // record_ownership_after_finalize is idempotent.
        view_b.record_ownership_after_finalize(&d2);
        view_b.record_ownership_after_finalize(&d2); // 2nd call is a no-op semantically

        // repo-a owns d1 ("shared", 6 bytes) + d2 ("a-only", 6 bytes) = 12.
        assert_eq!(cache.working_set_bytes(&key("repo-a")), 6 + 6);
        // repo-b owns d1 (6 bytes) + d2 (adopted via record_ownership_after_finalize, 6 bytes) = 12.
        assert_eq!(cache.working_set_bytes(&key("repo-b")), 6 + 6);
    }

    #[test]
    fn working_set_bytes_sums_owned_blobs_only() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 1 << 20).unwrap());
        let view_a = MountCacheView::new(cache.clone(), key("repo-a"));
        let view_b = MountCacheView::new(cache.clone(), key("repo-b"));

        let d1 = Digest::from_sha1_hex("aaaa000000000000000000000000000000000000");
        let d2 = Digest::from_sha1_hex("bbbb000000000000000000000000000000000000");

        view_a.put(&d1, &[0u8; 100]).unwrap();
        view_b.put(&d2, &[0u8; 200]).unwrap();

        assert_eq!(cache.working_set_bytes(&key("repo-a")), 100);
        assert_eq!(cache.working_set_bytes(&key("repo-b")), 200);
    }

    #[test]
    fn add_owner_pre_claims_uncached_blob() {
        // Prove the manifest-time-ownership path: add_owner on a hex that
        // has no cache entry yet. Working-set is 0 (no cached bytes yet);
        // when a put later fetches that blob, ownership is already in
        // place and working_set_bytes reflects the new size.
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 1 << 20).unwrap());
        let k = key("repo-a");
        let d = Digest::from_sha1_hex("cccc000000000000000000000000000000000000");

        cache.add_owner(&k, &d.hex);
        assert_eq!(cache.working_set_bytes(&k), 0); // no bytes cached yet

        // Subsequent plain put — ownership already pre-claimed → working set
        // is updated once the blob is in the cache.
        cache.put(&d, &[1u8; 50]).unwrap();
        assert_eq!(cache.working_set_bytes(&k), 50);
    }
}
