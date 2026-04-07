//! Resolution cache: maps package spec strings to GitHub coordinates.
//!
//! Persisted as a JSON file on disk for survival across daemon restarts.
//! Pinned versions never expire; "latest" entries expire after `latest_ttl_secs`.

use ctxfs_provider_common::resolver::ResolvedSource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// A single cached registry-to-GitHub resolution.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResolution {
    /// The resolved GitHub coordinates.
    pub source: ResolvedSource,
    /// Unix timestamp (seconds) at which this entry was stored.
    pub resolved_at: u64,
    /// Whether this entry tracked a "latest" pointer (as opposed to a pinned version).
    pub is_latest: bool,
}

/// Persistent mapping from package spec key to `ResolvedSource`.
///
/// The cache is stored as a single JSON file.  Writes use an atomic
/// write-then-rename pattern so the file is never observed in a partial state.
#[derive(Debug)]
pub struct ResolutionCache {
    entries: HashMap<String, CachedResolution>,
    file_path: PathBuf,
    /// TTL applied to `is_latest = true` entries. Zero means "always expired".
    latest_ttl_secs: u64,
}

// ── helpers ──────────────────────────────────────────────────────────────────

fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

fn persist(file_path: &PathBuf, entries: &HashMap<String, CachedResolution>) -> io::Result<()> {
    // Ensure parent directory exists.
    if let Some(parent) = file_path.parent() {
        fs::create_dir_all(parent)?;
    }

    let json = serde_json::to_vec_pretty(entries)
        .map_err(io::Error::other)?;

    // Atomic write: write to .tmp then rename.
    let tmp_path = file_path.with_extension("tmp");
    fs::write(&tmp_path, &json)?;
    fs::rename(&tmp_path, file_path)?;
    Ok(())
}

// ── impl ──────────────────────────────────────────────────────────────────────

impl ResolutionCache {
    /// Create an empty in-memory cache (nothing is read from disk).
    pub fn new(file_path: PathBuf, latest_ttl_secs: u64) -> Self {
        Self {
            entries: HashMap::new(),
            file_path,
            latest_ttl_secs,
        }
    }

    /// Load a cache from disk.  Returns an empty cache if the file is missing
    /// or cannot be parsed (corrupt file is treated as a cold start).
    pub fn load(file_path: PathBuf, latest_ttl_secs: u64) -> Self {
        let entries = fs::read(&file_path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<HashMap<String, CachedResolution>>(&bytes).ok())
            .unwrap_or_default();

        Self {
            entries,
            file_path,
            latest_ttl_secs,
        }
    }

    /// Look up a key.
    ///
    /// Returns `None` when:
    /// - the key is not present, or
    /// - the entry has `is_latest = true` and has exceeded `latest_ttl_secs`.
    ///
    /// Pinned entries (`is_latest = false`) never expire.
    pub fn get(&self, key: &str) -> Option<&ResolvedSource> {
        let entry = self.entries.get(key)?;

        if entry.is_latest {
            let age = now_secs().saturating_sub(entry.resolved_at);
            if age > self.latest_ttl_secs {
                return None;
            }
        }

        Some(&entry.source)
    }

    /// Insert or update an entry, then atomically persist the entire cache to disk.
    pub fn put(&mut self, key: String, source: ResolvedSource, is_latest: bool) -> io::Result<()> {
        let entry = CachedResolution {
            source,
            resolved_at: now_secs(),
            is_latest,
        };
        let _ = self.entries.insert(key, entry);
        persist(&self.file_path, &self.entries)
    }

    /// Number of stored entries (including expired ones still in memory).
    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    /// Remove all entries and persist the empty state.
    pub fn clear(&mut self) -> io::Result<()> {
        self.entries.clear();
        persist(&self.file_path, &self.entries)
    }
}

// ── tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use ctxfs_provider_common::resolver::ResolvedSource;
    use tempfile::tempdir;

    fn sample_source() -> ResolvedSource {
        ResolvedSource {
            owner: "facebook".to_string(),
            repo: "react".to_string(),
            git_ref: "v19.1.0".to_string(),
            subpath: None,
        }
    }

    #[test]
    fn put_and_get_pinned() {
        let dir = tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 3600);
        let src = sample_source();
        cache.put("npm:react@19.1.0".to_string(), src.clone(), false).unwrap();
        let got = cache.get("npm:react@19.1.0").expect("should be present");
        assert_eq!(*got, src);
    }

    #[test]
    fn pinned_never_expires() {
        let dir = tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 0);
        let src = sample_source();
        cache.put("npm:react@19.1.0".to_string(), src.clone(), false).unwrap();

        // Manually backdate the entry so it would expire if it were a "latest" entry.
        cache.entries.get_mut("npm:react@19.1.0").unwrap().resolved_at = 0;

        let got = cache.get("npm:react@19.1.0");
        assert!(got.is_some(), "pinned entries must never expire");
    }

    #[test]
    fn latest_expires_after_ttl() {
        let dir = tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 1);
        let src = sample_source();
        cache.put("npm:react@latest".to_string(), src, true).unwrap();

        // Backdate so the entry is older than the TTL.
        cache.entries.get_mut("npm:react@latest").unwrap().resolved_at = 0;

        let got = cache.get("npm:react@latest");
        assert!(got.is_none(), "expired latest entries must return None");
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let dir = tempdir().unwrap();
        let cache = ResolutionCache::new(dir.path().join("res.json"), 3600);
        assert!(cache.get("npm:nonexistent@1.0.0").is_none());
    }

    #[test]
    fn persistence_across_restarts() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("res.json");
        let src = sample_source();

        {
            let mut cache = ResolutionCache::new(path.clone(), 3600);
            cache.put("npm:react@19.1.0".to_string(), src.clone(), false).unwrap();
        }

        // Simulate restart by loading from the same file path.
        let cache = ResolutionCache::load(path, 3600);
        let got = cache.get("npm:react@19.1.0").expect("should survive restart");
        assert_eq!(*got, src);
    }

    #[test]
    fn stats_counts_entries() {
        let dir = tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 3600);
        assert_eq!(cache.entry_count(), 0);

        cache.put("npm:react@19.1.0".to_string(), sample_source(), false).unwrap();
        assert_eq!(cache.entry_count(), 1);

        let src2 = ResolvedSource {
            owner: "facebook".to_string(),
            repo: "react".to_string(),
            git_ref: "v18.3.0".to_string(),
            subpath: None,
        };
        cache.put("npm:react@18.3.0".to_string(), src2, false).unwrap();
        assert_eq!(cache.entry_count(), 2);
    }

    #[test]
    fn clear_removes_all() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("res.json");
        let mut cache = ResolutionCache::new(path.clone(), 3600);

        cache.put("npm:react@19.1.0".to_string(), sample_source(), false).unwrap();
        cache.put("npm:react@latest".to_string(), sample_source(), true).unwrap();
        assert_eq!(cache.entry_count(), 2);

        cache.clear().unwrap();
        assert_eq!(cache.entry_count(), 0);
        assert!(cache.get("npm:react@19.1.0").is_none());

        // Also verify the cleared state persisted.
        let reloaded = ResolutionCache::load(path, 3600);
        assert_eq!(reloaded.entry_count(), 0);
    }
}
