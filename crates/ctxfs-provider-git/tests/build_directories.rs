//! Integration test: End-to-end snapshot construction from GitHub tree entries
//! through directory building, cache storage, and manifest deserialization.

#[path = "common/mod.rs"]
mod common;

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

// ── Assembled-path test (M2 carry-forward) ─────────────────────────────────
//
// Verifies that the full `fetch_snapshot_with_options` path (commit resolve →
// tree fetch → build_directories → cache store) produces a correctly assembled
// directory hierarchy accessible via cache lookups.

const AP_OWNER: &str = "ap-owner";
const AP_REPO: &str = "ap-repo";
const AP_GIT_REF: &str = "main";
const AP_COMMIT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const AP_TREE_SHA: &str = "tree444444444444444444444444444444444444";

#[tokio::test(flavor = "multi_thread")]
async fn assembled_path_snapshot_has_correct_directory_hierarchy() {
    use common::{commit_json, make_provider, tree_json, MockRoute, MockServer};
    use ctxfs_provider_common::fetcher::{PrefetchPolicy, TarballSingleflightMap};
    use ctxfs_provider_common::observability::Observability;
    use ctxfs_provider_git::FetchOptions;

    // Tree: README.md at root + src/ subtree with main.rs + lib.rs.
    let tree_entries: Vec<serde_json::Value> = vec![
        serde_json::json!({"path": "README.md", "mode": "100644", "type": "blob",
                           "sha": "sha_readme", "size": 100u64}),
        serde_json::json!({"path": "src",       "mode": "040000", "type": "tree",
                           "sha": "tree_src"}),
        serde_json::json!({"path": "src/main.rs","mode": "100644", "type": "blob",
                           "sha": "sha_main",   "size": 200u64}),
        serde_json::json!({"path": "src/lib.rs", "mode": "100644", "type": "blob",
                           "sha": "sha_lib",    "size": 150u64}),
    ];

    let server = MockServer::spawn(vec![
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{AP_OWNER}/{AP_REPO}/commits"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: commit_json(AP_COMMIT),
            hit_count: None,
            delay_ms: None,
        },
        MockRoute {
            method: "GET",
            path_prefix: format!("/repos/{AP_OWNER}/{AP_REPO}/git/trees"),
            status: 200,
            headers: vec![("Content-Type", "application/json".to_string())],
            body: tree_json(AP_TREE_SHA, &tree_entries, false),
            hit_count: None,
            delay_ms: None,
        },
    ])
    .await;

    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp.path().to_path_buf(), 64 * 1024 * 1024).unwrap());
    let obs = Arc::new(Observability::new());
    let sf = Arc::new(TarballSingleflightMap::new());
    let provider = make_provider(&server, cache.clone(), obs, sf);

    let source = SourceSpec::parse(&format!("github:{AP_OWNER}/{AP_REPO}@{AP_GIT_REF}")).unwrap();
    let opts = FetchOptions {
        prefetch: PrefetchPolicy::Disabled,
        prefetch_threshold_count: 30,
        prefetch_max_bytes: 256 * 1024 * 1024,
    };

    let snapshot_bytes = provider
        .fetch_snapshot_with_options(&source, &opts)
        .await
        .expect("fetch_snapshot_with_options must succeed");

    let snapshot: Snapshot = serde_json::from_slice(&snapshot_bytes).unwrap();
    assert_eq!(
        snapshot.commit_sha, AP_COMMIT,
        "snapshot must reference resolved commit"
    );

    // ── Root directory has README.md and src/ ──────────────────────────────
    let root_data = cache
        .get(&snapshot.root_directory)
        .expect("root directory digest must be in cache");
    let root_dir: Directory = serde_json::from_slice(&root_data).unwrap();
    let root_names: Vec<&str> = root_dir.entries.iter().map(DirEntry::name).collect();
    assert!(
        root_names.contains(&"README.md"),
        "root must contain README.md; got {root_names:?}"
    );
    assert!(
        root_names.contains(&"src"),
        "root must contain src/; got {root_names:?}"
    );

    // ── src/ directory has main.rs and lib.rs ──────────────────────────────
    let src_entry = root_dir
        .entries
        .iter()
        .find(|e| e.name() == "src")
        .expect("src entry must exist");
    let src_digest = match src_entry {
        DirEntry::Directory(d) => d.digest.clone(),
        _ => panic!("expected DirEntry::Directory for 'src'"),
    };
    let src_data = cache
        .get(&src_digest)
        .expect("src/ directory digest must be in cache");
    let src_dir: Directory = serde_json::from_slice(&src_data).unwrap();
    let src_names: Vec<&str> = src_dir.entries.iter().map(DirEntry::name).collect();
    assert!(
        src_names.contains(&"main.rs"),
        "src/ must contain main.rs; got {src_names:?}"
    );
    assert!(
        src_names.contains(&"lib.rs"),
        "src/ must contain lib.rs; got {src_names:?}"
    );
}
