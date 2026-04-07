# Tiered Caching Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add three-tier caching (resolution, tree, Redis-shared tree) so repeat mounts skip registry and GitHub API calls entirely.

**Architecture:** Resolution cache maps package specs to GitHub coordinates (in-memory + JSON on disk). Tree cache stores directory manifests per commit SHA (disk files + optional Redis via `SharedTreeCache` trait). Existing blob cache unchanged.

**Tech Stack:** Rust, serde_json, async-trait, redis (async/tokio), zstd

---

## File Structure

| File | Responsibility |
|------|---------------|
| `crates/ctxfs-core/src/config.rs` | Add `redis_url`, `latest_ttl_secs`, `tree_cache_max_bytes` fields |
| `crates/ctxfs-cache/src/lib.rs` | Re-export new modules |
| `crates/ctxfs-cache/src/resolution.rs` | `ResolutionCache` — in-memory HashMap + JSON disk persistence |
| `crates/ctxfs-cache/src/tree.rs` | `TreeCache` — disk-backed tree manifest cache with soft-cap LRU |
| `crates/ctxfs-cache/src/shared.rs` | `SharedTreeCache` trait definition |
| `crates/ctxfs-cache-redis/src/lib.rs` | `RedisTreeCache` implementing `SharedTreeCache` |
| `crates/ctxfs-cache-redis/Cargo.toml` | New crate with redis + zstd deps |
| `crates/ctxfs-provider-git/src/github.rs` | Check tree cache before GitHub API in `fetch_snapshot()` |
| `crates/ctxfs-daemon/src/daemon.rs` | Wire resolution cache + tree cache + shared cache into mount flow |
| `crates/ctxfs-ipc/src/service.rs` | Extend `CacheStats` with tree/resolution counts |
| `crates/ctxfs-cli/src/main.rs` | Extend `cache stats` and `cache prune` with `--trees`/`--resolutions` flags |
| `Cargo.toml` | Add `ctxfs-cache-redis` workspace member and deps |

---

### Task 1: Extend Config with new cache fields

**Files:**
- Modify: `crates/ctxfs-core/src/config.rs`

- [ ] **Step 1: Write failing tests for new config fields**

Add these tests to the existing `mod tests` in `crates/ctxfs-core/src/config.rs`:

```rust
#[test]
fn default_config_has_cache_tier_fields() {
    let config = Config::default();
    assert_eq!(config.latest_ttl_secs, 3600);
    assert_eq!(config.tree_cache_max_bytes, 500 * 1024 * 1024);
    assert!(config.redis_url.is_none());
}

#[test]
fn config_serde_roundtrip_with_redis() {
    let mut config = Config::default();
    config.redis_url = Some("redis://localhost:6379".into());
    config.latest_ttl_secs = 7200;
    config.tree_cache_max_bytes = 1024 * 1024 * 1024;
    let config2 = config.serde_roundtrip().unwrap();
    assert_eq!(config.redis_url, config2.redis_url);
    assert_eq!(config.latest_ttl_secs, config2.latest_ttl_secs);
    assert_eq!(config.tree_cache_max_bytes, config2.tree_cache_max_bytes);
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ctxfs-core`
Expected: FAIL — fields don't exist on `Config`

- [ ] **Step 3: Add the fields to Config**

In `crates/ctxfs-core/src/config.rs`, add three fields to the `Config` struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub socket_path: PathBuf,
    pub pid_file: PathBuf,
    pub cache_dir: PathBuf,
    pub cache_max_bytes: u64,
    pub log_level: String,
    pub github_token: Option<String>,
    pub redis_url: Option<String>,
    pub latest_ttl_secs: u64,
    pub tree_cache_max_bytes: u64,
}
```

Update `Default`:

```rust
impl Default for Config {
    fn default() -> Self {
        let base = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".ctxfs");
        Self {
            socket_path: base.join("ctxfs.sock"),
            pid_file: base.join("ctxfs.pid"),
            cache_dir: base.join("cache"),
            cache_max_bytes: 512 * 1024 * 1024,
            log_level: "info".to_string(),
            github_token: None,
            redis_url: None,
            latest_ttl_secs: 3600,
            tree_cache_max_bytes: 500 * 1024 * 1024,
        }
    }
}
```

Update `from_env()` — add after the existing env var parsing, before the `github_token` line:

```rust
config.redis_url = std::env::var("CTXFS_REDIS_URL").ok().filter(|s| !s.is_empty());
if let Ok(v) = std::env::var("CTXFS_LATEST_TTL_SECS") {
    if let Ok(n) = v.parse() {
        config.latest_ttl_secs = n;
    }
}
if let Ok(v) = std::env::var("CTXFS_TREE_CACHE_MAX_BYTES") {
    if let Ok(n) = v.parse() {
        config.tree_cache_max_bytes = n;
    }
}
```

- [ ] **Step 4: Run tests to verify they pass**

Run: `cargo test -p ctxfs-core`
Expected: PASS

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-core/src/config.rs
git commit -m "feat(config): add redis_url, latest_ttl_secs, tree_cache_max_bytes"
```

---

### Task 2: Resolution Cache

**Files:**
- Create: `crates/ctxfs-cache/src/resolution.rs`
- Modify: `crates/ctxfs-cache/src/lib.rs`
- Modify: `crates/ctxfs-cache/Cargo.toml`

- [ ] **Step 1: Write failing tests**

Create `crates/ctxfs-cache/src/resolution.rs` with tests only:

