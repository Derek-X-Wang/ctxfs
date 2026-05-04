# Phase 4 — M5: B3-label + B5 per-repo cache reservation + B6 LFS detect — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Plan version:** v2 (Codex-reviewed 2026-04-30 via counsel; review at `/tmp/counsel/20260430-061414-claude-to-codex-c95252/codex.md`). Verdict: **revise**. v2 applies all 10 required edits.

**v2 changes vs v1** (each tied to Codex's required edits):
1. **T1 cache layout** — `LruState::CacheEntry` now records `algorithm: HashAlgorithm` so eviction's `remove_blob_file` consults it. `rebuild_index` deduplicates if both `sha1/<hex>` and `sha256/<hex>` exist for the same hex (prefers `sha1/`; deletes the loser). Eliminates the eviction leak Codex flagged. (Codex #1.)
2. **T1 TreeCache schema bump** — `SCHEMA_VERSION` 2 → 3. Old cached manifests label Git blob digests as `HashAlgorithm::Sha256`; bumping invalidates them so they re-fetch with correct labels. (Codex #2.)
3. **B5 ownership re-rooted on manifest membership.** Ownership is no longer recorded in `MountCacheView::put`. Instead, `BlobCache::register_mount(key, reservation_bytes, manifest_digests: &[String])` is called by the daemon after the snapshot is built; cache adds `key` to the owner-set of every blob the manifest references — whether or not the blob is currently cached. `BlobCache::put` then arrives at a *pre-claimed* owner set; truncated-tree late-discovered blobs use `BlobCache::add_owner(key, hex)` to extend ownership without re-registering the whole mount. `unregister_mount` removes `key` from all owner sets. This makes the B5 invariant enforceable for cache hits, contains_all skips, and tree-cache hits. (Codex #3, #4.)
4. **Single mutex `CacheState`** — LRU entries, blob_owners map, reservations table, and per-repo footprint cache live behind one `Mutex<CacheState>`. Eviction holds it throughout — no lock-order dance, no snapshot/reacquire. Existing `BlobCache::state` is repurposed to this. (Codex #5.)
5. **Default reservation rebalance** — `ReservationEntry { reserved_bytes, is_explicit_override, refcount }`. On `register_mount` / `unregister_mount`, walk reservations; for entries with `!is_explicit_override`, set `reserved_bytes = max_bytes / count(active)`. Existing default-mount reservations adjust as new mounts come/go. Explicit overrides untouched. (Codex #6.)
6. **B5 invariant test data fixed** — old test had A=300 + B=700 = 1000 = cache_max (no eviction). v2: cache_max=500, A reserves 400 with working set 300 (≤ reservation), B writes 400 forcing eviction; without protection oldest A blob would evict; with protection cache overflows to 700 and A's blobs all survive. (Codex #7.)
7. **B6 single detection site for non-tarball + manifest-derived sha→path map** — `fetch_blob_content` is the single leaf for both lazy and small-blob-prefetch (the prefetch calls `fetch_blob_with_sha` → `fetch_blob_content`); detecting once there avoids double-counting. `GitHubProvider` stashes a `sha_to_path: Mutex<HashMap<String, PathBuf>>` populated post-snapshot from the tree entries; detection consults it for sample-path. Tarball detection has `mount_path` already in scope. (Codex #8.)
8. **Status assembly seam** — `DaemonServer::get_status` no longer just returns `observability.status_report()`. New `DaemonServer::assemble_status_report` calls observability for the base report, then walks `report.mounts` and augments each `MountSummary` with `cache.working_set_bytes(key)`, `cache.reservation_bytes(key)`, plus the `lfs_pointer_files`/`lfs_pointer_sample_paths` fields from the per-mount counters. Cache-global field `cache_eviction_attempts_blocked_by_reservation` populated from `BlobCache::eviction_attempts_blocked_by_reservation()`. (Codex #9.)
9. **`#[serde(default)]` on additive fields** — every new field on `MountSummary` and `StatusReportV1` carries the attribute so newer clients can deserialize older v1 payloads cleanly. (Codex #10.)
10. **No new deps** — manual 3-line LFS pointer parser instead of pulling in `regex` (Codex extra). Tarball detection extends the **existing local `Tee`** at `crates/ctxfs-provider-git/src/github.rs:1255` (a 5-line struct in scope of `fetch_tarball_into_cache`); does **not** reference a non-existent `std::io::Tee`. (Codex extra.)

**Goal:** Close the three remaining triage-bugs (B3-label, B5, B6) so Phase 4 ends with B1, B2, B3-label, B4, B5, B6, B7 shipped (B8 deferred to Phase 5 by design). After M5, the only Phase 4 follow-up is M6 — a decision memo, not code.

**Architecture:**

- **B3-label** is a labeling bug. `Digest::from_sha256_hex(&sha)` is currently called with 40-char Git blob SHA-1 hexes from the GitHub Trees API. Fix is additive: add `HashAlgorithm::Sha1`, add `Digest::from_sha1_hex`, route the five GitHub-blob construction sites in `provider-git` through the new constructor. **Cache layout uses per-entry algorithm tracking** so eviction and rebuild stay correct: `CacheEntry` learns `algorithm: HashAlgorithm`, `remove_blob_file` consults it, and `rebuild_index` walks both `sha1/` and `sha256/` subdirs and deduplicates by hex (prefer `sha1/` on collision; delete the loser). New GitHub puts land at `sha1/<hex>`; old `sha256/<hex>` entries are recognized correctly until LRU clears them. **TreeCache `SCHEMA_VERSION` bumps 2 → 3** — old cached manifests carry mislabeled digests and would round-trip incorrectly; the bump invalidates them at first read and they're re-fetched with the right labels. No on-disk corruption hazard.

- **B6 (LFS detect-and-surface)** lives in `provider-common::lfs` as a pure-bytes helper `detect_lfs_pointer(bytes: &[u8]) -> Option<LfsPointerInfo>`. A manual 3-line parser (no regex dep) returns `Some(info)` only when the canonical anchored format matches. Two detection sites in `provider-git`:
  - `fetch_blob_content` — single leaf for lazy reads AND `fetch_small_blobs_concurrent` (which calls `fetch_blob_with_sha` → `fetch_blob_content`). Detecting once here avoids double-counting.
  - `fetch_tarball_into_cache` — tarball entries take a different (streaming) code path; detection extends the existing local `Tee` adapter at `github.rs:1255` to mirror entry bytes into a small `Vec<u8>` when `expected_size <= 1024`, then runs `detect_lfs_pointer` after the SHA-1 verify succeeds.

  Sample-path resolution: `GitHubProvider` stashes a `sha_to_path: Mutex<HashMap<String, PathBuf>>` populated in `fetch_snapshot_inner` from the tree entries. `fetch_blob_content` consults it; tarball already has `mount_path` in scope. The map is per-provider-instance (per-mount) and clears on snapshot rebuild. On detection: per-mount counter increment, `tracing::warn!`, and a 3-deep bounded sample buffer push. `ctxfs status` gains an "LFS pointer files (Phase 5: smudge)" section.

- **B5 (per-repo cache reservation).** Three layered sub-tasks with **manifest-time ownership** as the primary signal (Codex #3 redesign — putting ownership on `put` was insufficient because cache hits, contains_all skips, and tree-cache hits all bypass `put`):
  - **T3a — foundation:** `RepoKey { host, owner, repo }` and `MountCacheView` (a thin mount-bound handle) in `ctxfs-cache::reservation`. `BlobCache::state: Mutex<CacheState>` is restructured: a single `CacheState` holds the existing `LruState` (with the new `algorithm` field on `CacheEntry`), plus `blob_owners: HashMap<String, BTreeSet<RepoKey>>` and `reservations: HashMap<RepoKey, ReservationEntry>`. `BlobCache::register_mount(key, reservation_bytes, manifest_digests: &[String])` adds `key` to the owner-set of every digest in the manifest (regardless of cache state) — that's the moment ownership is established. Future puts find the owner-set already populated. `BlobCache::add_owner(key, hex)` extends ownership for late-discovered blobs (truncated tree fallback). `BlobCache::unregister_mount(key)` removes `key` from all owner-sets; refcount decrements ensure same-repo concurrent mounts only fully deregister on last unmount.
  - **T3b — reservation + eviction skip:** Eviction loop now consults reservations under the same mutex (no lock-order dance). For each LRU candidate: check owner-set against active reservations whose working set ≤ reservation. If the eviction would drop a protected repo's working-set below its reservation, the candidate is skipped (rotated to LRU back) and `eviction_attempts_blocked_by_reservation` increments. Best-effort overflow: if the entire LRU is reservation-protected, the put completes and the cache exceeds `max_bytes` temporarily — `ctxfs status` flags it. Default reservation is `max_bytes / count(active)`, **rebalanced on every `register_mount` / `unregister_mount`**: explicit overrides (per-mount `--cache-reservation`) are flagged via `is_explicit_override: bool` and never touched by rebalance. Per-mount override flows through `MountOptions.cache_reservation_bytes: Option<u64>`.
  - **T3c — status surfacing:** `MountSummary` gains `working_set_bytes`, `cache_reservation_bytes`, `lfs_pointer_files`, `lfs_pointer_sample_paths` (all `#[serde(default)]`). `StatusReportV1` gains `cache_eviction_attempts_blocked_by_reservation` (also `#[serde(default)]`). **`DaemonServer::assemble_status_report`** is the new seam: calls `observability.status_report()` for the base, then walks `report.mounts` and augments each summary with cache lookups (`working_set_bytes`, `reservation_bytes`) keyed by `RepoKey` derived from the mount's source. CLI `print_global_status` renders a "Per-mount cache usage" section with an "OVER RESERVATION — best-effort eviction" warning when usage exceeds reservation, and an LFS section.

- **`MountCacheView`** is a thin handle pinning `(Arc<BlobCache>, RepoKey)`. Its primary role is **API ergonomics** for providers — `put`/`get`/`contains` calls forward to `BlobCache` and pass the right `RepoKey` for owner-set bookkeeping (e.g., tarball-commit + small-blob commit go through `MountCacheView::record_ownership_after_finalize(digest)` for streaming-finalize paths that don't fit `put_for`). It is *not* the place where ownership is established — that's `register_mount` with the manifest digests.

- **`ProviderContext`** (introduced in M4) gains `mount_cache: Option<Arc<MountCacheView>>`. Tarball + small-blob + lazy fetch paths use it when present; tests/FSKit shared paths leave it `None` (no ownership tracking, no reservation enforcement).

- **No new external deps.**

**Tech Stack:** Rust 2021. Workspace lints inherited.

**Spec reference:** `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` § Milestones (M5). Exit criteria:
- B3-label: `Digest::Sha1(...)` exists and is used for GitHub blob IDs.
- B5: regression test — mount A (working set ≤ reservation), mount B under cache pressure, scan A, assert `cache_hits` for A's working set unchanged. Assert `eviction_attempts_blocked_by_reservation` counter incremented when B's writes try to evict A's reserved blobs.
- B6: `ctxfs status` shows LFS pointer count and ≤ 3 sample paths when the test corpus includes LFS-tracked files.

---

## File Structure

```
crates/
  ctxfs-core/
    src/
      digest.rs                              # MODIFY: add HashAlgorithm::Sha1 + Digest::from_sha1_hex; HashAlgorithm::Display covers "sha1"
  ctxfs-provider-common/
    src/
      lfs.rs                                 # CREATE: detect_lfs_pointer (manual parser, no regex) + LfsPointerInfo
      lib.rs                                 # MODIFY: pub mod lfs; pub use lfs::{detect_lfs_pointer, LfsPointerInfo}
      counters.rs                            # MODIFY: add eviction_attempts_blocked_by_reservation (cache-global counter exposed via Observability/StatusReportV1); LfsSampleBuffer (3-deep) + record_lfs_pointer_with_path; CounterSnapshot fields with #[serde(default)]
      status.rs                              # MODIFY: extend MountSummary with #[serde(default)] working_set_bytes, cache_reservation_bytes, lfs_pointer_files, lfs_pointer_sample_paths; StatusReportV1 gains #[serde(default)] cache_eviction_attempts_blocked_by_reservation
  ctxfs-cache/
    src/
      lib.rs                                 # MAJOR MODIFY: CacheEntry tracks algorithm; remove_blob_file consults it; rebuild_index walks sha1/ + sha256/ and dedupes by hex (prefers sha1/); single Mutex<CacheState> holds LRU + blob_owners + reservations; eviction-skip path; register/unregister_mount with manifest_digests; add_owner; working_set_bytes(key); reservation_bytes(key); eviction_attempts_blocked_by_reservation()
      reservation.rs                         # CREATE: RepoKey, MountCacheView, ReservationEntry { reserved_bytes, is_explicit_override, refcount }, ownership accessors
      tree.rs                                # MODIFY: SCHEMA_VERSION 2 -> 3 with documented reason; add v3 history bullet
    tests/
      reservation.rs                         # CREATE: integration tests for B5 invariant (cache_max=500, A reserves 400 with ws=300, B writes 400, A's blobs survive, counter > 0)
  ctxfs-provider-git/
    src/
      context.rs                             # MODIFY: add mount_cache: Option<Arc<MountCacheView>> field
      github.rs                              # MODIFY: tree_entry_to_request + four other from_sha256_hex sites use from_sha1_hex; ContentKind::LfsPointer set when detected; sha_to_path: Mutex<HashMap<String, PathBuf>> populated post-snapshot in fetch_snapshot_inner; LFS detection in fetch_blob_content (single leaf for lazy + small-blob) + fetch_tarball_into_cache (existing local Tee extended to mirror small entries); mount_cache used by tarball + small-blob commit paths via record_ownership_after_finalize
    tests/
      replay_lfs_detect_surfaces_count.rs    # CREATE: B6 replay test
      replay_b5_reservation_protects_active.rs # CREATE: B5 replay test (mount A + mount B + scan A)
  ctxfs-ipc/
    src/
      service.rs                             # MODIFY: MountOptions gains cache_reservation_bytes: Option<u64>
  ctxfs-daemon/
    src/
      daemon.rs                              # MODIFY: prepare_mount derives RepoKey, builds MountCacheView, calls cache.register_mount(key, reservation_bytes, &manifest_digests) AFTER snapshot; unmount unregisters; new assemble_status_report seam augments observability.status_report() with cache lookups; MountInfo carries RepoKey for unregister
  ctxfs-cli/
    src/
      main.rs                                # MODIFY: --cache-reservation flag on mount (parse_size_bytes); print_global_status prints per-mount working-set vs reservation + cache-global blocked counter + LFS pointer block; deps/mount.rs threads the new flag
CHANGELOG.md                                  # MODIFY: M5 entry
```

---

## Task 1: B3-label — `HashAlgorithm::Sha1` variant + `Digest::from_sha1_hex` + call-site updates

**Files:**
- Modify: `crates/ctxfs-core/src/digest.rs`
- Modify: `crates/ctxfs-cache/src/lib.rs` (`rebuild_index` walks both `sha1/` and `sha256/` algo dirs; `BlobCache::get` falls back to alternate-algo path when primary misses)
- Modify: `crates/ctxfs-provider-git/src/github.rs` (5 sites: `tree_entry_to_request`, `build_directories_inner` file/tree branches, the `fetch_small_blobs_concurrent` cache key, the tarball-commit-path digest)

**Why first:** lightest task; warms up the engineer on cache layout + Digest plumbing before B5's heavier surface.

### Step 1: Write the failing test (digest)

Append to `crates/ctxfs-core/src/digest.rs::tests`:

```rust
#[test]
fn from_sha1_hex_roundtrip() {
    let git_blob_sha1 = "356a192b7913b04c54574d18c28d46e6395428ab";
    let d = Digest::from_sha1_hex(git_blob_sha1);
    assert_eq!(d.algorithm, HashAlgorithm::Sha1);
    assert_eq!(d.hex, git_blob_sha1);
}

#[test]
fn sha1_to_path_uses_sha1_subdir() {
    let d = Digest::from_sha1_hex("356a192b7913b04c54574d18c28d46e6395428ab");
    assert_eq!(
        d.to_path().to_str().unwrap(),
        "sha1/35/6a192b7913b04c54574d18c28d46e6395428ab"
    );
}

#[test]
fn hash_algorithm_sha1_display() {
    assert_eq!(HashAlgorithm::Sha1.to_string(), "sha1");
}

#[test]
fn sha1_serde_roundtrip() {
    let d = Digest::from_sha1_hex("aabbccdd00112233445566778899aabbccddeeff");
    let json = serde_json::to_string(&d).unwrap();
    let d2: Digest = serde_json::from_str(&json).unwrap();
    assert_eq!(d2, d);
    assert_eq!(d2.algorithm, HashAlgorithm::Sha1);
}
```

### Step 2: Run to verify failure

```
cargo test -p ctxfs-core digest
```

Expected: 4 new test names FAIL with `no variant Sha1` / `no method from_sha1_hex`.

### Step 3: Add `Sha1` variant + constructor + Display

In `crates/ctxfs-core/src/digest.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    Sha256,
    Sha1,
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashAlgorithm::Sha256 => write!(f, "sha256"),
            HashAlgorithm::Sha1 => write!(f, "sha1"),
        }
    }
}

impl Digest {
    /// Existing constructor stays untouched.
    pub fn from_sha256_hex(hex_str: impl Into<String>) -> Self {
        Self {
            algorithm: HashAlgorithm::Sha256,
            hex: hex_str.into(),
        }
    }

    /// Construct a Digest from an existing SHA-1 hex string. The 40-char
    /// hexes returned by the GitHub Trees API are Git blob SHA-1s; this
    /// constructor labels them correctly so future readers don't conclude
    /// the cache stores SHA-256 content.
    pub fn from_sha1_hex(hex_str: impl Into<String>) -> Self {
        Self {
            algorithm: HashAlgorithm::Sha1,
            hex: hex_str.into(),
        }
    }
}
```

### Step 4: Run digest tests — expect green

```
cargo test -p ctxfs-core digest
```

Expected: all 4 new tests + the 7 existing tests PASS.

### Step 5: Cache layout — `CacheEntry` tracks algorithm; `remove_blob_file` consults it; `rebuild_index` dedupes

**Why this version (vs v1's "fall back on get"):** Codex review #1 — the v1 design leaks files on eviction because `remove_blob_file(hex)` always tries `sha256/<hex>` and would silently miss `sha1/<hex>` files. v2 fixes this by tracking the algorithm per-entry so eviction always knows the right path.

In `crates/ctxfs-cache/src/lib.rs`:

```rust
use ctxfs_core::digest::HashAlgorithm;

struct CacheEntry {
    size: u64,
    algorithm: HashAlgorithm,
}
```

`remove_blob_file` becomes:

```rust
fn remove_blob_file(&self, hex: &str, algorithm: HashAlgorithm) {
    let digest = Digest {
        algorithm,
        hex: hex.to_string(),
    };
    let path = self.blob_path(&digest);
    let _ = fs::remove_file(path);
}
```

The eviction call sites pass the algorithm by reading it off the entry being removed. `evict_oldest`'s return type changes:

```rust
fn evict_oldest(&mut self) -> Option<(String, u64, HashAlgorithm)> {
    self.entries.pop_front().map(|(key, entry)| {
        self.total_bytes -= entry.size;
        (key, entry.size, entry.algorithm)
    })
}
```

`lru_insert_evict` and `prune` / `prune_blobs` / `set_max_bytes` thread the algorithm through to `remove_blob_file` accordingly.

`rebuild_index` walks both algo subdirs and **deduplicates by hex** — preferring `sha1/` over `sha256/` when both exist (the new canonical) and deleting the loser:

```rust
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
                "tmp" => continue,
                _ => continue, // unknown algo dir — skip; future-proof
            };
            Self::scan_fan_out_dir(&algo_path, algorithm, &mut entries)?;
        }
    }

    entries.sort_by_key(|(_, _, _, mtime)| *mtime);

    // Dedupe by hex: if both sha1 and sha256 paths exist for the same hex,
    // prefer sha1 (the new canonical from M5). Delete the sha256 file on disk.
    let mut by_hex: HashMap<String, (HashAlgorithm, u64, std::time::SystemTime)> = HashMap::new();
    let mut to_delete: Vec<(String, HashAlgorithm)> = Vec::new();
    for (hex, algo, size, mtime) in entries {
        match by_hex.entry(hex.clone()) {
            std::collections::hash_map::Entry::Vacant(v) => {
                let _ = v.insert((algo, size, mtime));
            }
            std::collections::hash_map::Entry::Occupied(mut o) => {
                let existing = o.get();
                if existing.0 == HashAlgorithm::Sha1 {
                    // Existing wins; delete the new candidate.
                    to_delete.push((hex.clone(), algo));
                } else if algo == HashAlgorithm::Sha1 {
                    // Replace existing with the sha1 version.
                    let old = std::mem::replace(o.get_mut(), (algo, size, mtime));
                    to_delete.push((hex.clone(), old.0));
                } else {
                    // Both sha256 — keep older. Shouldn't really happen.
                    to_delete.push((hex.clone(), algo));
                }
            }
        }
    }

    let mut state = self.state.lock().unwrap();
    state.entries.clear();
    state.total_bytes = 0;

    let mut sorted: Vec<(String, HashAlgorithm, u64, std::time::SystemTime)> =
        by_hex.into_iter().map(|(h, (a, s, m))| (h, a, s, m)).collect();
    sorted.sort_by_key(|(_, _, _, mtime)| *mtime);

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
```

`scan_fan_out_dir` now takes the algorithm and tags each entry:

```rust
fn scan_fan_out_dir(
    algo_path: &Path,
    algorithm: HashAlgorithm,
    entries: &mut Vec<(String, HashAlgorithm, u64, std::time::SystemTime)>,
) -> Result<(), CtxfsError> {
    // (existing fan-out walk code, but emit (hex, algorithm, size, mtime))
}
```

`BlobCache::get` does **not** need a fallback — once `rebuild_index` has tagged each entry with its on-disk algorithm, the LRU lookup returns an entry whose `algorithm` matches the file location. The `get` path constructs `digest.to_path()` from the *caller's* digest; when caller passes `Sha1` and entry is tagged `Sha256` (legacy), the path still mismatches.

**Resolution:** `BlobCache::get` consults the LRU entry's tagged algorithm to compute the on-disk path, ignoring the caller's digest's algorithm:

```rust
pub fn get(&self, digest: &Digest) -> Option<Vec<u8>> {
    let key = digest.hex.clone();
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
```

This handles the mixed-cache case correctly: an old `sha256/<git-sha-1-hex>` entry is found and served even when the new code looks it up via `from_sha1_hex`. New puts always go to the canonical `digest.to_path()` (so `Sha1` digests land at `sha1/`), and rebuild dedupes if both happen to exist.

### Step 6: TreeCache — bump `SCHEMA_VERSION`

In `crates/ctxfs-cache/src/tree.rs`:

```rust
/// History:
/// - v1: initial format (pre-M2). Manifests had `inline_content: None` and
///   empty `target` for symlinks; reads from these would bypass M2's prefetch
///   path and serve broken manifests.
/// - v2: M2 — `FileEntry::inline_content` populated for ≤4 KB blobs and
///   `SymlinkEntry::target` decoded from prefetched bytes.
/// - v3: M5 — Git blob digests now carry `HashAlgorithm::Sha1` instead of
///   being mislabeled `HashAlgorithm::Sha256`. Older v2 manifests would
///   round-trip the wrong algorithm tag and confuse callers that key by
///   `digest.algorithm`. Bump invalidates them; first read after upgrade
///   refetches with correct labels.
pub const SCHEMA_VERSION: u32 = 3;
```

The existing version-mismatch handling at line 60+ already discards stale entries; no other changes needed in tree.rs. A new test in `crates/ctxfs-cache/tests/lifecycle.rs` (or wherever existing version-bump tests live) asserts that a v2-on-disk file is treated as stale after a v3 read. Engineer: model after the existing M2 test.

### Step 7: Add backward-compat get test for blob cache

Add in `crates/ctxfs-cache/src/lib.rs::tests`:

```rust
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
        cache.put(&Digest::from_sha256_hex(git_hex), b"old layout").unwrap();
    }
    // Manually drop a sha1/<hex> file simulating mid-migration state.
    let sha1_path = dir.path().join(format!("sha1/{}/{}", &git_hex[..2], &git_hex[2..]));
    fs::create_dir_all(sha1_path.parent().unwrap()).unwrap();
    fs::write(&sha1_path, b"new layout").unwrap();

    let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
    // Sha1 wins on dedupe; sha256 file should be gone.
    let sha256_path = dir.path().join(format!("sha256/{}/{}", &git_hex[..2], &git_hex[2..]));
    assert!(!sha256_path.exists(), "rebuild_index should delete the sha256 loser");
    assert!(sha1_path.exists(), "rebuild_index should keep the sha1 winner");

    let stored = cache.get(&Digest::from_sha1_hex(git_hex)).unwrap();
    assert_eq!(&stored, b"new layout");
}
```

### Step 8: Run cache tests — expect green

```
cargo test -p ctxfs-cache
```

Expected: existing cache tests + the two new dedupe/legacy tests + the new tree-cache version test PASS.

### Step 9: Update GitHub-blob construction sites

In `crates/ctxfs-provider-git/src/github.rs`, the five sites currently calling `Digest::from_sha256_hex` with GitHub-supplied SHA-1 hexes:

| Line | Context | Change |
|---|---|---|
| 648 | `fetch_small_blobs_concurrent`: cache lookup for tree blobs | `Digest::from_sha1_hex(&sha)` |
| 825 | `build_directories_inner`: `DirEntry::File` branch | `Digest::from_sha1_hex(&entry.sha)` |
| 833 | `build_directories_inner`: `DirEntry::Directory` placeholder | `Digest::from_sha1_hex(&entry.sha)` |
| 1208 | `tree_entry_to_request` | `Digest::from_sha1_hex(&entry.sha)` |
| 1429 | `fetch_tarball_into_cache`: post-verify commit | `Digest::from_sha1_hex(&expected_sha)` |

Each is a single-token replacement: `from_sha256_hex` → `from_sha1_hex`. The B3-label pending comment on line ~1426 ("Manifest stores Git blob SHA-1 in the digest hex…") can now read accurately as "stored under HashAlgorithm::Sha1" — update that comment.

### Step 10: Add a regression test

In `crates/ctxfs-provider-git/src/github.rs::tests`:

```rust
#[test]
fn tree_entry_to_request_labels_blob_digest_as_sha1() {
    let entry = TreeEntry {
        path: "src/lib.rs".to_string(),
        mode: "100644".to_string(),
        entry_type: "blob".to_string(),
        sha: "356a192b7913b04c54574d18c28d46e6395428ab".to_string(),
        size: Some(42),
    };
    let req = GitHubProvider::tree_entry_to_request(&entry).expect("blob -> Some");
    let digest = req.digest.expect("blob has digest");
    assert_eq!(digest.algorithm, ctxfs_core::digest::HashAlgorithm::Sha1);
    assert_eq!(digest.hex, "356a192b7913b04c54574d18c28d46e6395428ab");
}
```

`tree_entry_to_request` is `pub(crate)`; this test must live inline in `github.rs::tests`.

### Step 11: Run full workspace tests + clippy + fmt

```
cargo test
cargo clippy --all-targets --tests -- -D warnings
cargo fmt --all -- --check
```

Expected: all tests pass; lints clean.

### Step 12: Commit

```
git add crates/ctxfs-core/src/digest.rs \
        crates/ctxfs-cache/src/lib.rs \
        crates/ctxfs-cache/src/tree.rs \
        crates/ctxfs-cache/tests/lifecycle.rs \
        crates/ctxfs-provider-git/src/github.rs

git commit -m "$(cat <<'EOF'
feat(core,cache,provider-git): B3-label — HashAlgorithm::Sha1 variant for Git blob digests

Adds HashAlgorithm::Sha1 + Digest::from_sha1_hex and routes the
five GitHubProvider blob-construction sites (tree_entry_to_request,
build_directories file/tree branches, small-blobs cache lookup,
tarball post-verify commit) through it. The 40-char hexes returned
by the GitHub Trees API are Git blob SHA-1s, not SHA-256s; the M3
TODO comment on the tarball commit path called this out
explicitly.

Cache layout (Codex M5-plan-v1 #1): CacheEntry now tracks the
on-disk algorithm so eviction's remove_blob_file uses the correct
fan-out subdir (sha1/ or sha256/). rebuild_index walks both algo
subdirs and dedupes by hex — sha1/ wins on collision, the loser
on disk is unlinked. BlobCache::get consults the LRU's tagged
algorithm to compute the on-disk path so legacy sha256-stored
Git SHA-1 hexes remain queryable after the M5 upgrade. New puts
go to the canonical digest.to_path() (sha1/ for Git blobs); old
entries fade out via LRU.

TreeCache SCHEMA_VERSION bumped 2 -> 3 (Codex M5-plan-v1 #2):
v2 manifests on disk carry mislabeled HashAlgorithm::Sha256 for
Git blob digests. v3 invalidates them; first read after upgrade
refetches with correct labels. No on-disk corruption.

Closes B3-label. (B3-verification — full SHA-1 chain verification
across cache tiers — stays Phase 5+.)
EOF
)"
```

---

## Task 2: B6 — LFS pointer detect-and-surface

**Files:**
- Create: `crates/ctxfs-provider-common/src/lfs.rs` (manual parser; no regex dep)
- Modify: `crates/ctxfs-provider-common/src/lib.rs` (re-export)
- Modify: `crates/ctxfs-provider-common/src/counters.rs` (LfsSampleBuffer + counter snapshot fields)
- Modify: `crates/ctxfs-provider-common/src/status.rs` (MountSummary fields, all `#[serde(default)]`)
- Modify: `crates/ctxfs-provider-git/src/github.rs` (sha→path map; 2 detection sites: fetch_blob_content + fetch_tarball_into_cache via existing local Tee)
- Modify: `crates/ctxfs-cli/src/main.rs` (`print_global_status` LFS section)

**v2 changes (Codex M5-plan-v1 #8 + extras):** single detection site for non-tarball paths (was 3 sites; the small-blob prefetch path goes through `fetch_blob_content`, so detecting once there avoids double-counting); manual parser instead of regex dep ("no new deps" promise); sample-path is a *real path*, not a sha — `GitHubProvider` builds a sha→path map at snapshot time.

### Step 1: Write the failing detector tests (manual parser, no regex dep)

Create `crates/ctxfs-provider-common/src/lfs.rs`:

```rust
//! Git LFS pointer detection.
//!
//! Pure-bytes helpers used by GitHub fetch paths to detect LFS pointer
//! files. Phase 4 surfaces the count + sample paths in `ctxfs status`;
//! Phase 5 will smudge to real bytes via the LFS smudge endpoint.
//!
//! Manual parser instead of `regex` to keep the no-new-deps promise.
//! The pointer format is rigid: exactly three newline-terminated lines.

/// Parsed LFS pointer fields. The pointer-content bytes themselves stay in
/// the cache verbatim (M5 surfaces only; smudge is Phase 5).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LfsPointerInfo {
    /// The `oid sha256:<hex>` value — the SHA-256 of the actual object.
    pub oid_sha256: String,
    /// Object size in bytes as declared in the pointer.
    pub size: u64,
}

const LFS_VERSION_LINE: &str = "version https://git-lfs.github.com/spec/v1";
const LFS_OID_PREFIX: &str = "oid sha256:";
const LFS_SIZE_PREFIX: &str = "size ";
const LFS_POINTER_MAX_BYTES: usize = 1024;

/// Detect a Git LFS pointer file. The pointer format is well-defined:
/// three lines (`version`, `oid sha256:<64-hex>`, `size <decimal>`), each
/// newline-terminated, with no trailing content. Returns `Some(info)` only
/// when the entire input matches; `None` otherwise.
///
/// The 1024-byte cap fast-paths non-LFS reads — pointers are ≤ ~200 bytes.
/// False-positive rate against real source trees is essentially zero.
#[must_use]
pub fn detect_lfs_pointer(bytes: &[u8]) -> Option<LfsPointerInfo> {
    if bytes.is_empty() || bytes.len() > LFS_POINTER_MAX_BYTES {
        return None;
    }
    let s = std::str::from_utf8(bytes).ok()?;

    // Split into exactly four pieces: three content lines + the empty
    // remainder after the trailing newline. Anything else is rejected.
    let mut iter = s.split('\n');
    let v_line = iter.next()?;
    let o_line = iter.next()?;
    let s_line = iter.next()?;
    let trailer = iter.next()?;
    if iter.next().is_some() {
        return None; // extra newline / content after size line
    }
    if !trailer.is_empty() {
        return None; // bytes after the size line's terminator
    }

    if v_line != LFS_VERSION_LINE {
        return None;
    }
    let oid = o_line.strip_prefix(LFS_OID_PREFIX)?;
    if oid.len() != 64 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let size_str = s_line.strip_prefix(LFS_SIZE_PREFIX)?;
    let size: u64 = size_str.parse().ok()?;

    Some(LfsPointerInfo {
        oid_sha256: oid.to_string(),
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(oid: &str, size: u64) -> Vec<u8> {
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
            .into_bytes()
    }

    #[test]
    fn detects_canonical_pointer() {
        let oid = "a".repeat(64);
        let info = detect_lfs_pointer(&pointer(&oid, 12345)).expect("matches");
        assert_eq!(info.oid_sha256, oid);
        assert_eq!(info.size, 12345);
    }

    #[test]
    fn rejects_non_pointer_text() {
        let bytes = b"hello world\n";
        assert!(detect_lfs_pointer(bytes).is_none());
    }

    #[test]
    fn rejects_truncated_pointer_missing_final_newline() {
        let bytes = b"version https://git-lfs.github.com/spec/v1\noid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize 1";
        assert!(detect_lfs_pointer(bytes).is_none());
    }

    #[test]
    fn rejects_oversized_input() {
        let bytes = vec![b'a'; 2048];
        assert!(detect_lfs_pointer(&bytes).is_none());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(detect_lfs_pointer(&[]).is_none());
    }

    #[test]
    fn rejects_extra_trailing_content() {
        let oid = "f".repeat(64);
        let mut bytes = pointer(&oid, 10);
        bytes.extend_from_slice(b"trailing junk");
        assert!(detect_lfs_pointer(&bytes).is_none());
    }

    #[test]
    fn rejects_wrong_version_url() {
        let bytes = b"version https://git-lfs.gitlab.com/spec/v1\noid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize 1\n";
        assert!(detect_lfs_pointer(bytes).is_none());
    }

    #[test]
    fn rejects_non_hex_oid() {
        let bytes = b"version https://git-lfs.github.com/spec/v1\noid sha256:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\nsize 1\n";
        assert!(detect_lfs_pointer(bytes).is_none());
    }

    #[test]
    fn rejects_non_decimal_size() {
        let oid = "a".repeat(64);
        let bytes = format!(
            "version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize one\n"
        );
        assert!(detect_lfs_pointer(bytes.as_bytes()).is_none());
    }
}
```

**No new deps** — manual parser stays within stdlib. Do NOT add `regex` to `Cargo.toml`.

In `crates/ctxfs-provider-common/src/lib.rs` add:

```rust
pub mod lfs;
```

### Step 2: Run detector tests — expect green

```
cargo test -p ctxfs-provider-common lfs::
```

Expected: 7 new tests pass.

### Step 3: Add `LfsSampleBuffer` to counters

In `crates/ctxfs-provider-common/src/counters.rs`, after the existing `MountCounters` definition, add:

```rust
use std::sync::Mutex;

/// Bounded ring of sample paths recorded when LFS pointer files are
/// detected. Caps at 3 to keep `ctxfs status` output compact while still
/// giving operators the breadcrumbs they need to understand which files
/// will not work until Phase 5 ships LFS smudge.
#[derive(Debug, Default)]
pub struct LfsSampleBuffer {
    inner: Mutex<Vec<String>>,
}

impl LfsSampleBuffer {
    /// Cap, hard-coded.
    pub const CAP: usize = 3;

    /// Append `path` if there's still room in the buffer. Idempotent on
    /// duplicate paths (the same pointer file detected twice doesn't
    /// double-count in the sample list, though counters tick each time).
    pub fn push(&self, path: String) {
        let mut v = self.inner.lock().unwrap();
        if v.len() >= Self::CAP {
            return;
        }
        if !v.iter().any(|p| p == &path) {
            v.push(path);
        }
    }

    pub fn snapshot(&self) -> Vec<String> {
        self.inner.lock().unwrap().clone()
    }
}
```

Extend `MountCounters` with the buffer + helper that calls both counter and buffer:

```rust
#[derive(Debug, Default)]
pub struct MountCounters {
    // ...existing fields...
    eviction_attempts_blocked_by_reservation: AtomicU64,
    lfs_samples: LfsSampleBuffer,
}
```

Add helpers:

```rust
pub fn record_lfs_pointer_with_path(&self, path: &str) {
    let _ = self.lfs_pointer_files.fetch_add(1, Ordering::Relaxed);
    self.lfs_samples.push(path.to_string());
}

pub fn record_eviction_blocked_by_reservation(&self) {
    let _ = self
        .eviction_attempts_blocked_by_reservation
        .fetch_add(1, Ordering::Relaxed);
}
```

Extend `CounterSnapshot`:

```rust
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CounterSnapshot {
    // ...existing fields...
    pub eviction_attempts_blocked_by_reservation: u64,
    pub lfs_pointer_sample_paths: Vec<String>,
}
```

Extend `MountCounters::merge_from_snapshot` and `MountCounters::snapshot` accordingly. The sample-buffer merge: append from `snap.lfs_pointer_sample_paths` while respecting the cap.

### Step 4: Wire detection — sha→path map + two detection sites

**v2 changes (Codex M5-plan-v1 #8):** drop the third detection site (`fetch_small_blobs_concurrent`) — it ultimately calls `fetch_blob_content` via `fetch_blob_with_sha`, so detection in `fetch_blob_content` covers both lazy reads AND small-blob prefetch. Detecting in both produces double-counts. Keep two sites: `fetch_blob_content` (single leaf) and `fetch_tarball_into_cache` (separate streaming code path). Resolve sample path via a sha→path map populated post-snapshot, not by storing shas as paths.

#### 4.1 — Add `sha_to_path` map to `GitHubProvider`

In `crates/ctxfs-provider-git/src/github.rs`, add a field:

```rust
pub struct GitHubProvider {
    // ...existing fields...
    /// Populated in `fetch_snapshot_inner` from the tree manifest. Maps
    /// blob SHA-1 -> mount-relative path so lazy fetches (which only know
    /// the sha) can surface a meaningful sample path on LFS detection.
    /// Cleared and re-populated on each snapshot rebuild.
    sha_to_path: std::sync::Mutex<HashMap<String, PathBuf>>,
}
```

Initialize to empty in `GitHubProvider::new` and `new_with_codeload_host`.

In `fetch_snapshot_inner`, after the manifest is built and `requests` is collected, populate the map:

```rust
{
    let mut map = self.sha_to_path.lock().unwrap();
    map.clear();
    for r in &requests {
        if let Some(d) = &r.digest {
            let _ = map.insert(d.hex.clone(), r.path.clone());
        }
    }
}
```

(For symlinks the entry's `path` is still the mount-relative path, which is correct for LFS surfacing.)

#### 4.2 — Detection point A: `fetch_blob_content` (the single non-tarball leaf)

In `fetch_blob_content` (around line 449+), after `decoded` is produced:

```rust
if let Some(_info) = ctxfs_provider_common::lfs::detect_lfs_pointer(&decoded) {
    let path_str = self
        .sha_to_path
        .lock()
        .unwrap()
        .get(sha)
        .map(|p| p.display().to_string())
        .unwrap_or_else(|| format!("<sha:{}>", &sha[..8.min(sha.len())]));
    if let Some(key) = self.counter_key.lock().unwrap().clone() {
        self.observability
            .counters_for(key)
            .record_lfs_pointer_with_path(&path_str);
    }
    tracing::warn!(
        target: "ctxfs.provider.lfs",
        sha = sha,
        path = path_str.as_str(),
        "LFS pointer detected (Phase 5: smudge)"
    );
}
```

Fallback `<sha:abcd1234>` only fires when the map hasn't been populated (e.g., a `fetch_blob` called outside a snapshot lifecycle); preserves observability.

#### 4.3 — Detection point B: `fetch_tarball_into_cache` (extend the existing local Tee)

The existing local `Tee` adapter at `github.rs:1255` already mirrors bytes between the SHA-1 hasher and the `BlobTempWriter`. Extend it (or wrap it) to mirror small entries into a `Vec<u8>` buffer, run detection after SHA-1 verify succeeds, and surface counter + sample path.

Concretely, the existing struct:

```rust
struct Tee<'a, W: std::io::Write> {
    hasher: &'a mut GitBlobSha1,
    writer: &'a mut W,
}
impl<W: std::io::Write> std::io::Write for Tee<'_, W> { /* ... */ }
```

becomes:

```rust
struct Tee<'a, W: std::io::Write> {
    hasher: &'a mut GitBlobSha1,
    writer: &'a mut W,
    /// When `Some`, mirror written bytes here for post-write inspection
    /// (LFS pointer detection on small entries).
    peek: Option<&'a mut Vec<u8>>,
}

impl<W: std::io::Write> std::io::Write for Tee<'_, W> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        let n = self.writer.write(buf)?;
        self.hasher.update(&buf[..n]);
        if let Some(p) = self.peek.as_deref_mut() {
            p.extend_from_slice(&buf[..n]);
        }
        Ok(n)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.writer.flush()
    }
}
```

In the per-entry block, build the peek buffer only for small entries:

```rust
const LFS_POINTER_MAX_BYTES: u64 = 1024;
let mut peek_buf: Option<Vec<u8>> = if expected_size <= LFS_POINTER_MAX_BYTES {
    Some(Vec::with_capacity(expected_size as usize))
} else {
    None
};
let mut tee = Tee {
    hasher: &mut hasher,
    writer: &mut writer,
    peek: peek_buf.as_mut(),
};
let _ = std::io::copy(&mut entry, &mut tee)
    .map_err(|e| CtxfsError::Provider(format!("tar entry stream: {e}")))?;

// ... existing SHA-1 verify ...

// After verify succeeds and before writer.finalize, run LFS detection.
if let Some(buf) = peek_buf.as_ref() {
    if let Some(_info) = ctxfs_provider_common::lfs::detect_lfs_pointer(buf) {
        let path_str = mount_path.display().to_string();
        if let Some(ref c) = counters {
            c.record_lfs_pointer_with_path(&path_str);
        }
        tracing::warn!(
            target: "ctxfs.provider.lfs",
            path = path_str.as_str(),
            sha = expected_sha.as_str(),
            "LFS pointer detected (Phase 5: smudge)"
        );
    }
}
```

`mount_path` is already in scope as a `PathBuf` (the tarball's per-entry mount-relative path used for build_path_to_sha_size_from_requests lookup). The streaming guarantees stay intact: the temp file is fsynced and atomically renamed by `writer.finalize` regardless of LFS detection outcome.

### Step 5: Surface in `ctxfs status`

Extend `MountSummary` in `crates/ctxfs-provider-common/src/status.rs` with `#[serde(default)]` on every new field so newer clients can deserialize older v1 payloads cleanly (Codex M5-plan-v1 #10):

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountSummary {
    pub mount_id: String,
    pub source: String,
    pub repo: String,
    pub commit: String,
    pub rest_calls_total: u64,
    pub bytes_total: u64,
    pub prefetch_hits: u64,
    pub cache_hit_ratio: Option<f64>,
    /// Total bytes currently consumed by this mount's working set in the
    /// blob cache (sum of sizes of cached blobs whose owner-set contains
    /// this mount's RepoKey). Populated in T3c (B5).
    #[serde(default)]
    pub working_set_bytes: u64,
    /// Reservation registered for this mount's RepoKey at mount time
    /// (default: cache_max / count(active mounts) at that moment, or the
    /// user's `--cache-reservation` override). Populated in T3c (B5).
    #[serde(default)]
    pub cache_reservation_bytes: u64,
    /// Number of LFS pointer files detected during this mount's fetches.
    #[serde(default)]
    pub lfs_pointer_files: u64,
    /// Up to 3 sample paths (mount-relative) of detected LFS pointers.
    #[serde(default)]
    pub lfs_pointer_sample_paths: Vec<String>,
}
```

Extend `StatusReportV1` likewise — the cache-global "blocked-by-reservation" counter is added in T3c, but the field can be defined now with `#[serde(default)]` to avoid a second schema bump:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReportV1 {
    pub schema_version: u32,
    pub budgets: Vec<BudgetEntry>,
    pub counters: Vec<CounterEntry>,
    pub mounts: Vec<MountSummary>,
    /// Cache-global counter: number of LRU-eviction candidates skipped
    /// because evicting them would have violated a per-repo reservation
    /// (B5). Populated by daemon's status assembly in T3c.
    #[serde(default)]
    pub cache_eviction_attempts_blocked_by_reservation: u64,
}
```

Roundtrip test in `status.rs::tests`:

```rust
#[test]
fn additive_fields_default_when_absent() {
    // Older v1 JSON without the new fields must deserialize cleanly.
    let old_json = r#"{"schema_version":1,"budgets":[],"counters":[],"mounts":[]}"#;
    let r: StatusReportV1 = serde_json::from_str(old_json).unwrap();
    assert_eq!(r.cache_eviction_attempts_blocked_by_reservation, 0);
    assert_eq!(r.schema_version, 1);
}

#[test]
fn mount_summary_defaults_for_legacy_payload() {
    let old_json = r#"{
        "mount_id":"m1","source":"github","repo":"a/b","commit":"abc",
        "rest_calls_total":0,"bytes_total":0,"prefetch_hits":0,
        "cache_hit_ratio":null
    }"#;
    let m: MountSummary = serde_json::from_str(old_json).unwrap();
    assert_eq!(m.working_set_bytes, 0);
    assert_eq!(m.cache_reservation_bytes, 0);
    assert_eq!(m.lfs_pointer_files, 0);
    assert!(m.lfs_pointer_sample_paths.is_empty());
}
```

In `crates/ctxfs-cli/src/main.rs::print_global_status`, after the existing per-mount block, render:

```rust
// LFS pointer summary
let lfs_total: u64 = report.mounts.iter().map(|m| m.lfs_pointer_files).sum();
if lfs_total > 0 {
    println!();
    println!("LFS pointer files (Phase 5: smudge): {lfs_total} detected");
    for m in &report.mounts {
        if m.lfs_pointer_files == 0 {
            continue;
        }
        println!(
            "  {} ({}/{}): {}",
            m.mount_id, m.source, m.repo, m.lfs_pointer_files
        );
        for p in m.lfs_pointer_sample_paths.iter().take(3) {
            println!("    - {p}");
        }
    }
}
```

### Step 6: Daemon assembles MountSummary — new `assemble_status_report` seam (Codex M5-plan-v1 #9)

The current `DaemonServer::get_status` (around `daemon.rs:898`) just returns `observability.status_report()`. That report cannot know cache reservations, working-set bytes, or per-mount sample paths because observability doesn't own the cache. v2 introduces a daemon-side seam that augments the base report.

In `crates/ctxfs-daemon/src/daemon.rs`:

```rust
impl DaemonServer {
    /// Build the full StatusReportV1 by augmenting observability's
    /// budget+counter view with cache-level details (working-set bytes,
    /// reservation bytes, per-mount LFS sample paths). Single seam so
    /// future cache fields plug in without re-scattering augmentation.
    fn assemble_status_report(&self) -> StatusReportV1 {
        let mut report = self.observability.status_report();

        // T2 augmentation: per-mount LFS fields. Fold sample paths and
        // count from the per-mount CounterSnapshot already on the
        // CounterEntry stream, indexed by mount_id.
        let lfs_by_mount: HashMap<String, (u64, Vec<String>)> = report
            .counters
            .iter()
            .map(|ce| {
                (
                    ce.key.mount_id.clone(),
                    (
                        ce.counters.lfs_pointer_files,
                        ce.counters.lfs_pointer_sample_paths.clone(),
                    ),
                )
            })
            .collect();

        for m in &mut report.mounts {
            if let Some((count, samples)) = lfs_by_mount.get(&m.mount_id) {
                m.lfs_pointer_files = *count;
                m.lfs_pointer_sample_paths = samples.clone();
            }
            // working_set_bytes / cache_reservation_bytes left 0 here;
            // T3c augments them with cache lookups keyed by RepoKey.
        }

        report
    }
}
```

`get_status` becomes a single line:

```rust
async fn get_status(self, _: tarpc::context::Context) -> Result<StatusReportV1, String> {
    Ok(self.assemble_status_report())
}
```

(Engineer: confirm exact tarpc handler shape against the live trait; the current handler returns `Result<StatusReportV1, String>`.)

### Step 7: Test detection wires through to status output

Inline test in daemon.rs or a focused integration test:

```rust
#[tokio::test]
async fn lfs_pointer_count_appears_in_status() {
    // build observability + counters; record an LFS pointer with a path;
    // build StatusReportV1 via the daemon's assembly path; assert
    // mount summary has lfs_pointer_files == 1 and the sample path.
}
```

(Engineer: model after existing daemon tests; if no straightforward seam exists, add one as a `pub(crate) fn build_mount_summary(...)` helper and unit-test it directly.)

### Step 8: Run + commit

```
cargo test
cargo clippy --all-targets --tests -- -D warnings
cargo fmt --all -- --check
```

```
git commit -m "$(cat <<'EOF'
feat(provider-common,provider-git,daemon,cli): B6 — detect LFS pointer files and surface in status

Adds a pure-bytes `detect_lfs_pointer` helper in provider-common
(manual 3-line parser, no regex dep), called from two GitHub
fetch paths:

- fetch_blob_content — single leaf for both lazy reads and the
  small-blob prefetch (which goes through fetch_blob_with_sha ->
  fetch_blob_content). Detecting once here avoids the double-count
  Codex flagged on M5-plan-v1.
- fetch_tarball_into_cache — separate streaming code path. The
  existing local Tee adapter is extended with an optional peek
  buffer that mirrors small entries (≤1024 bytes) so detection
  runs after SHA-1 verify succeeds, before atomic finalize.

GitHubProvider stashes a sha_to_path: Mutex<HashMap<String, PathBuf>>
populated post-snapshot from the tree manifest. The lazy detection
site looks up sha -> mount-relative path so sample paths in
ctxfs status are real paths (Codex M5-plan-v1 #8 — using the sha
as a path violated spec wording).

DaemonServer gains an assemble_status_report seam (Codex
M5-plan-v1 #9): observability supplies budgets + counters; daemon
augments per-mount summaries with LFS fields here (working_set
and reservation_bytes are added in T3c).

All new MountSummary / StatusReportV1 fields carry
#[serde(default)] so newer clients deserialize older v1 payloads
cleanly (Codex M5-plan-v1 #10).

On detection: increment per-mount lfs_pointer_files counter,
push mount-relative path into a 3-deep bounded sample buffer,
emit tracing::warn under target ctxfs.provider.lfs. Pointer
bytes are cached verbatim (no smudge yet — that's Phase 5).

The 1024-byte short-circuit fast-paths non-pointer reads. Manual
parser keeps "no new deps" promise.

Closes B6 (detect-and-surface). Full smudge to real bytes is
Phase 5.
EOF
)"
```

---

## Task 3a: B5 foundation — `RepoKey`, `MountCacheView`, manifest-time ownership, single mutex

**Files:**
- Create: `crates/ctxfs-cache/src/reservation.rs`
- Modify: `crates/ctxfs-cache/src/lib.rs` (single Mutex<CacheState> holds LRU + blob_owners + reservations; `register_mount` records ownership at manifest time; `add_owner`; `put_for` / `record_ownership_after_finalize` for adoption-style writes)
- Modify: `crates/ctxfs-provider-git/src/context.rs` (add `mount_cache: Option<Arc<MountCacheView>>`)

**Why split T3 into T3a/T3b/T3c:** B5 lands ~600 lines of touched code total. Sub-tasks let spec/quality reviewers verify foundational types before the eviction logic + status surfacing pile on. Sub-task boundaries are TDD-friendly — each sub-task ships green tests independently.

**v2 redesign rationale (Codex M5-plan-v1 #3, #4, #5):**

The original T3a recorded ownership only on `MountCacheView::put`. That misses the dominant ownership signal: **blobs the manifest references but that are already cached** (cache hits, contains_all skips, tree-cache hits). Without recording ownership for those, the B5 invariant is unenforceable at startup. v2 anchors ownership on **manifest membership**: when the daemon finishes building the snapshot, it hands the cache the full set of blob hexes (`register_mount(key, reservation_bytes, manifest_digests)`); the cache adds `key` to every digest's owner set whether or not the blob is currently cached. Future put-time updates (`MountCacheView::put` for fresh fetches; `record_ownership_after_finalize` for streaming tarball commits) only handle late additions like truncated-tree-fallback discoveries.

Atomicity: ownership is recorded *before* the snapshot returns and any read pressure starts. By the time the eviction loop ever runs, the owner-sets are in place. There is no "unowned new blob during eviction" window because new blobs land into a pre-claimed owner set.

Lock-order: a single `Mutex<CacheState>` holds the LRU entries, blob_owners, and reservations. The eviction loop holds it throughout. No two-lock dance, no snapshot/reacquire, no deadlock.

### Step 1: Define `RepoKey`, `ReservationEntry`, `MountCacheView`, and the new `CacheState` shape

In `crates/ctxfs-cache/src/reservation.rs` (new file):

```rust
//! Per-repo cache reservation primitives (B5).
//!
//! - `RepoKey { host, owner, repo }` identifies a logical repo
//!   independent of commit. Two mounts of the same repo at different
//!   commits share one reservation.
//! - `ReservationEntry` tracks reserved bytes, whether the value was
//!   explicitly user-supplied (and so should not be touched by the
//!   default-rebalance logic in T3b), and a refcount of active mounts.
//! - `MountCacheView` is a thin handle over `(Arc<BlobCache>, RepoKey)`
//!   used by providers; the *primary* ownership signal is
//!   `BlobCache::register_mount(key, reservation_bytes, manifest_digests)`,
//!   which records ownership for every blob the manifest names. The
//!   view's `put`/`record_ownership_after_finalize` cover late additions
//!   (truncated-tree fallbacks, fresh fetches outside the snapshot path).

use std::collections::{BTreeSet, HashMap};

#[derive(Debug, Clone, Eq, PartialEq, Hash, Ord, PartialOrd)]
pub struct RepoKey {
    pub host: String,
    pub owner: String,
    pub repo: String,
}

impl RepoKey {
    pub fn new(host: impl Into<String>, owner: impl Into<String>, repo: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            owner: owner.into(),
            repo: repo.into(),
        }
    }
}

/// Per-repo reservation budget held inside CacheState.
#[derive(Debug, Clone)]
pub(crate) struct ReservationEntry {
    /// Currently-effective reservation in bytes. T3b's default-rebalance
    /// code adjusts this for non-explicit entries on register/unregister.
    pub(crate) reserved_bytes: u64,
    /// True iff the user supplied --cache-reservation for this mount;
    /// such entries are *never* touched by default rebalance.
    pub(crate) is_explicit_override: bool,
    /// Number of currently active mounts for this RepoKey. Same repo at
    /// two commits means refcount=2; only on refcount->0 does the entry
    /// disappear from the table.
    pub(crate) refcount: u32,
}
```

Append the `MountCacheView` (no ownership-on-put indirection — manifest time is the primary signal):

```rust
use crate::BlobCache;
use ctxfs_core::error::CtxfsError;
use ctxfs_core::Digest;
use std::sync::Arc;

#[derive(Clone)]
pub struct MountCacheView {
    cache: Arc<BlobCache>,
    repo_key: RepoKey,
}

impl std::fmt::Debug for MountCacheView {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("MountCacheView")
            .field("cache", &self.cache)
            .field("repo_key", &self.repo_key)
            .finish()
    }
}

impl MountCacheView {
    pub fn new(cache: Arc<BlobCache>, repo_key: RepoKey) -> Self {
        Self { cache, repo_key }
    }

    pub fn cache(&self) -> &Arc<BlobCache> {
        &self.cache
    }

    pub fn repo_key(&self) -> &RepoKey {
        &self.repo_key
    }

    /// Put a blob and ensure ownership for this view's RepoKey is recorded.
    /// Used for late additions outside the snapshot's manifest (e.g.,
    /// truncated-tree fallback fetches discovered after `register_mount`).
    pub fn put(&self, digest: &Digest, data: &[u8]) -> Result<(), CtxfsError> {
        self.cache.put_for(&self.repo_key, digest, data)
    }

    /// Record ownership for an already-finalized blob. Called after the
    /// streaming tarball commit path (BlobTempWriter::finalize) for blobs
    /// that don't go through `put_for`. Idempotent.
    pub fn record_ownership_after_finalize(&self, digest: &Digest) {
        self.cache.add_owner(&self.repo_key, &digest.hex);
    }

    pub fn get(&self, digest: &Digest) -> Option<Vec<u8>> {
        self.cache.get(digest)
    }

    pub fn contains(&self, digest: &Digest) -> bool {
        self.cache.contains(digest)
    }
}
```

### Step 2: Restructure `BlobCache` around a single `CacheState` mutex

In `crates/ctxfs-cache/src/lib.rs`, replace the existing `state: Mutex<LruState>` with a unified `CacheState`:

```rust
use crate::reservation::{RepoKey, ReservationEntry};
use std::collections::{BTreeSet, HashMap};

pub(crate) struct CacheState {
    pub(crate) entries: linked_hash_map::LinkedHashMap<String, CacheEntry>,
    pub(crate) total_bytes: u64,
    /// blob hex -> set of repos whose manifest references this blob.
    pub(crate) blob_owners: HashMap<String, BTreeSet<RepoKey>>,
    /// Per-repo reservation budgets and refcounts.
    pub(crate) reservations: HashMap<RepoKey, ReservationEntry>,
}

pub struct BlobCache {
    root: PathBuf,
    max_bytes: Arc<AtomicU64>,
    state: Mutex<CacheState>,
    /// Cache-global counter: how many LRU eviction candidates were
    /// skipped because eviction would have violated a reservation.
    eviction_blocked_total: AtomicU64,
}
```

`evict_oldest`, `lru_insert_evict`, `prune`, `prune_blobs`, `set_max_bytes`, `rebuild_index`, `working_set_bytes`, and `contains_all` all operate on the unified state. The single mutex eliminates the lock-order dance.

Helpers:

```rust
impl BlobCache {
    /// Record ownership for a single blob hex without writing data.
    /// Used by tarball post-finalize (data already on disk via
    /// BlobTempWriter) and by `register_mount` to seed manifest membership.
    pub fn add_owner(&self, repo_key: &RepoKey, hex: &str) {
        let mut state = self.state.lock().unwrap();
        let _ = state
            .blob_owners
            .entry(hex.to_string())
            .or_default()
            .insert(repo_key.clone());
    }

    /// Idempotent equivalent of `put` + `add_owner`. Called by
    /// MountCacheView::put.
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

    /// Sum the sizes of cached blobs whose owner-set contains `key`.
    pub fn working_set_bytes(&self, key: &RepoKey) -> u64 {
        let state = self.state.lock().unwrap();
        state
            .blob_owners
            .iter()
            .filter(|(_, owners)| owners.contains(key))
            .filter_map(|(hex, _)| state.entries.get(hex.as_str()).map(|e| e.size))
            .sum()
    }

    /// Cache-global counter exposed for status assembly.
    pub fn eviction_attempts_blocked_by_reservation(&self) -> u64 {
        self.eviction_blocked_total.load(Ordering::Relaxed)
    }

    /// Lookup current reservation budget (T3b-populated). T3a returns
    /// None until register_mount lands.
    pub fn reservation_bytes(&self, key: &RepoKey) -> Option<u64> {
        let state = self.state.lock().unwrap();
        state.reservations.get(key).map(|e| e.reserved_bytes)
    }
}
```

`register_mount` lands in T3b along with reservation policy; T3a only ships the data structures and ownership-write helpers (`put_for`, `add_owner`).

Eviction-time owner cleanup: when an eviction succeeds, the loop in T3b's restructured `lru_insert_evict` removes `hex` from `blob_owners` (so working-set computations stay accurate). T3a leaves a `// TODO(T3b)` at the eviction point.

### Step 3: Inline tests for ownership recording

In `crates/ctxfs-cache/src/reservation.rs::tests`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::BlobCache;
    use ctxfs_core::Digest;
    use std::sync::Arc;

    fn key(repo: &str) -> RepoKey {
        RepoKey::new("api.github.com", "owner", repo)
    }

    #[test]
    fn repo_key_eq_and_hash() {
        let k1 = key("foo");
        let k2 = key("foo");
        let k3 = key("bar");
        assert_eq!(k1, k2);
        assert_ne!(k1, k3);
    }

    #[test]
    fn add_owner_and_put_for_record_ownership() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 1 << 20).unwrap());
        let view_a = MountCacheView::new(cache.clone(), key("repo-a"));
        let view_b = MountCacheView::new(cache.clone(), key("repo-b"));

        let d1 = Digest::from_sha1_hex("aaaa000000000000000000000000000000000000");
        let d2 = Digest::from_sha1_hex("bbbb000000000000000000000000000000000000");

        // put_for records ownership.
        view_a.put(&d1, b"shared").unwrap();
        view_b.put(&d1, b"shared").unwrap(); // same blob, different mount
        view_a.put(&d2, b"a-only").unwrap();

        // record_ownership_after_finalize is idempotent.
        view_b.record_ownership_after_finalize(&d2);
        view_b.record_ownership_after_finalize(&d2); // 2nd call is a no-op semantically

        // Sanity-check via working_set_bytes after the puts.
        assert_eq!(cache.working_set_bytes(&key("repo-a")), 6 + 6); // "shared"+"a-only"
        assert_eq!(cache.working_set_bytes(&key("repo-b")), 6 + 6); // "shared"+"a-only" (b adopted d2 via record_ownership_after_finalize)
    }

    #[test]
    fn working_set_bytes_sums_owned_blobs_only() {
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 1 << 20).unwrap());
        let view_a = MountCacheView::new(cache.clone(), key("repo-a"));
        let view_b = MountCacheView::new(cache.clone(), key("repo-b"));

        let d1 = Digest::from_sha1_hex("aaaa000000000000000000000000000000000000");
        let d2 = Digest::from_sha1_hex("bbbb000000000000000000000000000000000000");

        view_a.put(&d1, &vec![0u8; 100]).unwrap();
        view_b.put(&d2, &vec![0u8; 200]).unwrap();

        assert_eq!(cache.working_set_bytes(&key("repo-a")), 100);
        assert_eq!(cache.working_set_bytes(&key("repo-b")), 200);
    }

    #[test]
    fn add_owner_pre_claims_uncached_blob() {
        // Prove the manifest-time-ownership path: add_owner on a hex that
        // has no cache entry yet. Working-set is 0 (no cached bytes yet);
        // when a put later fetches that blob, ownership is already in
        // place and working_set_bytes reflects the new size.
        let dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 1 << 20).unwrap());
        let k = key("repo-a");
        let d = Digest::from_sha1_hex("cccc000000000000000000000000000000000000");

        cache.add_owner(&k, &d.hex);
        assert_eq!(cache.working_set_bytes(&k), 0);

        // Subsequent put — ownership is already in place; size now counts.
        cache.put(&d, &vec![1u8; 50]).unwrap();
        assert_eq!(cache.working_set_bytes(&k), 50);
    }
}
```

### Step 4: `ProviderContext` gains `mount_cache`

In `crates/ctxfs-provider-git/src/context.rs`:

```rust
use ctxfs_cache::reservation::MountCacheView;

#[derive(Clone)]
pub struct ProviderContext {
    pub api_host: String,
    pub observability: Arc<Observability>,
    pub cache: Arc<BlobCache>,
    pub tree_cache: Option<Arc<TreeCache>>,
    pub shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    pub singleflight: Arc<TarballSingleflightMap>,
    /// Per-mount cache view that pins a RepoKey for B5 ownership tracking.
    /// `None` for paths that don't need ownership tracking (NFS test
    /// helpers, FSKit shared-cache paths).
    pub mount_cache: Option<Arc<MountCacheView>>,
}
```

Update `Debug` impl, the test `make_test_provider_context` helper, and the daemon `prepare_mount` site to construct a `MountCacheView` from `(api_host, owner, repo)` and pass it.

`GitHubProvider`: in T3a, store `Option<Arc<MountCacheView>>` on the struct; T3b's tarball + small-blob commit paths use it. T3a does *not* yet rewrite blob commits — wiring + foundation only.

### Step 5: Run + commit

```
cargo test
cargo clippy --all-targets --tests -- -D warnings
cargo fmt --all -- --check
```

```
git commit -m "$(cat <<'EOF'
feat(cache,provider-git): B5 foundation — RepoKey, MountCacheView, manifest-time ownership, single mutex

T3a: B5 sub-task 1 of 3. No reservation policy yet — just the
foundation types and ownership-tracking primitives that T3b
builds on.

Codex M5-plan-v1 review re-rooted ownership on manifest
membership rather than per-put recording. Recording only on put
would miss cache hits, contains_all skips, and tree-cache hits —
all of which would silently break the B5 invariant at startup.
Manifest-time `register_mount(key, reservation_bytes,
manifest_digests)` (lands in T3b) seeds owner sets for every
blob the manifest names; T3a ships the data structures
(`add_owner`, `put_for`, `record_ownership_after_finalize`) that
the manifest-time path uses.

Cache state restructured behind a single Mutex<CacheState>
holding LRU entries + blob_owners + reservations. Eliminates the
lock-order dance Codex flagged in M5-plan-v1 #5; eviction code
in T3b will hold this mutex throughout.

ReservationEntry { reserved_bytes, is_explicit_override,
refcount } enables T3b's default-rebalance logic without
touching user-supplied --cache-reservation values.

ProviderContext gains Option<Arc<MountCacheView>>. Tarball
commit + small-blob commit paths in T3b will use
record_ownership_after_finalize after BlobTempWriter::finalize.
EOF
)"
```

---

## Task 3b: B5 reservation policy + eviction-skip path

**Files:**
- Modify: `crates/ctxfs-cache/src/lib.rs` (eviction loop honors reservations under single mutex; eviction-time owner-set cleanup; `register_mount` with manifest_digests + default-rebalance; `unregister_mount` with rebalance)
- Modify: `crates/ctxfs-cache/src/reservation.rs` (default-rebalance helper)
- Modify: `crates/ctxfs-ipc/src/service.rs` (`MountOptions.cache_reservation_bytes: Option<u64>`)
- Modify: `crates/ctxfs-cli/src/main.rs` (`--cache-reservation` mount flag with size-suffix parser, `deps/mount.rs` threading)
- Modify: `crates/ctxfs-daemon/src/daemon.rs` (post-snapshot register_mount with manifest digest list; unmount unregisters; MountInfo carries RepoKey)
- Modify: `crates/ctxfs-provider-git/src/github.rs` (tarball commit + small-blobs commit invoke MountCacheView::record_ownership_after_finalize / put)

**v2 redesign rationale (Codex M5-plan-v1 #6, #7):**
- Default `cache_max / count(active)` cannot freeze at first-mount registration; otherwise the first mount holds the entire cache forever. Register/unregister now **rebalance** all `!is_explicit_override` entries.
- Test data fixed: cache_max=500, A reserves 400 with working_set 300, B writes 400 → forces eviction; old test (A=300+B=700=1000=cache_max) didn't actually trigger eviction.

### Step 1: register/unregister/rebalance tests + the invariant test

In `crates/ctxfs-cache/src/reservation.rs::tests` (extends T3a's tests):

```rust
#[test]
fn register_mount_with_manifest_seeds_owner_set_for_uncached_blobs() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
    let k = key("foo");
    let manifest = vec![
        "aaaa000000000000000000000000000000000000".to_string(),
        "bbbb000000000000000000000000000000000000".to_string(),
    ];
    cache.register_mount(&k, Some(500), &manifest);

    // Both blobs claimed even though neither is cached yet.
    assert_eq!(cache.working_set_bytes(&k), 0); // no bytes cached yet

    let d = Digest::from_sha1_hex("aaaa000000000000000000000000000000000000");
    cache.put(&d, &vec![1u8; 50]).unwrap();
    // Ownership pre-claimed at register_mount → put updates working set.
    assert_eq!(cache.working_set_bytes(&k), 50);
}

#[test]
fn register_then_unregister_decrements_then_removes_with_rebalance() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1000).unwrap();

    // Default reservation: 1000 / 1 = 1000.
    cache.register_mount(&key("foo"), None, &[]);
    assert_eq!(cache.reservation_bytes(&key("foo")), Some(1000));

    // Adding a second mount halves both default reservations.
    cache.register_mount(&key("bar"), None, &[]);
    assert_eq!(cache.reservation_bytes(&key("foo")), Some(500));
    assert_eq!(cache.reservation_bytes(&key("bar")), Some(500));

    // Explicit override never gets touched by rebalance.
    cache.register_mount(&key("baz"), Some(700), &[]);
    assert_eq!(cache.reservation_bytes(&key("baz")), Some(700));
    assert_eq!(cache.reservation_bytes(&key("foo")), Some(150)); // (1000 - 700) / 2 — see policy below
    assert_eq!(cache.reservation_bytes(&key("bar")), Some(150));

    // Unregister rebalances back.
    cache.unregister_mount(&key("baz"));
    assert_eq!(cache.reservation_bytes(&key("foo")), Some(500));
    assert_eq!(cache.reservation_bytes(&key("bar")), Some(500));

    cache.unregister_mount(&key("foo"));
    cache.unregister_mount(&key("bar"));
    assert!(cache.reservation_bytes(&key("foo")).is_none());
}

#[test]
fn refcount_keeps_entry_alive_under_two_mounts_same_repo() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1024).unwrap();
    let k = key("foo");
    cache.register_mount(&k, Some(100), &[]);
    cache.register_mount(&k, Some(200), &[]); // second mount, same repo
    // First registration's reservation wins for refcount > 1; subsequent
    // calls are no-op on bytes (same repo, just refcount increment).
    assert_eq!(cache.reservation_bytes(&k), Some(100));

    cache.unregister_mount(&k);
    assert_eq!(cache.reservation_bytes(&k), Some(100)); // still 1 mount left
    cache.unregister_mount(&k);
    assert!(cache.reservation_bytes(&k).is_none());
}
```

**Policy for explicit override + default mix:** `default_reservation = max(0, (cache_max - sum(explicit_overrides)) / count(default_mounts))`. If explicit overrides exhaust the cache, default reservations clamp to 0 (mounts get no protection until an override mount unregisters).

### Step 2: B5 locked-invariant test (data fixed per Codex M5-plan-v1 #7)

Create `crates/ctxfs-cache/tests/reservation.rs`:

```rust
//! Integration test for the B5 locked invariant:
//! "an active repo with working set ≤ its reservation receives ZERO
//! evictions triggered by other repos' activity."

