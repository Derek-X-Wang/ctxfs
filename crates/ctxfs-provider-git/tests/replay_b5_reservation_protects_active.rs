//! Replay test: B5 — active repo within reservation receives zero evictions
//! from another repo's cache pressure.
//!
//! Uses a shared `BlobCache` (20 KB limit). Repo A fetches 3 blobs × 5 KB
//! via tarball, then is registered with a 16 KB reservation (working set
//! 15 KB ≤ reservation). Repo B fetches 2 blobs × 6 KB via tarball;
//! inserting them pushes the total to 27 KB > 20 KB, but each of A's blobs
//! is reservation-protected, so B's blobs self-evict immediately.
//!
//! Exit criteria (B5):
//! - A's 3 blobs are all still in cache after B's fetch completes.
//! - `cache.eviction_attempts_blocked_by_reservation() > 0`.

#[path = "common/mod.rs"]
mod common;

use common::{
    blob_entry, build_codeload_tarball, commit_json, git_blob_sha1, make_provider, tree_json,
    MockRoute, MockServer,
};
use ctxfs_cache::{BlobCache, RepoKey};
use ctxfs_core::{source::SourceSpec, Digest};
use ctxfs_provider_common::fetcher::{PrefetchPolicy, TarballSingleflightMap};
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_git::FetchOptions;
use std::sync::Arc;
use tempfile::tempdir;

// ── Repo A constants ─────────────────────────────────────────────────────────
const OWNER_A: &str = "org-a";
const REPO_A: &str = "corp-a";
const GIT_REF_A: &str = "main";
const COMMIT_A: &str = "aaaa0000000000000000000000000000000001aa";
const TREE_A: &str = "tree000000000000000000000000000000001aaa";
const WRAPPER_A: &str = "org-a-corp-a-aaaa0000"; // {owner}-{repo}-{commit[:8]}

// ── Repo B constants ─────────────────────────────────────────────────────────
const OWNER_B: &str = "org-b";
const REPO_B: &str = "corp-b";
const GIT_REF_B: &str = "main";
const COMMIT_B: &str = "bbbb0000000000000000000000000000000001bb";
const TREE_B: &str = "tree000000000000000000000000000000002bbb";
const WRAPPER_B: &str = "org-b-corp-b-bbbb0000";

// ── Cache sizing ──────────────────────────────────────────────────────────────
/// Blob sizes > 4096 → not in small_blob_shas → only stored via tarball path.
const BLOB_SIZE_A: usize = 5_000;
const N_BLOBS_A: usize = 3; // 3 × 5 000 = 15 000 bytes
const BLOB_SIZE_B: usize = 6_000;
const N_BLOBS_B: usize = 2; // 2 × 6 000 = 12 000 bytes; total 27 000 > 20 000

const CACHE_MAX: u64 = 20_000;
/// 16 000 > 15 000 (A's working set) → all of A's blobs are protected.
const RESERVATION_A: u64 = 16_000;

