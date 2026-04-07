//! Integration test for the full NFS read path against a real GitHub repo.
//!
//! This test hits the GitHub API (octocat/Hello-World, one of the smallest
//! public repos) and drives [`CtxfsNfs`] directly through its
//! `NFSFileSystem` trait — no kernel mount, no sudo, no IPC.
//!
//! It validates the exact code path a real `cat README` triggers:
//!   `fetch_snapshot` → `build_directories` → `readdir` → `lookup` → `read` → `fetch_blob`
//!
//! # Network
//!
//! Skips cleanly if `CTXFS_E2E_SKIP_NETWORK=1` is set.

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

use ctxfs_cache::BlobCache;
use ctxfs_core::provider::Provider;
use ctxfs_core::source::SourceSpec;
use ctxfs_manifest::Snapshot;
use ctxfs_nfs::CtxfsNfs;
use ctxfs_provider_git::GitHubProvider;
use nfsserve::nfs::filename3;
use nfsserve::vfs::NFSFileSystem;
use std::sync::Arc;

fn network_allowed() -> bool {
    std::env::var("CTXFS_E2E_SKIP_NETWORK").is_err()
}

async fn build_fs_for(owner: &str, repo: &str, git_ref: &str) -> (CtxfsNfs, tempfile::TempDir) {
    let tempdir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tempdir.path().to_path_buf(), 64 * 1024 * 1024).unwrap());

    let token = std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty());
    let provider = Arc::new(GitHubProvider::new(token.as_deref(), cache.clone()));
    let source = SourceSpec::parse(&format!("github:{owner}/{repo}@{git_ref}")).unwrap();

    let snapshot_bytes = provider.fetch_snapshot(&source).await.unwrap();
    let snapshot: Snapshot = serde_json::from_slice(&snapshot_bytes).unwrap();

    let fs = CtxfsNfs::new(provider, source, cache, snapshot);
    (fs, tempdir)
}

#[tokio::test]
async fn readdir_returns_repo_files() {
    if !network_allowed() {
        eprintln!("skipping: network disabled");
        return;
    }
    let (fs, _tmp) = build_fs_for("octocat", "Hello-World", "master").await;

    let root = fs.root_dir();
    let result = fs.readdir(root, 0, 100).await.unwrap();

    // Hello-World has a single `README` file in its root.
    let names: Vec<String> = result
        .entries
        .iter()
        .map(|e| String::from_utf8_lossy(e.name.as_ref()).into_owned())
        .collect();
    assert!(
        names.iter().any(|n| n == "README"),
        "expected README in root, got {names:?}"
    );
}

#[tokio::test]
async fn lookup_followed_by_read_returns_file_bytes() {
    if !network_allowed() {
        eprintln!("skipping: network disabled");
        return;
    }
    let (fs, _tmp) = build_fs_for("octocat", "Hello-World", "master").await;

    let root = fs.root_dir();
    let readme_id = fs
        .lookup(root, &filename3::from(b"README".to_vec()))
        .await
        .expect("README should be findable in root");

    // Read the first 1 KB of README.
    let (bytes, _eof) = fs
        .read(readme_id, 0, 1024)
        .await
        .expect("read should succeed");

    let text = String::from_utf8_lossy(&bytes);
    assert!(!bytes.is_empty(), "README should not be empty");
    assert!(
        text.starts_with("Hello World"),
        "expected README to start with 'Hello World', got: {text}"
    );
}

#[tokio::test]
async fn read_honors_offset_and_count() {
    if !network_allowed() {
        eprintln!("skipping: network disabled");
        return;
    }
    let (fs, _tmp) = build_fs_for("octocat", "Hello-World", "master").await;

    let root = fs.root_dir();
    let readme_id = fs
        .lookup(root, &filename3::from(b"README".to_vec()))
        .await
        .unwrap();

    // Full content.
    let (full, full_eof) = fs.read(readme_id, 0, 4096).await.unwrap();
    assert!(full_eof, "4KB read of small README should hit EOF");

    // Sliced read: skip first byte, take 3.
    let (sliced, _) = fs.read(readme_id, 1, 3).await.unwrap();
    assert_eq!(sliced.len(), 3);
    assert_eq!(sliced, &full[1..4]);

    // Offset past end: should return empty, eof=true.
    let (empty, eof) = fs
        .read(readme_id, full.len() as u64 + 100, 10)
        .await
        .unwrap();
    assert!(empty.is_empty());
    assert!(eof);
}

#[tokio::test]
async fn second_read_uses_cache_not_network() {
    // After one read of the same digest, the cache is populated and subsequent
    // reads should succeed even if we swap to a provider that can't reach the
    // network. We prove this by timing: the second read should be much faster.
    if !network_allowed() {
        eprintln!("skipping: network disabled");
        return;
    }
    let (fs, _tmp) = build_fs_for("octocat", "Hello-World", "master").await;

    let root = fs.root_dir();
    let readme_id = fs
        .lookup(root, &filename3::from(b"README".to_vec()))
        .await
        .unwrap();

    let t0 = std::time::Instant::now();
    let (first, _) = fs.read(readme_id, 0, 4096).await.unwrap();
    let cold_ms = t0.elapsed().as_millis();

    let t1 = std::time::Instant::now();
    let (second, _) = fs.read(readme_id, 0, 4096).await.unwrap();
    let warm_ms = t1.elapsed().as_millis();

    assert_eq!(first, second, "cached read should return identical bytes");
    // A cache hit should be at least 10x faster than a network round-trip.
    // We use a conservative multiplier to avoid flakiness on fast networks.
    assert!(
        warm_ms * 5 < cold_ms.max(5),
        "expected warm read to be noticeably faster: cold={cold_ms}ms warm={warm_ms}ms"
    );
}