use ctxfs_cache::reservation::{MountCacheView, RepoKey};
use ctxfs_cache::BlobCache;
use ctxfs_core::Digest;
use std::sync::Arc;

fn k(repo: &str) -> RepoKey {
    RepoKey::new("api.github.com", "owner", repo)
}

#[test]
fn active_repo_within_reservation_receives_zero_evictions_from_other_repo() {
    // Cache total = 500 bytes. Two mounts:
    //   A: reservation 400, working set 300 (≤ reservation)
    //   B: reservation 400, working set 0 initially
    // B then writes 400 bytes; total goes to 700 > 500 → eviction needed.
    // Without protection, oldest A blob would evict (LRU order).
    // With protection, all of A's blobs stay; cache overflows to 700
    // (best-effort beyond reservation, per spec).
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 500).unwrap());

    let manifest_a = vec![
        format!("a{:039}", 0),
        format!("a{:039}", 1),
        format!("a{:039}", 2),
    ];
    cache.register_mount(&k("repo-a"), Some(400), &manifest_a);
    cache.register_mount(&k("repo-b"), Some(400), &[]); // B's manifest seeded later

    let view_a = MountCacheView::new(cache.clone(), k("repo-a"));
    let view_b = MountCacheView::new(cache.clone(), k("repo-b"));

    // A puts its three 100-byte blobs (working set = 300 ≤ reservation 400).
    let da_blobs: Vec<_> = (0..3u8)
        .map(|i| {
            let hex = format!("a{i:039}");
            let digest = Digest::from_sha1_hex(&hex);
            view_a.put(&digest, &vec![i; 100]).unwrap();
            digest
        })
        .collect();
    assert_eq!(cache.working_set_bytes(&k("repo-a")), 300);

    // B writes a 400-byte blob; total goes 300+400 = 700, > 500.
    // Eviction triggers; A's blobs are reservation-protected.
    let db = Digest::from_sha1_hex(&"b".repeat(40));
    view_b.put(&db, &vec![9u8; 400]).unwrap();

    // All of A's blobs must still be in the cache (B5 invariant).
    for d in &da_blobs {
        assert!(
            cache.contains(d),
            "B5 invariant violated: {d:?} was evicted despite reservation"
        );
    }
    // B's blob is also there (it was the latest write).
    assert!(cache.contains(&db));

    // Cache exceeds max_bytes (best-effort overflow).
    assert!(cache.total_bytes() > 500);

    // The blocked-eviction counter must have been incremented (≥1) when
    // the eviction loop tried to shed A's blobs.
    let blocked = cache.eviction_attempts_blocked_by_reservation();
    assert!(
        blocked >= 1,
        "expected eviction_attempts_blocked_by_reservation >= 1, got {blocked}"
    );
}

