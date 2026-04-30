//! Replay test: B6 — LFS pointer detection surfaces in counters.
//!
//! A single-file mock GitHub repo returns a canonical Git LFS pointer payload
//! as the blob content. After `fetch_snapshot_with_options`, the small-blob
//! prefetch path (`prefetch_small_blobs` → `fetch_blob_content`) detects the
//! pointer and records the count + sample path in the per-mount counters.
//!
//! Exit criteria (B6):
//! - `lfs_pointer_files == 1` after fetch completes.
//! - `lfs_pointer_sample_paths` contains the tree-entry path of the LFS file.

#[path = "common/mod.rs"]
mod common;

use base64::Engine as _;
use common::{
    blob_entry, commit_json, git_blob_sha1, make_provider, tree_json, MockRoute, MockServer,
};
use ctxfs_cache::BlobCache;
use ctxfs_core::source::SourceSpec;
use ctxfs_provider_common::counters::CounterKey;
use ctxfs_provider_common::fetcher::{PrefetchPolicy, TarballSingleflightMap};
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_git::FetchOptions;
use std::sync::Arc;
use tempfile::tempdir;

const OWNER: &str = "lfs-owner";
const REPO: &str = "lfs-repo";
const GIT_REF: &str = "main";
const COMMIT_SHA: &str = "cccc111100000000000000000000000000001234";
const TREE_SHA: &str = "dddd222200000000000000000000000000005678";
/// Mount-relative path of the LFS-tracked file as it appears in the tree.
const LFS_FILE_PATH: &str = "model/weights.bin";

#[tokio::test(flavor = "multi_thread")]
async fn lfs_pointer_blob_surfaces_in_counters() {
    // ── Build a canonical LFS pointer payload ──────────────────────────────
    // 64-char lowercase hex OID, typical LFS object size declaration.
    let lfs_oid = "a".repeat(64);
    let lfs_size: u64 = 1_234_567;
    let lfs_bytes = format!(
        "version https://git-lfs.github.com/spec/v1\noid sha256:{lfs_oid}\nsize {lfs_size}\n"
    )
    .into_bytes();

    // Git blob SHA-1 of the pointer bytes — what the GitHub Trees API returns.
    let blob_sha = git_blob_sha1(&lfs_bytes);

    // GitHub blob API returns base64-encoded content.
    let lfs_b64 = base64::engine::general_purpose::STANDARD.encode(&lfs_bytes);
    let blob_api_body = serde_json::json!({
        "sha": blob_sha,
        "encoding": "base64",
        "content": lfs_b64,
    })
    .to_string()
    .into_bytes();

    // Tree: one blob entry; size is the pointer byte-length (< 4096 → small-blob path).
    let tree_entries = vec![blob_entry(LFS_FILE_PATH, &blob_sha, lfs_bytes.len() as u64)];

    // ── Mock server: commit + tree + blob API ──────────────────────────────
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
        // The small-blob prefetch path calls GET /repos/{o}/{r}/git/blobs/{sha}.
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER}/{REPO}/git/blobs"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: blob_api_body,
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
    // Threshold = 30 → 1 file < 30 → tarball auto-gate does not fire.
    // Small-blob prefetch always runs for blobs ≤ 4096 bytes regardless of policy.
    let opts = FetchOptions {
        prefetch: PrefetchPolicy::Auto,
        prefetch_threshold_count: 30,
        prefetch_max_bytes: 256 * 1024 * 1024,
    };

    let snapshot_bytes = provider
        .fetch_snapshot_with_options(&source, &opts)
        .await
        .expect("fetch_snapshot_with_options must succeed");
    assert!(!snapshot_bytes.is_empty(), "snapshot must be non-empty");

    // ── Counter assertions ─────────────────────────────────────────────────
    // sha_to_path is populated from tree entries before prefetch runs, so the
    // sample path is the tree-entry path, not a "<sha:...>" fallback.
    let key = CounterKey {
        source: "github".to_string(),
        repo: format!("{OWNER}/{REPO}"),
        commit: COMMIT_SHA.to_string(),
        mount_id: source.id(),
    };
    let snap = obs.counters_for(key).snapshot();

    assert_eq!(
        snap.lfs_pointer_files, 1,
        "expected lfs_pointer_files == 1 after small-blob fetch; got {}",
        snap.lfs_pointer_files
    );
    assert_eq!(
        snap.lfs_pointer_sample_paths,
        vec![LFS_FILE_PATH.to_string()],
        "sample path must match the tree-entry path (sha_to_path populated pre-prefetch)"
    );
}