```rust
//! Resolution cache — maps package specs to GitHub coordinates.

use ctxfs_provider_common::resolver::ResolvedSource;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CachedResolution {
    pub source: ResolvedSource,
    pub resolved_at: u64,
    pub is_latest: bool,
}

#[derive(Debug)]
pub struct ResolutionCache {
    entries: HashMap<String, CachedResolution>,
    file_path: PathBuf,
    latest_ttl_secs: u64,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_source() -> ResolvedSource {
        ResolvedSource {
            owner: "facebook".into(),
            repo: "react".into(),
            git_ref: "abc123".into(),
            subpath: None,
        }
    }

    #[test]
    fn put_and_get_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 3600);

        let src = make_source();
        cache.put("npm:react@19.1.0", src.clone(), false).unwrap();

        let result = cache.get("npm:react@19.1.0");
        assert!(result.is_some());
        assert_eq!(result.unwrap().owner, "facebook");
    }

    #[test]
    fn pinned_never_expires() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 0);

        let src = make_source();
        cache.put("npm:react@19.1.0", src, false).unwrap();

        // TTL is 0 but pinned entries should never expire
        assert!(cache.get("npm:react@19.1.0").is_some());
    }

    #[test]
    fn latest_expires_after_ttl() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 1);

        let mut src = make_source();
        src.git_ref = "v19.1.0".into();

        // Insert with a timestamp in the past (resolved_at = 0 means epoch)
        let entry = CachedResolution {
            source: src,
            resolved_at: 0, // far in the past
            is_latest: true,
        };
        cache.entries.insert("npm:react@latest".into(), entry);

        // Should be expired since resolved_at is epoch and TTL is 1 second
        assert!(cache.get("npm:react@latest").is_none());
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = ResolutionCache::new(dir.path().join("res.json"), 3600);
        assert!(cache.get("npm:nonexistent@1.0.0").is_none());
    }

    #[test]
    fn persistence_across_restarts() {
        let dir = tempfile::tempdir().unwrap();
        let file_path = dir.path().join("res.json");

        {
            let mut cache = ResolutionCache::new(file_path.clone(), 3600);
            cache.put("npm:react@19.1.0", make_source(), false).unwrap();
        }

        // Reload from disk
        let cache = ResolutionCache::load(file_path, 3600);
        assert!(cache.get("npm:react@19.1.0").is_some());
    }

    #[test]
    fn stats_counts_entries() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 3600);
        assert_eq!(cache.entry_count(), 0);

        cache.put("npm:react@19.1.0", make_source(), false).unwrap();
        assert_eq!(cache.entry_count(), 1);

        cache.put("npm:lodash@4.17.21", make_source(), false).unwrap();
        assert_eq!(cache.entry_count(), 2);
    }

    #[test]
    fn clear_removes_all() {
        let dir = tempfile::tempdir().unwrap();
        let mut cache = ResolutionCache::new(dir.path().join("res.json"), 3600);

        cache.put("npm:react@19.1.0", make_source(), false).unwrap();
        cache.put("npm:lodash@4.17.21", make_source(), false).unwrap();
        assert_eq!(cache.entry_count(), 2);

        cache.clear().unwrap();
        assert_eq!(cache.entry_count(), 0);
    }
}
```

- [ ] **Step 2: Add module to lib.rs and update Cargo.toml**

In `crates/ctxfs-cache/src/lib.rs`, add at the top (the file currently starts with `use ctxfs_core::...`):

```rust
mod resolution;
pub use resolution::ResolutionCache;
```

In `crates/ctxfs-cache/Cargo.toml`, add `ctxfs-provider-common` to dependencies:

```toml
[dependencies]
ctxfs-core = { workspace = true }
ctxfs-manifest = { workspace = true }
ctxfs-provider-common = { workspace = true }
linked-hash-map = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
tracing = { workspace = true }
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ctxfs-cache`
Expected: FAIL — `ResolutionCache` methods not implemented

- [ ] **Step 4: Implement ResolutionCache**

Replace the test-only `ResolutionCache` struct with the full implementation in `crates/ctxfs-cache/src/resolution.rs`. Add these methods above the `#[cfg(test)]` block:

```rust
impl ResolutionCache {
    pub fn new(file_path: PathBuf, latest_ttl_secs: u64) -> Self {
        Self {
            entries: HashMap::new(),
            file_path,
            latest_ttl_secs,
        }
    }

    /// Load from disk if the file exists; otherwise return an empty cache.
    pub fn load(file_path: PathBuf, latest_ttl_secs: u64) -> Self {
        let entries = std::fs::read_to_string(&file_path)
            .ok()
            .and_then(|s| serde_json::from_str::<HashMap<String, CachedResolution>>(&s).ok())
            .unwrap_or_default();

        Self {
            entries,
            file_path,
            latest_ttl_secs,
        }
    }

    pub fn get(&self, key: &str) -> Option<&ResolvedSource> {
        let entry = self.entries.get(key)?;

        // Check TTL for latest entries
        if entry.is_latest {
            let now = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs();
            if now - entry.resolved_at > self.latest_ttl_secs {
                return None;
            }
        }

        Some(&entry.source)
    }

    pub fn put(
        &mut self,
        key: &str,
        source: ResolvedSource,
        is_latest: bool,
    ) -> Result<(), std::io::Error> {
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();

        let entry = CachedResolution {
            source,
            resolved_at: now,
            is_latest,
        };

        let _ = self.entries.insert(key.to_string(), entry);
        self.persist()
    }

    pub fn entry_count(&self) -> usize {
        self.entries.len()
    }

    pub fn clear(&mut self) -> Result<(), std::io::Error> {
        self.entries.clear();
        self.persist()
    }

    /// Atomic write-rename to avoid corruption.
    fn persist(&self) -> Result<(), std::io::Error> {
        let json = serde_json::to_string_pretty(&self.entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        let tmp_path = self.file_path.with_extension("json.tmp");
        std::fs::write(&tmp_path, json)?;
        std::fs::rename(&tmp_path, &self.file_path)?;
        Ok(())
    }
}
```

Also add a `Serialize`/`Deserialize` derive to `ResolvedSource` in `crates/ctxfs-provider-common/src/resolver.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedSource {
    pub owner: String,
    pub repo: String,
    pub git_ref: String,
    pub subpath: Option<String>,
}
```

Add `serde` to `crates/ctxfs-provider-common/Cargo.toml` if not already present.

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ctxfs-cache`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-cache/src/resolution.rs crates/ctxfs-cache/src/lib.rs \
       crates/ctxfs-cache/Cargo.toml crates/ctxfs-provider-common/src/resolver.rs \
       crates/ctxfs-provider-common/Cargo.toml
git commit -m "feat(cache): add ResolutionCache for registry→GitHub mappings"
```

---

### Task 3: Tree Cache (local disk)

**Files:**
- Create: `crates/ctxfs-cache/src/tree.rs`
- Modify: `crates/ctxfs-cache/src/lib.rs`

- [ ] **Step 1: Write failing tests**

Create `crates/ctxfs-cache/src/tree.rs`:

```rust
//! Tree cache — persists directory tree manifests to disk.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::SystemTime;

const SCHEMA_VERSION: u32 = 1;

#[derive(Debug)]
pub struct TreeCache {
    root: PathBuf,
    max_bytes: u64,
}

/// Wrapper for versioned on-disk format.
#[derive(serde::Serialize, serde::Deserialize)]
struct VersionedTree {
    version: u32,
    data: serde_json::Value,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn put_and_get() {
        let dir = tempfile::tempdir().unwrap();
        let cache = TreeCache::new(dir.path().join("trees"), 100 * 1024 * 1024);

        let data = b"{\"test\": true}";
        cache.put("owner", "repo", "abc123", data).unwrap();

        let result = cache.get("owner", "repo", "abc123");
        assert!(result.is_some());
        assert_eq!(result.unwrap(), data);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = TreeCache::new(dir.path().join("trees"), 100 * 1024 * 1024);
        assert!(cache.get("owner", "repo", "nonexistent").is_none());
    }

    #[test]
    fn persistence_across_instances() {
        let dir = tempfile::tempdir().unwrap();
        let tree_dir = dir.path().join("trees");

        {
            let cache = TreeCache::new(tree_dir.clone(), 100 * 1024 * 1024);
            cache.put("owner", "repo", "abc123", b"snapshot_data").unwrap();
        }

        let cache = TreeCache::new(tree_dir, 100 * 1024 * 1024);
        assert!(cache.get("owner", "repo", "abc123").is_some());
    }

    #[test]
    fn schema_version_mismatch_returns_none() {
        let dir = tempfile::tempdir().unwrap();
        let cache = TreeCache::new(dir.path().join("trees"), 100 * 1024 * 1024);

        // Write a file with wrong schema version directly
        let file_path = cache.file_path("owner", "repo", "abc123");
        fs::create_dir_all(file_path.parent().unwrap()).unwrap();
        let bad = VersionedTree {
            version: 999,
            data: serde_json::json!("old"),
        };
        fs::write(&file_path, serde_json::to_vec(&bad).unwrap()).unwrap();

        assert!(cache.get("owner", "repo", "abc123").is_none());
    }

    #[test]
    fn stats_reports_counts_and_size() {
        let dir = tempfile::tempdir().unwrap();
        let cache = TreeCache::new(dir.path().join("trees"), 100 * 1024 * 1024);

        let (count, size) = cache.stats();
        assert_eq!(count, 0);
        assert_eq!(size, 0);

        cache.put("owner", "repo", "abc123", b"data1").unwrap();
        let (count, size) = cache.stats();
        assert_eq!(count, 1);
        assert!(size > 0);
    }

    #[test]
    fn prune_removes_all() {
        let dir = tempfile::tempdir().unwrap();
        let cache = TreeCache::new(dir.path().join("trees"), 100 * 1024 * 1024);

        cache.put("owner", "repo", "abc123", b"data").unwrap();
        cache.put("owner", "repo", "def456", b"data2").unwrap();
        assert_eq!(cache.stats().0, 2);

        cache.prune_all().unwrap();
        assert_eq!(cache.stats().0, 0);
    }

    #[test]
    fn eviction_when_over_max_bytes() {
        let dir = tempfile::tempdir().unwrap();
        // Very small limit — each entry is at least ~30 bytes with version wrapper
        let cache = TreeCache::new(dir.path().join("trees"), 60);

        cache.put("o", "r", "sha1", b"aaaa").unwrap();
        cache.put("o", "r", "sha2", b"bbbb").unwrap();
        cache.put("o", "r", "sha3", b"cccc").unwrap();

        let (count, size) = cache.stats();
        // After eviction, should be under 60 bytes
        assert!(size <= 60, "size {size} should be <= 60");
        assert!(count < 3, "count {count} should be < 3");
    }
}
```

- [ ] **Step 2: Add module to lib.rs**

In `crates/ctxfs-cache/src/lib.rs`, add:

```rust
mod tree;
pub use tree::TreeCache;
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p ctxfs-cache`
Expected: FAIL — `TreeCache` methods not implemented

- [ ] **Step 4: Implement TreeCache**

Add the implementation above the `#[cfg(test)]` block in `crates/ctxfs-cache/src/tree.rs`:

```rust
impl TreeCache {
    pub fn new(root: PathBuf, max_bytes: u64) -> Self {
        Self { root, max_bytes }
    }

    fn file_path(&self, owner: &str, repo: &str, commit_sha: &str) -> PathBuf {
        self.root.join(owner).join(repo).join(format!("{commit_sha}.json"))
    }

    pub fn get(&self, owner: &str, repo: &str, commit_sha: &str) -> Option<Vec<u8>> {
        let path = self.file_path(owner, repo, commit_sha);
        let raw = fs::read(&path).ok()?;

        let versioned: VersionedTree = serde_json::from_slice(&raw).ok()?;
        if versioned.version != SCHEMA_VERSION {
            // Schema mismatch — discard stale entry
            let _ = fs::remove_file(&path);
            return None;
        }

        // Re-serialize just the data portion (the actual snapshot JSON)
        serde_json::to_vec(&versioned.data).ok()
    }

    pub fn put(
        &self,
        owner: &str,
        repo: &str,
        commit_sha: &str,
        data: &[u8],
    ) -> Result<(), std::io::Error> {
        let path = self.file_path(owner, repo, commit_sha);

        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let data_value: serde_json::Value = serde_json::from_slice(data)
            .unwrap_or_else(|_| serde_json::Value::String(base64_encode(data)));

        let versioned = VersionedTree {
            version: SCHEMA_VERSION,
            data: data_value,
        };

        let json = serde_json::to_vec(&versioned)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        // Atomic write
        let tmp_path = path.with_extension("json.tmp");
        fs::write(&tmp_path, &json)?;
        fs::rename(&tmp_path, &path)?;

        // Check if we need to evict
        self.maybe_evict()?;

        Ok(())
    }

    pub fn stats(&self) -> (usize, u64) {
        let mut count = 0;
        let mut total_bytes = 0;

        if let Ok(entries) = self.walk_files() {
            for (_, size, _) in &entries {
                count += 1;
                total_bytes += size;
            }
        }

        (count, total_bytes)
    }

    pub fn prune_all(&self) -> Result<(), std::io::Error> {
        if self.root.exists() {
            fs::remove_dir_all(&self.root)?;
            fs::create_dir_all(&self.root)?;
        }
        Ok(())
    }

    fn maybe_evict(&self) -> Result<(), std::io::Error> {
        let mut entries = self.walk_files()?;
        let total: u64 = entries.iter().map(|(_, size, _)| size).sum();

        if total <= self.max_bytes {
            return Ok(());
        }

        // Sort by mtime ascending (oldest first)
        entries.sort_by_key(|(_, _, mtime)| *mtime);

        let mut current = total;
        for (path, size, _) in &entries {
            if current <= self.max_bytes {
                break;
            }
            let _ = fs::remove_file(path);
            current -= size;
        }

        Ok(())
    }

    fn walk_files(&self) -> Result<Vec<(PathBuf, u64, SystemTime)>, std::io::Error> {
        let mut results = Vec::new();

        if !self.root.exists() {
            return Ok(results);
        }

        Self::walk_dir_recursive(&self.root, &mut results)?;
        Ok(results)
    }

    fn walk_dir_recursive(
        dir: &Path,
        results: &mut Vec<(PathBuf, u64, SystemTime)>,
    ) -> Result<(), std::io::Error> {
        for entry in fs::read_dir(dir)? {
            let entry = entry?;
            let path = entry.path();
            if path.is_dir() {
                Self::walk_dir_recursive(&path, results)?;
            } else if path.extension().and_then(|e| e.to_str()) == Some("json") {
                if let Ok(meta) = path.metadata() {
                    let mtime = meta.modified().unwrap_or(SystemTime::UNIX_EPOCH);
                    results.push((path, meta.len(), mtime));
                }
            }
        }
        Ok(())
    }
}

fn base64_encode(data: &[u8]) -> String {
    use base64::Engine;
    base64::engine::general_purpose::STANDARD.encode(data)
}
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ctxfs-cache`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-cache/src/tree.rs crates/ctxfs-cache/src/lib.rs
git commit -m "feat(cache): add TreeCache for disk-backed directory manifests"
```

---

### Task 4: SharedTreeCache trait

**Files:**
- Create: `crates/ctxfs-cache/src/shared.rs`
- Modify: `crates/ctxfs-cache/src/lib.rs`
- Modify: `crates/ctxfs-cache/Cargo.toml`

- [ ] **Step 1: Create the trait definition**

Create `crates/ctxfs-cache/src/shared.rs`:

```rust
//! Shared tree cache trait — backend-agnostic interface for distributed tree caching.