#[test]
fn over_reservation_repo_loses_blobs_on_pressure() {
    // A's working set > reservation → A is best-effort; eviction proceeds.
    let dir = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(dir.path().to_path_buf(), 500).unwrap());

    let manifest_a: Vec<String> = (0..4u8).map(|i| format!("c{i:039}")).collect();
    cache.register_mount(&k("repo-a"), Some(100), &manifest_a); // tiny reservation
    let view_a = MountCacheView::new(cache.clone(), k("repo-a"));

    for i in 0..4u8 {
        let hex = format!("c{i:039}");
        let digest = Digest::from_sha1_hex(&hex);
        view_a.put(&digest, &vec![i; 100]).unwrap();
    }
    // working_set_a = 400 > reservation 100 → over-reservation → best-effort.

    cache.register_mount(&k("repo-b"), Some(100), &[]);
    let view_b = MountCacheView::new(cache.clone(), k("repo-b"));
    let big = Digest::from_sha1_hex(&"d".repeat(40));
    view_b.put(&big, &vec![0u8; 200]).unwrap();

    // At least one of A's blobs must have evicted (A is over-reservation).
    let surviving_a = (0..4u8)
        .filter(|i| {
            let hex = format!("c{i:039}");
            cache.contains(&Digest::from_sha1_hex(&hex))
        })
        .count();
    assert!(surviving_a < 4, "over-reservation A should lose at least one blob");
}
```

### Step 3: Eviction-skip implementation under single mutex

Restructure `BlobCache::lru_insert_evict` to consult reservations under the same lock — no two-lock dance:

```rust
fn lru_insert_evict(&self, key_hex: String, size: u64, algorithm: HashAlgorithm) -> Vec<(String, HashAlgorithm)> {
    let mut evicted: Vec<(String, HashAlgorithm)> = Vec::new();
    let mut state = self.state.lock().unwrap();

    if let Some(existing) = state.entries.get(&key_hex) {
        state.total_bytes -= existing.size;
    }
    let _ = state.entries.insert(key_hex, CacheEntry { size, algorithm });
    state.total_bytes += size;

    let limit = self.max_bytes.load(Ordering::Relaxed);
    let mut skipped: usize = 0;

    while state.total_bytes > limit {
        let total_entries = state.entries.len();
        if total_entries == 0 || skipped >= total_entries {
            break; // entire LRU is reservation-protected → best-effort overflow
        }

        // Front of LRU is the eviction candidate.
        let (cand_hex, cand_size) = match state.entries.front() {
            Some((k, e)) => (k.clone(), e.size),
            None => break,
        };

        // Compute working_set_bytes for each owner of this candidate as if
        // we evicted. Since we hold the only lock, this is a direct read.
        let owners: Vec<RepoKey> = state
            .blob_owners
            .get(&cand_hex)
            .map(|o| o.iter().cloned().collect())
            .unwrap_or_default();

        let mut blocked = false;
        for owner in &owners {
            let Some(reservation) = state.reservations.get(owner) else {
                continue; // owner not active → no protection
            };
            // Compute owner's current working_set_bytes and the post-eviction
            // value. If post-eviction < reservation AND current >= reservation,
            // the eviction would drop them below their reservation: BLOCK.
            let owner_ws: u64 = state
                .blob_owners
                .iter()
                .filter(|(_, owners_of)| owners_of.contains(owner))
                .filter_map(|(hex, _)| state.entries.get(hex).map(|e| e.size))
                .sum();
            let post = owner_ws.saturating_sub(cand_size);
            if owner_ws <= reservation.reserved_bytes
                && post < reservation.reserved_bytes
            {
                blocked = true;
                break;
            }
        }

        if blocked {
            let _ = self
                .eviction_blocked_total
                .fetch_add(1, Ordering::Relaxed);
            // Rotate candidate to the back so the next iteration tries
            // the next-oldest entry. total_bytes is unchanged.
            if let Some(ent) = state.entries.remove(&cand_hex) {
                let _ = state.entries.insert(cand_hex, ent);
            }
            skipped += 1;
            continue;
        }

        // Not blocked — evict for real.
        if let Some((k, ent)) = state.entries.pop_front() {
            state.total_bytes -= ent.size;
            // Owner-set cleanup so working_set_bytes stays accurate.
            let _ = state.blob_owners.remove(&k);
            evicted.push((k, ent.algorithm));
        }
    }

    drop(state);
    evicted
}
```

The eviction loop now operates entirely under the single mutex. `remove_blob_file` runs after the lock drops, reading the algorithm from the returned tuple. The `skipped >= total_entries` termination handles the best-effort overflow case (cache stays > max_bytes).

`register_mount` / `unregister_mount` (also under the same mutex):

```rust
pub fn register_mount(
    &self,
    key: &RepoKey,
    reservation_bytes: Option<u64>,
    manifest_digests: &[String],
) {
    let max = self.max_bytes.load(Ordering::Relaxed);
    let mut state = self.state.lock().unwrap();

    // Refcount bookkeeping: same RepoKey under multiple mounts.
    state
        .reservations
        .entry(key.clone())
        .and_modify(|e| e.refcount = e.refcount.saturating_add(1))
        .or_insert_with(|| ReservationEntry {
            reserved_bytes: reservation_bytes.unwrap_or(0), // rebalanced below
            is_explicit_override: reservation_bytes.is_some(),
            refcount: 1,
        });

    // Seed manifest membership in blob_owners.
    for hex in manifest_digests {
        let _ = state
            .blob_owners
            .entry(hex.clone())
            .or_default()
            .insert(key.clone());
    }

    // Rebalance defaults: leave is_explicit_override entries alone; for
    // the rest, recompute as max - sum(explicit_overrides) / count(defaults).
    let explicit_total: u64 = state
        .reservations
        .values()
        .filter(|e| e.is_explicit_override)
        .map(|e| e.reserved_bytes)
        .sum();
    let default_count = state
        .reservations
        .values()
        .filter(|e| !e.is_explicit_override)
        .count() as u64;
    if default_count > 0 {
        let pool = max.saturating_sub(explicit_total);
        let per = pool / default_count;
        for entry in state.reservations.values_mut() {
            if !entry.is_explicit_override {
                entry.reserved_bytes = per;
            }
        }
    }
}

