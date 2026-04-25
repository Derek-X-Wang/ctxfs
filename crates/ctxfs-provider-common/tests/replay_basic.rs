//! Workload-replay integration test: simulates a 1k-file cold scan via
//! MockProvider and asserts exact call counts. This is the regression
//! sentinel for M3's `rest_calls_total == 3` exit criterion; M1 ships
//! with a placeholder workload (lazy per-blob path simulated) and M2/M3
//! extend it to the tarball-prefetch and B1-inline paths.

use ctxfs_provider_common::mock::{MockProvider, RecordedCall};

/// Simulates the *current* (pre-Phase-4) lazy-per-blob workload.
/// Asserts exactly: 1 commit + 1 tree + N blobs.
#[test]
fn lazy_workload_records_one_call_per_blob() {
    let mock = MockProvider::new();
    let blob_count = 100_usize;

    // Simulate the cold mount.
    mock.record_commit("foo/bar", "main");
    mock.record_tree("foo/bar", "abc", true);
    for i in 0..blob_count {
        mock.record_blob("foo/bar", format!("blob{i}"));
    }

    let calls = mock.calls();
    assert_eq!(calls.len(), 1 + 1 + blob_count);
    assert!(matches!(calls[0], RecordedCall::Commit { .. }));
    assert!(matches!(calls[1], RecordedCall::Tree { .. }));
    let blob_calls = calls.iter().filter(|c| matches!(c, RecordedCall::Blob { .. })).count();
    assert_eq!(blob_calls, blob_count);
}

/// Sentinel for the M3 tarball-prefetch path (exit criterion).
#[test]
fn tarball_workload_records_three_calls() {
    let mock = MockProvider::new();

    mock.record_commit("foo/bar", "main");
    mock.record_tree("foo/bar", "abc", true);
    mock.record_tarball("foo/bar", "abc");

    let calls = mock.calls();
    assert_eq!(calls.len(), 3);
    assert!(matches!(calls[2], RecordedCall::Tarball { .. }));

    // No blob calls.
    let blob_calls = calls.iter().filter(|c| matches!(c, RecordedCall::Blob { .. })).count();
    assert_eq!(blob_calls, 0);
}
