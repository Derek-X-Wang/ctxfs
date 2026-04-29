//! Atomic per-mount usage counters.
//!
//! Keyed by `(source, repo, commit, mount_id)` because a single repo can
//! have multiple concurrent mounts at different commits, and per-mount
//! attribution is needed for `ctxfs status` to surface "top mounts by
//! cost".

use std::sync::atomic::{AtomicU64, Ordering};

/// Prefix used to mark transient "resolving ref" counter buckets created
/// before a concrete commit SHA is known. Format: `"<resolving:{ref}>"`.
/// Filtered out of `ctxfs status` summaries; merged into the real key after
/// ref resolution completes.
pub const PLACEHOLDER_COMMIT_PREFIX: &str = "<resolving:";

/// Key for a per-mount counter bucket. All four dimensions are required:
/// two mounts of the same `(source, repo, commit)` at different mount points
/// hold separate counters keyed by `mount_id`.
#[derive(Debug, Clone, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CounterKey {
    pub source: String,
    pub repo: String,
    pub commit: String,
    pub mount_id: String,
}

/// Atomic counters for one mount. Cheaply incremented on hot paths.
#[derive(Debug, Default)]
pub struct MountCounters {
    rest_calls_total: AtomicU64,
    http_transfers_total: AtomicU64,
    bytes_total: AtomicU64,
    throttle_events: AtomicU64,
    prefetch_hits: AtomicU64,
    prefetch_failures: AtomicU64,
    prefetch_skipped_oversized: AtomicU64,
    tarball_digest_mismatch: AtomicU64,
    tarball_invalid_entries: AtomicU64,
    truncated_tree_fallbacks: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    lfs_pointer_files: AtomicU64,
}

/// Read-only snapshot for serialization in `StatusReportV1`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CounterSnapshot {
    pub rest_calls_total: u64,
    pub http_transfers_total: u64,
    pub bytes_total: u64,
    pub throttle_events: u64,
    pub prefetch_hits: u64,
    pub prefetch_failures: u64,
    pub prefetch_skipped_oversized: u64,
    pub tarball_digest_mismatch: u64,
    pub tarball_invalid_entries: u64,
    pub truncated_tree_fallbacks: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub lfs_pointer_files: u64,
}

