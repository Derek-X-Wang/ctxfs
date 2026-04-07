//! Redis-backed [`SharedTreeCache`] implementation with zstd compression.

use async_trait::async_trait;
use ctxfs_cache::SharedTreeCache;
use redis::aio::ConnectionManager;
use redis::AsyncCommands;
use tracing::warn;

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
}

#[async_trait]
impl SharedTreeCache for RedisTreeCache {
    async fn get_tree(&self, owner: &str, repo: &str, commit_sha: &str) -> Option<Vec<u8>> {
        let key = Self::cache_key(owner, repo, commit_sha);
        let mut conn = self.client.clone();
        let compressed: Vec<u8> = conn
            .get(&key)
            .await
            .map_err(|e| warn!("redis get error for {key}: {e}"))
            .ok()?;
        if compressed.is_empty() {
            return None;
        }
        zstd::decode_all(compressed.as_slice())
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
        let mut conn = self.client.clone();
        let result: Result<(), _> = conn.set(&key, compressed).await;
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
}
