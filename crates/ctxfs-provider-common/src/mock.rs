//! Test fixtures for provider-level unit and integration testing.
//!
//! - [`MockProvider`]: HTTP-shaped recorder that tracks call counts without
//!   hitting a real GitHub API. Used by workload-replay integration tests.
//! - [`MockContentFetcher`]: trivial [`crate::fetcher::ContentFetcher`] impl
//!   that returns canned bytes. Proves the trait is implementable from outside
//!   `provider-git`.
//!
//! Lives in `src/` (not `tests/`) so cross-crate test suites can import it.

use crate::counters::CounterKey;
use crate::fetcher::{
    default_cost_estimate, ContentFetcher, ContentRequest, CostEstimate, FetchBatchContext,
    FetchMode,
};
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RecordedCall {
    Commit {
        repo: String,
        reference: String,
    },
    Tree {
        repo: String,
        sha: String,
        recursive: bool,
    },
    Blob {
        repo: String,
        sha: String,
    },
    Tarball {
        repo: String,
        sha: String,
    },
}

#[derive(Debug, Default)]
pub struct MockProvider {
    calls: Mutex<Vec<RecordedCall>>,
}

impl MockProvider {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_commit(&self, repo: impl Into<String>, reference: impl Into<String>) {
        self.calls
            .lock()
            .expect("MockProvider mutex poisoned")
            .push(RecordedCall::Commit {
                repo: repo.into(),
                reference: reference.into(),
            });
    }

    pub fn record_tree(&self, repo: impl Into<String>, sha: impl Into<String>, recursive: bool) {
        self.calls
            .lock()
            .expect("MockProvider mutex poisoned")
            .push(RecordedCall::Tree {
                repo: repo.into(),
                sha: sha.into(),
                recursive,
            });
    }

    pub fn record_blob(&self, repo: impl Into<String>, sha: impl Into<String>) {
        self.calls
            .lock()
            .expect("MockProvider mutex poisoned")
            .push(RecordedCall::Blob {
                repo: repo.into(),
                sha: sha.into(),
            });
    }

    pub fn record_tarball(&self, repo: impl Into<String>, sha: impl Into<String>) {
        self.calls
            .lock()
            .expect("MockProvider mutex poisoned")
            .push(RecordedCall::Tarball {
                repo: repo.into(),
                sha: sha.into(),
            });
    }

    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls
            .lock()
            .expect("MockProvider mutex poisoned")
            .clone()
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.calls
            .lock()
            .expect("MockProvider mutex poisoned")
            .len()
    }
}

/// Trivial [`ContentFetcher`] impl for tests. Returns canned bytes for paths
/// in `canned_bytes`; missing paths produce an empty-map entry per the trait's
/// best-effort contract. Proves the trait is implementable from outside
/// `provider-git`.
#[derive(Debug, Default)]
pub struct MockContentFetcher {
    /// Pre-seeded bytes keyed by mount-relative path.
    pub canned_bytes: HashMap<PathBuf, Vec<u8>>,
}

#[async_trait::async_trait]
impl ContentFetcher for MockContentFetcher {
    fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate {
        default_cost_estimate(requests)
    }

    async fn fetch_batch(
        &self,
        _ctx: &FetchBatchContext,
        requests: &[ContentRequest],
        _mode: FetchMode,
        _counter_key: Option<CounterKey>,
    ) -> Result<HashMap<PathBuf, Vec<u8>>, ctxfs_core::error::CtxfsError> {
        Ok(requests
            .iter()
            .filter_map(|r| {
                self.canned_bytes
                    .get(&r.path)
                    .map(|b| (r.path.clone(), b.clone()))
            })
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_commit_tree_blob_tarball_in_order() {
        let m = MockProvider::new();
        m.record_commit("foo/bar", "main");
        m.record_tree("foo/bar", "abc", true);
        m.record_blob("foo/bar", "blob1");
        m.record_tarball("foo/bar", "abc");
        assert_eq!(m.count(), 4);
        let calls = m.calls();
        assert!(matches!(&calls[0], RecordedCall::Commit { .. }));
        assert!(matches!(&calls[3], RecordedCall::Tarball { .. }));
    }

    #[test]
    fn count_starts_at_zero() {
        let m = MockProvider::new();
        assert_eq!(m.count(), 0);
    }
}
