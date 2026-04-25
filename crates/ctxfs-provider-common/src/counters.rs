//! Atomic per-mount usage counters.
//!
//! Keyed by `(source, repo, commit, mount_id)` because a single repo can
//! have multiple concurrent mounts at different commits, and per-mount
//! attribution is needed for `ctxfs status` to surface "top mounts by
//! cost".

use std::sync::atomic::{AtomicU64, Ordering};

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

    pub fn record_prefetch_failure(&self) {
        let _ = self.prefetch_failures.fetch_add(1, Ordering::Relaxed);
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

    #[must_use]
    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            rest_calls_total: self.rest_calls_total.load(Ordering::Relaxed),
            http_transfers_total: self.http_transfers_total.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            throttle_events: self.throttle_events.load(Ordering::Relaxed),
            prefetch_hits: self.prefetch_hits.load(Ordering::Relaxed),
            prefetch_failures: self.prefetch_failures.load(Ordering::Relaxed),
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
}