#[tokio::test(flavor = "multi_thread")]
async fn mount_a_within_reservation_unaffected_by_mount_b_pressure() {
    // ── Generate blob contents and compute git SHA-1s ─────────────────────
    let a_contents: Vec<Vec<u8>> = (0..N_BLOBS_A)
        .map(|i| {
            let mut v = vec![b'a'; BLOB_SIZE_A - 4];
            v.extend_from_slice(&(i as u32).to_le_bytes());
            v
        })
        .collect();
    let a_shas: Vec<String> = a_contents.iter().map(|c| git_blob_sha1(c)).collect();

    let b_contents: Vec<Vec<u8>> = (0..N_BLOBS_B)
        .map(|i| {
            let mut v = vec![b'b'; BLOB_SIZE_B - 4];
            v.extend_from_slice(&(i as u32).to_le_bytes());
            v
        })
        .collect();
    let b_shas: Vec<String> = b_contents.iter().map(|c| git_blob_sha1(c)).collect();

    // ── Build tree JSON entries ────────────────────────────────────────────
    let tree_entries_a: Vec<serde_json::Value> = (0..N_BLOBS_A)
        .map(|i| blob_entry(&format!("file_a_{i}.bin"), &a_shas[i], BLOB_SIZE_A as u64))
        .collect();
    let tree_entries_b: Vec<serde_json::Value> = (0..N_BLOBS_B)
        .map(|i| blob_entry(&format!("file_b_{i}.bin"), &b_shas[i], BLOB_SIZE_B as u64))
        .collect();

    // ── Build codeload-format tarballs ────────────────────────────────────
    let tarball_a_files: Vec<(String, Vec<u8>)> = (0..N_BLOBS_A)
        .map(|i| (format!("{WRAPPER_A}/file_a_{i}.bin"), a_contents[i].clone()))
        .collect();
    let tarball_a = build_codeload_tarball(WRAPPER_A, &tarball_a_files);

    let tarball_b_files: Vec<(String, Vec<u8>)> = (0..N_BLOBS_B)
        .map(|i| (format!("{WRAPPER_B}/file_b_{i}.bin"), b_contents[i].clone()))
        .collect();
    let tarball_b = build_codeload_tarball(WRAPPER_B, &tarball_b_files);

    // ── One mock server handles both repos via distinct path prefixes ──────
    let server = MockServer::spawn(vec![
        // Repo A
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER_A}/{REPO_A}/commits"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: commit_json(COMMIT_A),
            hit_count: None,
            delay_ms: None,
        },
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER_A}/{REPO_A}/git/trees"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: tree_json(TREE_A, &tree_entries_a, false),
            hit_count: None,
            delay_ms: None,
        },
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER_A}/{REPO_A}/tarball"),
            status: 200,
            headers: vec![
                ("Content-Type", "application/gzip".to_string()),
                ("Content-Encoding", "gzip".to_string()),
            ],
            body: tarball_a,
            hit_count: None,
            delay_ms: None,
        },
        // Repo B
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER_B}/{REPO_B}/commits"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: commit_json(COMMIT_B),
            hit_count: None,
            delay_ms: None,
        },
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER_B}/{REPO_B}/git/trees"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: tree_json(TREE_B, &tree_entries_b, false),
            hit_count: None,
            delay_ms: None,
        },
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{OWNER_B}/{REPO_B}/tarball"),
            status: 200,
            headers: vec![
                ("Content-Type", "application/gzip".to_string()),
                ("Content-Encoding", "gzip".to_string()),
            ],
            body: tarball_b,
            hit_count: None,
            delay_ms: None,
        },
    ])
    .await;

    // ── Shared cache; small enough that A + B together overflow ───────────
    let tmp = tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp.path().to_path_buf(), CACHE_MAX).unwrap());

    // Threshold = 2 → 3 blobs (A) and 2 blobs (B) both trigger tarball.
    // Blobs > 4096 bytes are not in small_blob_shas → only stored via tarball.
    let opts = FetchOptions {
        prefetch: PrefetchPolicy::Auto,
        prefetch_threshold_count: 2,
        prefetch_max_bytes: 256 * 1024 * 1024,
    };
    let sf = Arc::new(TarballSingleflightMap::new());

    // ── Mount A: fetch snapshot (tarball stores A's blobs in cache) ────────
    let obs_a = Arc::new(Observability::new());
    let provider_a = make_provider(&server, cache.clone(), obs_a.clone(), sf.clone());
    let source_a = SourceSpec::parse(&format!("github:{OWNER_A}/{REPO_A}@{GIT_REF_A}")).unwrap();
    let _ = provider_a
        .fetch_snapshot_with_options(&source_a, &opts)
        .await
        .expect("repo-a snapshot must succeed");

    // Sanity: A's 3 blobs are now in cache.
    let a_digests: Vec<Digest> = a_shas.iter().map(Digest::from_sha1_hex).collect();
    for (i, d) in a_digests.iter().enumerate() {
        assert!(
            cache.contains(d),
            "A blob {i} must be in cache after tarball fetch"
        );
    }

    // Register mount A with an explicit reservation and seed blob ownership.
    // (In production the daemon calls this after prepare_mount; here we do it
    // manually since the replay test bypasses the daemon layer.)
    let repo_key_a = RepoKey::new("github.com", OWNER_A, REPO_A);
    cache.register_mount(&repo_key_a, Some(RESERVATION_A), &a_shas);
    assert_eq!(
        cache.working_set_bytes(&repo_key_a),
        (N_BLOBS_A * BLOB_SIZE_A) as u64,
        "working_set_bytes must equal A's total blob size after registration"
    );

    // ── Mount B: fetch snapshot (triggers eviction) ────────────────────────
    // Inserting B's 6 KB blobs pushes total > 20 KB. Each of A's blobs is
    // protected (working_set ≤ reservation), so B's blobs self-evict.
    let obs_b = Arc::new(Observability::new());
    let provider_b = make_provider(&server, cache.clone(), obs_b.clone(), sf.clone());
    let source_b = SourceSpec::parse(&format!("github:{OWNER_B}/{REPO_B}@{GIT_REF_B}")).unwrap();
    let _ = provider_b
        .fetch_snapshot_with_options(&source_b, &opts)
        .await
        .expect("repo-b snapshot must succeed");

    // ── Assertions ─────────────────────────────────────────────────────────

    // A's blobs must all survive.
    for (i, d) in a_digests.iter().enumerate() {
        assert!(
            cache.contains(d),
            "A blob {i} must survive in cache after B's write pressure \
             (reservation={RESERVATION_A}, working_set={})",
            cache.working_set_bytes(&repo_key_a)
        );
    }

    // The eviction-skip counter must have fired at least once — confirming
    // that the protection path was exercised, not bypassed.
    let blocked = cache.eviction_attempts_blocked_by_reservation();
    assert!(
        blocked > 0,
        "eviction_attempts_blocked_by_reservation must be > 0; \
         got {blocked} (B's writes should have triggered reservation protection for A)"
    );
}
