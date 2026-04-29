//! Replay test: malicious tarball entries with path-traversal attacks are
//! rejected; legitimate entries still land in the blob cache.
//!
//! Spec exit criterion:
//! - `tarball_invalid_entries >= 1` (the traversal entry was rejected)
//! - `prefetch_hits >= 1` (the legitimate entry was committed)
//! - The escape blob does NOT appear in cache

#[path = "common/mod.rs"]
mod common;

use common::{
    blob_entry, build_tarball, commit_json, git_blob_sha1, make_provider, tree_json, MockRoute,
    MockServer,
};
use ctxfs_cache::BlobCache;
use ctxfs_core::source::SourceSpec;
use ctxfs_provider_common::fetcher::{PrefetchPolicy, TarballSingleflightMap};
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_git::FetchOptions;
use std::sync::Arc;
use tempfile::tempdir;

const OWNER: &str = "pt-owner";
const REPO: &str = "pt-repo";
const GIT_REF: &str = "main";
const COMMIT_SHA: &str = "ee000000000000000000000000000000000000ee";
const TREE_SHA: &str = "tree222222222222222222222222222222222222";
const WRAPPER: &str = "pt-owner-pt-repo-ee000000";

#[tokio::test(flavor = "multi_thread")]
async fn path_traversal_entry_rejected_legit_entry_lands() {
    // One legitimate file.
    let legit_content = b"legitimate content for the cache test";
    let legit_sha = git_blob_sha1(legit_content);

    // One "escape" blob we'll inject into the tarball at a traversal path.
    // This blob must NOT be in the tree manifest (so it can't match any
    // manifest entry), but the path-validation rejection happens BEFORE the
    // manifest lookup.
    let escape_content = b"SHOULD NOT REACH CACHE";
    let escape_sha = git_blob_sha1(escape_content);

    let tree_entries = vec![blob_entry(
        "legit.rs",
        &legit_sha,
        legit_content.len() as u64,
    )];

    // Build the tarball manually: include both the traversal entry and the
    // legitimate entry.  The traversal path contains `..` which validate_tar_entry_path
    // rejects.
    let tarball_bytes = build_tarball(&[
        // Traversal attack: wrapper/../escape.  Path is stored raw in the tar
        // header (no OS normalisation), so the `..'  segment is preserved.
        (format!("{WRAPPER}/../escape.txt"), escape_content.to_vec()),
        // Legitimate entry.
        (format!("{WRAPPER}/legit.rs"), legit_content.to_vec()),
    ]);

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
        prefetch: PrefetchPolicy::Force, // Force so gate doesn't block on low count
        prefetch_threshold_count: 30,
        prefetch_max_bytes: 256 * 1024 * 1024,
    };

    // fetch_snapshot_with_options succeeds — tarball errors are non-fatal.
    let _ = provider
        .fetch_snapshot_with_options(&source, &opts)
        .await
        .expect("fetch_snapshot_with_options must succeed even with bad tarball entries");

    // ── Counter assertions ─────────────────────────────────────────────────
    let key = ctxfs_provider_common::counters::CounterKey {
        source: "github".to_string(),
        repo: format!("{OWNER}/{REPO}"),
        commit: COMMIT_SHA.to_string(),
        mount_id: source.id(),
    };
    let snap = obs.counters_for(key).snapshot();

    assert!(
        snap.tarball_invalid_entries >= 1,
        "traversal entry must increment tarball_invalid_entries, got {}",
        snap.tarball_invalid_entries
    );
    assert!(
        snap.prefetch_hits >= 1,
        "legitimate entry must increment prefetch_hits, got {}",
        snap.prefetch_hits
    );

    // ── Escape blob must NOT be in cache ───────────────────────────────────
    let escape_digest = ctxfs_core::Digest::from_sha256_hex(&escape_sha);
    assert!(
        cache.get(&escape_digest).is_none(),
        "escape blob must not appear in cache after path-traversal rejection"
    );

    // ── Legit blob IS in cache ─────────────────────────────────────────────
    let legit_digest = ctxfs_core::Digest::from_sha256_hex(&legit_sha);
    assert!(
        cache.get(&legit_digest).is_some(),
        "legitimate blob must be in cache after tarball extraction"
    );
}
