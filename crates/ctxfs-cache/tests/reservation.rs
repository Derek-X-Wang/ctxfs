//! B5 reservation integration tests: eviction-skip under real cache pressure.
//!
//! These are integration tests (not unit tests) because they exercise the full
//! BlobCache → lru_insert_evict → eviction-skip loop, which requires writing
//! real files on disk via `BlobCache::put`.

use ctxfs_cache::{BlobCache, RepoKey};
use ctxfs_core::Digest;
use std::sync::Arc;

fn repo_key(name: &str) -> RepoKey {
    RepoKey::new("api.github.com", "owner", name)
}

/// Helper: make a 40-hex-char digest that won't collide with other test blobs.
/// `prefix` is 2 uppercase hex chars (e.g. "aa"); `idx` distinguishes blobs
/// within a repo.
fn make_digest(prefix: &str, idx: u8) -> Digest {
    let hex = format!("{}{:038x}", prefix, idx as u32);
    assert_eq!(hex.len(), 40, "digest hex must be 40 chars");
    Digest::from_sha1_hex(&hex)
}

/// Active repo within its reservation receives zero evictions when another repo
/// pushes total usage past the cache limit.
///
/// Setup: cache = 500 bytes. Repo A has reservation = 400 bytes, writes 3 blobs
/// × 100 bytes = 300 bytes (WS ≤ reservation). Repo B writes 3 blobs × 100 bytes
/// → total 600 > 500. The 100 bytes evicted must come from Repo B, not Repo A.
#[test]
fn active_repo_within_reservation_receives_zero_evictions_from_other_repo() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 500).unwrap());

    let repo_a = repo_key("repo-a");
    let repo_b = repo_key("repo-b");

    // Repo A: write 3 blobs of 100 bytes each = 300 bytes.
    let a_digests: Vec<Digest> = (0..3u8).map(|i| make_digest("aa", i)).collect();
    let a_hexes: Vec<String> = a_digests.iter().map(|d| d.hex.clone()).collect();

    for (i, d) in a_digests.iter().enumerate() {
        cache.put(d, &[i as u8; 100]).unwrap();
    }
    // Sanity: 300 bytes in cache.
    assert_eq!(cache.total_bytes(), 300);

    // Register Repo A with a 400-byte reservation and its manifest digests.
    // register_mount must be called AFTER the blobs are in cache so that
    // working_set_bytes == 300 is visible at eviction time.
    cache.register_mount(&repo_a, Some(400), &a_hexes);
    assert_eq!(cache.working_set_bytes(&repo_a), 300);

    // Repo B: register with no reservation, then write 3 × 100 bytes → total 600.
    cache.register_mount(&repo_b, None, &[]);
    let b_digests: Vec<Digest> = (0..3u8).map(|i| make_digest("bb", i)).collect();
    for (i, d) in b_digests.iter().enumerate() {
        cache.put(d, &[(i as u8) | 0x10; 100]).unwrap();
    }

    // All 3 of Repo A's blobs must survive (WS=300 ≤ reservation=400).
    for (i, d) in a_digests.iter().enumerate() {
        assert!(
            cache.contains(d),
            "Repo A blob {i} (hex {}) must survive eviction (protected by reservation)",
            d.hex
        );
    }

    // At least one eviction-skip was recorded.
    assert!(
        cache.eviction_attempts_blocked_by_reservation() > 0,
        "eviction_blocked_total must be > 0 when reservation protection fired"
    );
}

/// A repo that has written more than its reservation can lose blobs under pressure.
///
/// Setup: cache = 400 bytes. Repo C: reservation = 200 bytes, writes 4 blobs × 100
/// bytes = 400 bytes (WS=400 > reservation=200). Repo D writes 1 blob × 100 bytes
/// → total 500 > 400. Repo C is over budget so its oldest blobs are evicted.
#[test]
fn over_reservation_repo_loses_blobs_on_pressure() {
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 400).unwrap());

    let repo_c = repo_key("repo-c");
    let repo_d = repo_key("repo-d");

    // Repo C: write 4 × 100 = 400 bytes (fills cache exactly).
    let c_digests: Vec<Digest> = (0..4u8).map(|i| make_digest("cc", i)).collect();
    let c_hexes: Vec<String> = c_digests.iter().map(|d| d.hex.clone()).collect();
    for (i, d) in c_digests.iter().enumerate() {
        cache.put(d, &[i as u8; 100]).unwrap();
    }
    assert_eq!(cache.total_bytes(), 400);

    // Repo C: reservation = 200 (half of WS; over-reservation).
    cache.register_mount(&repo_c, Some(200), &c_hexes);
    assert_eq!(cache.working_set_bytes(&repo_c), 400);

    // Repo D writes 1 × 100 bytes → pushes total to 500 > 400.
    cache.register_mount(&repo_d, None, &[]);
    let d0 = make_digest("dd", 0);
    cache.put(&d0, &[0xd0u8; 100]).unwrap();

    // Repo C's blobs must have shrunk: at least one was evicted.
    let c_remaining: usize = c_digests.iter().filter(|d| cache.contains(d)).count();
    assert!(
        c_remaining < 4,
        "at least one Repo C blob must be evicted when WS ({}) > reservation (200)",
        cache.working_set_bytes(&repo_c)
    );
}
