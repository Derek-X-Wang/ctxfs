//! Replay test: when the manifest's estimated bytes exceed `prefetch_max_bytes`,
//! the auto-gate classifies the decision as `LazyOversized` and skips the
//! tarball download entirely.
//!
//! Spec exit criterion:
//! - Mock tarball hit count == 0 (no tarball request sent)
//! - `prefetch_skipped_oversized == 1`

#[path = "common/mod.rs"]
mod common;

use common::{commit_json, make_provider, tree_json, MockRoute, MockServer};
use ctxfs_cache::BlobCache;
use ctxfs_core::source::SourceSpec;
use ctxfs_provider_common::fetcher::{PrefetchPolicy, TarballSingleflightMap};
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_git::FetchOptions;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tempfile::tempdir;

const OWNER: &str = "big-owner";
const REPO: &str = "big-repo";
const GIT_REF: &str = "main";
const COMMIT_SHA: &str = "ff000000000000000000000000000000000000ff";
const TREE_SHA: &str = "tree333333333333333333333333333333333333";

#[tokio::test(flavor = "multi_thread")]
async fn oversized_manifest_skips_tarball_download() {
    // Build a tree with 50 blobs where each reports size = 25 MB.
    // Total estimated_bytes = 50 * 25 MB = 1.25 GB >> prefetch_max_bytes (100 MB).
    const N: usize = 50;
    const REPORTED_SIZE_BYTES: u64 = 25 * 1024 * 1024; // 25 MB per blob

    let tree_entries: Vec<serde_json::Value> = (0..N)
        .map(|i| {
            serde_json::json!({
                "path": format!("big_{i:04}.bin"),
                "mode": "100644",
                "type": "blob",
                "sha": format!("{i:040x}"),
                "size": REPORTED_SIZE_BYTES,
            })
        })
        .collect();

    let tarball_hits = Arc::new(AtomicU64::new(0));

    let server = MockServer::spawn(vec![
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER}/{REPO}/commits"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: commit_json(COMMIT_SHA),
            hit_count: None,
            delay_ms: None,
        },
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER}/{REPO}/git/trees"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: tree_json(TREE_SHA, &tree_entries, false),
            hit_count: None,
            delay_ms: None,
        },
        // Tarball route: must NOT be hit.
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER}/{REPO}/tarball"),
            status: 200,
            headers: vec![],
            body: b"should not be requested".to_vec(),
            hit_count: Some(Arc::clone(&tarball_hits)),
            delay_ms: None,
        },
    ])
    .await;

    let tmp = tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp.path().to_path_buf(), 256 * 1024 * 1024).unwrap());
    let obs = Arc::new(Observability::new());
    let sf = Arc::new(TarballSingleflightMap::new());
    let provider = make_provider(&server, cache, obs.clone(), sf);

    let source = SourceSpec::parse(&format!("github:{OWNER}/{REPO}@{GIT_REF}")).unwrap();
    // prefetch_max_bytes = 100 MB; manifest = 1.25 GB → LazyOversized
    let opts = FetchOptions {
        prefetch: PrefetchPolicy::Auto,
        prefetch_threshold_count: 30, // 50 > 30 → gate tries to proceed
        prefetch_max_bytes: 100 * 1024 * 1024, // 100 MB cap
    };

    let _ = provider
        .fetch_snapshot_with_options(&source, &opts)
        .await
        .expect("fetch_snapshot_with_options must succeed even when tarball is skipped");

    // ── Counter assertions ─────────────────────────────────────────────────
    let key = ctxfs_provider_common::counters::CounterKey {
        source: "github".to_string(),
        repo: format!("{OWNER}/{REPO}"),
        commit: COMMIT_SHA.to_string(),
        mount_id: source.id(),
    };
    let snap = obs.counters_for(key).snapshot();

    assert_eq!(
        snap.prefetch_skipped_oversized, 1,
        "expected prefetch_skipped_oversized == 1 when manifest exceeds byte cap, got {}",
        snap.prefetch_skipped_oversized
    );

    // ── Tarball must not have been requested ───────────────────────────────
    let hits = tarball_hits.load(Ordering::Relaxed);
    assert_eq!(
        hits, 0,
        "tarball endpoint must not be hit when auto-gate decides LazyOversized, got {hits} hits"
    );
}
