pub mod reservation;
pub use reservation::{MountCacheView, RepoKey};

mod resolution;
pub use resolution::{CachedResolution, ResolutionCache};

mod tree;
pub use tree::{TreeCache, SCHEMA_VERSION};

mod shared;
pub use shared::SharedTreeCache;

use crate::reservation::ReservationEntry;
use ctxfs_core::digest::HashAlgorithm;
use ctxfs_core::error::CtxfsError;
use ctxfs_core::Digest;
use linked_hash_map::LinkedHashMap;
use std::collections::{BTreeSet, HashMap};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Entry in the LRU tracking state. Tracks both size and on-disk algorithm
/// so eviction can delete the file from the correct fan-out subdir (`sha1/`
/// or `sha256/`) without re-examining the caller's digest.
struct CacheEntry {
    size: u64,
    algorithm: HashAlgorithm,
}

/// Unified cache state behind a single mutex.
///
/// Holds the LRU entries, per-blob ownership sets, and per-repo reservation
/// budgets together so the eviction loop (T3b) can consult all three without
/// a second lock acquisition or a lock-order dance.
pub(crate) struct CacheState {
    pub(crate) entries: LinkedHashMap<String, CacheEntry>,
    pub(crate) total_bytes: u64,
    /// blob hex → set of repos whose manifest references this blob.
    /// Pre-populated at manifest time by `register_mount` (T3b); extended
    /// by `add_owner` / `put_for` for late-discovered blobs.
    pub(crate) blob_owners: HashMap<String, BTreeSet<RepoKey>>,
    /// Per-repo reservation budgets and active-mount refcounts.
    pub(crate) reservations: HashMap<RepoKey, ReservationEntry>,
}

impl CacheState {
    /// Returns the `(key, size, algorithm)` of the evicted entry, or `None`
    /// if the LRU was already empty. Does NOT remove the on-disk file;
    /// callers must do that themselves.
    fn evict_oldest(&mut self) -> Option<(String, u64, HashAlgorithm)> {
        self.entries.pop_front().map(|(key, entry)| {
            self.total_bytes -= entry.size;
            (key, entry.size, entry.algorithm)
        })
    }
}

pub struct BlobCache {
    root: PathBuf,
    max_bytes: Arc<AtomicU64>,
    state: Mutex<CacheState>,
    /// Cache-global counter: how many LRU eviction candidates were skipped
    /// because evicting them would have violated a per-repo reservation.
    /// Incremented by the T3b eviction loop; read by T3c status assembly.
    eviction_blocked_total: AtomicU64,
}

