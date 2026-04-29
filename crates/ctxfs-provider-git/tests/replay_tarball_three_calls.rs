//! Replay test: cold scan of a repo with 50 blobs triggers the tarball auto-gate.
//!
//! Spec exit criterion: cold mount of a repo with `blob_count >= threshold`
//! AND `estimated_bytes <= prefetch_max_bytes` produces exactly 3 REST calls:
//!   1. `GET /repos/{o}/{r}/commits/{ref}` — resolve ref
//!   2. `GET /repos/{o}/{r}/git/trees/{sha}?recursive=1` — fetch tree
//!   3. `GET /repos/{o}/{r}/tarball/{sha}` — download tarball (200, no redirect)
//!
//! File sizes are set to 5000 bytes (> 4096 threshold) so the small-blob
//! prefetch path is bypassed and all `prefetch_hits` come from the tarball.

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
const FILE_SIZE: usize = 5000; // > 4096 threshold → skips small-blob prefetch
const OWNER: &str = "test-owner";
const REPO: &str = "test-repo";
const GIT_REF: &str = "main";
const COMMIT_SHA: &str = "aabbccdd11223344556677889900aabbccdd1122";
const TREE_SHA: &str = "tree000000000000000000000000000000000001";
const WRAPPER: &str = "test-owner-test-repo-aabbccdd";

#[tokio::test(flavor = "multi_thread")]
async fn tarball_prefetch_produces_three_rest_calls() {
    // Generate N_FILES files with unique content (size > small-blob threshold).
    let mut file_contents: Vec<Vec<u8>> = Vec::with_capacity(N_FILES);
    let mut file_shas: Vec<String> = Vec::with_capacity(N_FILES);
    for i in 0..N_FILES {
        // Each file is FILE_SIZE bytes; content is a repeated byte to keep it unique.
        let byte = b'a' + (i % 26) as u8;
        // To ensure different files have different digests, mix in the index.
        let mut content = vec![byte; FILE_SIZE - 4];
        content.extend_from_slice(&(i as u32).to_le_bytes());
        let sha = git_blob_sha1(&content);
        file_contents.push(content);
        file_shas.push(sha);
    }

    // Build tree JSON entries.
    let tree_entries: Vec<serde_json::Value> = (0..N_FILES)
        .map(|i| blob_entry(&format!("file_{i:04}.txt"), &file_shas[i], FILE_SIZE as u64))
        .collect();

    // Build the codeload tarball (with wrapper dir).
    let tarball_files: Vec<(String, Vec<u8>)> = (0..N_FILES)
        .map(|i| {
            (
                format!("{WRAPPER}/file_{i:04}.txt"),
                file_contents[i].clone(),
            )
        })
        .collect();
    let tarball_bytes = build_codeload_tarball(WRAPPER, &tarball_files);

    // Spin up the mock server with 3 routes.
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
        prefetch_threshold_count: 30, // 50 > 30 → auto-gate fires
        prefetch_max_bytes: 256 * 1024 * 1024,
    };

    let snapshot_bytes = provider
        .fetch_snapshot_with_options(&source, &opts)
        .await
        .expect("fetch_snapshot_with_options must succeed");
    assert!(!snapshot_bytes.is_empty(), "snapshot must be non-empty");

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
        "expected exactly 3 REST calls (commit + tree + tarball), got {}",
        snap.rest_calls_total
    );
    assert_eq!(
        snap.prefetch_hits, N_FILES as u64,
        "expected {N_FILES} prefetch_hits (one per committed blob), got {}",
        snap.prefetch_hits
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