pub fn unregister_mount(&self, key: &RepoKey) {
    let max = self.max_bytes.load(Ordering::Relaxed);
    let mut state = self.state.lock().unwrap();

    let removed = if let Some(entry) = state.reservations.get_mut(key) {
        entry.refcount = entry.refcount.saturating_sub(1);
        entry.refcount == 0
    } else {
        false
    };
    if removed {
        let _ = state.reservations.remove(key);
        // Drop key from blob_owners (so eviction stops protecting on its
        // behalf). Naturally-unowned blobs may now be evicted normally.
        for owners in state.blob_owners.values_mut() {
            let _ = owners.remove(key);
        }
        state.blob_owners.retain(|_, owners| !owners.is_empty());
    }

    // Rebalance defaults.
    let explicit_total: u64 = state
        .reservations
        .values()
        .filter(|e| e.is_explicit_override)
        .map(|e| e.reserved_bytes)
        .sum();
    let default_count = state
        .reservations
        .values()
        .filter(|e| !e.is_explicit_override)
        .count() as u64;
    if default_count > 0 {
        let pool = max.saturating_sub(explicit_total);
        let per = pool / default_count;
        for entry in state.reservations.values_mut() {
            if !entry.is_explicit_override {
                entry.reserved_bytes = per;
            }
        }
    }
}
```

Per-mount counter increments are derived in status assembly — the primary counter is cache-global (`eviction_blocked_total: AtomicU64` on `BlobCache`) since reservation is a cache-level concept. Document this in the impl comments.

### Step 4: Wire `cache_reservation_bytes` through the stack + post-snapshot register_mount

In `crates/ctxfs-ipc/src/service.rs`:

```rust
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MountOptions {
    pub prefetch: PrefetchPolicy,
    /// Override the equal-share default. `None` -> default rebalances on
    /// register/unregister.
    #[serde(default)]
    pub cache_reservation_bytes: Option<u64>,
}
```

In `crates/ctxfs-cli/src/main.rs`, on the mount Commands variant:

```rust
/// Per-mount cache reservation in bytes (overrides the default
/// equal-share). Accepts size suffixes (e.g., 256MB, 1G).
#[arg(long, value_parser = parse_size_bytes)]
cache_reservation: Option<u64>,
```

`parse_size_bytes` parses `<n>[KMG][B]?` (e.g., `256MB`, `1G`, `512K`, raw `1048576`). Inline implementation:

```rust
fn parse_size_bytes(s: &str) -> Result<u64, String> {
    let s = s.trim().to_uppercase();
    let (num_str, mult): (&str, u64) = if let Some(n) = s.strip_suffix("GB").or_else(|| s.strip_suffix('G')) {
        (n, 1_073_741_824)
    } else if let Some(n) = s.strip_suffix("MB").or_else(|| s.strip_suffix('M')) {
        (n, 1_048_576)
    } else if let Some(n) = s.strip_suffix("KB").or_else(|| s.strip_suffix('K')) {
        (n, 1_024)
    } else if let Some(n) = s.strip_suffix('B') {
        (n, 1)
    } else {
        (s.as_str(), 1)
    };
    let n: u64 = num_str
        .trim()
        .parse()
        .map_err(|e| format!("invalid size '{s}': {e}"))?;
    n.checked_mul(mult)
        .ok_or_else(|| format!("size '{s}' overflows u64"))
}
```

`deps/mount.rs` (or wherever the CLI converts to MountOptions) threads the new value into the IPC payload.

**Daemon changes — `prepare_mount` calls `register_mount` *after* the snapshot is built**, so the manifest's blob hexes are available to seed ownership:

```rust
// crates/ctxfs-daemon/src/daemon.rs (inside prepare_mount, post-snapshot)

