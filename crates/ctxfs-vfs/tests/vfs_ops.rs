use async_trait::async_trait;
use ctxfs_cache::BlobCache;
use ctxfs_core::error::CtxfsError;
use ctxfs_core::provider::Provider;
use ctxfs_core::Digest;
use ctxfs_manifest::{
    DirEntry, Directory, DirectoryEntry, FileEntry, Snapshot, SymlinkEntry,
};
use ctxfs_vfs::{NodeType, VfsError, VfsState};
use std::collections::HashMap;
use std::sync::Arc;

// ---------------------------------------------------------------------------
// Mock provider
// ---------------------------------------------------------------------------

struct MockProvider {
    directories: HashMap<String, Vec<u8>>,
    blobs: HashMap<String, Vec<u8>>,
}

#[async_trait]
impl Provider for MockProvider {
    async fn fetch_snapshot(&self, _source: &ctxfs_core::source::SourceSpec) -> Result<Vec<u8>, CtxfsError> {
        Err(CtxfsError::Provider("not implemented".into()))
    }

    async fn fetch_directory(&self, digest: &Digest) -> Result<Vec<u8>, CtxfsError> {
        self.directories
            .get(&digest.hex)
            .cloned()
            .ok_or_else(|| CtxfsError::NotFound(format!("directory {}", digest.hex)))
    }

    async fn fetch_blob(&self, digest: &Digest) -> Result<Vec<u8>, CtxfsError> {
        self.blobs
            .get(&digest.hex)
            .cloned()
            .ok_or_else(|| CtxfsError::NotFound(format!("blob {}", digest.hex)))
    }
}

// ---------------------------------------------------------------------------
// Test fixture builder
// ---------------------------------------------------------------------------

/// Build the test fixture:
/// ```text
/// root/
///   README.md   (inline, 36 bytes)
///   src/
///     main.rs   (not inline, fetched from provider)
///   link → README.md  (symlink)
/// ```
fn build_fixture() -> (Arc<dyn Provider>, Snapshot, Vec<u8>) {
    let readme_content = b"# Hello World\nThis is a readme.\n";
    let readme_digest = Digest::sha256(readme_content);

    let main_rs_content = b"fn main() { println!(\"hello\"); }";
    let main_rs_digest = Digest::sha256(main_rs_content);

    // src/ directory
    let src_entries = vec![DirEntry::File(FileEntry {
        name: "main.rs".into(),
        digest: main_rs_digest.clone(),
        size: main_rs_content.len() as u64,
        executable: false,
        inline_content: None, // must be fetched
    })];
    let src_digest = Directory::compute_digest(&src_entries);
    let src_dir = Directory {
        digest: src_digest.clone(),
        entries: src_entries,
    };

    // root directory
    let root_entries = vec![
        DirEntry::File(FileEntry {
            name: "README.md".into(),
            digest: readme_digest.clone(),
            size: readme_content.len() as u64,
            executable: false,
            inline_content: Some(readme_content.to_vec()),
        }),
        DirEntry::Directory(DirectoryEntry {
            name: "src".into(),
            digest: src_digest.clone(),
        }),
        DirEntry::Symlink(SymlinkEntry {
            name: "link".into(),
            target: "README.md".into(),
        }),
    ];
    let root_digest = Directory::compute_digest(&root_entries);
    let root_dir = Directory {
        digest: root_digest.clone(),
        entries: root_entries,
    };

    let mut directories = HashMap::new();
    let _ = directories.insert(
        root_digest.hex.clone(),
        serde_json::to_vec(&root_dir).unwrap(),
    );
    let _ = directories.insert(
        src_digest.hex.clone(),
        serde_json::to_vec(&src_dir).unwrap(),
    );

    let mut blobs = HashMap::new();
    let _ = blobs.insert(main_rs_digest.hex.clone(), main_rs_content.to_vec());

    let provider = Arc::new(MockProvider { directories, blobs });

    let snapshot = Snapshot {
        source: "test:fixture".into(),
        commit_sha: "abc123".into(),
        root_directory: root_digest,
        created_at: "2025-01-01T00:00:00Z".into(),
    };

    (provider, snapshot, main_rs_content.to_vec())
}

fn make_cache() -> Arc<BlobCache> {
    let dir = tempfile::tempdir().unwrap();
    // Leak the tempdir so it persists for the test duration.
    let path = dir.keep();
    Arc::new(BlobCache::new(path, 10 * 1024 * 1024).unwrap())
}