use async_trait::async_trait;

/// Trait for shared tree cache backends (Redis, HTTP, etc.).
///
/// Implementations should handle errors gracefully — a failed `get` returns `None`,
/// a failed `put` is silently dropped. The caller falls back to the GitHub API.
#[async_trait]
pub trait SharedTreeCache: Send + Sync + std::fmt::Debug {
    /// Retrieve a cached tree manifest. Returns the raw snapshot JSON bytes.
    async fn get_tree(&self, owner: &str, repo: &str, commit_sha: &str) -> Option<Vec<u8>>;

    /// Store a tree manifest. Errors are logged but not propagated.
    async fn put_tree(&self, owner: &str, repo: &str, commit_sha: &str, data: &[u8]);
}
```

- [ ] **Step 2: Add module to lib.rs and async-trait to Cargo.toml**

In `crates/ctxfs-cache/src/lib.rs`, add:

```rust
mod shared;
pub use shared::SharedTreeCache;
```

In `crates/ctxfs-cache/Cargo.toml`, add `async-trait`:

```toml
[dependencies]
ctxfs-core = { workspace = true }
ctxfs-manifest = { workspace = true }
ctxfs-provider-common = { workspace = true }
linked-hash-map = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
sha2 = { workspace = true }
hex = { workspace = true }
tracing = { workspace = true }
async-trait = { workspace = true }
base64 = { workspace = true }
```

- [ ] **Step 3: Run to verify it compiles**

Run: `cargo build -p ctxfs-cache`
Expected: SUCCESS

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-cache/src/shared.rs crates/ctxfs-cache/src/lib.rs crates/ctxfs-cache/Cargo.toml
git commit -m "feat(cache): add SharedTreeCache trait for distributed backends"
```

---

### Task 5: Redis SharedTreeCache implementation

**Files:**
- Create: `crates/ctxfs-cache-redis/Cargo.toml`
- Create: `crates/ctxfs-cache-redis/src/lib.rs`
- Modify: `Cargo.toml` (workspace root)

- [ ] **Step 1: Create the crate structure**

Create `crates/ctxfs-cache-redis/Cargo.toml`:

```toml
[package]
name = "ctxfs-cache-redis"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
ctxfs-cache = { workspace = true }
async-trait = { workspace = true }
redis = { version = "0.27", features = ["tokio-comp", "connection-manager"] }
zstd = "0.13"
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Write the implementation with tests**

Create `crates/ctxfs-cache-redis/src/lib.rs`:

```rust
//! Redis-backed shared tree cache with zstd compression.

use async_trait::async_trait;
use ctxfs_cache::SharedTreeCache;
use redis::AsyncCommands;
use tracing::warn;

/// Redis implementation of `SharedTreeCache`.
///
/// Trees are zstd-compressed before storage and decompressed on retrieval.
/// All errors are logged and swallowed — Redis is a best-effort optimization.
#[derive(Debug)]
pub struct RedisTreeCache {
    client: redis::aio::ConnectionManager,
}

impl RedisTreeCache {
    /// Connect to Redis. Returns `None` if the connection fails.
    pub async fn connect(url: &str) -> Option<Self> {
        let client = redis::Client::open(url).ok()?;
        let manager = client.get_connection_manager().await.ok()?;
        Some(Self { client: manager })
    }

    fn cache_key(owner: &str, repo: &str, commit_sha: &str) -> String {
        format!("ctxfs:tree:{owner}/{repo}@{commit_sha}")
    }
}

#[async_trait]
impl SharedTreeCache for RedisTreeCache {
    async fn get_tree(&self, owner: &str, repo: &str, commit_sha: &str) -> Option<Vec<u8>> {
        let key = Self::cache_key(owner, repo, commit_sha);
        let compressed: Vec<u8> = match self.client.clone().get(&key).await {
            Ok(data) => data,
            Err(e) => {
                warn!("Redis GET failed for {key}: {e}");
                return None;
            }
        };

        if compressed.is_empty() {
            return None;
        }

        match zstd::decode_all(compressed.as_slice()) {
            Ok(data) => Some(data),
            Err(e) => {
                warn!("zstd decompress failed for {key}: {e}");
                None
            }
        }
    }