impl MountCounters {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_rest_call(&self) {
        let _ = self.rest_calls_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_http_transfer(&self) {
        let _ = self.http_transfers_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_bytes_transferred(&self, bytes: u64) {
        let _ = self.bytes_total.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_throttle_event(&self) {
        let _ = self.throttle_events.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_prefetch_hit(&self) {
        let _ = self.prefetch_hits.fetch_add(1, Ordering::Relaxed);
    }

    /// Batch-add `delta` to the prefetch_hits counter. Useful when a caller
    /// has already aggregated the count (e.g., post-prefetch the inline-map
    /// length) and would otherwise loop a single-event helper.
    pub fn record_prefetch_hits(&self, delta: u64) {
        let _ = self.prefetch_hits.fetch_add(delta, Ordering::Relaxed);
    }

    pub fn record_prefetch_failure(&self) {
        let _ = self.prefetch_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_prefetch_skipped_oversized(&self) {
        let _ = self
            .prefetch_skipped_oversized
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tarball_digest_mismatch(&self) {
        let _ = self.tarball_digest_mismatch.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_tarball_invalid_entries(&self) {
        let _ = self.tarball_invalid_entries.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_truncated_tree_fallback(&self) {
        let _ = self
            .truncated_tree_fallbacks
            .fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cache_hit(&self) {
        let _ = self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cache_miss(&self) {
        let _ = self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_lfs_pointer_file(&self) {
        let _ = self.lfs_pointer_files.fetch_add(1, Ordering::Relaxed);
    }

    /// Fold all counts from `snap` into this bucket by atomic addition.
    ///
    /// Used by [`crate::observability::Observability::merge_and_drop_placeholder`]
    /// to migrate a `<resolving:ref>` placeholder bucket's accumulated counts
    /// into the real commit bucket after ref resolution completes.
    pub fn merge_from_snapshot(&self, snap: &CounterSnapshot) {
        let _ = self
            .rest_calls_total
            .fetch_add(snap.rest_calls_total, Ordering::Relaxed);
        let _ = self
            .http_transfers_total
            .fetch_add(snap.http_transfers_total, Ordering::Relaxed);
        let _ = self
            .bytes_total
            .fetch_add(snap.bytes_total, Ordering::Relaxed);
        let _ = self
            .throttle_events
            .fetch_add(snap.throttle_events, Ordering::Relaxed);
        let _ = self
            .prefetch_hits
            .fetch_add(snap.prefetch_hits, Ordering::Relaxed);
        let _ = self
            .prefetch_failures
            .fetch_add(snap.prefetch_failures, Ordering::Relaxed);
        let _ = self
            .prefetch_skipped_oversized
            .fetch_add(snap.prefetch_skipped_oversized, Ordering::Relaxed);
        let _ = self
            .tarball_digest_mismatch
            .fetch_add(snap.tarball_digest_mismatch, Ordering::Relaxed);
        let _ = self
            .tarball_invalid_entries
            .fetch_add(snap.tarball_invalid_entries, Ordering::Relaxed);
        let _ = self
            .truncated_tree_fallbacks
            .fetch_add(snap.truncated_tree_fallbacks, Ordering::Relaxed);
        let _ = self
            .cache_hits
            .fetch_add(snap.cache_hits, Ordering::Relaxed);
        let _ = self
            .cache_misses
            .fetch_add(snap.cache_misses, Ordering::Relaxed);
        let _ = self
            .lfs_pointer_files
            .fetch_add(snap.lfs_pointer_files, Ordering::Relaxed);
    }

    #[must_use]
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            rest_calls_total: self.rest_calls_total.load(Ordering::Relaxed),
            http_transfers_total: self.http_transfers_total.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            throttle_events: self.throttle_events.load(Ordering::Relaxed),
            prefetch_hits: self.prefetch_hits.load(Ordering::Relaxed),
            prefetch_failures: self.prefetch_failures.load(Ordering::Relaxed),
            prefetch_skipped_oversized: self.prefetch_skipped_oversized.load(Ordering::Relaxed),
            tarball_digest_mismatch: self.tarball_digest_mismatch.load(Ordering::Relaxed),
            tarball_invalid_entries: self.tarball_invalid_entries.load(Ordering::Relaxed),
            truncated_tree_fallbacks: self.truncated_tree_fallbacks.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            lfs_pointer_files: self.lfs_pointer_files.load(Ordering::Relaxed),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CounterKey {
        CounterKey {
            source: "github".to_string(),
            repo: "foo/bar".to_string(),
            commit: "abc123".to_string(),
            mount_id: "mnt-1".to_string(),
        }
    }

    #[test]
    fn snapshot_starts_at_zero() {
        let c = MountCounters::new();
        let s = c.snapshot();
        assert_eq!(s.rest_calls_total, 0);
        assert_eq!(s.bytes_total, 0);
        assert_eq!(s.cache_hits, 0);
    }

    #[test]
    fn rest_calls_increment_is_atomic() {
        let c = MountCounters::new();
        c.record_rest_call();
        c.record_rest_call();
        c.record_rest_call();
        assert_eq!(c.snapshot().rest_calls_total, 3);
    }

    #[test]
    fn bytes_total_accumulates() {
        let c = MountCounters::new();
        c.record_bytes_transferred(100);
        c.record_bytes_transferred(250);
        assert_eq!(c.snapshot().bytes_total, 350);
    }

    #[test]
    fn counter_key_equality() {
        let k1 = key();
        let k2 = key();
        assert_eq!(k1, k2);
    }

    #[test]
    fn record_prefetch_hits_batch_matches_single_event_loop() {
        let single = MountCounters::new();
        for _ in 0..7 {
            single.record_prefetch_hit();
        }
        let batch = MountCounters::new();
        batch.record_prefetch_hits(7);
        assert_eq!(
            single.snapshot().prefetch_hits,
            batch.snapshot().prefetch_hits
        );
        assert_eq!(batch.snapshot().prefetch_hits, 7);
    }

    #[test]
    fn record_prefetch_hits_zero_is_noop() {
        let c = MountCounters::new();
        c.record_prefetch_hits(0);
        assert_eq!(c.snapshot().prefetch_hits, 0);
    }

    #[test]
    fn new_counters_increment_independently() {
        let c = MountCounters::new();
        c.record_prefetch_skipped_oversized();
        c.record_tarball_digest_mismatch();
        c.record_tarball_invalid_entries();
        c.record_tarball_invalid_entries();
        let s = c.snapshot();
        assert_eq!(s.prefetch_skipped_oversized, 1);
        assert_eq!(s.tarball_digest_mismatch, 1);
        assert_eq!(s.tarball_invalid_entries, 2);
    }
}
