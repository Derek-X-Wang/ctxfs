mod resolution;
pub use resolution::{CachedResolution, ResolutionCache};

mod tree;
pub use tree::TreeCache;

mod shared;
pub use shared::SharedTreeCache;

use ctxfs_core::error::CtxfsError;
use ctxfs_core::Digest;
use linked_hash_map::LinkedHashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

/// Entry in the LRU tracking state.
struct CacheEntry {
    size: u64,
}

struct LruState {
    entries: LinkedHashMap<String, CacheEntry>,
    total_bytes: u64,
}

pub struct BlobCache {
    root: PathBuf,
    max_bytes: u64,
    state: Mutex<LruState>,
}

impl std::fmt::Debug for BlobCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobCache")
            .field("root", &self.root)
            .field("max_bytes", &self.max_bytes)
            .field("state", &"<locked>")
            .finish()
    }
}

impl BlobCache {
    pub fn new(root: PathBuf, max_bytes: u64) -> Result<Self, CtxfsError> {
        fs::create_dir_all(&root)
            .map_err(|e| CtxfsError::Cache(format!("failed to create cache dir: {e}")))?;

        let cache = Self {
            root,
            max_bytes,
            state: Mutex::new(LruState {
                entries: LinkedHashMap::new(),
                total_bytes: 0,
            }),
        };

        cache.rebuild_index()?;
        Ok(cache)
    }

    fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.root.join(digest.to_path())
    }

    /// Remove the on-disk blob file for the given hex key.
    fn remove_blob_file(&self, hex: &str) {
        let digest = Digest::from_sha256_hex(hex);
        let path = self.blob_path(&digest);
        let _ = fs::remove_file(path);
    }

    pub fn get(&self, digest: &Digest) -> Option<Vec<u8>> {
        // Check LRU membership first to avoid a wasted syscall on cache miss
        let key = digest.hex.clone();
        {
            let mut state = self.state.lock().unwrap();
            let _ = state.entries.get_refresh(&key)?;
        }

        let path = self.blob_path(digest);
        fs::read(&path).ok()
    }

    pub fn put(&self, digest: &Digest, data: &[u8]) -> Result<(), CtxfsError> {
        let path = self.blob_path(digest);

        // Ensure parent dir exists
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CtxfsError::Cache(format!("mkdir failed: {e}")))?;
        }

        fs::write(&path, data).map_err(|e| CtxfsError::Cache(format!("write failed: {e}")))?;

        let size = data.len() as u64;
        let key = digest.hex.clone();
        let mut state = self.state.lock().unwrap();
        if let Some(existing) = state.entries.get(&key) {
            state.total_bytes -= existing.size;
        }
        let _ = state.entries.insert(key, CacheEntry { size });
        state.total_bytes += size;

        // Evict if over limit
        while state.total_bytes > self.max_bytes && !state.entries.is_empty() {
            if let Some((evicted_key, evicted_entry)) = state.entries.pop_front() {
                state.total_bytes -= evicted_entry.size;
                self.remove_blob_file(&evicted_key);
            }
        }

        Ok(())
    }

    pub fn contains(&self, digest: &Digest) -> bool {
        let state = self.state.lock().unwrap();
        state.entries.contains_key(&digest.hex)
    }

    pub fn prune(&self, max_bytes: Option<u64>) -> Result<u64, CtxfsError> {
        let limit = max_bytes.unwrap_or(self.max_bytes);
        let mut state = self.state.lock().unwrap();
        let mut freed = 0u64;

        while state.total_bytes > limit && !state.entries.is_empty() {
            if let Some((evicted_key, evicted_entry)) = state.entries.pop_front() {
                state.total_bytes -= evicted_entry.size;
                freed += evicted_entry.size;
                self.remove_blob_file(&evicted_key);
            }
        }

        Ok(freed)
    }

    pub fn stats(&self) -> (u64, usize) {
        let state = self.state.lock().unwrap();
        (state.total_bytes, state.entries.len())
    }

    /// Rebuild the LRU index by scanning the cache directory.
    fn rebuild_index(&self) -> Result<(), CtxfsError> {
        let mut entries: Vec<(String, u64, std::time::SystemTime)> = Vec::new();

        if let Ok(algo_dirs) = fs::read_dir(&self.root) {
            for algo_entry in algo_dirs.flatten() {
                let algo_path = algo_entry.path();
                if !algo_path.is_dir() {
                    continue;
                }
                Self::scan_fan_out_dir(&algo_path, &mut entries)?;
            }
        }

        // Sort by modification time (oldest first for LRU ordering)
        entries.sort_by_key(|(_, _, mtime)| *mtime);

        let mut state = self.state.lock().unwrap();
        state.entries.clear();
        state.total_bytes = 0;

        for (hex, size, _) in entries {
            state.total_bytes += size;
            let _ = state.entries.insert(hex, CacheEntry { size });
        }

        Ok(())
    }

    fn scan_fan_out_dir(
        algo_path: &Path,
        entries: &mut Vec<(String, u64, std::time::SystemTime)>,
    ) -> Result<(), CtxfsError> {
        let prefix_dirs =
            fs::read_dir(algo_path).map_err(|e| CtxfsError::Cache(format!("scan failed: {e}")))?;

        for prefix_entry in prefix_dirs.flatten() {
            let prefix_path = prefix_entry.path();
            if !prefix_path.is_dir() {
                continue;
            }
            let prefix = prefix_entry.file_name().to_string_lossy().to_string();

            let blob_files = fs::read_dir(&prefix_path)
                .map_err(|e| CtxfsError::Cache(format!("scan failed: {e}")))?;

            for blob_entry in blob_files.flatten() {
                let blob_path = blob_entry.path();
                if !blob_path.is_file() {
                    continue;
                }
                let suffix = blob_entry.file_name().to_string_lossy().to_string();
                let hex = format!("{prefix}{suffix}");

                if let Ok(metadata) = blob_path.metadata() {
                    let mtime = metadata
                        .modified()
                        .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
                    entries.push((hex, metadata.len(), mtime));
                }
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxfs_core::Digest;

    #[test]
    fn put_get_contains() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024 * 1024).unwrap();

        let digest = Digest::sha256(b"hello");
        assert!(!cache.contains(&digest));

        cache.put(&digest, b"hello").unwrap();
        assert!(cache.contains(&digest));

        let data = cache.get(&digest).unwrap();
        assert_eq!(data, b"hello");
    }

    #[test]
    fn lru_eviction() {
        let dir = tempfile::tempdir().unwrap();
        // Max 20 bytes
        let cache = BlobCache::new(dir.path().to_path_buf(), 20).unwrap();

        let d1 = Digest::sha256(b"first");
        let d2 = Digest::sha256(b"second");
        let d3 = Digest::sha256(b"third");

        cache.put(&d1, &[0u8; 10]).unwrap();
        cache.put(&d2, &[1u8; 10]).unwrap();
        // At 20 bytes, at capacity
        assert!(cache.contains(&d1));
        assert!(cache.contains(&d2));

        // This should evict d1
        cache.put(&d3, &[2u8; 10]).unwrap();
        assert!(!cache.contains(&d1));
        assert!(cache.contains(&d2));
        assert!(cache.contains(&d3));
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
        let digest = Digest::sha256(b"not stored");
        assert!(cache.get(&digest).is_none());
    }

    #[test]
    fn put_overwrite_same_digest() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();

        let digest = Digest::sha256(b"key");
        cache.put(&digest, b"first").unwrap();
        cache.put(&digest, b"second_longer").unwrap();

        let data = cache.get(&digest).unwrap();
        assert_eq!(data, b"second_longer");

        let (total, count) = cache.stats();
        assert_eq!(count, 1);
        assert_eq!(total, 13); // "second_longer".len()
    }

    #[test]
    fn stats_tracks_correctly() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024 * 1024).unwrap();

        let (total, count) = cache.stats();
        assert_eq!(total, 0);
        assert_eq!(count, 0);

        let d1 = Digest::sha256(b"a");
        cache.put(&d1, b"aaaa").unwrap();
        let (total, count) = cache.stats();
        assert_eq!(total, 4);
        assert_eq!(count, 1);

        let d2 = Digest::sha256(b"b");
        cache.put(&d2, b"bb").unwrap();
        let (total, count) = cache.stats();
        assert_eq!(total, 6);
        assert_eq!(count, 2);
    }

    #[test]
    fn prune_frees_space() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();

        for i in 0..10u8 {
            let digest = Digest::sha256(&[i]);
            cache.put(&digest, &[i; 100]).unwrap();
        }

        let (total_before, count_before) = cache.stats();
        assert_eq!(total_before, 1000);
        assert_eq!(count_before, 10);

        // Prune to 500 bytes
        let freed = cache.prune(Some(500)).unwrap();
        assert!(freed >= 500);

        let (total_after, count_after) = cache.stats();
        assert!(total_after <= 500);
        assert!(count_after < 10);
    }

    #[test]
    fn prune_no_op_when_under_limit() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();

        let digest = Digest::sha256(b"small");
        cache.put(&digest, b"tiny").unwrap();

        let freed = cache.prune(None).unwrap();
        assert_eq!(freed, 0);
        assert!(cache.contains(&digest));
    }

    #[test]
    fn rebuild_index_on_restart() {
        let dir = tempfile::tempdir().unwrap();

        // Populate cache
        {
            let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
            let d1 = Digest::sha256(b"persist1");
            let d2 = Digest::sha256(b"persist2");
            cache.put(&d1, b"data1").unwrap();
            cache.put(&d2, b"data2").unwrap();
        }

        // Re-open cache (simulates restart)
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
        let d1 = Digest::sha256(b"persist1");
        let d2 = Digest::sha256(b"persist2");
        assert!(cache.contains(&d1));
        assert!(cache.contains(&d2));
        assert_eq!(cache.get(&d1).unwrap(), b"data1");

        let (total, count) = cache.stats();
        assert_eq!(count, 2);
        assert_eq!(total, 10); // 5 + 5
    }

    #[test]
    fn eviction_removes_oldest_first() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 30).unwrap();

        let d1 = Digest::sha256(b"oldest");
        let d2 = Digest::sha256(b"middle");
        let d3 = Digest::sha256(b"newest");

        cache.put(&d1, &[0u8; 10]).unwrap();
        cache.put(&d2, &[1u8; 10]).unwrap();
        cache.put(&d3, &[2u8; 10]).unwrap();
        // All fit: 30 bytes

        // Access d1 to make it most-recently-used
        let _ = cache.get(&d1);

        // Add d4, which should evict d2 (oldest untouched)
        let d4 = Digest::sha256(b"evict_trigger");
        cache.put(&d4, &[3u8; 10]).unwrap();

        assert!(cache.contains(&d1), "d1 was accessed, should survive");
        assert!(!cache.contains(&d2), "d2 was oldest, should be evicted");
        assert!(cache.contains(&d3));
        assert!(cache.contains(&d4));
    }

    #[test]
    fn large_single_entry_evicts_everything() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 50).unwrap();

        let d1 = Digest::sha256(b"small1");
        let d2 = Digest::sha256(b"small2");
        cache.put(&d1, &[0u8; 20]).unwrap();
        cache.put(&d2, &[1u8; 20]).unwrap();

        // Insert one big entry that exceeds capacity
        let d3 = Digest::sha256(b"big");
        cache.put(&d3, &[2u8; 50]).unwrap();

        assert!(!cache.contains(&d1));
        assert!(!cache.contains(&d2));
        assert!(cache.contains(&d3));
    }

    #[test]
    fn debug_impl() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
        let debug = format!("{cache:?}");
        assert!(debug.contains("BlobCache"));
        assert!(debug.contains("max_bytes"));
    }
}