async fn make_vfs(subpath: Option<String>) -> (VfsState, Vec<u8>) {
    let (provider, snapshot, main_rs) = build_fixture();
    let cache = make_cache();
    let vfs = VfsState::new(provider, cache, snapshot, subpath)
        .await
        .unwrap();
    (vfs, main_rs)
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[tokio::test]
async fn lookup_root_children() {
    let (vfs, _) = make_vfs(None).await;
    let root = vfs.root_id();

    // Lookup README.md — should be a File
    let (id, attr) = vfs.lookup(root, "README.md").await.unwrap();
    assert!(id > root);
    assert_eq!(attr.kind, NodeType::File);
    assert!(!attr.executable);

    // Lookup src/ — should be a Directory
    let (src_id, src_attr) = vfs.lookup(root, "src").await.unwrap();
    assert!(src_id > root);
    assert_eq!(src_attr.kind, NodeType::Directory);

    // Lookup link — should be a Symlink
    let (link_id, link_attr) = vfs.lookup(root, "link").await.unwrap();
    assert!(link_id > root);
    assert_eq!(link_attr.kind, NodeType::Symlink);
}

#[tokio::test]
async fn lookup_not_found() {
    let (vfs, _) = make_vfs(None).await;
    let err = vfs.lookup(vfs.root_id(), "nonexistent.txt").await;
    assert!(matches!(err, Err(VfsError::NotFound)));
}

#[tokio::test]
async fn read_file_inline() {
    let (vfs, _) = make_vfs(None).await;
    let root = vfs.root_id();
    let (id, _) = vfs.lookup(root, "README.md").await.unwrap();

    let data = vfs.read(id, 0, 4096).await.unwrap();
    assert_eq!(data, b"# Hello World\nThis is a readme.\n");
}

#[tokio::test]
async fn read_file_from_provider() {
    let (vfs, main_rs) = make_vfs(None).await;
    let root = vfs.root_id();

    // Navigate to src/main.rs
    let (src_id, _) = vfs.lookup(root, "src").await.unwrap();
    let (main_id, _) = vfs.lookup(src_id, "main.rs").await.unwrap();

    let data = vfs.read(main_id, 0, 4096).await.unwrap();
    assert_eq!(data, main_rs);
}

#[tokio::test]
async fn read_with_offset() {
    let (vfs, _) = make_vfs(None).await;
    let root = vfs.root_id();
    let (id, _) = vfs.lookup(root, "README.md").await.unwrap();

    // Read 5 bytes starting at offset 2
    let data = vfs.read(id, 2, 5).await.unwrap();
    assert_eq!(data, b"Hello");
}

#[tokio::test]
async fn readdir_root() {
    let (vfs, _) = make_vfs(None).await;
    let entries = vfs.readdir(vfs.root_id()).await.unwrap();
    assert_eq!(entries.len(), 3);

    let names: Vec<&str> = entries.iter().map(|(_, n, _)| n.as_str()).collect();
    assert!(names.contains(&"README.md"));
    assert!(names.contains(&"src"));
    assert!(names.contains(&"link"));
}

#[tokio::test]
async fn readlink() {
    let (vfs, _) = make_vfs(None).await;
    let root = vfs.root_id();
    let (link_id, _) = vfs.lookup(root, "link").await.unwrap();

    let target = vfs.readlink(link_id).await.unwrap();
    assert_eq!(target, "README.md");
}

#[tokio::test]
async fn getattr_root() {
    let (vfs, _) = make_vfs(None).await;
    let attr = vfs.getattr(vfs.root_id()).await.unwrap();
    assert_eq!(attr.inode, 1);
    assert_eq!(attr.kind, NodeType::Directory);
}

#[tokio::test]
async fn subpath_reroots() {
    let (vfs, main_rs) = make_vfs(Some("src".into())).await;
    let root = vfs.root_id();

    // Root should now contain main.rs directly
    let entries = vfs.readdir(root).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, "main.rs");

    // Should be able to read main.rs from the re-rooted root
    let (main_id, _) = vfs.lookup(root, "main.rs").await.unwrap();
    let data = vfs.read(main_id, 0, 4096).await.unwrap();
    assert_eq!(data, main_rs);
}

#[tokio::test]
async fn lookup_populates_parent_inode() {
    let (provider, snapshot, _main_rs) = build_fixture();
    let cache = make_cache();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let root = vfs.root_id();

    // Root's parent is itself
    let root_attr = vfs.getattr(root).await.unwrap();
    assert_eq!(root_attr.parent_inode, root);

    // README.md's parent is root
    let (_readme_id, readme_attr) = vfs.lookup(root, "README.md").await.unwrap();
    assert_eq!(readme_attr.parent_inode, root);

    // src/main.rs: parent is src/, not root
    let (src_id, _) = vfs.lookup(root, "src").await.unwrap();
    let (_, main_rs_attr) = vfs.lookup(src_id, "main.rs").await.unwrap();
    assert_eq!(main_rs_attr.parent_inode, src_id);
    assert_ne!(main_rs_attr.parent_inode, root);
}