// 1. Resolve source -> (api_host, owner, repo).
let repo_key = RepoKey::new(api_host.clone(), owner.clone(), repo.clone());

// 2. Build the snapshot first (so manifest digests are known).
let snapshot = provider.fetch_snapshot_with_options(&source, &fetch_opts).await?;

// 3. Collect manifest digests for ownership seeding.
let manifest_digests: Vec<String> = snapshot
    .all_blob_digests()
    .iter()
    .map(|d| d.hex.clone())
    .collect();

// 4. Register the mount with the cache. Reservation: explicit override
//    or None (cache rebalances internally).
self.cache.register_mount(
    &repo_key,
    options.cache_reservation_bytes,
    &manifest_digests,
);
```

`Snapshot::all_blob_digests` is a small new helper that walks `directories` and pulls every `FileEntry::digest` (already exists conceptually for build-directories; engineer adds the convenience accessor in `ctxfs-manifest` if not present).

`MountInfo` carries `repo_key: RepoKey` so the unmount handler can call `cache.unregister_mount`. `RepoKey` derives `Serialize`/`Deserialize` so it can ride the IPC `MountInfo` if needed; otherwise daemon stores it in its in-memory mount registry.

On unmount:

```rust
self.cache.unregister_mount(&mount_info.repo_key);
```

Late-discovered blobs (truncated-tree fallback): when `fetch_directory` discovers a blob the original snapshot didn't list, the GitHubProvider calls `mount_cache.record_ownership_after_finalize(&digest)` (see Step 5) so ownership extends without a full re-register.

### Step 5: Tarball + small-blob commit paths invoke `MountCacheView`

In `crates/ctxfs-provider-git/src/github.rs`:

- **Tarball path** (`fetch_tarball_into_cache`): the streaming path uses `BlobCache::commit_atomic_with_writer()` directly — that returns a `BlobTempWriter` that finalizes by `rename(2)`. After `writer.finalize(&digest)` succeeds, call `mount_cache.record_ownership_after_finalize(&digest)` so ownership extends to any tarball entry the original manifest didn't list (rare but possible).

  ```rust
  writer.finalize(&digest)?;
  if let Some(mc) = self.mount_cache.as_ref() {
      mc.record_ownership_after_finalize(&digest);
  }
  ```

- **Small-blob prefetch** (`fetch_small_blobs_concurrent`): each blob is buffered into memory before commit. The commit path is `cache.put` today; switch to `mount_cache.put` (which is `cache.put_for`) when `mount_cache` is `Some`:

  ```rust
  if let Some(mc) = &self.mount_cache {
      let _ = mc.put(&digest, &bytes);
  } else {
      let _ = self.cache.put(&digest, &bytes);
  }
  ```

- **Lazy single blob** (`fetch_blob_content`): same shape — prefer `mount_cache.put` when present.

For paths where `mount_cache` is `None` (NFS test helpers, FSKit shared paths), the legacy `cache.put` path keeps working. No reservation enforcement applies in those paths — by design; only daemon-managed mounts opt into B5.

### Step 6: Run + commit

```
cargo test
cargo clippy --all-targets --tests -- -D warnings
cargo fmt --all -- --check
```

```
git commit -m "$(cat <<'EOF'
feat(cache,daemon,cli): B5 reservation + eviction-skip + per-mount flag

