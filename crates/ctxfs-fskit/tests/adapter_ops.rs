//! Integration tests for `FilesystemAdapter` against a mock VFS.
//! No `FSKit` runtime required.

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

use async_trait::async_trait;
use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::Digest;
use ctxfs_fskit::FilesystemAdapter;
use ctxfs_manifest::{DirEntry, Directory, DirectoryEntry, FileEntry, Snapshot};
use ctxfs_vfs::VfsState;
use fskit_rs::{Filesystem, ItemType, TaskOptions};
use std::ffi::OsStr;
use std::sync::Arc;

struct MockProvider {
    directories: std::collections::HashMap<String, Vec<u8>>,
    blobs: std::collections::HashMap<String, Vec<u8>>,
}

#[async_trait]
impl ctxfs_core::provider::Provider for MockProvider {
    async fn fetch_snapshot(
        &self,
        _source: &ctxfs_core::source::SourceSpec,
    ) -> Result<Vec<u8>, ctxfs_core::error::CtxfsError> {
        unimplemented!()
    }
    async fn fetch_directory(
        &self,
        digest: &Digest,
    ) -> Result<Vec<u8>, ctxfs_core::error::CtxfsError> {
        self.directories
            .get(&digest.hex)
            .cloned()
            .ok_or_else(|| ctxfs_core::error::CtxfsError::NotFound(digest.hex.clone()))
    }
    async fn fetch_blob(
        &self,
        digest: &Digest,
    ) -> Result<Vec<u8>, ctxfs_core::error::CtxfsError> {
        self.blobs
            .get(&digest.hex)
            .cloned()
            .ok_or_else(|| ctxfs_core::error::CtxfsError::NotFound(digest.hex.clone()))
    }
}

fn make_digest(hex: &str) -> Digest {
    Digest {
        algorithm: ctxfs_core::digest::HashAlgorithm::Sha256,
        hex: hex.to_string(),
    }
}

async fn build_adapter() -> FilesystemAdapter {
    let readme_digest = make_digest("readme_sha256");
    let readme_content = b"# Hello\n".to_vec();
    let main_rs_digest = make_digest("main_rs_sha256");
    let main_rs_content = b"fn main() {}\n".to_vec();

    let src_dir = Directory {
        digest: make_digest("src_dir_sha256"),
        entries: vec![DirEntry::File(FileEntry {
            name: "main.rs".into(),
            digest: main_rs_digest.clone(),
            size: main_rs_content.len() as u64,
            executable: false,
            inline_content: None,
        })],
    };
    let root_dir = Directory {
        digest: make_digest("root_dir_sha256"),
        entries: vec![
            DirEntry::File(FileEntry {
                name: "README.md".into(),
                digest: readme_digest.clone(),
                size: readme_content.len() as u64,
                executable: false,
                inline_content: Some(readme_content.clone()),
            }),
            DirEntry::Directory(DirectoryEntry {
                name: "src".into(),
                digest: src_dir.digest.clone(),
            }),
        ],
    };

    let mut directories = std::collections::HashMap::new();
    directories.insert(root_dir.digest.hex.clone(), serde_json::to_vec(&root_dir).unwrap());
    directories.insert(src_dir.digest.hex.clone(), serde_json::to_vec(&src_dir).unwrap());
    let mut blobs = std::collections::HashMap::new();
    blobs.insert(readme_digest.hex.clone(), readme_content);
    blobs.insert(main_rs_digest.hex.clone(), main_rs_content);

    let provider: SharedProvider = Arc::new(MockProvider { directories, blobs });
    let snapshot = Snapshot {
        source: "github:test/repo@main".into(),
        commit_sha: "abc".into(),
        root_directory: root_dir.digest,
        created_at: "2026-04-13T00:00:00Z".into(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp.path().to_path_buf(), 64 * 1024 * 1024).unwrap());
    let vfs = Arc::new(VfsState::new(provider, cache, snapshot, None).await.unwrap());
    FilesystemAdapter::new(vfs, "test-vol".into())
}

#[tokio::test]
async fn activate_returns_root_at_fskit_id_2() {
    let mut adapter = build_adapter().await;
    let root = adapter.activate(TaskOptions::default()).await.unwrap();
    let attrs = root.attributes.unwrap();
    assert_eq!(attrs.file_id, Some(2));
    assert_eq!(attrs.r#type, Some(ItemType::Directory as i32));
}

#[tokio::test]
async fn lookup_child_from_root() {
    let mut adapter = build_adapter().await;
    let item = adapter.lookup_item(OsStr::new("README.md"), 2).await.unwrap();
    let attrs = item.attributes.unwrap();
    assert_eq!(attrs.r#type, Some(ItemType::File as i32));
    assert_eq!(attrs.size, Some(8));
    assert_eq!(attrs.parent_id, Some(2));  // README's parent is root
    assert_eq!(attrs.file_id, Some(3));    // VFS id 2 → FSKit id 3
}

#[tokio::test]
async fn nested_file_has_correct_parent_id() {
    let mut adapter = build_adapter().await;
    let src = adapter.lookup_item(OsStr::new("src"), 2).await.unwrap();
    let src_fskit_id = src.attributes.unwrap().file_id.unwrap();

    let main_rs = adapter
        .lookup_item(OsStr::new("main.rs"), src_fskit_id)
        .await
        .unwrap();
    let attrs = main_rs.attributes.unwrap();

    // Critical: parent_id must be src's FSKit id, NOT root.
    assert_eq!(attrs.parent_id, Some(src_fskit_id));
    assert_ne!(attrs.parent_id, Some(2));
}

#[tokio::test]
async fn enumerate_returns_real_sizes() {
    let mut adapter = build_adapter().await;
    let dir = adapter.enumerate_directory(2, 0, 0).await.unwrap();
    let readme = dir
        .entries
        .iter()
        .find_map(|e| e.item.as_ref().filter(|i| i.name == b"README.md"))
        .unwrap();
    let attrs = readme.attributes.as_ref().unwrap();
    // Must not be 0 (Codex finding #3)
    assert_eq!(attrs.size, Some(8));
}

#[tokio::test]
async fn lookup_missing_returns_enoent() {
    let mut adapter = build_adapter().await;
    let err = adapter.lookup_item(OsStr::new("nope"), 2).await.unwrap_err();
    match err {
        fskit_rs::Error::Posix(e) => assert_eq!(e, libc::ENOENT),
    }
}

#[tokio::test]
async fn read_file_contents() {
    let mut adapter = build_adapter().await;
    let readme = adapter.lookup_item(OsStr::new("README.md"), 2).await.unwrap();
    let file_id = readme.attributes.unwrap().file_id.unwrap();
    let bytes = adapter.read(file_id, 0, 1024).await.unwrap();
    assert_eq!(bytes, b"# Hello\n");
}

#[tokio::test]
async fn write_returns_erofs() {
    let mut adapter = build_adapter().await;
    let err = adapter.write(vec![1], 3, 0).await.unwrap_err();
    match err {
        fskit_rs::Error::Posix(e) => assert_eq!(e, libc::EROFS),
    }
}

#[tokio::test]
async fn getattr_root_is_directory() {
    let mut adapter = build_adapter().await;
    let attrs = adapter.get_attributes(2).await.unwrap();
    assert_eq!(attrs.r#type, Some(ItemType::Directory as i32));
    assert_eq!(attrs.file_id, Some(2));
    assert_eq!(attrs.parent_id, Some(2));
}
