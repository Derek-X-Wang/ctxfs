//! Replay test: two concurrent `fetch_snapshot_with_options` calls for the
//! same `(owner, repo, commit)` share one tarball download via the singleflight
//! registry.
//!
//! Spec exit criterion:
//! - Mock tarball hit count == 1 (downloaded once, not twice)
//! - Both calls return `Ok`
//!
//! A 100 ms delay is added to the tarball response so both concurrent callers
//! are guaranteed to be in-flight when the first tarball request arrives at the
//! mock, ensuring the singleflight claim is exercised rather than the cache
//! pre-check path.

#[path = "common/mod.rs"]
mod common;

use common::{
    blob_entry, build_codeload_tarball, commit_json, git_blob_sha1, make_provider, tree_json,
    MockRoute, MockServer,
};
use ctxfs_cache::BlobCache;
use ctxfs_core::source::SourceSpec;
use ctxfs_provider_common::fetcher::{PrefetchPolicy, TarballSingleflightMap};
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_git::FetchOptions;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tempfile::tempdir;

const OWNER: &str = "sf-owner";
const REPO: &str = "sf-repo";
const GIT_REF: &str = "main";
const COMMIT_SHA: &str = "dd00000000000000000000000000000000000dd1";
const TREE_SHA: &str = "tree111111111111111111111111111111111111";
const WRAPPER: &str = "sf-owner-sf-repo-dd000000";

#[tokio::test(flavor = "multi_thread")]
async fn two_concurrent_mounts_share_one_tarball_download() {
    // Two files, each 5001 bytes (> small-blob threshold).
    let contents: Vec<Vec<u8>> = (0..2u8)
        .map(|i| {
            let mut v = vec![b'x'; 5000];
            v.push(i);
            v
        })
        .collect();
    let shas: Vec<String> = contents.iter().map(|c| git_blob_sha1(c)).collect();

    let tree_entries: Vec<serde_json::Value> = (0..2)
        .map(|i| blob_entry(&format!("f{i}.bin"), &shas[i], contents[i].len() as u64))
        .collect();
    let tarball_files: Vec<(String, Vec<u8>)> = (0..2)
        .map(|i| (format!("{WRAPPER}/f{i}.bin"), contents[i].clone()))
        .collect();
    let tarball_bytes = build_codeload_tarball(WRAPPER, &tarball_files);

    // Tarball hit counter.
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
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER}/{REPO}/tarball"),
            status: 200,
            headers: vec![
                ("Content-Type", "application/gzip".to_string()),
                ("Content-Encoding", "gzip".to_string()),
            ],
            body: tarball_bytes,
            hit_count: Some(Arc::clone(&tarball_hits)),
            // 100 ms delay: ensures both callers reach dispatch_fetch_policy
            // and attempt to claim the singleflight slot before the tarball
            // response arrives.
            delay_ms: Some(100),
        },
    ])
    .await;

    let tmp = tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp.path().to_path_buf(), 64 * 1024 * 1024).unwrap());

    // Shared singleflight registry — the key property under test.
    let sf = Arc::new(TarballSingleflightMap::new());

    let obs1 = Arc::new(Observability::new());
    let obs2 = Arc::new(Observability::new());
    let provider1 = make_provider(&server, cache.clone(), obs1, sf.clone());
    let provider2 = make_provider(&server, cache.clone(), obs2, sf.clone());

    let source = SourceSpec::parse(&format!("github:{OWNER}/{REPO}@{GIT_REF}")).unwrap();
    let opts = FetchOptions {
        prefetch: PrefetchPolicy::Auto,
        prefetch_threshold_count: 1, // threshold=1 → auto-gate fires for 2 blobs
        prefetch_max_bytes: 256 * 1024 * 1024,
    };

    // Run both concurrently.
    let source2 = source.clone();
    let opts2 = opts.clone();
    let (r1, r2) = tokio::join!(
        provider1.fetch_snapshot_with_options(&source, &opts),
        provider2.fetch_snapshot_with_options(&source2, &opts2),
    );

    let _ = r1.expect("provider1 fetch must succeed");
    let _ = r2.expect("provider2 fetch must succeed");

    // ── Singleflight assertion ─────────────────────────────────────────────
    let hits = tarball_hits.load(Ordering::Relaxed);
    assert_eq!(
        hits, 1,
        "singleflight must deduplicate concurrent tarball downloads: expected 1 hit, got {hits}"
    );
}