T3b: B5 sub-task 2 of 3. Implements the reservation policy and
the LRU eviction skip that enforces the locked invariant: an
active repo with working set <= reservation receives ZERO
evictions from other repos' activity.

- Default reservation rebalances on register_mount /
  unregister_mount: max - sum(explicit_overrides) /
  count(default_mounts). Explicit overrides via
  --cache-reservation are flagged via is_explicit_override and
  never touched by rebalance. Codex M5-plan-v1 #6: original v1
  computed default at first mount and froze, so a single mount
  held the entire cache forever. Fixed.
- IPC MountOptions.cache_reservation_bytes: Option<u64> with
  #[serde(default)] threads the override through to the daemon.
- BlobCache::register_mount(key, reservation_bytes,
  manifest_digests) seeds blob_owners for every blob the
  manifest references — including those already cached or never
  going through put. Codex M5-plan-v1 #3: this anchors the B5
  invariant on manifest membership instead of the unreliable
  per-put signal.
- Eviction loop runs entirely under the single CacheState mutex
  (no two-lock dance): for each LRU candidate, compute the
  post-eviction working_set_bytes for each owner; if the candidate
  is currently within reservation but evicting it would drop the
  owner below, skip and rotate to LRU back. eviction_blocked_total
  increments per skip. If the whole LRU rotates without finding
  a non-protected candidate, the put completes and the cache is
  allowed to exceed max_bytes temporarily — best-effort beyond
  reservation, per spec.