    async fn put_tree(&self, owner: &str, repo: &str, commit_sha: &str, data: &[u8]) {
        let key = Self::cache_key(owner, repo, commit_sha);
        let compressed = match zstd::encode_all(data, 3) {
            Ok(c) => c,
            Err(e) => {
                warn!("zstd compress failed for {key}: {e}");
                return;
            }
        };

        if let Err(e) = self.client.clone().set::<_, _, ()>(&key, &compressed).await {
            warn!("Redis SET failed for {key}: {e}");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cache_key_format() {
        let key = RedisTreeCache::cache_key("facebook", "react", "abc123");
        assert_eq!(key, "ctxfs:tree:facebook/react@abc123");
    }

    #[test]
    fn zstd_roundtrip() {
        let data = b"test snapshot data for compression";
        let compressed = zstd::encode_all(&data[..], 3).unwrap();
        let decompressed = zstd::decode_all(compressed.as_slice()).unwrap();
        assert_eq!(data.as_slice(), decompressed.as_slice());
    }
}
```

- [ ] **Step 3: Register in workspace**

In root `Cargo.toml`, add to `[workspace]` members:

```toml
members = [
    "crates/ctxfs-core",
    "crates/ctxfs-manifest",
    "crates/ctxfs-cache",
    "crates/ctxfs-cache-redis",
    # ... rest unchanged
]
```

Add to `[workspace.dependencies]`:

```toml
ctxfs-cache-redis = { path = "crates/ctxfs-cache-redis", version = "0.0.0" }
```

- [ ] **Step 4: Run to verify it compiles and tests pass**

Run: `cargo test -p ctxfs-cache-redis`
Expected: PASS (only unit tests, no Redis server needed)

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-cache-redis/ Cargo.toml
git commit -m "feat(cache-redis): add RedisTreeCache with zstd compression"
```

---

### Task 6: Wire tree cache into GitHubProvider

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`
- Modify: `crates/ctxfs-provider-git/Cargo.toml`

- [ ] **Step 1: Add TreeCache and SharedTreeCache to GitHubProvider**

In `crates/ctxfs-provider-git/Cargo.toml`, ensure `ctxfs-cache` is a dependency (it already is).

In `crates/ctxfs-provider-git/src/github.rs`, modify the `GitHubProvider` struct to hold tree cache references:

```rust
use ctxfs_cache::{BlobCache, SharedTreeCache, TreeCache};

pub struct GitHubProvider {
    client: reqwest::Client,
    cache: Arc<BlobCache>,
    tree_cache: Option<Arc<TreeCache>>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    active_source: std::sync::Mutex<Option<SourceSpec>>,
}
```

Update the `new` constructor to accept the new caches:

```rust
impl GitHubProvider {
    pub fn new(
        token: Option<&str>,
        cache: Arc<BlobCache>,
        tree_cache: Option<Arc<TreeCache>>,
        shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    ) -> Self {
        // ... existing client setup unchanged ...

        Self {
            client,
            cache,
            tree_cache,
            shared_tree_cache,
            active_source: std::sync::Mutex::new(None),
        }
    }
}
```

- [ ] **Step 2: Add tree cache lookup to fetch_snapshot**

In the `Provider` impl's `fetch_snapshot`, add tree cache checks between resolving the ref and calling the tree API. Replace the current `fetch_snapshot` body:

```rust
async fn fetch_snapshot(&self, source: &SourceSpec) -> Result<Vec<u8>, CtxfsError> {
    debug!("fetching snapshot for {source}");

    *self.active_source.lock().unwrap() = Some(source.clone());

    let commit_sha = self.resolve_ref(source).await?;
    debug!("resolved ref {} -> {}", source.version, commit_sha);

    let (owner, repo) = owner_repo(source)?;

    // Tier 2a: check local tree cache
    if let Some(ref tc) = self.tree_cache {
        if let Some(data) = tc.get(owner, repo, &commit_sha) {
            debug!("tree cache HIT for {owner}/{repo}@{commit_sha}");
            return Ok(data);
        }
    }

    // Tier 2b: check shared (Redis) tree cache
    if let Some(ref stc) = self.shared_tree_cache {
        if let Some(data) = stc.get_tree(owner, repo, &commit_sha).await {
            debug!("shared tree cache HIT for {owner}/{repo}@{commit_sha}");
            // Populate local cache
            if let Some(ref tc) = self.tree_cache {
                let _ = tc.put(owner, repo, &commit_sha, &data);
            }
            return Ok(data);
        }
    }

    // Tier 2c: fetch from GitHub API
    let tree = self.fetch_tree(source, &commit_sha).await?;
    debug!("fetched tree with {} entries", tree.tree.len());

    let (root_digest, directories) = Self::build_directories(&tree.tree, source);

    // Cache all directory objects in blob cache
    for (path, dir) in &directories {
        let json = serde_json::to_vec(dir)
            .map_err(|e| CtxfsError::Manifest(format!("serialize directory: {e}")))?;
        self.cache.put(&dir.digest, &json)?;
        debug!("cached directory '{}' as {}", path, dir.digest);
    }

    let snapshot = Snapshot {
        source: source.to_string(),
        commit_sha: commit_sha.clone(),
        root_directory: root_digest,
        created_at: chrono::Utc::now().to_rfc3339(),
    };

    let json = serde_json::to_vec(&snapshot)
        .map_err(|e| CtxfsError::Manifest(format!("serialize snapshot: {e}")))?;

    // Store in local tree cache
    if let Some(ref tc) = self.tree_cache {
        let _ = tc.put(owner, repo, &commit_sha, &json);
    }

    // Store in shared (Redis) tree cache
    if let Some(ref stc) = self.shared_tree_cache {
        stc.put_tree(owner, repo, &commit_sha, &json).await;
    }

    Ok(json)
}
```

- [ ] **Step 3: Update all call sites passing `None` for new params**

In `crates/ctxfs-daemon/src/daemon.rs`, update the `GitHubProvider::new` call at line 260:

```rust
let provider = Arc::new(GitHubProvider::new(
    self.config.github_token.as_deref(),
    self.cache.clone(),
    None,  // tree_cache — will be wired in Task 8
    None,  // shared_tree_cache — will be wired in Task 8
));
```

- [ ] **Step 4: Fix existing tests**

Existing tests that call `GitHubProvider::new` need to pass `None` for the new params. Check tests in `crates/ctxfs-provider-git/` and update any direct constructor calls.

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-provider-git/src/github.rs crates/ctxfs-provider-git/Cargo.toml \
       crates/ctxfs-daemon/src/daemon.rs
git commit -m "feat(provider-git): integrate tree cache into fetch_snapshot"
```

---

### Task 7: Extend IPC CacheStats

**Files:**
- Modify: `crates/ctxfs-ipc/src/service.rs`

- [ ] **Step 1: Write failing test**

Add to the existing `mod tests` in `crates/ctxfs-ipc/src/service.rs`:

```rust
#[test]
fn cache_stats_with_tiers_serde_roundtrip() {
    let stats = CacheStats {
        total_bytes: 1024,
        entry_count: 10,
        freed_bytes: 512,
        tree_count: 5,
        tree_bytes: 2048,
        resolution_count: 3,
    };
    let json = serde_json::to_string(&stats).unwrap();
    let stats2: CacheStats = serde_json::from_str(&json).unwrap();
    assert_eq!(stats.tree_count, stats2.tree_count);
    assert_eq!(stats.tree_bytes, stats2.tree_bytes);
    assert_eq!(stats.resolution_count, stats2.resolution_count);
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p ctxfs-ipc`
Expected: FAIL — fields don't exist

- [ ] **Step 3: Add fields to CacheStats**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub total_bytes: u64,
    pub entry_count: usize,
    pub freed_bytes: u64,
    pub tree_count: usize,
    pub tree_bytes: u64,
    pub resolution_count: usize,
}
```

- [ ] **Step 4: Fix all places constructing CacheStats**

In `crates/ctxfs-daemon/src/daemon.rs`, update `cache_stats` and `cache_prune` to include the new fields (set to 0 for now — will be wired in Task 8):

```rust
async fn cache_stats(self, _: tarpc::context::Context) -> Result<CacheStats, String> {
    let (total_bytes, entry_count) = self.cache.stats();
    Ok(CacheStats {
        total_bytes,
        entry_count,
        freed_bytes: 0,
        tree_count: 0,
        tree_bytes: 0,
        resolution_count: 0,
    })
}

async fn cache_prune(
    self,
    _: tarpc::context::Context,
    max_bytes: Option<u64>,
) -> Result<CacheStats, String> {
    let freed = self
        .cache
        .prune(max_bytes)
        .map_err(|e| format!("prune failed: {e}"))?;
    let (total_bytes, entry_count) = self.cache.stats();
    Ok(CacheStats {
        total_bytes,
        entry_count,
        freed_bytes: freed,
        tree_count: 0,
        tree_bytes: 0,
        resolution_count: 0,
    })
}
```

- [ ] **Step 5: Run all tests**

Run: `cargo test`
Expected: PASS

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-ipc/src/service.rs crates/ctxfs-daemon/src/daemon.rs
git commit -m "feat(ipc): extend CacheStats with tree and resolution counts"
```

---

### Task 8: Wire everything into daemon

**Files:**
- Modify: `crates/ctxfs-daemon/src/daemon.rs`
- Modify: `crates/ctxfs-daemon/Cargo.toml`

- [ ] **Step 1: Add resolution cache and tree cache to Daemon struct**

In `crates/ctxfs-daemon/src/daemon.rs`, update imports:

```rust
use ctxfs_cache::{BlobCache, ResolutionCache, SharedTreeCache, TreeCache};
```

Update the `Daemon` struct:

```rust
pub struct Daemon {
    config: Config,
    cache: Arc<BlobCache>,
    tree_cache: Arc<TreeCache>,
    resolution_cache: std::sync::Mutex<ResolutionCache>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    mounts: Arc<RwLock<HashMap<String, MountHandle>>>,
    cancel: CancellationToken,
}
```

Update `DaemonServer`:

```rust
#[derive(Clone)]
struct DaemonServer {
    cache: Arc<BlobCache>,
    tree_cache: Arc<TreeCache>,
    resolution_cache: Arc<std::sync::Mutex<ResolutionCache>>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    mounts: Arc<RwLock<HashMap<String, MountHandle>>>,
    config: Config,
    rt_handle: tokio::runtime::Handle,
}
```

- [ ] **Step 2: Update Daemon::new to initialize caches**

```rust
pub fn new(config: Config) -> Result<Self> {
    let cache = Arc::new(
        BlobCache::new(config.cache_dir.clone(), config.cache_max_bytes)
            .context("failed to initialize cache")?,
    );

    let tree_cache = Arc::new(TreeCache::new(
        config.cache_dir.join("trees"),
        config.tree_cache_max_bytes,
    ));

    let resolution_cache = ResolutionCache::load(
        config.cache_dir.join("resolutions.json"),
        config.latest_ttl_secs,
    );

    Ok(Self {
        config,
        cache,
        tree_cache,
        resolution_cache: std::sync::Mutex::new(resolution_cache),
        shared_tree_cache: None, // Set by run() if redis_url configured
        mounts: Arc::new(RwLock::new(HashMap::new())),
        cancel: CancellationToken::new(),
    })
}
```

- [ ] **Step 3: Update Daemon::run to initialize Redis and emit warnings**

At the start of `run()`, after `self.write_pid_file()`:

```rust
pub async fn run(&self) -> Result<()> {
    self.write_pid_file()?;

    // Check for Redis configuration
    if let Some(ref redis_url) = self.config.redis_url {
        #[cfg(feature = "redis")]
        {
            if let Some(redis) = ctxfs_cache_redis::RedisTreeCache::connect(redis_url).await {
                info!("connected to Redis shared tree cache");
                // Note: shared_tree_cache would need interior mutability or be set at construction
                // For simplicity, we pass it through DaemonServer instead
            } else {
                warn!("failed to connect to Redis at {redis_url}; proceeding without shared cache");
            }
        }
        #[cfg(not(feature = "redis"))]
        {
            warn!(
                "CTXFS_REDIS_URL is set but Redis support is not compiled in. \
                 Build with --features redis to enable shared tree caching."
            );
        }
    }

    // ... rest of run() unchanged ...
```

- [ ] **Step 4: Wire resolution cache into do_mount**

In `do_mount`, after `SourceSpec::parse` and before the resolver logic, add resolution cache lookup:

```rust
fn do_mount(&self, source_str: &str, mount_point: &str) -> Result<MountInfo, String> {
    let mut source =
        SourceSpec::parse(source_str).map_err(|e| format!("invalid source: {e}"))?;

    // For registry sources, check resolution cache first
    if source.provider_type != ProviderType::GitHub {
        let is_latest = source.version == "latest";
        let cache_key = source.to_string();

        // Check resolution cache
        {
            let res_cache = self.resolution_cache.lock().unwrap();
            if let Some(resolved) = res_cache.get(&cache_key) {
                info!("resolution cache HIT for {cache_key}");
                let github_source = SourceSpec {
                    provider_type: ProviderType::GitHub,
                    name: format!("{}/{}", resolved.owner, resolved.repo),
                    version: resolved.git_ref.clone(),
                    subpath: source.subpath.clone().or_else(|| resolved.subpath.clone()),
                };

                // Skip directly to GitHub provider
                return self.mount_github_source(
                    &github_source,
                    source_str,
                    mount_point,
                    github_source.subpath.clone(),
                );
            }
        }

        // Resolution cache MISS — resolve from registry
        let resolver = Self::make_resolver(&source)?;

        if is_latest {
            source.version = self
                .rt_handle
                .block_on(resolver.resolve_latest(&source.name))
                .map_err(|e| format!("failed to resolve latest: {e}"))?;
        }

        let src = self
            .rt_handle
            .block_on(resolver.resolve(&source.name, &source.version))
            .map_err(|e| format!("{e}"))?;

        // Cache the resolution
        {
            let mut res_cache = self.resolution_cache.lock().unwrap();
            if let Err(e) = res_cache.put(&cache_key, src.clone(), is_latest) {
                warn!("failed to cache resolution: {e}");
            }
        }

        let sp = source.subpath.clone().or(src.subpath);
        let github_source = SourceSpec {
            provider_type: ProviderType::GitHub,
            name: format!("{}/{}", src.owner, src.repo),
            version: src.git_ref,
            subpath: sp.clone(),
        };

        return self.mount_github_source(&github_source, source_str, mount_point, sp);
    }

    // GitHub source — direct mount
    let subpath = source.subpath.clone();
    self.mount_github_source(&source, source_str, mount_point, subpath)
}
```

Extract the GitHub mount logic into a helper:

```rust
fn mount_github_source(
    &self,
    github_source: &SourceSpec,
    original_source_str: &str,
    mount_point: &str,
    subpath: Option<String>,
) -> Result<MountInfo, String> {
    let provider = Arc::new(GitHubProvider::new(
        self.config.github_token.as_deref(),
        self.cache.clone(),
        Some(self.tree_cache.clone()),
        self.shared_tree_cache.clone(),
    ));

    let snapshot_data = self
        .rt_handle
        .block_on(provider.fetch_snapshot(github_source))
        .map_err(|e| format!("failed to fetch snapshot: {e}"))?;

    let snapshot: Snapshot = serde_json::from_slice(&snapshot_data)
        .map_err(|e| format!("failed to parse snapshot: {e}"))?;

    std::fs::create_dir_all(mount_point)
        .map_err(|e| format!("failed to create mount point: {e}"))?;

    let source_for_id = SourceSpec::parse(original_source_str)
        .unwrap_or_else(|_| github_source.clone());
    let id = source_for_id.id();
    let commit_sha = snapshot.commit_sha.clone();

    let port = pick_free_port()?;
    let addr = format!("127.0.0.1:{port}");

    let fs = CtxfsNfs::new_with_subpath(
        provider,
        github_source.clone(),
        self.cache.clone(),
        snapshot,
        subpath,
    );
    let nfs_handle = self
        .rt_handle
        .block_on(fs.spawn(&addr))
        .map_err(|e| format!("failed to start NFS server on {addr}: {e}"))?;

    info!("NFS server listening on {} for {original_source_str}", nfs_handle.addr);

    let info = MountInfo {
        id: id.clone(),
        source: original_source_str.to_string(),
        mount_point: mount_point.to_string(),
        commit_sha,
        status: MountStatus::Ready,
        mounted_at: chrono::Utc::now().to_rfc3339(),
        nfs_port: port,
    };

    let handle = MountHandle {
        info: info.clone(),
        _nfs: nfs_handle,
    };

    self.rt_handle.block_on(async {
        let _ = self.mounts.write().await.insert(id, handle);
    });

    Ok(info)
}
```

- [ ] **Step 5: Wire tree/resolution stats into cache_stats**

```rust
async fn cache_stats(self, _: tarpc::context::Context) -> Result<CacheStats, String> {
    let (total_bytes, entry_count) = self.cache.stats();
    let (tree_count, tree_bytes) = self.tree_cache.stats();
    let resolution_count = self.resolution_cache.lock().unwrap().entry_count();
    Ok(CacheStats {
        total_bytes,
        entry_count,
        freed_bytes: 0,
        tree_count,
        tree_bytes,
        resolution_count,
    })
}
```

- [ ] **Step 6: Add feature flag for Redis to daemon Cargo.toml**

In `crates/ctxfs-daemon/Cargo.toml`:

```toml
[features]
default = []
redis = ["ctxfs-cache-redis"]

[dependencies]
# ... existing deps ...
ctxfs-cache-redis = { workspace = true, optional = true }
```

- [ ] **Step 7: Run all tests**

Run: `cargo test`
Expected: PASS

- [ ] **Step 8: Commit**

```bash
git add crates/ctxfs-daemon/src/daemon.rs crates/ctxfs-daemon/Cargo.toml
git commit -m "feat(daemon): wire resolution cache, tree cache, and Redis into mount flow"
```

---

### Task 9: Extend CLI cache commands

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs`

- [ ] **Step 1: Update CacheAction with new flags**

```rust
#[derive(Subcommand)]
enum CacheAction {
    /// Show cache statistics
    Stats,
    /// Prune cache to free space
    Prune {
        /// Maximum blob cache size (e.g., 500000000 for ~500MB)
        #[arg(long)]
        max_size: Option<u64>,
        /// Clear all cached tree manifests
        #[arg(long)]
        trees: bool,
        /// Clear all cached registry resolutions
        #[arg(long)]
        resolutions: bool,
    },
}
```

- [ ] **Step 2: Update the Stats display**

```rust
CacheAction::Stats => {
    let client = connect(&config).await?;
    let stats = client
        .cache_stats(tarpc::context::current())
        .await?
        .map_err(|e| anyhow::anyhow!(e))?;

    println!("Cache statistics:");
    println!("  Blobs:        {} entries, {} bytes", stats.entry_count, stats.total_bytes);
    println!("  Trees:        {} entries, {} bytes", stats.tree_count, stats.tree_bytes);
    println!("  Resolutions:  {} entries", stats.resolution_count);
}
```

- [ ] **Step 3: Update the Prune handler**

The prune handler stays as-is for blob cache (using `cache_prune` RPC). The `--trees` and `--resolutions` flags will need new RPC methods or be handled client-side. For MVP, add them as a note — the daemon already has the caches, but we'd need new RPC methods to expose prune per tier. For now, `--trees` and `--resolutions` will call the existing prune RPC which only handles blobs, and print a message:

```rust
CacheAction::Prune {
    max_size,
    trees,
    resolutions,
} => {
    let client = connect(&config).await?;

    if trees || resolutions {
        // TODO: add dedicated RPC methods for per-tier pruning
        println!("Per-tier pruning not yet available via RPC. Use blob pruning for now.");
    }

    let stats = client
        .cache_prune(tarpc::context::current(), max_size)
        .await?
        .map_err(|e| anyhow::anyhow!(e))?;

    println!("Cache pruned:");
    println!("  Freed:       {} bytes", stats.freed_bytes);
    println!("  Blobs:       {} entries, {} bytes", stats.entry_count, stats.total_bytes);
    println!("  Trees:       {} entries, {} bytes", stats.tree_count, stats.tree_bytes);
    println!("  Resolutions: {} entries", stats.resolution_count);
}
```

- [ ] **Step 4: Run to verify it compiles**

Run: `cargo build -p ctxfs`
Expected: SUCCESS

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): extend cache stats/prune with tree and resolution tiers"
```

---

### Task 10: Update CLAUDE.md and workspace docs

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Update architecture section**

In the Architecture section of `CLAUDE.md`, add the new crate:

```
- ctxfs-cache-redis: Optional Redis shared tree cache (depends on cache)
```

Add new env vars to the Environment section:

```
- `CTXFS_REDIS_URL`: Optional Redis URL for shared tree caching
- `CTXFS_LATEST_TTL_SECS`: TTL for @latest resolution cache (default: 3600)
- `CTXFS_TREE_CACHE_MAX_BYTES`: Max local tree cache size (default: 500MB)
```

- [ ] **Step 2: Commit**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md with tiered caching architecture"
```

---

### Task 11: Integration test — resolution + tree cache roundtrip

**Files:**
- Create: `crates/ctxfs-cache/tests/tiered_cache.rs`

- [ ] **Step 1: Write the integration test**

```rust
//! Integration tests for the tiered cache system.

use ctxfs_cache::{ResolutionCache, TreeCache};
use ctxfs_provider_common::resolver::ResolvedSource;

#[test]
fn resolution_cache_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let file_path = dir.path().join("resolutions.json");

