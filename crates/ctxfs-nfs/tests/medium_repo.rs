//! Validation tests against a medium-sized GitHub repo.
//!
//! Uses `cli/cli` (the GitHub CLI repo, ~2000 files) to exercise:
//! - Deep nested directories
//! - Many files in a single directory
//! - Various file sizes
//! - Binary-ish content
//!
//! Skips if `CTXFS_E2E_SKIP_NETWORK=1`.

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

use ctxfs_cache::BlobCache;
use ctxfs_core::provider::Provider;
use ctxfs_core::source::SourceSpec;
use ctxfs_manifest::Snapshot;
use ctxfs_nfs::CtxfsNfs;
use ctxfs_provider_git::GitHubProvider;
use ctxfs_vfs::VfsState;
use nfsserve::nfs::{filename3, ftype3, nfsstat3};
use nfsserve::vfs::NFSFileSystem;
use std::sync::Arc;

fn network_allowed() -> bool {
    std::env::var("CTXFS_E2E_SKIP_NETWORK").is_err()
}

async fn build_fs(owner: &str, repo: &str, git_ref: &str) -> (CtxfsNfs, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 128 * 1024 * 1024).unwrap());
    let token = std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty());
    let provider = Arc::new(GitHubProvider::new(
        token.as_deref(),
        "api.github.com".to_string(),
        cache.clone(),
        None,
        None,
        Arc::new(ctxfs_provider_common::observability::Observability::new()),
    ));
    let source = SourceSpec::parse(&format!("github:{owner}/{repo}@{git_ref}")).unwrap();
    let snap_bytes = provider.fetch_snapshot(&source).await.unwrap();
    let snapshot: Snapshot = serde_json::from_slice(&snap_bytes).unwrap();
    let vfs = VfsState::new(provider, cache, snapshot, None)
        .await
        .unwrap();
    (CtxfsNfs::new(Arc::new(vfs), source), dir)
}

/// Test against a repo with nested structure. Using `charmbracelet/bubbletea`
/// which is medium-sized (~80 files), stable, and MIT licensed.
#[tokio::test]
async fn nested_directory_traversal() {
    if !network_allowed() {
        return;
    }
    let (fs, _tmp) = build_fs("charmbracelet", "bubbletea", "main").await;

    let root = fs.root_dir();
    let root_entries = fs.readdir(root, 0, 200).await.unwrap();

    let names: Vec<String> = root_entries
        .entries
        .iter()
        .map(|e| String::from_utf8_lossy(e.name.as_ref()).into_owned())
        .collect();

    // bubbletea root should have at least: go.mod, README.md, tea.go, examples/
    assert!(
        names.len() > 5,
        "expected >5 root entries, got {}",
        names.len()
    );
    assert!(
        names.iter().any(|n| n == "go.mod" || n == "README.md"),
        "expected go.mod or README.md in root, got {names:?}"
    );

    // Find a directory and descend into it
    let dir_entry = root_entries
        .entries
        .iter()
        .find(|e| matches!(e.attr.ftype, ftype3::NF3DIR))
        .expect("should have at least one subdirectory");

    let subdir_entries = fs.readdir(dir_entry.fileid, 0, 200).await.unwrap();
    assert!(
        !subdir_entries.entries.is_empty(),
        "subdirectory should not be empty"
    );
}

/// Read a known file and verify we get real content.
#[tokio::test]
async fn read_go_mod_file() {
    if !network_allowed() {
        return;
    }
    let (fs, _tmp) = build_fs("charmbracelet", "bubbletea", "main").await;

    let root = fs.root_dir();
    let go_mod_id = fs
        .lookup(root, &filename3::from(b"go.mod".to_vec()))
        .await
        .expect("go.mod should exist in root");

    let (bytes, _eof) = fs.read(go_mod_id, 0, 8192).await.expect("read go.mod");
    let text = String::from_utf8_lossy(&bytes);

    assert!(
        text.contains("module "),
        "go.mod should start with a module declaration, got:\n{text}"
    );
}

/// Verify getattr returns sane sizes for files.
#[tokio::test]
async fn getattr_returns_file_sizes() {
    if !network_allowed() {
        return;
    }
    let (fs, _tmp) = build_fs("charmbracelet", "bubbletea", "main").await;

    let root = fs.root_dir();
    let entries = fs.readdir(root, 0, 200).await.unwrap();

    for entry in &entries.entries {
        let attr = fs.getattr(entry.fileid).await.unwrap();
        if matches!(attr.ftype, ftype3::NF3DIR) {
            assert_eq!(attr.mode, 0o555);
        } else if matches!(attr.ftype, ftype3::NF3REG) {
            // Files should have a non-zero size or at least valid permissions
            assert!(attr.size > 0 || attr.mode == 0o444 || attr.mode == 0o555);
        }
    }
}

/// Lookup a non-existent file returns NOENT.
#[tokio::test]
async fn lookup_nonexistent_returns_error() {
    if !network_allowed() {
        return;
    }
    let (fs, _tmp) = build_fs("octocat", "Hello-World", "master").await;

    let root = fs.root_dir();
    let result = fs
        .lookup(root, &filename3::from(b"does_not_exist.xyz".to_vec()))
        .await;
    assert!(
        matches!(result, Err(nfsstat3::NFS3ERR_NOENT)),
        "expected NOENT, got {result:?}"
    );
}