- Eviction-time owner-set cleanup keeps working_set_bytes
  accurate.
- Tarball + small-blob + lazy commit paths route through
  MountCacheView::put or
  MountCacheView::record_ownership_after_finalize when
  mount_cache is Some.

Test data (Codex M5-plan-v1 #7): cache_max=500, A reserves 400
with working_set 300, B writes 400. Old v1 numbers
(A=300+B=700=1000=cache_max) didn't trigger eviction.

Closes B5 (per-repo cache reservation). Surfacing in ctxfs status
is T3c.
EOF
)"
```

---

## Task 3c: B5 status surfacing

**Files:**
- Modify: `crates/ctxfs-daemon/src/daemon.rs` (`assemble_status_report` from T2 step 6 grows: B5 fields populated by `cache.working_set_bytes(key)` + `cache.reservation_bytes(key)` + `cache.eviction_attempts_blocked_by_reservation()`)
- Modify: `crates/ctxfs-cli/src/main.rs` (`print_global_status` per-mount line + over-reservation warning + cache-global blocked counter)

The status fields and `#[serde(default)]` were already added in T2 Step 5. T3c populates them and renders.

### Step 1: Status assembly populates B5 fields

Extend the `assemble_status_report` helper introduced in T2 Step 6:

```rust
fn assemble_status_report(&self) -> StatusReportV1 {
    let mut report = self.observability.status_report();

    // T2 augmentation (LFS) — same as before.
    let lfs_by_mount: HashMap<String, (u64, Vec<String>)> = report
        .counters
        .iter()
        .map(|ce| (ce.key.mount_id.clone(), (ce.counters.lfs_pointer_files, ce.counters.lfs_pointer_sample_paths.clone())))
        .collect();

    // T3c augmentation: B5 cache lookups per RepoKey.
    let registry = self.registry.lock().unwrap(); // or .blocking_lock per the daemon's Mutex flavor
    let repo_key_by_mount: HashMap<String, RepoKey> = registry
        .iter()
        .map(|(id, info)| (id.clone(), info.repo_key.clone()))
        .collect();
    drop(registry);

    for m in &mut report.mounts {
        if let Some((count, samples)) = lfs_by_mount.get(&m.mount_id) {
            m.lfs_pointer_files = *count;
            m.lfs_pointer_sample_paths = samples.clone();
        }
        if let Some(rk) = repo_key_by_mount.get(&m.mount_id) {
            m.working_set_bytes = self.cache.working_set_bytes(rk);
            m.cache_reservation_bytes = self.cache.reservation_bytes(rk).unwrap_or(0);
        }
    }

    report.cache_eviction_attempts_blocked_by_reservation =
        self.cache.eviction_attempts_blocked_by_reservation();

    report
}
```

(Engineer: confirm exact registry-lock shape. The daemon may use `std::sync::Mutex` or `tokio::sync::Mutex` — the helper should match. Pick the synchronous flavor if the existing `status_report` is sync; otherwise wrap accordingly.)

### Step 2: CLI prints per-mount reservation status

Extend `print_global_status` in `crates/ctxfs-cli/src/main.rs`:

```rust
println!();
println!("Per-mount cache usage:");
for m in report.mounts.iter().take(10) {
    let pct = if m.cache_reservation_bytes > 0 {
        (m.working_set_bytes as f64 / m.cache_reservation_bytes as f64) * 100.0
    } else {
        0.0
    };
    let warn = if m.working_set_bytes > m.cache_reservation_bytes && m.cache_reservation_bytes > 0 {
        " [OVER RESERVATION — best-effort eviction]"
    } else {
        ""
    };
    println!(
        "  {} ({}/{}): {} / {} bytes ({:.1}%){}",
        m.mount_id,
        m.source,
        m.repo,
        m.working_set_bytes,
        m.cache_reservation_bytes,
        pct,
        warn
    );
}

let blocked = report.cache_eviction_attempts_blocked_by_reservation;
println!(
    "Cache evictions blocked by reservation: {blocked}"
);
```

