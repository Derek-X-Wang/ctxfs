use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub struct TreeCache {
    root: PathBuf,
    max_bytes: u64,
}

/// Versioned on-disk format — allows detecting stale cache after upgrades.
#[derive(serde::Serialize, serde::Deserialize)]
struct VersionedTree {
    version: u32,
    data: serde_json::Value,
}

impl TreeCache {
    pub fn new(root: impl Into<PathBuf>, max_bytes: u64) -> Self {
        Self {
            root: root.into(),
            max_bytes,
        }
    }

    fn file_path(&self, owner: &str, repo: &str, commit_sha: &str) -> PathBuf {
        self.root
            .join(owner)
            .join(repo)
            .join(format!("{commit_sha}.json"))
    }

    pub fn get(&self, owner: &str, repo: &str, commit_sha: &str) -> Option<Vec<u8>> {
        let path = self.file_path(owner, repo, commit_sha);
        let raw = fs::read(&path).ok()?;

        let versioned: VersionedTree = serde_json::from_slice(&raw).ok()?;
        if versioned.version != SCHEMA_VERSION {
            let _ = fs::remove_file(&path);
            return None;
        }

        serde_json::to_vec(&versioned.data).ok()
    }

    pub fn put(
        &self,
        owner: &str,
        repo: &str,
        commit_sha: &str,
        data: &[u8],
    ) -> Result<(), io::Error> {
        let path = self.file_path(owner, repo, commit_sha);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        // Try to parse as JSON; fall back to base64-encoded string.
        let json_data: serde_json::Value = serde_json::from_slice(data).unwrap_or_else(|_| {
            use base64::Engine as _;
            serde_json::Value::String(base64::engine::general_purpose::STANDARD.encode(data))
        });

        let versioned = VersionedTree {
            version: SCHEMA_VERSION,
            data: json_data,
        };

        let serialized = serde_json::to_vec(&versioned)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?;

        // Atomic write: write to tmp then rename.
        let tmp_path = path.with_extension("tmp");
        fs::write(&tmp_path, &serialized)?;
        fs::rename(&tmp_path, &path)?;

        self.maybe_evict();
        Ok(())
    }

    /// Returns `(count, total_bytes)` of all `.json` files under root.
    pub fn stats(&self) -> (usize, u64) {
        let files = self.walk_files();
        let total: u64 = files.iter().map(|(_, size, _)| size).sum();
        (files.len(), total)
    }

    /// Remove all cached trees and recreate the root directory.
    pub fn prune_all(&self) -> Result<(), io::Error> {
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
        }
        fs::create_dir_all(&self.root)?;
        Ok(())
    }

    /// Walk the cache tree and collect `(path, size, mtime)` for every `.json` file.
    fn walk_files(&self) -> Vec<(PathBuf, u64, SystemTime)> {
        let mut result = Vec::new();
        if self.root.is_dir() {
            walk_dir_recursive(&self.root, &mut result);
        }
        result
    }

    /// If total cache size exceeds `max_bytes`, delete oldest files by mtime until under limit.
    fn maybe_evict(&self) {
        let mut files = self.walk_files();
        let total: u64 = files.iter().map(|(_, size, _)| size).sum();

        if total <= self.max_bytes {
            return;
        }

        // Sort oldest first.
        files.sort_by_key(|(_, _, mtime)| *mtime);

        let mut remaining = total;
        for (path, size, _) in &files {
            if remaining <= self.max_bytes {
                break;
            }
            let _ = fs::remove_file(path);
            remaining = remaining.saturating_sub(*size);
        }
    }
}

fn walk_dir_recursive(dir: &Path, out: &mut Vec<(PathBuf, u64, SystemTime)>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            walk_dir_recursive(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
            if let Ok(meta) = path.metadata() {
                let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                out.push((path, meta.len(), mtime));
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn put_and_get() {
        let dir = tempdir().unwrap();
        let cache = TreeCache::new(dir.path(), 1024 * 1024);

        let data = br#"{"files":["a","b"]}"#;
        cache.put("owner", "repo", "abc123", data).unwrap();

        let got = cache.get("owner", "repo", "abc123").unwrap();
        // The round-trip through serde_json::Value may reformat but must be valid JSON
        // representing the same value.
        let expected: serde_json::Value = serde_json::from_slice(data).unwrap();
        let actual: serde_json::Value = serde_json::from_slice(&got).unwrap();
        assert_eq!(expected, actual);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let dir = tempdir().unwrap();
        let cache = TreeCache::new(dir.path(), 1024 * 1024);
        assert!(cache.get("no", "such", "sha").is_none());
    }

    #[test]
    fn persistence_across_instances() {
        let dir = tempdir().unwrap();

        {
            let cache = TreeCache::new(dir.path(), 1024 * 1024);
            cache
                .put("octocat", "hello-world", "deadbeef", br#"{"x":1}"#)
                .unwrap();
        }

        let cache2 = TreeCache::new(dir.path(), 1024 * 1024);
        let got = cache2.get("octocat", "hello-world", "deadbeef").unwrap();
        let val: serde_json::Value = serde_json::from_slice(&got).unwrap();
        assert_eq!(val["x"], serde_json::json!(1));
    }

    #[test]
    fn schema_version_mismatch_returns_none() {
        let dir = tempdir().unwrap();
        let cache = TreeCache::new(dir.path(), 1024 * 1024);

        // Manually write a file with version=999
        let path = cache.file_path("owner", "repo", "sha1");
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        let bad = serde_json::json!({"version": 999, "data": {"x": 1}});
        fs::write(&path, serde_json::to_vec(&bad).unwrap()).unwrap();

        assert!(cache.get("owner", "repo", "sha1").is_none());
        // File should have been deleted.
        assert!(!path.exists());
    }

    #[test]
    fn stats_reports_counts_and_size() {
        let dir = tempdir().unwrap();
        let cache = TreeCache::new(dir.path(), 1024 * 1024);

        let (count, size) = cache.stats();
        assert_eq!(count, 0);
        assert_eq!(size, 0);

        cache.put("a", "b", "sha1", br#"{"k":"v"}"#).unwrap();
        cache.put("a", "b", "sha2", br#"{"k":"v2"}"#).unwrap();

        let (count, size) = cache.stats();
        assert_eq!(count, 2);
        assert!(size > 0);
    }

    #[test]
    fn prune_removes_all() {
        let dir = tempdir().unwrap();
        let cache = TreeCache::new(dir.path(), 1024 * 1024);

        cache.put("x", "y", "sha1", br#"{"a":1}"#).unwrap();
        cache.put("x", "y", "sha2", br#"{"b":2}"#).unwrap();

        let (count, _) = cache.stats();
        assert_eq!(count, 2);

        cache.prune_all().unwrap();

        let (count, size) = cache.stats();
        assert_eq!(count, 0);
        assert_eq!(size, 0);
    }

    #[test]
    fn eviction_when_over_max_bytes() {
        let dir = tempdir().unwrap();

        // A single entry will be ~60-80 bytes once wrapped in VersionedTree.
        // Set max to 1 byte so that after inserting 3 entries at least some are evicted.
        let cache = TreeCache::new(dir.path(), 1);

        cache.put("o", "r", "sha1", br#"{"n":1}"#).unwrap();
        cache.put("o", "r", "sha2", br#"{"n":2}"#).unwrap();
        cache.put("o", "r", "sha3", br#"{"n":3}"#).unwrap();

        let (count, size) = cache.stats();
        assert!(count < 3, "expected eviction but got count={count}");
        assert!(size <= 1 || count < 3, "size={size} count={count}");
    }
}