impl std::fmt::Debug for BlobCache {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobCache")
            .field("root", &self.root)
            .field("max_bytes", &self.max_bytes.load(Ordering::Relaxed))
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
            max_bytes: Arc::new(AtomicU64::new(max_bytes)),
            state: Mutex::new(CacheState {
                entries: LinkedHashMap::new(),
                total_bytes: 0,
                blob_owners: HashMap::new(),
                reservations: HashMap::new(),
            }),
            eviction_blocked_total: AtomicU64::new(0),
        };

        cache.rebuild_index()?;
        Ok(cache)
    }

    fn blob_path(&self, digest: &Digest) -> PathBuf {
        self.root.join(digest.to_path())
    }

    /// Remove the on-disk blob file for the given hex key.
    /// Uses the on-disk algorithm so the file is found in the correct
    /// fan-out subdir (`sha1/` or `sha256/`).
    fn remove_blob_file(&self, hex: &str, algorithm: HashAlgorithm) {
        let digest = Digest {
            algorithm,
            hex: hex.to_string(),
        };
        let path = self.blob_path(&digest);
        let _ = fs::remove_file(path);
    }

    pub fn get(&self, digest: &Digest) -> Option<Vec<u8>> {
        let key = digest.hex.clone();
        // Read the on-disk algorithm from the LRU entry. After rebuild_index,
        // this reflects where the file actually lives (sha1/ or sha256/),
        // which may differ from the caller's digest.algorithm when an old
        // sha256-labeled Git blob coexists with new sha1-labeled code.
        let on_disk_algo = {
            let mut state = self.state.lock().unwrap();
            let entry = state.entries.get_refresh(&key)?;
            entry.algorithm
        };

        // Use the LRU's known on-disk algorithm to compute the path; the
        // caller's `digest.algorithm` is a labeling hint, but the file lives
        // wherever rebuild_index found it.
        let on_disk_digest = Digest {
            algorithm: on_disk_algo,
            hex: digest.hex.clone(),
        };
        fs::read(self.blob_path(&on_disk_digest)).ok()
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
        for (k, algo) in self.lru_insert_evict(key, size, digest.algorithm) {
            self.remove_blob_file(&k, algo);
        }

        Ok(())
    }

    /// Insert `(key, size, algorithm)` into the LRU, then evict entries until
    /// the total is within `max_bytes`. Returns `(key, algorithm)` pairs for
    /// evicted entries; callers must remove the corresponding on-disk files.
    fn lru_insert_evict(
        &self,
        key: String,
        size: u64,
        algorithm: HashAlgorithm,
    ) -> Vec<(String, HashAlgorithm)> {
        let mut evicted = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            if let Some(existing) = state.entries.get(&key) {
                state.total_bytes -= existing.size;
            }
            let _ = state.entries.insert(key, CacheEntry { size, algorithm });
            state.total_bytes += size;
            let limit = self.max_bytes.load(Ordering::Relaxed);
            while state.total_bytes > limit && !state.entries.is_empty() {
                // TODO(T3b): before evicting, check blob_owners vs
                // reservations; skip and increment eviction_blocked_total
                // when the candidate is protected by its repo's reservation.
                if let Some((k, _, algo)) = state.evict_oldest() {
                    evicted.push((k, algo));
                }
            }
        }
        evicted
    }

    pub fn contains(&self, digest: &Digest) -> bool {
        let state = self.state.lock().unwrap();
        state.entries.contains_key(&digest.hex)
    }

    /// Returns `true` iff every digest in `digests` is currently tracked in
    /// the LRU. Cheap — single mutex acquire — used by the singleflight
    /// fast-path: if the manifest's blobs are all already cached, skip the
    /// tarball entirely.
    pub fn contains_all<'a, I>(&self, digests: I) -> bool
    where
        I: IntoIterator<Item = &'a Digest>,
    {
        let state = self.state.lock().unwrap();
        digests
            .into_iter()
            .all(|d| state.entries.contains_key(&d.hex))
    }

    /// Bytes-in-memory atomic commit. Verifies `Digest::sha256(data) == digest`
    /// **before** writing — if the check fails the cache is not modified.
    ///
    /// Use this for content you have fully buffered and wish to verify before
    /// persisting. For streaming content (e.g. tarball entries), use
    /// [`commit_atomic_with_writer`] and verify externally with the
    /// appropriate algorithm.
    ///
    /// LRU bookkeeping mirrors `put`. Use this method (not `put`) for content
    /// where corruption is a real risk — bulk tarball hydration, concurrent
    /// prefetch, etc.
    pub fn commit_atomic(&self, digest: &Digest, data: &[u8]) -> Result<(), CtxfsError> {
        // External verify: bytes-in-memory variant has the data in hand.
        // Compares SHA-256 because non-Git callers store with Digest::sha256.
        // (Git callers go through the streaming path with their own SHA-1
        // verification.)
        let computed = Digest::sha256(data);
        if computed.hex != digest.hex {
            return Err(CtxfsError::Cache(format!(
                "blob digest mismatch: expected {}, got {}",
                digest.hex, computed.hex
            )));
        }
        let mut writer = self.commit_atomic_with_writer()?;
        use std::io::Write;
        writer
            .write_all(data)
            .map_err(|e| CtxfsError::Cache(format!("commit write: {e}")))?;
        writer.finalize(digest)
    }

    /// Streaming variant. Trust-the-caller: caller must verify content matches
    /// the digest passed to [`BlobTempWriter::finalize`]. The writer does not
    /// hash content internally — different M3 callers verify against different
    /// algorithms (SHA-256 for the bytes-in-memory [`commit_atomic`]; Git blob
    /// SHA-1 for the tarball path's external hasher + tee).
    ///
    /// See [`commit_atomic`] for the self-verifying entry point.
    pub fn commit_atomic_with_writer(&self) -> Result<BlobTempWriter, CtxfsError> {
        let tmp_dir = self.root.join("tmp");
        fs::create_dir_all(&tmp_dir)
            .map_err(|e| CtxfsError::Cache(format!("mkdir tmp failed: {e}")))?;
        let temp = tempfile::NamedTempFile::new_in(&tmp_dir)
            .map_err(|e| CtxfsError::Cache(format!("tmp file create: {e}")))?;
        Ok(BlobTempWriter {
            cache_root: self.root.clone(),
            cache: self,
            temp: Some(temp),
            bytes_written: 0,
        })
    }

    /// Sweep `<root>/tmp/` of files older than `older_than` (mtime-based).
    /// Called by the daemon on startup to clear orphans from a crash
    /// mid-commit. Returns the count of files unlinked. A missing tmp/ dir
    /// returns 0 without error.
    pub fn cleanup_orphan_temps(&self, older_than: std::time::Duration) -> Result<u64, CtxfsError> {
        let tmp_dir = self.root.join("tmp");
        if !tmp_dir.exists() {
            return Ok(0);
        }
        let mut cleared = 0u64;
        let now = std::time::SystemTime::now();
        for entry in fs::read_dir(&tmp_dir)
            .map_err(|e| CtxfsError::Cache(format!("read_dir tmp: {e}")))?
            .flatten()
        {
            let p = entry.path();
            if !p.is_file() {
                continue;
            }
            let mtime = entry
                .metadata()
                .and_then(|m| m.modified())
                .unwrap_or(std::time::SystemTime::UNIX_EPOCH);
            if let Ok(age) = now.duration_since(mtime) {
                if age > older_than && fs::remove_file(&p).is_ok() {
                    cleared += 1;
                }
            }
        }
        Ok(cleared)
    }

    pub fn prune(&self, max_bytes: Option<u64>) -> Result<u64, CtxfsError> {
        let limit = max_bytes.unwrap_or_else(|| self.max_bytes.load(Ordering::Relaxed));
        let mut evicted = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            while state.total_bytes > limit && !state.entries.is_empty() {
                if let Some(entry) = state.evict_oldest() {
                    evicted.push(entry);
                }
            }
        }
        let freed = evicted.iter().map(|(_, size, _)| size).sum();
        for (key, _, algo) in evicted {
            self.remove_blob_file(&key, algo);
        }
        Ok(freed)
    }

    pub fn stats(&self) -> (u64, usize) {
        let state = self.state.lock().unwrap();
        (state.total_bytes, state.entries.len())
    }

    /// Returns the current maximum cache size in bytes.
    pub fn max_bytes(&self) -> u64 {
        self.max_bytes.load(Ordering::Relaxed)
    }

    /// Returns the number of blob entries currently tracked in the cache.
    pub fn count(&self) -> u64 {
        self.state.lock().unwrap().entries.len() as u64
    }

    /// Returns the total bytes currently stored in the cache.
    pub fn total_bytes(&self) -> u64 {
        self.state.lock().unwrap().total_bytes
    }

    /// Prune blob cache entries until total usage fits within `target_bytes`.
    /// Returns bytes freed. Does NOT touch the tree cache. Returns 0 if already
    /// under the target.
    pub fn prune_blobs(&self, target_bytes: u64) -> u64 {
        let mut evicted = Vec::new();
        let initial;
        {
            let mut state = self.state.lock().unwrap();
            initial = state.total_bytes;
            while state.total_bytes > target_bytes {
                match state.evict_oldest() {
                    Some(entry) => evicted.push(entry),
                    None => break,
                }
            }
        }
        for (key, _, algo) in evicted {
            self.remove_blob_file(&key, algo);
        }
        let freed = self.state.lock().unwrap().total_bytes;
        initial.saturating_sub(freed)
    }

    /// Record ownership for a single blob hex without writing data.
    ///
    /// Used by `register_mount` (T3b) to seed manifest membership at
    /// snapshot time, and by `MountCacheView::record_ownership_after_finalize`
    /// for the streaming tarball commit path. Idempotent.
    pub fn add_owner(&self, repo_key: &RepoKey, hex: &str) {
        let mut state = self.state.lock().unwrap();
        let _ = state
            .blob_owners
            .entry(hex.to_string())
            .or_default()
            .insert(repo_key.clone());
    }

    /// Put a blob and record `repo_key` as an owner.
    ///
    /// Ownership is recorded *after* the write; the eviction loop can
    /// briefly observe the blob as unowned between these two steps. For
    /// truncated-tree fallbacks and late-discovered blobs (after
    /// `register_mount`), this is acceptable by design — the B5
    /// reservation invariant is scoped to manifest-time membership;
    /// late additions are best-effort. Use `register_mount(key,
    /// reservation_bytes, manifest_digests)` at mount time for full
    /// reservation protection.
    pub fn put_for(
        &self,
        repo_key: &RepoKey,
        digest: &Digest,
        data: &[u8],
    ) -> Result<(), CtxfsError> {
        self.put(digest, data)?;
        self.add_owner(repo_key, &digest.hex);
        Ok(())
    }

    /// Sum the sizes of **cached** blobs whose owner-set contains `key`.
    ///
    /// Blobs claimed via `add_owner` but not yet written to the cache (e.g.,
    /// pre-claimed at manifest time with no local copy yet) contribute 0.
    #[must_use]
    pub fn working_set_bytes(&self, key: &RepoKey) -> u64 {
        let state = self.state.lock().unwrap();
        let mut total = 0u64;
        for (hex, owners) in &state.blob_owners {
            if owners.contains(key) {
                if let Some(entry) = state.entries.get(hex.as_str()) {
                    total += entry.size;
                }
            }
        }
        total
    }

    /// Cache-global counter: how many times an eviction candidate was skipped
    /// because evicting it would have violated a per-repo reservation.
    ///
    /// Always returns 0 until T3b lands the reservation-aware eviction loop.
    #[must_use]
    pub fn eviction_attempts_blocked_by_reservation(&self) -> u64 {
        self.eviction_blocked_total.load(Ordering::Relaxed)
    }

    /// Current reservation budget for `key`, or `None` if `key` has no
    /// active reservation (not yet registered, or already unregistered).
    ///
    /// Always returns `None` until T3b lands `register_mount`.
    #[must_use]
    pub fn reservation_bytes(&self, key: &RepoKey) -> Option<u64> {
        let state = self.state.lock().unwrap();
        state.reservations.get(key).map(|e| e.reserved_bytes)
    }

    /// Update the maximum cache size at runtime. If `new_max` is smaller than
    /// the current usage, entries are evicted (oldest-first) until usage fits.
    pub fn set_max_bytes(&self, new_max: u64) {
        self.max_bytes.store(new_max, Ordering::Relaxed);
        let mut evicted = Vec::new();
        {
            let mut state = self.state.lock().unwrap();
            while state.total_bytes > new_max {
                if let Some(entry) = state.evict_oldest() {
                    evicted.push(entry);
                } else {
                    break;
                }
            }
        }
        for (key, _, algo) in evicted {
            self.remove_blob_file(&key, algo);
        }
    }

    /// Rebuild the LRU index by scanning the cache directory.
    ///
    /// Walks both `sha1/` and `sha256/` fan-out subdirs and tags each entry
    /// with its on-disk algorithm. When both subdirs contain the same hex
    /// (migration overlap from pre-M5 code), the `sha1/` entry wins and the
    /// `sha256/` file is deleted — sha1 is the new canonical for Git blobs.
    fn rebuild_index(&self) -> Result<(), CtxfsError> {
        let mut entries: Vec<(String, HashAlgorithm, u64, std::time::SystemTime)> = Vec::new();

        if let Ok(algo_dirs) = fs::read_dir(&self.root) {
            for algo_entry in algo_dirs.flatten() {
                let algo_path = algo_entry.path();
                if !algo_path.is_dir() {
                    continue;
                }
                let algo_name = algo_entry.file_name().to_string_lossy().into_owned();
                let algorithm = match algo_name.as_str() {
                    "sha256" => HashAlgorithm::Sha256,
                    "sha1" => HashAlgorithm::Sha1,
                    "tmp" => continue, // partial blobs; cleanup_orphan_temps handles these
                    _ => continue,     // unknown algo dir — skip; future-proof
                };
                Self::scan_fan_out_dir(&algo_path, algorithm, &mut entries)?;
            }
        }

        entries.sort_by_key(|(_, _, _, mtime)| *mtime);

        // Dedupe by hex: if both sha1 and sha256 paths exist for the same hex,
        // prefer sha1 (the new canonical from M5). Delete the sha256 file on disk.
        let mut by_hex: HashMap<String, (HashAlgorithm, u64, std::time::SystemTime)> =
            HashMap::new();
        let mut to_delete: Vec<(String, HashAlgorithm)> = Vec::new();
        for (hex, algo, size, mtime) in entries {
            match by_hex.entry(hex.clone()) {
                std::collections::hash_map::Entry::Vacant(v) => {
                    let _ = v.insert((algo, size, mtime));
                }
                std::collections::hash_map::Entry::Occupied(mut o) => {
                    let existing_algo = o.get().0;
                    if existing_algo == HashAlgorithm::Sha1 {
                        // Existing sha1 wins; delete the new candidate.
                        to_delete.push((hex.clone(), algo));
                    } else if algo == HashAlgorithm::Sha1 {
                        // Replace existing sha256 with the sha1 version.
                        *o.get_mut() = (algo, size, mtime);
                        to_delete.push((hex.clone(), existing_algo));
                    } else {
                        // Both sha256 — keep the older (already in map). Shouldn't happen.
                        to_delete.push((hex.clone(), algo));
                    }
                }
            }
        }

        let mut sorted: Vec<(String, HashAlgorithm, u64, std::time::SystemTime)> = by_hex
            .into_iter()
            .map(|(h, (a, s, m))| (h, a, s, m))
            .collect();
        sorted.sort_by_key(|(_, _, _, mtime)| *mtime);

        let mut state = self.state.lock().unwrap();
        state.entries.clear();
        state.total_bytes = 0;

        for (hex, algorithm, size, _) in sorted {
            state.total_bytes += size;
            let _ = state.entries.insert(hex, CacheEntry { size, algorithm });
        }
        drop(state);

        for (hex, algo) in to_delete {
            self.remove_blob_file(&hex, algo);
        }

        Ok(())
    }

    fn scan_fan_out_dir(
        algo_path: &Path,
        algorithm: HashAlgorithm,
        entries: &mut Vec<(String, HashAlgorithm, u64, std::time::SystemTime)>,
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
                    entries.push((hex, algorithm, metadata.len(), mtime));
                }
            }
        }
        Ok(())
    }
}