    // Phase 1: populate
    {
        let mut cache = ResolutionCache::new(file_path.clone(), 3600);

        let src = ResolvedSource {
            owner: "facebook".into(),
            repo: "react".into(),
            git_ref: "abc123".into(),
            subpath: Some("packages/react".into()),
        };
        cache.put("npm:react@19.1.0", src, false).unwrap();

        let src2 = ResolvedSource {
            owner: "psf".into(),
            repo: "requests".into(),
            git_ref: "v2.31.0".into(),
            subpath: None,
        };
        cache.put("pypi:requests@2.31.0", src2, false).unwrap();

        assert_eq!(cache.entry_count(), 2);
    }

    // Phase 2: reload and verify
    let cache = ResolutionCache::load(file_path, 3600);
    let react = cache.get("npm:react@19.1.0").unwrap();
    assert_eq!(react.owner, "facebook");
    assert_eq!(react.subpath, Some("packages/react".into()));

    let requests = cache.get("pypi:requests@2.31.0").unwrap();
    assert_eq!(requests.owner, "psf");
    assert!(requests.subpath.is_none());
}

#[test]
fn tree_cache_full_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let tree_dir = dir.path().join("trees");

    let cache = TreeCache::new(tree_dir.clone(), 100 * 1024 * 1024);

    // Store a tree
    let snapshot_json = serde_json::json!({
        "source": "github:facebook/react@abc123",
        "commit_sha": "abc123",
        "root_directory": {"algorithm": "sha256", "hex": "deadbeef"},
        "created_at": "2026-01-01T00:00:00Z"
    });
    let data = serde_json::to_vec(&snapshot_json).unwrap();
    cache.put("facebook", "react", "abc123", &data).unwrap();

    // Retrieve it
    let result = cache.get("facebook", "react", "abc123").unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&result).unwrap();
    assert_eq!(parsed["commit_sha"], "abc123");

    // Stats
    let (count, size) = cache.stats();
    assert_eq!(count, 1);
    assert!(size > 0);

    // Different commit is a miss
    assert!(cache.get("facebook", "react", "def456").is_none());

    // Prune and verify
    cache.prune_all().unwrap();
    assert!(cache.get("facebook", "react", "abc123").is_none());
    assert_eq!(cache.stats().0, 0);
}

#[test]
fn tree_cache_survives_restart() {
    let dir = tempfile::tempdir().unwrap();
    let tree_dir = dir.path().join("trees");

    {
        let cache = TreeCache::new(tree_dir.clone(), 100 * 1024 * 1024);
        cache.put("owner", "repo", "sha1", b"{\"test\": true}").unwrap();
    }

    // New instance, same directory
    let cache = TreeCache::new(tree_dir, 100 * 1024 * 1024);
    assert!(cache.get("owner", "repo", "sha1").is_some());
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p ctxfs-cache --test tiered_cache`
Expected: PASS

- [ ] **Step 3: Commit**

```bash
git add crates/ctxfs-cache/tests/tiered_cache.rs
git commit -m "test: add integration tests for resolution and tree cache lifecycle"
```

---

Plan complete and saved to `docs/superpowers/plans/2026-04-07-tiered-caching.md`. Two execution options:

**1. Subagent-Driven (recommended)** - I dispatch a fresh subagent per task, review between tasks, fast iteration

**2. Inline Execution** - Execute tasks in this session using executing-plans, batch execution with checkpoints

Which approach?
