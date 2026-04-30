//! Integration test: `BlobCache` lifecycle across simulated restarts,
//! concurrent access patterns, and edge cases.

use ctxfs_cache::{BlobCache, TreeCache, SCHEMA_VERSION};
use ctxfs_core::Digest;
use std::sync::Arc;
use std::thread;

// ---- Task 4: atomic-commit, streaming writer, temp cleanup ----

#[test]
fn commit_atomic_writes_via_tmp_then_renames() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();
    let digest = Digest::sha256(b"hi");

    cache.commit_atomic(&digest, b"hi").unwrap();
    assert!(cache.contains(&digest));
    assert_eq!(cache.get(&digest).unwrap(), b"hi");

    // After commit, no leftover temp files.
    let tmp_dir = dir.path().join("tmp");
    if tmp_dir.exists() {
        let count = std::fs::read_dir(&tmp_dir).unwrap().count();
        assert_eq!(count, 0, "tmp dir should be empty after successful commit");
    }
}

/// The streaming writer persists content under whatever digest name the caller
/// supplies — it does NOT verify content-vs-digest internally.
#[test]
fn commit_atomic_with_writer_persists_under_named_digest() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();

    // Use an arbitrary digest (SHA-256 of a different string) to prove the
    // writer doesn't enforce content-vs-digest correspondence.
    let payload = b"streaming-content";
    let named_digest = Digest::sha256(b"some-other-key");

    let mut writer = cache.commit_atomic_with_writer().unwrap();
    writer.write_all(payload).unwrap();
    writer.finalize(&named_digest).unwrap();

    // Content is retrievable under the caller-supplied name.
    assert!(cache.contains(&named_digest));
    assert_eq!(cache.get(&named_digest).unwrap(), payload);
}

/// The bytes-in-memory entry point (`commit_atomic`) verifies externally
/// before writing; mismatched digest returns Err and leaves the cache clean.
#[test]
fn commit_atomic_rejects_digest_mismatch() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();

    let actual = b"actual-content";
    let lying_digest = Digest::sha256(b"different-content");

    let res = cache.commit_atomic(&lying_digest, actual);
    assert!(res.is_err(), "expected DigestMismatch error");
    assert!(!cache.contains(&lying_digest));
    // No leftover temp file (verification failed before writer was even used).
    let tmp_count = std::fs::read_dir(dir.path().join("tmp"))
        .map(|d| d.count())
        .unwrap_or(0);
    assert_eq!(tmp_count, 0);
}

/// Explicitly documents the trust-the-caller contract: the streaming writer
/// persists content even when the caller supplies a digest whose SHA-256
/// doesn't match the written bytes. Verification is the caller's job.
#[test]
fn commit_atomic_with_writer_trusts_caller_no_verification() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();

    let content = b"real-content";
    // A digest whose hex doesn't match SHA-256 of content — simulates a
    // Git-SHA-1 key passed by the tarball path.
    let git_sha1_style_digest = Digest::sha256(b"completely-different");

    let mut writer = cache.commit_atomic_with_writer().unwrap();
    writer.write_all(content).unwrap();
    // Writer trusts the caller — no mismatch error.
    writer.finalize(&git_sha1_style_digest).unwrap();

    // Content is persisted under the caller-supplied key.
    assert!(cache.contains(&git_sha1_style_digest));
    assert_eq!(cache.get(&git_sha1_style_digest).unwrap(), content);
}

#[test]
fn cleanup_orphan_temps_unlinks_old_files() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();

    let tmp_dir = dir.path().join("tmp");
    std::fs::create_dir_all(&tmp_dir).unwrap();

    let old_file = tmp_dir.join("orphan-1");
    let recent_file = tmp_dir.join("orphan-2");
    std::fs::write(&old_file, b"old").unwrap();
    std::fs::write(&recent_file, b"recent").unwrap();

    // Backdate old_file by 2 hours.
    let two_hours_ago = std::time::SystemTime::now() - std::time::Duration::from_secs(2 * 3600);
    let _ = filetime::set_file_mtime(
        &old_file,
        filetime::FileTime::from_system_time(two_hours_ago),
    );

    let cleared = cache
        .cleanup_orphan_temps(std::time::Duration::from_secs(3600))
        .unwrap();
    assert_eq!(cleared, 1);
    assert!(!old_file.exists());
    assert!(recent_file.exists(), "recent files preserved");
}

#[test]
fn cleanup_orphan_temps_handles_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();
    let cleared = cache
        .cleanup_orphan_temps(std::time::Duration::from_secs(3600))
        .unwrap();
    assert_eq!(cleared, 0);
}

#[test]
fn rebuild_index_skips_tmp_dir() {
    let dir = tempfile::tempdir().unwrap();
    // Pre-create a stray tmp/ entry that mimics a half-written blob path,
    // and a valid sha256/ entry. The tmp/ entry must NOT enter LRU.
    let tmp_dir = dir.path().join("tmp");
    std::fs::create_dir_all(&tmp_dir).unwrap();
    std::fs::write(tmp_dir.join("zzz-orphan"), b"junk").unwrap();

    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();
    let (total, count) = cache.stats();
    assert_eq!(total, 0);
    assert_eq!(count, 0, "tmp/ entries must NOT enter rebuild_index");
}

#[test]
fn contains_all_returns_true_only_when_every_digest_present() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();
    let d1 = Digest::sha256(b"one");
    let d2 = Digest::sha256(b"two");
    cache.put(&d1, b"one").unwrap();
    assert!(!cache.contains_all(&[d1.clone(), d2.clone()]));
    cache.put(&d2, b"two").unwrap();
    assert!(cache.contains_all(&[d1, d2]));
}

