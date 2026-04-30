//! Proves the `ContentFetcher` trait can be implemented from a crate that does
//! NOT depend on `provider-git`. Spec exit-criterion (M4): a hypothetical
//! second provider can plug in without touching `provider-git`.
//!
//! All assertions are on `MockContentFetcher` — a trivial impl in
//! `ctxfs-provider-common::mock`.

use ctxfs_core::source::SourceSpec;
use ctxfs_provider_common::fetcher::{
    ContentFetcher, ContentKind, ContentRequest, FetchBatchContext, FetchMode,
};
use ctxfs_provider_common::mock::MockContentFetcher;
use std::path::PathBuf;

fn test_ctx() -> FetchBatchContext {
    FetchBatchContext {
        source: SourceSpec::parse("github:owner/repo@main").unwrap(),
        resolved_revision: "abcdef0123456789abcdef0123456789abcdef01".to_string(),
    }
}

/// Mock fetcher returns canned bytes for paths it knows about.
#[tokio::test]
async fn mock_fetcher_returns_canned_bytes() {
    let mut canned = std::collections::HashMap::new();
    let _ = canned.insert(PathBuf::from("a.rs"), b"contents".to_vec());

    let fetcher = MockContentFetcher {
        canned_bytes: canned,
    };
    let requests = vec![ContentRequest {
        path: PathBuf::from("a.rs"),
        digest: None,
        size: Some(8),
        kind: ContentKind::File,
    }];

    let bytes_map = fetcher
        .fetch_batch(&test_ctx(), &requests, FetchMode::BulkPrefetch, None)
        .await
        .unwrap();
    assert_eq!(
        bytes_map.get(&PathBuf::from("a.rs")),
        Some(&b"contents".to_vec())
    );
}

/// Missing paths produce an empty map — not an error. Exercises the
/// trait's best-effort return contract.
#[tokio::test]
async fn mock_fetcher_missing_path_returns_empty_map_not_error() {
    let fetcher = MockContentFetcher {
        canned_bytes: Default::default(),
    };
    let requests = vec![ContentRequest {
        path: PathBuf::from("absent.rs"),
        digest: None,
        size: Some(0),
        kind: ContentKind::File,
    }];

    let bytes_map = fetcher
        .fetch_batch(&test_ctx(), &requests, FetchMode::BulkPrefetch, None)
        .await
        .unwrap();
    assert!(
        bytes_map.is_empty(),
        "missing paths produce empty map, not error"
    );
}

/// Forced mode is accepted (not rejected). Only Lazy should be rejected
/// by real providers; mock accepts any non-error mode.
#[tokio::test]
async fn mock_fetcher_accepts_forced_mode() {
    let mut canned = std::collections::HashMap::new();
    let _ = canned.insert(PathBuf::from("lib.rs"), b"fn main() {}".to_vec());

    let fetcher = MockContentFetcher {
        canned_bytes: canned,
    };
    let requests = vec![ContentRequest {
        path: PathBuf::from("lib.rs"),
        digest: None,
        size: Some(12),
        kind: ContentKind::File,
    }];

    let result = fetcher
        .fetch_batch(&test_ctx(), &requests, FetchMode::Forced, None)
        .await;
    assert!(result.is_ok(), "mock fetcher must accept Forced mode");
}

/// `estimate_cost` sums sizes when all requests carry known sizes.
#[test]
fn mock_fetcher_estimate_cost_sums_sizes() {
    let fetcher = MockContentFetcher {
        canned_bytes: Default::default(),
    };
    let requests = vec![
        ContentRequest {
            path: PathBuf::from("a.rs"),
            digest: None,
            size: Some(100),
            kind: ContentKind::File,
        },
        ContentRequest {
            path: PathBuf::from("b.rs"),
            digest: None,
            size: Some(200),
            kind: ContentKind::File,
        },
    ];
    let est = fetcher.estimate_cost(&requests);
    assert_eq!(est.request_count, 2);
    assert_eq!(est.total_bytes, Some(300));
}

/// `estimate_cost` returns `None` for `total_bytes` when any size is unknown.
#[test]
fn mock_fetcher_estimate_cost_none_when_any_size_unknown() {
    let fetcher = MockContentFetcher {
        canned_bytes: Default::default(),
    };
    let requests = vec![
        ContentRequest {
            path: PathBuf::from("a.rs"),
            digest: None,
            size: Some(100),
            kind: ContentKind::File,
        },
        ContentRequest {
            path: PathBuf::from("b.rs"),
            digest: None,
            size: None,
            kind: ContentKind::File,
        },
    ];
    let est = fetcher.estimate_cost(&requests);
    assert_eq!(est.total_bytes, None);
    assert_eq!(est.request_count, 2);
}
