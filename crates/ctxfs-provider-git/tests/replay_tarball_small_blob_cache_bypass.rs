//! Regression test: tarball-hydrated blobs are not re-fetched via REST.
//!
//! Files ≤ 4096 bytes qualify for BOTH the tarball auto-gate AND the
//! small-blob prefetch path. Without the cache-bypass fix, `prefetch_small_blobs`
//! would issue one REST call per blob even though the tarball already committed
//! them to BlobCache.
//!
//! The mock server registers only three routes (commit + tree + tarball). Any
//! per-blob REST call hits an unregistered route → 404 → `check_rate_limit`
//! fires → `rest_calls_total` increments. So:
//!   - Before fix: `rest_calls_total == 3 + N_FILES` (N_FILES redundant REST calls)
//!   - After fix:  `rest_calls_total == 3`          (cache hits, no REST)
//!
//! Also asserts `prefetch_hits == N_FILES` with no double-counting.

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
use std::sync::Arc;
use tempfile::tempdir;

const N_FILES: usize = 50;
const FILE_SIZE: usize = 1000; // < 4096 → eligible for small-blob prefetch AND tarball
const OWNER: &str = "bypass-owner";
const REPO: &str = "bypass-repo";
const GIT_REF: &str = "main";
const COMMIT_SHA: &str = "ccddee1122334455667788990011ccddee112233";
const TREE_SHA: &str = "tree000000000000000000000000000000000099";
const WRAPPER: &str = "bypass-owner-bypass-repo-ccddee11";

#[tokio::test(flavor = "multi_thread")]
async fn tarball_cache_bypass_prevents_double_rest_calls() {
    // Generate N_FILES small files (< small-blob threshold).
    let mut file_contents: Vec<Vec<u8>> = Vec::with_capacity(N_FILES);
    let mut file_shas: Vec<String> = Vec::with_capacity(N_FILES);
    for i in 0..N_FILES {
        let byte = b'a' + (i % 26) as u8;
        let mut content = vec![byte; FILE_SIZE - 4];
        content.extend_from_slice(&(i as u32).to_le_bytes());
        let sha = git_blob_sha1(&content);
        file_contents.push(content);
        file_shas.push(sha);
    }

    // Tree JSON entries — sizes ≤ 4096 so they're small-blob-eligible.
    let tree_entries: Vec<serde_json::Value> = (0..N_FILES)
        .map(|i| blob_entry(&format!("file_{i:04}.txt"), &file_shas[i], FILE_SIZE as u64))
        .collect();

    // Tarball with all N_FILES files under the codeload wrapper directory.
    let tarball_files: Vec<(String, Vec<u8>)> = (0..N_FILES)
        .map(|i| {
            (
                format!("{WRAPPER}/file_{i:04}.txt"),
                file_contents[i].clone(),
            )
        })
        .collect();
    let tarball_bytes = build_codeload_tarball(WRAPPER, &tarball_files);

    // Only THREE routes: commit, tree, tarball. Any per-blob REST call hits
    // an unregistered route and returns 404 — which is the signal the test
    // uses to detect the absence of the cache bypass.
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
            hit_count: None,
            delay_ms: None,
        },
    ])
    .await;

    let tmp = tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp.path().to_path_buf(), 64 * 1024 * 1024).unwrap());
    let obs = Arc::new(Observability::new());
    let sf = Arc::new(TarballSingleflightMap::new());
    let provider = make_provider(&server, cache.clone(), obs.clone(), sf);

    let source = SourceSpec::parse(&format!("github:{OWNER}/{REPO}@{GIT_REF}")).unwrap();
    let opts = FetchOptions {
        prefetch: PrefetchPolicy::Auto,
        prefetch_threshold_count: 30, // 50 > 30 → tarball auto-gate fires
        prefetch_max_bytes: 256 * 1024 * 1024,
    };

    let snapshot_bytes = provider
        .fetch_snapshot_with_options(&source, &opts)
        .await
        .expect("fetch_snapshot_with_options must succeed");
    assert!(!snapshot_bytes.is_empty());

    // ── Counter assertions ─────────────────────────────────────────────────
    let key = ctxfs_provider_common::counters::CounterKey {
        source: "github".to_string(),
        repo: format!("{OWNER}/{REPO}"),
        commit: COMMIT_SHA.to_string(),
        mount_id: source.id(),
    };
    let snap = obs.counters_for(key).snapshot();

    assert_eq!(
        snap.rest_calls_total, 3,
        "expected exactly 3 REST calls (commit + tree + tarball); \
         got {} — per-blob REST calls indicate the cache bypass is not working",
        snap.rest_calls_total
    );
    assert_eq!(
        snap.prefetch_hits, N_FILES as u64,
        "expected {N_FILES} prefetch_hits (from tarball only, no double-count); got {}",
        snap.prefetch_hits
    );
    assert_eq!(
        snap.prefetch_failures, 0,
        "expected 0 prefetch_failures; got {} (possible cache-miss → 404 path)",
        snap.prefetch_failures
    );

    // ── Cache population ───────────────────────────────────────────────────
    for (i, sha) in file_shas.iter().enumerate() {
        let digest = ctxfs_core::Digest::from_sha256_hex(sha);
        assert!(
            cache.get(&digest).is_some(),
            "blob {i} (sha={sha}) must be in cache after tarball prefetch"
        );
    }
}