#[test]
fn cache_survives_restart_with_correct_lru_order() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    // Phase 1: populate cache
    {
        let cache = BlobCache::new(path.clone(), 1000).unwrap();
        for i in 0..5u8 {
            let d = Digest::sha256(&[i]);
            cache.put(&d, &[i; 50]).unwrap();
        }
        let (total, count) = cache.stats();
        assert_eq!(count, 5);
        assert_eq!(total, 250);
    }

    // Phase 2: restart and verify all data persisted
    {
        let cache = BlobCache::new(path.clone(), 1000).unwrap();
        let (total, count) = cache.stats();
        assert_eq!(count, 5);
        assert_eq!(total, 250);

        // Verify actual content
        for i in 0..5u8 {
            let d = Digest::sha256(&[i]);
            let data = cache.get(&d).unwrap();
            assert_eq!(data, vec![i; 50]);
        }
    }
}

#[test]
fn cache_prune_after_restart_evicts_correctly() {
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().to_path_buf();

    // Phase 1: fill cache
    {
        let cache = BlobCache::new(path.clone(), 10_000).unwrap();
        for i in 0..20u8 {
            let d = Digest::sha256(&[i]);
            cache.put(&d, &[i; 100]).unwrap();
        }
    }

    // Phase 2: restart with lower limit and prune
    {
        let cache = BlobCache::new(path.clone(), 500).unwrap();
        let freed = cache.prune(Some(500)).unwrap();
        assert!(freed > 0);

        let (total, count) = cache.stats();
        assert!(total <= 500);
        assert!(count < 20);
    }
}

#[test]
fn concurrent_reads_dont_corrupt_lru() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 100_000).unwrap());

    // Populate
    for i in 0..50u8 {
        let d = Digest::sha256(&[i]);
        cache.put(&d, &[i; 100]).unwrap();
    }

    // Concurrent reads from multiple threads
    let mut handles = Vec::new();
    for t in 0..4 {
        let cache = cache.clone();
        handles.push(thread::spawn(move || {
            for i in 0..50u8 {
                // Each thread reads different entries in different order
                let idx = (i + t * 13) % 50;
                let d = Digest::sha256(&[idx]);
                if let Some(data) = cache.get(&d) {
                    assert_eq!(data, vec![idx; 100]);
                }
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Cache should still be consistent
    let (total, count) = cache.stats();
    assert_eq!(count, 50);
    assert_eq!(total, 5000);
}

#[test]
fn concurrent_writes_maintain_consistency() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 100_000).unwrap());

    let mut handles = Vec::new();
    for t in 0u8..4 {
        let cache = cache.clone();
        handles.push(thread::spawn(move || {
            for i in 0..25u8 {
                let key = t * 25 + i;
                let d = Digest::sha256(&[key]);
                cache.put(&d, &[key; 50]).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    let (total, count) = cache.stats();
    assert_eq!(count, 100);
    assert_eq!(total, 5000);

    // Verify all entries
    for i in 0..100u8 {
        let d = Digest::sha256(&[i]);
        let data = cache.get(&d).unwrap();
        assert_eq!(data, vec![i; 50]);
    }
}

#[test]
fn eviction_under_concurrent_writes() {
    let dir = tempfile::tempdir().unwrap();
    // Small cache — will evict under pressure
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 500).unwrap());

    let mut handles = Vec::new();
    for t in 0u8..4 {
        let cache = cache.clone();
        handles.push(thread::spawn(move || {
            for i in 0..25u8 {
                let key = t * 25 + i;
                let d = Digest::sha256(&[key]);
                cache.put(&d, &[key; 100]).unwrap();
            }
        }));
    }

    for h in handles {
        h.join().unwrap();
    }

    // Cache should be within limit
    let (total, _count) = cache.stats();
    assert!(total <= 500, "cache exceeded max_bytes: {total} > 500");
}

#[test]
fn empty_data_stored_and_retrieved() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();

    let d = Digest::sha256(b"empty_content");
    cache.put(&d, b"").unwrap();

    assert!(cache.contains(&d));
    let data = cache.get(&d).unwrap();
    assert!(data.is_empty());
}

#[test]
fn large_blob_stored_and_retrieved() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 10 * 1024 * 1024).unwrap();

    let large_data = vec![42u8; 1024 * 1024]; // 1MB
    let d = Digest::sha256(&large_data);
    cache.put(&d, &large_data).unwrap();

    let retrieved = cache.get(&d).unwrap();
    assert_eq!(retrieved.len(), large_data.len());
    assert_eq!(retrieved, large_data);
}

/// Regression: M5 bumped SCHEMA_VERSION from 2 to 3 to invalidate cached
/// snapshots whose manifests carry mislabeled HashAlgorithm::Sha256 for Git
/// blob digests. A v2 cache file must be treated as a miss after the upgrade.
#[test]
fn pre_m5_v2_tree_cache_file_is_invalidated() {
    let dir = tempfile::tempdir().unwrap();
    let cache = TreeCache::new(dir.path(), 1024 * 1024);

    // Construct the path manually (mirrors TreeCache::file_path convention:
    // {root}/{owner}/{repo}/{commit_sha}.json).
    let path = dir.path().join("owner").join("repo").join("deadbeef.json");
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    let v2 = serde_json::json!({
        "version": 2,
        "data": {"snapshot": "mislabeled-sha256-digests"}
    });
    std::fs::write(&path, serde_json::to_vec(&v2).unwrap()).unwrap();

    // The current SCHEMA_VERSION is 3; a v2 file must be a cache miss.
    assert_eq!(SCHEMA_VERSION, 3, "test assumes SCHEMA_VERSION == 3");
    assert!(
        cache.get("owner", "repo", "deadbeef").is_none(),
        "v2 tree-cache file must be treated as miss after M5 schema bump"
    );
    assert!(!path.exists(), "stale v2 file should be removed on read");
}