/// Content-agnostic transactional blob writer returned by
/// [`BlobCache::commit_atomic_with_writer`].
///
/// Implements [`std::io::Write`] so callers can use [`std::io::copy`] or any
/// reader pipeline directly. Does **not** hash content internally — different
/// M3 callers verify against different algorithms (SHA-256 for the in-memory
/// [`BlobCache::commit_atomic`]; Git blob SHA-1 for the tarball path's
/// external hasher + tee). [`BlobTempWriter::finalize`] does fsync → rename →
/// parent-dir fsync → LRU update only.
///
/// Drop without finalizing → temp file is cleaned via
/// [`tempfile::NamedTempFile`]'s RAII.
pub struct BlobTempWriter<'a> {
    cache_root: PathBuf,
    cache: &'a BlobCache,
    temp: Option<tempfile::NamedTempFile>,
    bytes_written: u64,
}

impl std::fmt::Debug for BlobTempWriter<'_> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BlobTempWriter")
            .field("cache_root", &self.cache_root)
            .field("bytes_written", &self.bytes_written)
            .finish_non_exhaustive()
    }
}

impl std::io::Write for BlobTempWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let f = self.temp.as_mut().expect("writer used after finalize");
        let n = f.as_file_mut().write(buf)?;
        self.bytes_written += n as u64;
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        let f = self.temp.as_mut().expect("writer used after finalize");
        f.as_file_mut().flush()
    }
}

