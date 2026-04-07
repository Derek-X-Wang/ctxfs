//! Integration test: End-to-end snapshot construction from GitHub tree entries
//! through directory building, cache storage, and manifest deserialization.

use ctxfs_cache::BlobCache;
use ctxfs_core::source::SourceSpec;
use ctxfs_manifest::{DirEntry, Directory, Snapshot};
use ctxfs_provider_git::GitHubProvider;
use std::sync::Arc;

/// Create a cache with a temporary directory.
/// Returns `(cache, tempdir)` — keep the tempdir alive for the test duration.
fn test_cache() -> (Arc<BlobCache>, tempfile::TempDir) {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 10 * 1024 * 1024).unwrap());
    (cache, dir)
}

fn source() -> SourceSpec {
    SourceSpec::parse("github:test/repo@main").unwrap()
}

/// Simulate what `fetch_snapshot` does: build directories, serialize, store in cache.
#[test]
fn snapshot_round_trip_through_cache() {
    let (cache, _dir) = test_cache();
    let src = source();

    // Build a snapshot from tree entries (simulating GitHub API response)
    let entries = make_tree_entries();
    let (root_digest, directories) = GitHubProvider::build_directories(&entries, &src);

    // Store all directories in cache (as the provider does)
    for dir in directories.values() {
        let json = serde_json::to_vec(dir).unwrap();
        cache.put(&dir.digest, &json).unwrap();
    }

    // Build and serialize the snapshot
    let snapshot = Snapshot {
        source: src.to_string(),
        commit_sha: "abc123def456".into(),
        root_directory: root_digest.clone(),
        created_at: "2025-01-01T00:00:00Z".into(),
    };
    let snapshot_json = serde_json::to_vec(&snapshot).unwrap();

    // Now retrieve: deserialize snapshot, fetch root directory from cache
    let snap2: Snapshot = serde_json::from_slice(&snapshot_json).unwrap();
    assert_eq!(snap2.root_directory, root_digest);

    let root_data = cache.get(&snap2.root_directory).unwrap();
    let root_dir: Directory = serde_json::from_slice(&root_data).unwrap();

    // Verify root has expected entries
    let names: Vec<&str> = root_dir.entries.iter().map(DirEntry::name).collect();
    assert!(names.contains(&"README.md"));
    assert!(names.contains(&"src"));
}

/// Verify nested directory resolution through cache lookups.
#[test]
fn nested_directory_resolution_through_cache() {
    let (cache, _dir) = test_cache();
    let src = source();

    let entries = make_tree_entries();
    let (root_digest, directories) = GitHubProvider::build_directories(&entries, &src);

    // Store all in cache
    for dir in directories.values() {
        let json = serde_json::to_vec(dir).unwrap();
        cache.put(&dir.digest, &json).unwrap();
    }

    // Fetch root
    let root_data = cache.get(&root_digest).unwrap();
    let root_dir: Directory = serde_json::from_slice(&root_data).unwrap();

    // Find "src" directory entry
    let src_entry = root_dir.entries.iter().find(|e| e.name() == "src").unwrap();

    let src_digest = match src_entry {
        DirEntry::Directory(d) => &d.digest,
        _ => panic!("expected directory entry for 'src'"),
    };

    // Fetch src directory from cache using the digest from the parent's DirectoryEntry
    let src_data = cache.get(src_digest).unwrap();
    let src_dir: Directory = serde_json::from_slice(&src_data).unwrap();

    // Verify src has the expected files
    let names: Vec<&str> = src_dir.entries.iter().map(DirEntry::name).collect();
    assert!(names.contains(&"main.rs"));
    assert!(names.contains(&"lib.rs"));
}

/// Verify file digests are content-addressable (Git SHA based).
#[test]
fn file_entries_have_git_sha_digests() {
    let src = source();
    let entries = make_tree_entries();
    let (_root_digest, directories) = GitHubProvider::build_directories(&entries, &src);

    let root = &directories[""];
    let readme = root
        .entries
        .iter()
        .find(|e| e.name() == "README.md")
        .unwrap();

    if let DirEntry::File(f) = readme {
        // Digest should be the Git SHA from the tree entry
        assert_eq!(f.digest.hex, "sha_readme");
        assert_eq!(f.size, 500);
        assert!(!f.executable);
    } else {
        panic!("expected file");
    }
}

/// Verify symlinks are constructed correctly.
#[test]
fn symlink_entries_parsed() {
    let src = source();

    let entries = vec![
        make_blob("link", "120000", "sha_link", Some(20)),
        make_blob("README.md", "100644", "sha_readme", Some(100)),
    ];

    let (_root_digest, directories) = GitHubProvider::build_directories(&entries, &src);
    let root = &directories[""];

    let link = root.entries.iter().find(|e| e.name() == "link").unwrap();
    match link {
        DirEntry::Symlink(s) => {
            assert_eq!(s.name, "link");
        }
        _ => panic!("expected symlink for mode 120000"),
    }
}

/// Verify deterministic digest computation for the same tree.
#[test]
fn directory_digests_are_deterministic() {
    let src = source();
    let entries = make_tree_entries();

    let (d1, dirs1) = GitHubProvider::build_directories(&entries, &src);
    let (d2, dirs2) = GitHubProvider::build_directories(&entries, &src);

    assert_eq!(d1, d2);
    assert_eq!(dirs1.len(), dirs2.len());

    for (path, dir1) in &dirs1 {
        let stored2 = &dirs2[path];
        assert_eq!(dir1.digest, stored2.digest);
    }
}

/// Verify deeply nested paths build correct hierarchy.
#[test]
fn deeply_nested_structure() {
    let src = source();

    let entries = vec![
        make_tree("a"),
        make_tree("a/b"),
        make_tree("a/b/c"),
        make_blob("a/b/c/deep.txt", "100644", "sha_deep", Some(10)),
    ];

    let (_root_digest, directories) = GitHubProvider::build_directories(&entries, &src);

    // Should have 4 directories: "", "a", "a/b", "a/b/c"
    assert!(directories.contains_key(""));
    assert!(directories.contains_key("a"));
    assert!(directories.contains_key("a/b"));
    assert!(directories.contains_key("a/b/c"));

    // a/b/c should contain deep.txt
    let abc = &directories["a/b/c"];
    assert_eq!(abc.entries.len(), 1);
    assert_eq!(abc.entries[0].name(), "deep.txt");
}

// --- Test fixtures ---

fn make_tree_entries() -> Vec<ctxfs_provider_git::TreeEntry> {
    vec![
        make_tree("src"),
        make_blob("README.md", "100644", "sha_readme", Some(500)),
        make_blob("src/main.rs", "100644", "sha_main", Some(300)),
        make_blob("src/lib.rs", "100644", "sha_lib", Some(200)),
        make_blob("run.sh", "100755", "sha_run", Some(50)),
    ]
}

fn make_blob(
    path: &str,
    mode: &str,
    sha: &str,
    size: Option<u64>,
) -> ctxfs_provider_git::TreeEntry {
    ctxfs_provider_git::TreeEntry {
        path: path.into(),
        mode: mode.into(),
        entry_type: "blob".into(),
        sha: sha.into(),
        size,
    }
}

fn make_tree(path: &str) -> ctxfs_provider_git::TreeEntry {
    ctxfs_provider_git::TreeEntry {
        path: path.into(),
        mode: "040000".into(),
        entry_type: "tree".into(),
        sha: format!("tree_{path}"),
        size: None,
    }
}