### Step 3: Status-end-to-end test

```rust
#[tokio::test]
async fn status_reports_working_set_and_reservation() {
    // build daemon test harness; mount a fake repo; populate cache;
    // call get_status; assert MountSummary has the right
    // working_set_bytes and cache_reservation_bytes.
}
```

(Engineer: model after existing `daemon` integration tests; if no harness, add a minimal one.)

### Step 4: Run + commit

```
cargo test
cargo clippy --all-targets --tests -- -D warnings
cargo fmt --all -- --check
```

```
git commit -m "$(cat <<'EOF'
feat(daemon,cli): B5 status surfacing — per-mount working set vs reservation

T3c: B5 sub-task 3 of 3. Closes B5 by making the reservation
behavior observable via `ctxfs status`.

- StatusReportV1.cache_eviction_attempts_blocked_by_reservation
  carries the cache-global counter for B5's exit criterion.
- MountSummary fields populated:
  - working_set_bytes: BlobCache::working_set_bytes(repo_key)
  - cache_reservation_bytes: BlobCache::reservation_bytes(key)
- print_global_status prints "Per-mount cache usage" with
  bytes used vs reservation and emits an "OVER RESERVATION —
  best-effort eviction" warning when usage exceeds reservation
  (best-effort behavior is correct per spec; the warning lets
  operators size the cache appropriately).
- Total cache evictions blocked counter shown as a tail line.

Closes B5 (per-repo cache reservation, observability complete).
EOF
)"
```

---

## Task 4: Replay tests for B5 + B6

**Files:**
- Create: `crates/ctxfs-provider-git/tests/replay_lfs_detect_surfaces_count.rs`
- Create: `crates/ctxfs-provider-git/tests/replay_b5_reservation_protects_active.rs`

### Step 1: B6 LFS replay test

```rust
//! Replay test: B6 — LFS pointer detection surfaces in counters.
//!
//! Spins up a tiny mock GitHub server that returns one LFS pointer
//! payload as a blob; confirms that fetch_blob increments
//! lfs_pointer_files and pushes the path to the sample buffer.

#[tokio::test]
async fn lfs_pointer_blob_surfaces_in_counters() {
    // Build mock server returning the canonical LFS pointer payload.
    // Construct a GitHubProvider pointed at the mock; call fetch_blob
    // on the test sha; snapshot counters; assert
    // lfs_pointer_files == 1 and the sample buffer contains the
    // expected sha-or-path.
}
```

(Engineer: model on `replay_basic.rs` or another existing replay test for HTTP mocking.)

### Step 2: B5 invariant replay test

```rust
//! Replay test: B5 — active repo within reservation receives ZERO
//! evictions from other repo's activity.
//!
//! End-to-end: build a daemon with a small cache; mount A (small
//! corpus, working set under reservation); mount B (large corpus,
//! forces eviction); scan A's files; assert all of A's blobs still
//! cache-hit and that eviction_attempts_blocked_by_reservation > 0.

#[tokio::test]
async fn mount_a_within_reservation_unaffected_by_mount_b_pressure() {
    // ...
}
```

(Engineer: this is the spec's stated regression test. Mock both repos with mock-server; validate via counters in StatusReportV1.)

### Step 3: Run + commit

```
cargo test
```

```
git commit -m "$(cat <<'EOF'
test(provider-git): B5 + B6 replay regression tests

Two end-to-end replay tests anchoring the M5 exit criteria:

- replay_lfs_detect_surfaces_count: a mock server returns an LFS
  pointer payload; fetch_blob detects it; counters report 1
  pointer with the expected sample path. (B6 exit.)
- replay_b5_reservation_protects_active: mount A (small working set
  ≤ reservation) + mount B (cache pressure) + scan A;
  cache_hits for A's blobs unchanged after B's writes;
  eviction_attempts_blocked_by_reservation > 0. (B5 exit.)

These tests are the canonical guard against regressions in the
B5 invariant and the B6 detect-and-surface flow.
EOF
)"
```

---

## Task 5: CHANGELOG + tag `v0.1.5-m5`

**Files:**
- Modify: `CHANGELOG.md`

### Step 1: Add CHANGELOG section

Prepend to the top under `# Changelog`:

```markdown
## v0.1.5-m5 — 2026-04-XX (Phase 4 M5)

**Closed bugs:**
- **B3-label**: GitHub blob digests now correctly carry `HashAlgorithm::Sha1`.
  `CacheEntry` tracks the on-disk algorithm so eviction always deletes the
  correct fan-out path. `rebuild_index` walks `sha1/` + `sha256/` and
  dedupes by hex (sha1 wins). `TreeCache` schema bumps 2 → 3, invalidating
  v2 manifests with mislabeled digests so they refetch correctly.
- **B5**: per-repo cache reservation in `ctxfs-cache`. Manifest-time
  ownership: each mount's `register_mount` seeds owner-sets for every
  blob the manifest references. Default reservation is
  `(cache_max - sum(explicit overrides)) / count(default mounts)`,
  rebalanced on register/unregister. `--cache-reservation` per-mount
  override flows through IPC `MountOptions.cache_reservation_bytes`.
  Eviction skips blobs whose eviction would drop a protected repo's
  working-set below its reservation (cache-global counter
  `eviction_attempts_blocked_by_reservation`); best-effort overflow when
  the entire LRU is reservation-protected. `ctxfs status` reports per-mount
  working-set vs reservation and the cache-global blocked counter.
- **B6**: LFS pointer files detected at fetch time. Manual 3-line parser
  in `provider-common::lfs` (no regex dep). Two detection sites in
  `provider-git`: `fetch_blob_content` (covers lazy + small-blob prefetch)
  and `fetch_tarball_into_cache` (extends the existing local Tee with an
  optional peek buffer for small entries). Detection increments
  `lfs_pointer_files` and pushes the mount-relative path into a 3-deep
  sample buffer; `ctxfs status` shows count + sample paths under an
  "LFS pointer files (Phase 5: smudge)" section. Pointer bytes are
  cached verbatim.

**Schema:**
- `TreeCache::SCHEMA_VERSION` 2 → 3 (B3-label corrects digest labels).
- `StatusReportV1` and `MountSummary` gain additive fields, all
  `#[serde(default)]` so older v1 payloads deserialize cleanly.

**Carry-forwards landed before milestone:**
- `default_cost_estimate` direct unit test (provider-common).
- `env_var_*` test race in `ctxfs-cli/backend` fixed via inner-fn refactor;
  tests no longer touch process-global state.

**Status:** Phase 4 code work complete. M6 follow-up is a decision memo
(`docs/phase5-stage2-decision.md`) drafted after 2–4 weeks of M5 telemetry
from real use.
```

### Step 2: Commit

```
git add CHANGELOG.md
git commit -m "$(cat <<'EOF'
chore(M5): CHANGELOG entry for v0.1.5-m5

Records B3-label, B5, and B6 closure plus the two pre-milestone
carry-forward commits (default_cost_estimate test, env_var race
fix).
EOF
)"
```

### Step 3: Tag (annotated, locally)

```
git tag -a v0.1.5-m5 -m "Phase 4 M5 — B3-label + B5 + B6"
```

**Do not push the tag here.** All five tags push together at the very end of M5 (T6).

---

## Task 6: Push all tags `v0.1.1-m1` through `v0.1.5-m5` together

**Files:** none (operational).

This is the **last action of Phase 4 code work** and the user's milestone-end-of-Phase-4 push instruction.

### Step 1: Confirm all five tags exist locally

```
git tag --list 'v0.1.*-m*'
```

Expected: `v0.1.1-m1`, `v0.1.2-m2`, `v0.1.3-m3`, `v0.1.4-m4`, `v0.1.5-m5`.

### Step 2: Push the M5 tag to be sure of HEAD-of-main

```
git push origin main
```

### Step 3: Push all tags together

```
git push --tags
```

Expected: five tag pushes succeed. CI/Sparkle release flow may pick these up; verify by checking the Releases page or Sparkle's appcast feed (per Phase 3 docs).

### Step 4: Mark milestone complete

Phase 4 code work is now closed. M6 follow-up is `docs/phase5-stage2-decision.md` after 2–4 weeks of telemetry; Phase 4 ends with that memo, not a code change.

---

## Self-Review Checklist (v2 — applied after Codex counsel)

1. **Spec coverage:** B3-label, B5, B6 all have a task + exit criterion test. ✓
2. **Codex required edits applied (10/10):**
   - #1 cache layout: `CacheEntry` tracks algorithm; `remove_blob_file` uses it; `rebuild_index` dedupes both algo dirs. ✓ (T1 Step 5)
   - #2 TreeCache schema bump 2→3 with v3 history bullet. ✓ (T1 Step 6)
   - #3 ownership re-rooted on manifest membership via `register_mount(key, reservation_bytes, manifest_digests)`. ✓ (T3a + T3b)
   - #4 atomicity: ownership recorded *before* eviction decisions; manifest-time registration happens before snapshot returns. ✓ (T3a + T3b Step 4)
   - #5 single `Mutex<CacheState>` holds LRU + blob_owners + reservations; eviction loop holds it throughout. ✓ (T3a Step 2 + T3b Step 3)
   - #6 default reservation rebalances on register/unregister; `is_explicit_override` flag protects user-supplied values. ✓ (T3b Step 1 + Step 3)
   - #7 invariant test data fixed: cache_max=500, A reserves 400 with ws=300, B writes 400 → forces eviction. ✓ (T3b Step 2)
   - #8 single non-tarball detection site in `fetch_blob_content`; sha→path map populated post-snapshot for sample paths. ✓ (T2 Step 4)
   - #9 daemon `assemble_status_report` seam augments observability with cache lookups. ✓ (T2 Step 6 + T3c Step 1)
   - #10 `#[serde(default)]` on every additive `MountSummary` and `StatusReportV1` field. ✓ (T2 Step 5)
   - extra: extends existing local Tee at `github.rs:1255` (no `std::io::Tee` reference). ✓ (T2 Step 4.3)
   - extra: manual 3-line LFS parser, no regex dep. ✓ (T2 Step 1)
3. **Placeholder scan:** No `TBD`/`TODO later`/etc. in normative content. The remaining `(Engineer: ...)` notes are concrete pointers (registry-lock flavor in T3c Step 1; B6 mock-server pattern in T4 Steps 1–2; `Snapshot::all_blob_digests` accessor location in T3b Step 4) — each has a clear next step rather than hand-waving. ✓
4. **Type consistency:** `RepoKey`, `MountCacheView`, `ReservationEntry`, `CacheState` all stable across T3a/T3b/T3c. `MountOptions.cache_reservation_bytes` (IPC) ↔ `--cache-reservation` (CLI) ↔ `register_mount(&key, Some(bytes), &manifest)` (cache). `eviction_attempts_blocked_by_reservation` consistent across `BlobCache` / `StatusReportV1`. ✓
5. **TDD discipline:** Each task starts with red tests; T3a foundations + ownership tests; T3b reservation/rebalance + invariant test; T3c status assembly test; T4 end-to-end replay. ✓
6. **No M4 carry-forward fold-in attempts**: L2, F5, M5/M6 quality items, L3 NOT folded — they don't intersect B3/B5/B6 surfaces. Deferred to Phase 5. ✓
7. **No new external deps**: manual LFS parser instead of `regex`. ✓

---

## Carry-forwards from M4 — explicitly *not* in M5 scope

These remain as Phase-5-perf or skip-able:

- **F2 (BlobTempWriter BufWriter)** — perf, low yield. Phase 5.
- **F4 (fetch_tree_walked sequential DFS → FuturesUnordered)** — only fires on truncated trees. Phase 5.
- **F5 (SlotClaim Drop impl)** — bounded growth on leader cancellation. Phase 5 candidate; needs guarded design (private `released` flag). Skip M5 because it doesn't intersect B3/B5/B6 surfaces.
- **F6 (update_gauge auth_identity clone)** — perf, low yield. Phase 5.
- **HeaderMap-direct refactor** — Phase 5 perf.
- **L2 (panic-as-Result on `Client::builder`)** — would propagate `Result<Self, CtxfsError>` through `prepare_mount`. M5 doesn't touch `GitHubProvider::new`'s constructor; folding L2 in dilutes scope. Phase 5.
- **L3 (numbered comments in `fetch_tarball_into_cache`)** — engineer may clean up incidentally if they touch the function for B6 detection (Step 4 detection-point C); not a required deliverable.
- **M5 quality (`format!("{e}")` in OnceCell), M6 quality (nested match in dispatch_tarball_for_requests)** — unrelated to M5; skip.

The M4 quality-reviewer Minor (`default_cost_estimate` direct test) and the env_var_* race **already landed** as pre-M5-prep commits before this plan began.

---

## Execution Handoff

**Plan v2 (Codex-reviewed, 10 required edits applied) ready for the team.**

Two execution options:

**1. Subagent-Driven (recommended)** — fresh subagent per task; team-lead reviews between tasks; established M2/M3/M4 protocol via the cmux team at `~/.claude/teams/phase4-impl/`.

**2. Inline Execution** — `superpowers:executing-plans` batch with checkpoints.

The team executes T1 → T2 → T3a → T3b → T3c → T4 → T5 → T6 strictly in order: each task's commits must land green before the next begins. Engineer rotation at clean task boundaries when context drops below ~30%; reviewers usually persist.