impl BlobTempWriter<'_> {
    /// Trust-the-caller finalize: fsync the temp file, rename(2) into the
    /// canonical path derived from `expected`, fsync the parent directory,
    /// then update LRU bookkeeping.
    ///
    /// Does **not** verify content against `expected` — the caller is
    /// responsible for verification (see [`BlobCache::commit_atomic`] for the
    /// self-verifying entry point, or verify externally before calling this).
    pub fn finalize(mut self, expected: &Digest) -> Result<(), CtxfsError> {
        let temp = self.temp.take().expect("temp present until finalize");
        temp.as_file()
            .sync_all()
            .map_err(|e| CtxfsError::Cache(format!("tmp fsync: {e}")))?;

        let dest = self.cache.blob_path(expected);
        if let Some(parent) = dest.parent() {
            fs::create_dir_all(parent)
                .map_err(|e| CtxfsError::Cache(format!("mkdir parent failed: {e}")))?;
        }
        let dest_for_persist = dest.clone();
        let _persisted = temp
            .persist(&dest_for_persist)
            .map_err(|e| CtxfsError::Cache(format!("rename to canonical: {e}")))?;

        // Fsync parent so the rename(2) is durable across crash.
        // Best-effort: parent fsync isn't supported on every fs.
        if let Some(parent) = dest.parent() {
            if let Ok(d) = std::fs::File::open(parent) {
                if let Err(e) = d.sync_all() {
                    tracing::debug!(
                        target: "ctxfs.cache.atomic",
                        path = %parent.display(),
                        error = ?e,
                        "parent dir fsync failed (non-fatal)"
                    );
                }
            }
        }

        // LRU bookkeeping (post-rename so file is durable before tracking).
        let size = self.bytes_written;
        let key = expected.hex.clone();
        for (k, algo) in self.cache.lru_insert_evict(key, size, expected.algorithm) {
            self.cache.remove_blob_file(&k, algo);
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
    fn prune_blobs_shrinks_blob_cache_only() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024 * 1024).unwrap();

        // Insert 5 blobs of 100 bytes each (500 bytes total)
        let digests: Vec<Digest> = (0..5u8)
            .map(|i| {
                let d = Digest::sha256(&[i]);
                cache.put(&d, &[i; 100]).unwrap();
                d
            })
            .collect();

        let (total_before, count_before) = cache.stats();
        assert_eq!(total_before, 500);
        assert_eq!(count_before, 5);

        // Prune to 250 bytes — should evict oldest 3 blobs (300 bytes freed)
        let freed = cache.prune_blobs(250);
        assert!(
            freed >= 250,
            "expected at least 250 bytes freed, got {freed}"
        );

        let (total_after, count_after) = cache.stats();
        assert!(
            total_after <= 250,
            "expected total_bytes <= 250, got {total_after}"
        );
        assert!(count_after < 5);

        // Oldest blobs (digests[0], digests[1], digests[2]) should be evicted from disk
        for (i, d) in digests.iter().enumerate() {
            let on_disk = cache.get(d).is_some();
            if i < 3 {
                // evicted
                assert!(
                    !on_disk,
                    "blob {i} should have been evicted from disk but was not"
                );
            } else {
                // retained
                assert!(on_disk, "blob {i} should be retained but was evicted");
            }
        }
    }

    #[test]
    fn prune_blobs_noop_when_already_under_target() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();

        let d = Digest::sha256(b"small");
        cache.put(&d, b"tiny").unwrap();

        let freed = cache.prune_blobs(1024);
        assert_eq!(freed, 0);
        assert!(cache.contains(&d));
    }

    #[test]
    fn debug_impl() {
        let dir = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
        let debug = format!("{cache:?}");
        assert!(debug.contains("BlobCache"));
        assert!(debug.contains("max_bytes"));
    }

    #[test]
    fn set_max_bytes_updates_limit() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(tmp.path().to_path_buf(), 1024).unwrap();
        assert_eq!(cache.max_bytes(), 1024);

        cache.set_max_bytes(2048);
        assert_eq!(cache.max_bytes(), 2048);
    }

    #[test]
    fn set_max_bytes_smaller_triggers_eviction() {
        let tmp = tempfile::tempdir().unwrap();
        let cache = BlobCache::new(tmp.path().to_path_buf(), 10_000).unwrap();

        // Put 3 blobs of ~2KB each (total ~6KB)
        for i in 0..3u8 {
            let digest = ctxfs_core::digest::Digest::sha256(&[i; 2048]);
            cache.put(&digest, &[i; 2048]).unwrap();
        }
        assert_eq!(cache.total_bytes(), 6144);

        // Shrink to 4KB — must evict at least one blob
        cache.set_max_bytes(4096);
        assert!(
            cache.total_bytes() <= 4096,
            "expected eviction down to 4KB, got {}",
            cache.total_bytes()
        );
    }

    #[test]
    fn get_serves_legacy_sha256_layout_after_sha1_label_added() {
        // Simulate a pre-M5 cache where a Git blob SHA-1 hex was stored
        // under sha256/<hex>. Re-open the cache (rebuild_index runs) then
        // look up via the new Sha1 label. rebuild_index tags the entry with
        // its on-disk algorithm (Sha256), so get reads from the correct
        // path despite the caller's digest using HashAlgorithm::Sha1.
        let dir = tempfile::tempdir().unwrap();
        let git_hex = "356a192b7913b04c54574d18c28d46e6395428ab";

        {
            let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
            let legacy_digest = Digest::from_sha256_hex(git_hex);
            cache.put(&legacy_digest, b"legacy blob bytes").unwrap();
        }

        // Re-open — rebuild_index tags the existing file as Sha256.
        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
        let new_digest = Digest::from_sha1_hex(git_hex);
        let bytes = cache.get(&new_digest);
        assert_eq!(bytes.as_deref(), Some(b"legacy blob bytes" as &[u8]));
    }

    #[test]
    fn rebuild_index_dedupes_both_algo_paths_for_same_hex() {
        let dir = tempfile::tempdir().unwrap();
        let git_hex = "356a192b7913b04c54574d18c28d46e6395428ab";

        {
            let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
            cache
                .put(&Digest::from_sha256_hex(git_hex), b"old layout")
                .unwrap();
        }
        // Manually write a sha1/<hex> file simulating mid-migration state.
        let sha1_path = dir
            .path()
            .join(format!("sha1/{}/{}", &git_hex[..2], &git_hex[2..]));
        fs::create_dir_all(sha1_path.parent().unwrap()).unwrap();
        fs::write(&sha1_path, b"new layout").unwrap();

        let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
        // Sha1 wins on dedupe; sha256 file should be gone.
        let sha256_path = dir
            .path()
            .join(format!("sha256/{}/{}", &git_hex[..2], &git_hex[2..]));
        assert!(
            !sha256_path.exists(),
            "rebuild_index should delete the sha256 loser"
        );
        assert!(
            sha1_path.exists(),
            "rebuild_index should keep the sha1 winner"
        );

        let stored = cache.get(&Digest::from_sha1_hex(git_hex)).unwrap();
        assert_eq!(&stored, b"new layout");
    }
}
