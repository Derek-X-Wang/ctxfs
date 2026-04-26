//! Redis-backed [`SharedTreeCache`] implementation with zstd compression.

use async_trait::async_trait;
use ctxfs_cache::{SharedTreeCache, SCHEMA_VERSION};
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use tracing::warn;

/// Length of the schema-version prefix prepended to every redis-cached
/// payload. Stored as a little-endian `u32`.
const VERSION_PREFIX_LEN: usize = 4;

/// Redis-backed tree cache with zstd compression.
///
/// Uses a [`ConnectionManager`] which transparently handles reconnections.
/// All errors are logged as warnings and silently dropped — callers fall back
/// to the GitHub API on cache miss.
#[derive(Clone)]
pub struct RedisTreeCache {
    client: ConnectionManager,
}

impl std::fmt::Debug for RedisTreeCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RedisTreeCache").finish_non_exhaustive()
    }
}

impl RedisTreeCache {
    /// Attempt to connect to the given Redis URL.
    ///
    /// Returns `None` if the connection cannot be established, allowing
    /// callers to degrade gracefully without a Redis instance.
    pub async fn connect(url: &str) -> Option<Self> {
        let client = redis::Client::open(url)
            .map_err(|e| warn!("redis: failed to parse URL: {e}"))
            .ok()?;
        let manager = ConnectionManager::new(client)
            .await
            .map_err(|e| warn!("redis: failed to connect: {e}"))
            .ok()?;
        Some(Self { client: manager })
    }

    /// Build the Redis key for a given tree.
    ///
    /// Format: `ctxfs:tree:{owner}/{repo}@{commit_sha}`
    fn cache_key(owner: &str, repo: &str, commit_sha: &str) -> String {
        format!("ctxfs:tree:{owner}/{repo}@{commit_sha}")
    }

    /// Prepend a 4-byte little-endian schema version to `compressed` so we can
    /// detect and discard payloads written by an older daemon after a manifest
    /// schema change. Pairs with [`Self::unwrap_versioned`].
    fn wrap_with_version(compressed: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(VERSION_PREFIX_LEN + compressed.len());
        out.extend_from_slice(&SCHEMA_VERSION.to_le_bytes());
        out.extend_from_slice(compressed);
        out
    }

    /// Inverse of [`Self::wrap_with_version`]. Returns the inner zstd-compressed
    /// bytes if the prefix matches the current `SCHEMA_VERSION`; otherwise
    /// returns `None` (treated as cache miss). Older payloads stored before the
    /// prefix was introduced (no version bytes at all) are also rejected — they
    /// will deserialize as a different `u32` and fail the equality check.
    fn unwrap_versioned(blob: &[u8]) -> Option<&[u8]> {
        if blob.len() < VERSION_PREFIX_LEN {
            return None;
        }
        let (version_bytes, rest) = blob.split_at(VERSION_PREFIX_LEN);
        let version = u32::from_le_bytes(version_bytes.try_into().ok()?);
        if version != SCHEMA_VERSION {
            return None;
        }
        Some(rest)
    }
}

#[async_trait]
impl SharedTreeCache for RedisTreeCache {
    async fn get_tree(&self, owner: &str, repo: &str, commit_sha: &str) -> Option<Vec<u8>> {
        let key = Self::cache_key(owner, repo, commit_sha);
        let mut conn = self.client.clone();
        let blob: Vec<u8> = conn
            .get(&key)
            .await
            .map_err(|e| warn!("redis get error for {key}: {e}"))
            .ok()?;
        if blob.is_empty() {
            return None;
        }
        // Reject payloads with a missing or stale schema-version prefix.
        // These are silently skipped (treated as cache miss) so the caller
        // falls back to a fresh fetch via the current code path.
        let compressed = Self::unwrap_versioned(&blob).or_else(|| {
            warn!(
                "redis: dropping cache entry for {key} with stale schema version (expected {SCHEMA_VERSION})"
            );
            None
        })?;
        zstd::decode_all(compressed)
            .map_err(|e| warn!("zstd decompress error for {key}: {e}"))
            .ok()
    }

    async fn put_tree(&self, owner: &str, repo: &str, commit_sha: &str, data: &[u8]) {
        let key = Self::cache_key(owner, repo, commit_sha);
        let compressed = match zstd::encode_all(data, 3) {
            Ok(c) => c,
            Err(e) => {
                warn!("zstd compress error for {key}: {e}");
                return;
            }
        };
        let payload = Self::wrap_with_version(&compressed);
        let mut conn = self.client.clone();
        let result: Result<(), _> = conn.set(&key, payload).await;
        if let Err(e) = result {
            warn!("redis set error for {key}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_format() {
        let key = RedisTreeCache::cache_key("octocat", "Hello-World", "abc123def456");
        assert_eq!(key, "ctxfs:tree:octocat/Hello-World@abc123def456");
    }

    #[test]
    fn zstd_roundtrip() {
        let original = b"snapshot data: {\"files\": [\"README.md\", \"src/main.rs\"]}";
        let compressed = zstd::encode_all(original.as_slice(), 3).unwrap();
        let decompressed = zstd::decode_all(compressed.as_slice()).unwrap();
        assert_eq!(decompressed, original);
    }

    #[test]
    fn version_prefix_roundtrips_current_schema() {
        let payload = b"compressed-snapshot-bytes";
        let wrapped = RedisTreeCache::wrap_with_version(payload);
        // First 4 bytes are the LE schema version.
        assert_eq!(
            &wrapped[..4],
            &SCHEMA_VERSION.to_le_bytes(),
            "wrap must prepend current SCHEMA_VERSION"
        );
        let unwrapped = RedisTreeCache::unwrap_versioned(&wrapped).expect("matching version");
        assert_eq!(unwrapped, payload);
    }

    #[test]
    fn unwrap_versioned_rejects_old_version() {
        // Simulate a payload written by a previous daemon version (v1).
        let mut blob = Vec::new();
        blob.extend_from_slice(&1u32.to_le_bytes());
        blob.extend_from_slice(b"old-payload");
        assert!(
            RedisTreeCache::unwrap_versioned(&blob).is_none(),
            "v1 payload must be rejected after M2 bump"
        );
    }

    #[test]
    fn unwrap_versioned_rejects_short_blob() {
        // Pre-prefix payloads (no version bytes at all) shorter than 4 bytes
        // are treated as cache miss.
        assert!(RedisTreeCache::unwrap_versioned(&[]).is_none());
        assert!(RedisTreeCache::unwrap_versioned(&[1, 2, 3]).is_none());
    }

    #[test]
    fn unwrap_versioned_rejects_pre_prefix_payloads() {
        // Per RFC 8478 § 3.1.1, zstd's magic value is 0xFD2FB528 stored
        // little-endian on disk as [0x28, 0xB5, 0x2F, 0xFD]. Reading those
        // bytes via u32::from_le_bytes recovers 0xFD2FB528 = 4_247_762_216,
        // which is ≠ SCHEMA_VERSION (2), so a pre-M2 raw-zstd payload is
        // correctly rejected as a stale entry.
        let pre_prefix = [0x28u8, 0xB5, 0x2F, 0xFD, /* body */ 0xAA, 0xBB];
        assert!(RedisTreeCache::unwrap_versioned(&pre_prefix).is_none());
    }
}
