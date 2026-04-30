//! Regression test: B5 — `snapshot_blob_hexes` non-empty on tree-cache hit.
//!
//! Confirms that when a **new** provider instance fetches a snapshot that is
//! already in the local `TreeCache`, `fetch_snapshot_inner` still populates
//! `sha_to_path` by walking the cached directory tree from `BlobCache`.
//! Without the fix, `sha_to_path` stays empty on tree-cache hits, causing
//! `snapshot_blob_hexes()` to return `[]` and `BlobCache::register_mount`
//! to seed no owners — breaking the B5 reservation invariant on remounts.
//!
//! Exit criteria:
//! - `provider2.snapshot_blob_hexes().len() == N_BLOBS` after a tree-cache hit.
//! - `cache.working_set_bytes(&repo_key) > 0` after `register_mount` with
//!   those hexes.

#[path = "common/mod.rs"]
mod common;

use common::{
    blob_entry, build_codeload_tarball, commit_json, git_blob_sha1, tree_json, MockRoute,
    MockServer,
};
use ctxfs_cache::{BlobCache, RepoKey, TreeCache};
use ctxfs_core::source::SourceSpec;
use ctxfs_provider_common::fetcher::{PrefetchPolicy, TarballSingleflightMap};
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_git::{FetchOptions, GitHubProvider, ProviderContext};
use std::sync::Arc;
use tempfile::tempdir;

const OWNER: &str = "org-tc";
const REPO: &str = "repo-tc";
const GIT_REF: &str = "main";
const COMMIT: &str = "cccc000000000000000000000000000000001ccc";
const TREE_SHA: &str = "tttt000000000000000000000000000000001ttt";
const WRAPPER: &str = "org-tc-repo-tc-cccc0000";

/// Blobs > 4096 bytes → not in small_blob_shas → only stored via tarball.
const BLOB_SIZE: usize = 5_100;
const N_BLOBS: usize = 3;

fn make_provider_with_tree_cache(
    server: &MockServer,
    cache: Arc<BlobCache>,
    tree_cache: Arc<TreeCache>,
    obs: Arc<Observability>,
    sf: Arc<TarballSingleflightMap>,
) -> GitHubProvider {
    let ctx = ProviderContext {
        observability: obs,
        singleflight: sf,
        tree_cache: Some(tree_cache),
        ..ProviderContext::minimal(server.host.clone(), cache)
    };
    GitHubProvider::new_with_codeload_host(None, Some(server.host.clone()), ctx)
}

#[tokio::test(flavor = "multi_thread")]
async fn tree_cache_hit_populates_sha_to_path_for_register_mount() {
    // ── Generate blob contents and SHA-1s ────────────────────────────────────
    let contents: Vec<Vec<u8>> = (0..N_BLOBS)
        .map(|i| {
            let mut v = vec![b'c'; BLOB_SIZE - 4];
            v.extend_from_slice(&(i as u32).to_le_bytes());
            v
        })
        .collect();
    let shas: Vec<String> = contents.iter().map(|c| git_blob_sha1(c)).collect();

    let tree_entries: Vec<serde_json::Value> = (0..N_BLOBS)
        .map(|i| blob_entry(&format!("file_{i}.bin"), &shas[i], BLOB_SIZE as u64))
        .collect();

    let tarball_files: Vec<(String, Vec<u8>)> = (0..N_BLOBS)
        .map(|i| (format!("{WRAPPER}/file_{i}.bin"), contents[i].clone()))
        .collect();
    let tarball = build_codeload_tarball(WRAPPER, &tarball_files);

    // Mock server: commit + tree + tarball.
    // Commit route is hit twice (once per provider instance for ref resolution).
    // Tree and tarball routes are hit only once (second fetch uses tree cache).
    let server = MockServer::spawn(vec![
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER}/{REPO}/commits"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: commit_json(COMMIT),
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
            body: tarball,
            hit_count: None,
            delay_ms: None,
        },
    ])
    .await;

    let tmp_blob = tempdir().unwrap();
    let tmp_tree = tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp_blob.path().to_path_buf(), 10 * 1024 * 1024).unwrap());
    let tree_cache = Arc::new(TreeCache::new(
        tmp_tree.path().to_path_buf(),
        10 * 1024 * 1024,
    ));
    let sf = Arc::new(TarballSingleflightMap::new());

    // Threshold = 2 → 3 blobs ≥ 2 → tarball fires on first fetch.
    let opts = FetchOptions {
        prefetch: PrefetchPolicy::Auto,
        prefetch_threshold_count: 2,
        prefetch_max_bytes: 256 * 1024 * 1024,
    };

    // ── Provider 1: full fetch (populates tree cache + blob cache) ────────────
    let obs1 = Arc::new(Observability::new());
    let provider1 =
        make_provider_with_tree_cache(&server, cache.clone(), tree_cache.clone(), obs1, sf.clone());
    let source = SourceSpec::parse(&format!("github:{OWNER}/{REPO}@{GIT_REF}")).unwrap();
    let _ = provider1
        .fetch_snapshot_with_options(&source, &opts)
        .await
        .expect("first fetch must succeed");

    let hexes1 = provider1.snapshot_blob_hexes();
    assert_eq!(
        hexes1.len(),
        N_BLOBS,
        "provider1 must see all {N_BLOBS} blob hexes after full fetch"
    );

    // ── Provider 2: new instance; sha_to_path starts empty ───────────────────
    // The tree cache was populated by provider1; this fetch should hit it.
    let obs2 = Arc::new(Observability::new());
    let provider2 =
        make_provider_with_tree_cache(&server, cache.clone(), tree_cache.clone(), obs2, sf.clone());
    let _ = provider2
        .fetch_snapshot_with_options(&source, &opts)
        .await
        .expect("second fetch (tree-cache hit) must succeed");

    let hexes2 = provider2.snapshot_blob_hexes();
    assert_eq!(
        hexes2.len(),
        N_BLOBS,
        "provider2 must see all {N_BLOBS} blob hexes after tree-cache hit \
         (collect_snapshot_blob_hexes must populate sha_to_path)"
    );

    // The hexes must be the same set as provider1 reported.
    let mut sorted1 = hexes1.clone();
    sorted1.sort_unstable();
    let mut sorted2 = hexes2.clone();
    sorted2.sort_unstable();
    assert_eq!(
        sorted1, sorted2,
        "hex sets must match between the two providers"
    );

    // ── Register mount using provider2's hexes; assert working_set > 0 ───────
    let repo_key = RepoKey::new("github.com", OWNER, REPO);
    cache.register_mount(&repo_key, None, &hexes2);

    let ws = cache.working_set_bytes(&repo_key);
    assert!(
        ws > 0,
        "working_set_bytes must be > 0 after register_mount with tree-cache-derived hexes; \
         got {ws} (blobs should all be in cache from the first tarball fetch)"
    );
}
