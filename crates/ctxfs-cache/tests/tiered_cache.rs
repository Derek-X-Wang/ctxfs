//! Integration tests for `ResolutionCache` and `TreeCache` lifecycle.

use ctxfs_cache::{ResolutionCache, TreeCache};
use ctxfs_provider_common::resolver::ResolvedSource;
use tempfile::tempdir;

// ── helpers ───────────────────────────────────────────────────────────────────

fn react_source() -> ResolvedSource {
    ResolvedSource {
        owner: "facebook".to_string(),
        repo: "react".to_string(),
        git_ref: "v19.1.0".to_string(),
        subpath: None,
    }
}

fn requests_source() -> ResolvedSource {
    ResolvedSource {
        owner: "psf".to_string(),
        repo: "requests".to_string(),
        git_ref: "v2.31.0".to_string(),
        subpath: Some("src/requests".to_string()),
    }
}

// ── ResolutionCache ───────────────────────────────────────────────────────────

#[test]
fn resolution_cache_full_lifecycle() {
    let dir = tempdir().unwrap();
    let path = dir.path().join("resolution.json");

    // Phase 1: populate the cache.
    {
        let mut cache = ResolutionCache::new(path.clone(), 3600);

        cache
            .put(
                "npm:react@19.1.0".to_string(),
                react_source(),
                false, // pinned version
            )
            .unwrap();

        cache
            .put(
                "pypi:requests@2.31.0".to_string(),
                requests_source(),
                false, // pinned version
            )
            .unwrap();

        assert_eq!(cache.entry_count(), 2);
    } // cache dropped here

    // Phase 2: reload from disk, verify both entries survive.
    let cache = ResolutionCache::load(path, 3600);

    assert_eq!(cache.entry_count(), 2, "both entries should survive reload");

    let got_react = cache
        .get("npm:react@19.1.0")
        .expect("react entry should be present after reload");
    assert_eq!(got_react.owner, "facebook");
    assert_eq!(got_react.repo, "react");
    assert_eq!(got_react.git_ref, "v19.1.0");
    assert!(got_react.subpath.is_none());

    let got_requests = cache
        .get("pypi:requests@2.31.0")
        .expect("requests entry should be present after reload");
    assert_eq!(got_requests.owner, "psf");
    assert_eq!(got_requests.repo, "requests");
    assert_eq!(got_requests.git_ref, "v2.31.0");
    assert_eq!(
        got_requests.subpath.as_deref(),
        Some("src/requests"),
        "subpath must round-trip correctly"
    );
}

// ── TreeCache ─────────────────────────────────────────────────────────────────

#[test]
fn tree_cache_full_lifecycle() {
    let dir = tempdir().unwrap();
    let cache = TreeCache::new(dir.path(), 1024 * 1024);

    let data = serde_json::json!({
        "commit_sha": "abc123",
        "tree": [
            {"path": "README.md", "type": "blob"},
            {"path": "src",       "type": "tree"}
        ]
    });
    let data_bytes = serde_json::to_vec(&data).unwrap();

    // Put and immediately retrieve.
    cache
        .put("facebook", "react", "abc123", &data_bytes)
        .unwrap();

    let got = cache
        .get("facebook", "react", "abc123")
        .expect("entry should be present");
    let got_val: serde_json::Value = serde_json::from_slice(&got).unwrap();
    assert_eq!(got_val["commit_sha"], "abc123");
    assert_eq!(got_val["tree"].as_array().unwrap().len(), 2);

    // Stats: one entry, non-zero size.
    let (count, size) = cache.stats();
    assert_eq!(count, 1);
    assert!(size > 0, "stored size must be > 0");

    // Miss for a different SHA.
    assert!(
        cache.get("facebook", "react", "deadbeef").is_none(),
        "different SHA must not match"
    );

    // prune_all → cache is empty.
    cache.prune_all().unwrap();
    let (count_after, size_after) = cache.stats();
    assert_eq!(count_after, 0, "prune_all must remove all entries");
    assert_eq!(size_after, 0);
}

#[test]
fn tree_cache_survives_restart() {
    let dir = tempdir().unwrap();

    let data = serde_json::json!({"commit_sha": "abc123", "files": 42});
    let data_bytes = serde_json::to_vec(&data).unwrap();

    // Populate, then drop.
    {
        let cache = TreeCache::new(dir.path(), 1024 * 1024);
        cache
            .put("facebook", "react", "abc123", &data_bytes)
            .unwrap();
    }

    // New instance pointing at the same directory.
    let cache2 = TreeCache::new(dir.path(), 1024 * 1024);
    let got = cache2
        .get("facebook", "react", "abc123")
        .expect("data must survive across TreeCache instances");

    let got_val: serde_json::Value = serde_json::from_slice(&got).unwrap();
    assert_eq!(got_val["commit_sha"], "abc123");
    assert_eq!(got_val["files"], 42);
}
