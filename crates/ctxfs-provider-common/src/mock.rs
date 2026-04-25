//! Test fixture: an HTTP-shaped `MockProvider` that records every call
//! it would have made, so workload-replay integration tests can assert
//! exact provider call counts without hitting the real GitHub API.
//!
//! Used cross-crate: M2's tests will pull this in from
//! `ctxfs-provider-git/tests/`, so it lives in `src/`, not `tests/`.

use std::sync::Mutex;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RecordedCall {
    Commit { repo: String, reference: String },
    Tree { repo: String, sha: String, recursive: bool },
    Blob { repo: String, sha: String },
    Tarball { repo: String, sha: String },
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
        self.calls.lock().expect("MockProvider mutex poisoned").push(RecordedCall::Commit {
            repo: repo.into(),
            reference: reference.into(),
        });
    }

    pub fn record_tree(&self, repo: impl Into<String>, sha: impl Into<String>, recursive: bool) {
        self.calls.lock().expect("MockProvider mutex poisoned").push(RecordedCall::Tree {
            repo: repo.into(),
            sha: sha.into(),
            recursive,
        });
    }

    pub fn record_blob(&self, repo: impl Into<String>, sha: impl Into<String>) {
        self.calls.lock().expect("MockProvider mutex poisoned").push(RecordedCall::Blob {
            repo: repo.into(),
            sha: sha.into(),
        });
    }

    pub fn record_tarball(&self, repo: impl Into<String>, sha: impl Into<String>) {
        self.calls.lock().expect("MockProvider mutex poisoned").push(RecordedCall::Tarball {
            repo: repo.into(),
            sha: sha.into(),
        });
    }

    #[must_use]
    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().expect("MockProvider mutex poisoned").clone()
    }

    #[must_use]
    pub fn count(&self) -> usize {
        self.calls.lock().expect("MockProvider mutex poisoned").len()
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
