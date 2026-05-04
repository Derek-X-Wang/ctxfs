# Phase 4 — M3: Tarball Prefetch with Smart Gate (+ B2, + Tarball Hardening, + Skeletal `ContentFetcher`) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Plan version:** v2 (Codex-reviewed 2026-04-27 via counsel; review at `/tmp/counsel/20260427-223719-claude-to-codex-79183d/codex.md`). Verdict: **ship with edits**. v2 applies the 10 required edits + Codex's other calls.

**Goal:** On the bulk-scan path, replace the current "1 REST call per blob ≥ 4 KB" with a single quota-bearing `/repos/{o}/{r}/tarball/{ref}` call that streams the entire repo as one tarball, hydrates blobs into `BlobCache` atomically (temp-and-verify-and-rename), and gates on a count + bytes auto-decision so we don't tarball repos where it isn't a win. Fix B2 truncated-tree fallback so the gate's `blob_count` and `estimated_bytes` are reliable on large repos. Introduce a *skeletal* `ContentFetcher` trait in `ctxfs-provider-common` so the tarball-vs-lazy decision lives in a `FetchPolicy` value, not as inline `if`/`else` in `GitHubProvider`; M4 fully lifts the trait without restructuring. Land the missing carry-forwards from M2 (symlink fail-strict, HeaderMap-direct classifier, `bearer_header()` helper, `<resolving:ref>` bucket pruning, assembled-path test) inside the same milestone so the M4 starting line is clean.

**Architecture:**
- A new `provider-common::fetcher` module defines `ContentRequest`, `ContentKind`, `CostEstimate`, `FetchMode`, `FetchPolicy`, `TarballKey`, and a `ContentFetcher` async trait. The trait is **skeletal** for M3: providers don't have to implement it yet, but `GitHubProvider` *expresses* its tarball-vs-lazy decision as a `FetchPolicy` value returned from a free function. M4 promotes `GitHubProvider` to the first concrete `ContentFetcher` impl without changing the call shape. **`TarballKey` lives in provider-common (not daemon) so provider-git can construct it without a dep cycle** (Codex edit #5).
- `ctxfs-core::Config` gains `prefetch_threshold_count`, `prefetch_max_bytes`, `github_host` fields with env-parsing (`CTXFS_PREFETCH_THRESHOLD_COUNT`, `CTXFS_PREFETCH_MAX_BYTES`, `CTXFS_GITHUB_HOST`). The byte cap default is `min(cache_max_bytes / 4, 256 MB)` so we never gate above a quarter of the cache budget. **The cap is recomputed after file/env apply** — if `cache_max_bytes` was set by config and `prefetch_max_bytes` was *not* explicitly set, we re-derive it (Codex edit #4).
- `MountOptions { prefetch: PrefetchPolicy }` becomes a parameter of the `mount` tarpc method. Wire format adds it explicitly. `PrefetchPolicy ∈ {Auto, Force, Disabled}`. CLI gets `--prefetch` / `--no-prefetch` flags (mutually exclusive; default = `Auto`). **Note:** adding a tarpc-method argument is **not** a backward-compatible wire change — every CLI + daemon must be rebuilt together (Codex other-call). M3 documents this in CHANGELOG; we do not promise compat.
- `BlobCache::commit_atomic` (bytes-in-memory variant) and **`BlobCache::commit_atomic_with_writer`** (streaming variant) are the new atomic-write entry points. The writer is **content-agnostic** — it does NOT hash internally. `BlobTempWriter::finalize(expected_digest)` does fsync → rename → **parent-dir fsync** (Codex other-call) → LRU update. Verification is the caller's responsibility (it has to be — different callers verify against different hash algorithms: SHA-256 for in-memory `commit_atomic`, Git blob SHA-1 for the tarball path). `commit_atomic(digest, data)` checks `Digest::sha256(data) == digest` *before* writing; Task 6's tarball path uses an external `GitBlobSha1` hasher + `Tee` writer to verify SHA-1 *before* calling `writer.finalize`. `BlobCache::put` keeps its current direct-write behavior for non-tarball callers (unchanged). **(Quality-reviewer M3-T4 review caught the bug: an internal SHA-256 hasher would always reject GitHub blob digests because GitHub manifest digests are 40-char SHA-1 hex, not 64-char SHA-256 hex.)**
- `BlobCache::cleanup_orphan_temps(older_than: Duration) -> u64` scans `<root>/tmp/` on daemon startup and unlinks files older than 1 hour (default). Counter `temp_orphans_cleared` per the spec. **`rebuild_index` skips `tmp/`** so partial blobs never enter the LRU (Codex other-call).
- `provider-git`'s tarball flow streams the gzipped response body via `bytes_stream() → StreamReader → SyncIoBridge`, runs `flate2::read::GzDecoder + tar::Archive` inside `tokio::task::spawn_blocking`, and pipes each entry directly into `BlobTempWriter` while hashing. Memory ceiling is **per-entry**, not per-archive. Each entry is path-validated, hashed (Git blob SHA-1: `sha1("blob <size>\0" || content)`), compared against the manifest digest, and atomically renamed into the cache — or discarded with a counter increment. (Codex edit #1 — buffered approach replaced with streaming.)
- The provider's reqwest client is built with **`reqwest::redirect::Policy::none()`** so reqwest never auto-follows the 302 with the Authorization header attached. Manual redirect handling (codeload-host whitelist, `Authorization` strip via fresh client, depth ≤ 3) lives in `follow_tarball_redirect` (Codex edit #2).
- `GitHubProvider` carries `api_host: String` plumbed in at construction time (no more hardcoded `GITHUB_HOST`). Used uniformly for `api_url`, `AuthIdentity`, and redirect-target validation. The daemon reads `config.github_host` and passes it into `GitHubProvider::new`. Tests set it explicitly (Codex edit #3).
- Singleflight dedupe map lives on the daemon: `Arc<DashMap<TarballKey, Arc<TarballSlot>>>`, where `TarballSlot { cell: OnceCell<Result<(), String>>, leader_id: Uuid }`. **`claim_singleflight_slot(&self, key) -> SlotClaim { cell, is_leader }`** is the new entry point: leader populates the cell and removes the slot via `remove_if(key, |slot| slot.leader_id == claim.leader_id)`, so an older claim cannot remove a newer slot (Codex edit #6). Errors stored in the cell are observed by waiters; same-flight retries are not attempted. Before claiming, the caller checks **`BlobCache::contains_all(digests)`** — if every manifest blob is already cached, skip tarball entirely.
- B2: when `fetch_tree`'s `truncated == true`, fall back to `fetch_tree_walked(source, &tree.sha)` — note: walked from the actual root tree SHA returned by the API, not `commit_sha` (Codex edit #8). Counter `truncated_tree_fallbacks` increments. **Manifest entries with `size == None` are treated as "unknown size, do not Auto-prefetch"** so the byte gate fails-closed on degraded data.
- **Symlink fail-strict (M2 carry-forward, folded here):** `build_directories_inner` symlink branch returns a hard error when the inline map entry is missing/oversized/invalid-UTF-8, instead of silently producing an empty target. Required because `readlink` has no lazy fallback in VFS.
- **`bearer_header(&str) -> HeaderValue` helper (M2 carry-forward):** one site in `provider-common::http`; three duplications in `provider-git` collapse to one call.
- **`<resolving:ref>` placeholder bucket pruning (M2 carry-forward):** after `counter_key` is replaced with the resolved commit SHA in `fetch_snapshot`, drop the placeholder bucket from `Observability.counters` (merging its `rest_calls_total` tick into the resolved bucket). Telemetry cleanup; no behavior change.
- **HeaderMap-direct refactor (M2 carry-forward) — DEFERRED.** Codex's review: "not load-bearing." We defer to Phase 5 perf work; the alloc savings are real but the M3 milestone has enough surface already.

**Tech Stack:** Rust 2021, reqwest with `stream` feature + `redirect::Policy::none()`, `tokio_util::io::{StreamReader, SyncIoBridge}`, `tar::Archive`, `flate2::read::GzDecoder`, `sha1` crate (workspace add), `tokio::task::spawn_blocking` (sync tar inside async fn), `tokio::sync::OnceCell` + `dashmap` (daemon singleflight). Workspace lints inherited.

**Spec reference:** `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` § Architecture (Crate-level changes, Data flow), § Milestones (M3 + B2). Exit criteria, sharpened by handoff:
- Cold scan of a 1k-file 30 MB repo: `rest_calls_total == 3` (commit + tree + tarball). Replay test against `MockProvider`.
- Truncated-tree replay test: per-directory walk fires; manifest is complete.
- Concurrent-mount replay test: two mounts of the same `(repo, commit)` produce one tarball call.
- Path-traversal replay test: `..` / absolute / escaping entries rejected; legitimate entries still land.
- Daemon-restart replay test: orphaned temp files cleared on startup.
- Below-byte-threshold replay test: prefetch skipped, `prefetch_skipped_oversized == 1`.
- After symlink fail-strict: a malformed symlink target in the test corpus produces a snapshot error, not an empty target.

---

## File Structure

```
crates/
  ctxfs-core/
    src/
      config.rs                          # MODIFY: add prefetch_threshold_count, prefetch_max_bytes, github_host fields + env parse
  ctxfs-provider-common/
    src/
      lib.rs                             # MODIFY: pub mod fetcher
      fetcher.rs                         # CREATE: ContentRequest/ContentKind/FetchMode/FetchPolicy/CostEstimate/ContentFetcher trait + TarballKey
      http.rs                            # MODIFY: bearer_header helper
      observability.rs                   # MODIFY: merge_and_drop_placeholder helper
  ctxfs-ipc/
    src/
      service.rs                         # MODIFY: PrefetchPolicy enum + MountOptions struct; mount RPC takes MountOptions
  ctxfs-cli/
    src/
      main.rs                            # MODIFY: --prefetch / --no-prefetch flags on Mount; pipe through handle_mount → client.mount
  ctxfs-cache/
    src/
      lib.rs                             # MODIFY: BlobCache::commit_atomic, commit_atomic_with_writer, BlobTempWriter, cleanup_orphan_temps, contains_all, skip tmp/ in rebuild_index
  ctxfs-provider-git/
    src/
      github.rs                          # MAJOR MODIFY: tarball endpoint (streaming), auto-gate, B2 walk, FetchPolicy dispatch, fail-strict symlinks, bearer_header sites, api_host plumbing, redirect::Policy::none()
    Cargo.toml                           # MODIFY: add tar, sha1, flate2, tokio-util deps
    tests/
      build_directories.rs               # MODIFY: assembled-path fetch_snapshot HTTP-mock test (M2 carry-forward)
      replay_tarball_three_calls.rs      # CREATE: cold-mount-1k-file-30MB → rest_calls_total == 3
      replay_truncated_tree_walk.rs      # CREATE: B2 fallback fires; manifest complete
      replay_singleflight_dedupe.rs      # CREATE: 2 concurrent mounts → 1 tarball call
      replay_path_traversal_rejected.rs  # CREATE: malicious tarball entries rejected
      replay_oversized_skipped.rs        # CREATE: prefetch skipped over byte cap; prefetch_skipped_oversized == 1
  ctxfs-daemon/
    src/
      daemon.rs                          # MODIFY: SingleflightTarballMap, MountOptions plumbing, restart cleanup_orphan_temps call, drop placeholder bucket
  ctxfs-cache/
    tests/
      lifecycle.rs                       # MODIFY: cleanup_orphan_temps test; commit_atomic crash-simulation test; rebuild_index skips tmp/
Cargo.toml                               # MODIFY: workspace [dependencies] adds tar, sha1, flate2, tokio-util features
```

---

## Task 1: Config + env vars in `ctxfs-core` (foundation)

**Files:**
- Modify: `crates/ctxfs-core/src/config.rs`

**Why first:** every later task references `Config::prefetch_threshold_count`, `Config::prefetch_max_bytes`, and `Config::github_host`. Landing this first removes a downstream blocker.

- [ ] **Step 1: Add unit tests for the new fields**

In the existing `#[cfg(test)] mod tests` block in `config.rs`, add:

```rust
    #[test]
    fn prefetch_threshold_count_default_is_30() {
        let c = Config::default();
        assert_eq!(c.prefetch_threshold_count, 30);
    }

    #[test]
    fn prefetch_max_bytes_default_is_min_quarter_cache_or_256mb() {
        let c = Config::default();
        // cache_max_bytes default = 512 MB, so quarter = 128 MB.
        // min(128 MB, 256 MB) = 128 MB.
        assert_eq!(c.prefetch_max_bytes, 128 * 1024 * 1024);
    }

    #[test]
    fn prefetch_max_bytes_capped_at_256mb_when_cache_is_huge() {
        // Exercise the helper directly with a 100 GB cache budget.
        let computed = Config::default_prefetch_max_bytes(100 * 1024 * 1024 * 1024);
        assert_eq!(computed, 256 * 1024 * 1024);
    }

    #[test]
    fn github_host_default_is_api_github_com() {
        let c = Config::default();
        assert_eq!(c.github_host, "api.github.com");
    }

    #[test]
    fn env_overrides_for_prefetch_and_host() {
        // Use a local Config to avoid touching real env vars in tests:
        let mut c = Config::default();
        let mut explicit = PrefetchExplicit::default();
        Config::apply_prefetch_env(&mut c, &mut explicit, |k| match k {
            "CTXFS_PREFETCH_THRESHOLD_COUNT" => Ok("100".to_string()),
            "CTXFS_PREFETCH_MAX_BYTES" => Ok("9999".to_string()),
            "CTXFS_GITHUB_HOST" => Ok("ghe.example.com".to_string()),
            _ => Err(std::env::VarError::NotPresent),
        });
        assert_eq!(c.prefetch_threshold_count, 100);
        assert_eq!(c.prefetch_max_bytes, 9999);
        assert!(explicit.max_bytes);
        assert_eq!(c.github_host, "ghe.example.com");
    }

    #[test]
    fn prefetch_max_bytes_recomputes_when_cache_changed_but_max_not_set() {
        // Simulate: file/env sets cache_max_bytes=1 GB but does NOT set
        // prefetch_max_bytes. After load(), prefetch_max_bytes should be
        // re-derived as min(1GB/4, 256MB) = 256MB, not the 128MB default.
        let mut c = Config::default();
        c.cache_max_bytes = 1024 * 1024 * 1024; // 1 GB
        let explicit = PrefetchExplicit::default(); // max_bytes NOT set
        Config::recompute_derived_defaults(&mut c, &explicit);
        assert_eq!(c.prefetch_max_bytes, 256 * 1024 * 1024);
    }

    #[test]
    fn prefetch_max_bytes_explicit_set_is_preserved_after_cache_change() {
        let mut c = Config::default();
        c.cache_max_bytes = 1024 * 1024 * 1024;
        c.prefetch_max_bytes = 50; // user-explicit
        let explicit = PrefetchExplicit { max_bytes: true, ..Default::default() };
        Config::recompute_derived_defaults(&mut c, &explicit);
        assert_eq!(c.prefetch_max_bytes, 50);
    }
```

(Using a closure-injected env reader rather than `std::env::set_var` avoids the parallel-env-var race the M2 handoff flagged.)

- [ ] **Step 2: Run, expect compile failure** — `cargo test -p ctxfs-core prefetch` → field missing.

- [ ] **Step 3: Add the three fields + helper + env parsing**

In `Config`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    // ... existing fields ...
    /// Auto-gate threshold for tarball prefetch. When the manifest reports at
    /// least this many blob entries AND `estimated_bytes <= prefetch_max_bytes`,
    /// `PrefetchPolicy::Auto` fires the tarball path. Default 30.
    pub prefetch_threshold_count: u64,
    /// Auto-gate byte cap for tarball prefetch. When the manifest's estimated
    /// bytes exceeds this value, `PrefetchPolicy::Auto` skips the tarball path
    /// and increments `prefetch_skipped_oversized`. Default = min(cache_max/4, 256 MB).
    pub prefetch_max_bytes: u64,
    /// Hostname of the GitHub API. Override for GHE deployments. Used by
    /// provider-git for both API URLs and tarball-redirect host validation
    /// (the codeload host is derived from this — see provider-git).
    /// Default "api.github.com".
    pub github_host: String,
}
```

In `Config::default()`:

```rust
let cache_max_bytes = 512 * 1024 * 1024;
Self {
    // ... existing field initializers ...
    cache_max_bytes,
    prefetch_threshold_count: 30,
    prefetch_max_bytes: Self::default_prefetch_max_bytes(cache_max_bytes),
    github_host: "api.github.com".to_string(),
}
```

Add helpers + an "explicit" tracker:

```rust
/// Tracks which prefetch fields were explicitly set by file or env so
/// `recompute_derived_defaults` knows whether `prefetch_max_bytes` is
/// safe to re-derive after `cache_max_bytes` changed.
#[derive(Debug, Default, Clone, Copy)]
pub(crate) struct PrefetchExplicit {
    pub max_bytes: bool,
    pub threshold_count: bool,
    pub github_host: bool,
}

impl Config {
    /// Default cap for tarball-prefetch bytes: min(cache_max / 4, 256 MB).
    /// Public so the CLI's `ctxfs status` global view can report the active cap
    /// without re-deriving it.
    pub fn default_prefetch_max_bytes(cache_max_bytes: u64) -> u64 {
        let quarter = cache_max_bytes / 4;
        let cap = 256 * 1024 * 1024;
        quarter.min(cap)
    }

    /// After file + env apply, re-derive `prefetch_max_bytes` from the
    /// (possibly-updated) `cache_max_bytes` IF the user did not explicitly
    /// set it. Without this, a config that changes only `cache_max_bytes`
    /// keeps the 128 MB default that was derived from the *old* 512 MB cache.
    pub(crate) fn recompute_derived_defaults(
        config: &mut Self,
        explicit: &PrefetchExplicit,
    ) {
        if !explicit.max_bytes {
            config.prefetch_max_bytes = Self::default_prefetch_max_bytes(config.cache_max_bytes);
        }
    }
}
```

`Config::apply_env` is restructured to call a new internal `apply_prefetch_env` that records explicitness, then calls `recompute_derived_defaults` once at the tail. Pseudocode:

```rust
    fn apply_env(config: &mut Self) {
        // ... existing fields (socket_path, cache_dir, cache_max_bytes, etc.) ...
        let mut explicit = PrefetchExplicit::default();
        Self::apply_prefetch_env(config, &mut explicit, |k| std::env::var(k));
        Self::recompute_derived_defaults(config, &explicit);
    }

    /// Closure-injected env reader (test seam) — single source of truth for
    /// the three M3 env vars. Records into `explicit` whether each field was
    /// touched, so the caller can re-derive defaults that depend on these.
    pub(crate) fn apply_prefetch_env<F>(
        config: &mut Self,
        explicit: &mut PrefetchExplicit,
        mut read: F,
    )
    where
        F: FnMut(&str) -> Result<String, std::env::VarError>,
    {
        if let Ok(v) = read("CTXFS_PREFETCH_THRESHOLD_COUNT") {
            if let Ok(n) = v.parse() {
                config.prefetch_threshold_count = n;
                explicit.threshold_count = true;
            }
        }
        if let Ok(v) = read("CTXFS_PREFETCH_MAX_BYTES") {
            if let Ok(n) = v.parse() {
                config.prefetch_max_bytes = n;
                explicit.max_bytes = true;
            }
        }
        if let Ok(v) = read("CTXFS_GITHUB_HOST") {
            if !v.is_empty() {
                config.github_host = v;
                explicit.github_host = true;
            }
        }
    }
```

`apply_file` is updated similarly: each prefetch field touched by file marks the matching `explicit.*` flag. `Config::load()` runs `apply_file` and `apply_env` in order, then `recompute_derived_defaults` once at the end (the explicit struct accumulates across both layers).

- [ ] **Step 4: Add fields to `ConfigFile` and `apply_file`** so the same three settings can be declared in `~/.ctxfs/config.toml`. Match the existing pattern. **`apply_file` accepts and updates `&mut PrefetchExplicit` for each prefetch field touched.** `Config::load()` then calls `recompute_derived_defaults` once after both `apply_file` and `apply_env` have run, so a TOML-only `cache_max_bytes` still re-derives `prefetch_max_bytes` correctly.

- [ ] **Step 5: Verify**

```bash
cargo test -p ctxfs-core
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-core/src/config.rs

git commit -m "$(cat <<'EOF'
feat(core,config): add prefetch_threshold_count, prefetch_max_bytes, github_host

These fields back the M3 tarball auto-gate. Defaults:
- prefetch_threshold_count = 30 (manifest blob count)
- prefetch_max_bytes = min(cache_max / 4, 256 MB)
  (so we never gate above a quarter of the cache budget; the codepath
   that triggers tarball prefetch is the same one that fills the cache)
- github_host = "api.github.com"

The byte-cap helper Config::default_prefetch_max_bytes is public so
`ctxfs status` can report the active cap without re-deriving it.

apply_prefetch_env uses a closure-injected env reader so the unit
test doesn't need to set process env vars; this avoids the
parallel-env-var test race documented in the M2 handoff.

PrefetchExplicit tracks which fields the file/env actually set.
recompute_derived_defaults runs once at end of load() and re-derives
prefetch_max_bytes from cache_max_bytes IF the user didn't set it
explicitly. Without this, changing only cache_max_bytes (file or env)
keeps the stale 128 MB default that was derived from the original
512 MB cache (Codex M3-plan-v1 review #4).

CTXFS_PREFETCH_THRESHOLD_COUNT, CTXFS_PREFETCH_MAX_BYTES, and
CTXFS_GITHUB_HOST env vars override defaults; ConfigFile fields
mirror them. Env always wins over file values (existing precedence).
EOF
)"
```

---

## Task 2: `ContentFetcher` skeletal trait in `provider-common`

**Files:**
- Create: `crates/ctxfs-provider-common/src/fetcher.rs`
- Modify: `crates/ctxfs-provider-common/src/lib.rs`

**Goal:** introduce the type vocabulary that M3's `GitHubProvider` will use to express its tarball-vs-lazy decision as a value (`FetchPolicy::Tarball{...} | FetchPolicy::Lazy`), and that M4 will then promote into a fully implemented trait. M3 does NOT change `Provider` or call sites — it only ships the types.

- [ ] **Step 1: Add the module skeleton**

`crates/ctxfs-provider-common/src/fetcher.rs`:

```rust
//! Source-agnostic content fetching contract.
//!
//! Skeletal in M3: the types and trait shape ship so that
//! `GitHubProvider`'s tarball-vs-lazy decision can be expressed as a
//! `FetchPolicy` value (rather than inline `if`/`else`), and so that
//! provider-common-level tests can describe expected behavior without
//! pulling in `provider-git`. M4 promotes `GitHubProvider` to the first
//! concrete `ContentFetcher` impl without restructuring the call shape.
//!
//! The trait is intentionally minimal: future native-CDN providers (npm
//! tarballs, PyPI sdists, crates.io `.crate` files) plug in by
//! implementing it.

use crate::counters::CounterKey;
use ctxfs_core::Digest;
use std::path::PathBuf;

/// What kind of mount-relative entry a request points at.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ContentKind {
    /// Regular file blob.
    File,
    /// Symlink — `digest` (when set) names the blob storing the link target.
    Symlink,
    /// Git LFS pointer file (M5 detect; M3 stores pointer bytes verbatim).
    LfsPointer,
}

/// Mount-relative content request. Provider-agnostic: GitHub blobs, npm
/// tarball entries, PyPI sdist entries can all be expressed.
#[derive(Debug, Clone)]
pub struct ContentRequest {
    /// Mount-relative path (semantic key — what the user would `cat`).
    pub path: PathBuf,
    /// Content hash if the source provides one. GitHub: blob SHA-1 (post-M5
    /// `Digest::Sha1`); npm: integrity SHA. None => no upstream digest available.
    pub digest: Option<Digest>,
    /// Estimated bytes from the manifest. None => unknown until fetched.
    pub size: Option<u64>,
    /// Entry shape.
    pub kind: ContentKind,
}

/// How a batch of requests should be fulfilled.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FetchMode {
    /// Best-effort lazy: request when the user reads the file.
    Lazy,
    /// Bulk prefetch (e.g., GitHub tarball, npm tarball-of-tarballs).
    BulkPrefetch,
    /// User explicitly forced (e.g., `--prefetch`).
    Forced,
}

/// User-facing prefetch policy from `MountOptions`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PrefetchPolicy {
    /// Auto-gate: tarball if (count >= threshold) AND (bytes <= cap).
    Auto,
    /// Bypass the byte cap; warn if estimated_bytes > cache budget.
    Force,
    /// Never prefetch; always lazy.
    Disabled,
}

impl Default for PrefetchPolicy {
    fn default() -> Self { Self::Auto }
}

/// Result of the auto-gate computation: which fetch shape to use this mount.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum FetchPolicy {
    /// Bulk-fetch via the source's archive endpoint (Stage 1 GitHub tarball).
    Tarball {
        estimated_bytes: u64,
        blob_count: u64,
    },
    /// Per-blob lazy fetch on read.
    Lazy,
    /// Auto-gate would have fired but the byte cap was exceeded.
    /// Telemetry-bearing variant so callers increment
    /// `prefetch_skipped_oversized` and surface the reason in `ctxfs status`.
    LazyOversized {
        estimated_bytes: u64,
        blob_count: u64,
        cap: u64,
    },
}

/// Cost-estimate signal a `ContentFetcher` can offer up front. M3 uses only
/// `total_bytes` and `request_count` — the rest are reserved for M4.
#[derive(Debug, Clone, Default)]
pub struct CostEstimate {
    pub total_bytes: Option<u64>,
    pub request_count: usize,
    pub fetch_mode: Option<FetchMode>,
}

/// Key for daemon-side singleflight tarball dedupe. Lives in provider-common
/// (not daemon) so provider-git can construct it without inducing a dep cycle
/// (provider-git → provider-common is the existing direction).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TarballKey {
    /// API host: `api.github.com`, or the configured `CTXFS_GITHUB_HOST` for
    /// GHE. Differentiates so a single daemon serving multiple hosts is fine.
    pub host: String,
    pub owner: String,
    pub repo: String,
    pub commit_sha: String,
}

/// Skeletal trait. M3 does not require providers to implement it; M4 will.
/// The signature is kept narrow on purpose — extending the surface is a
/// breaking change once a second provider implements it.
#[async_trait::async_trait]
pub trait ContentFetcher: Send + Sync {
    /// Cost-of-fulfillment estimate for a batch. Pure if possible; safe to
    /// call multiple times. M4 callers use this to drive scheduling decisions.
    fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate;

    /// Fetch the given requests. The provider chooses tarball vs lazy via
    /// its own `decide_policy(...)` (which is the M3 free function).
    /// The returned map is keyed by `ContentRequest::path`.
    async fn fetch_batch(
        &self,
        requests: &[ContentRequest],
        mode: FetchMode,
        counter_key: Option<CounterKey>,
    ) -> Result<std::collections::HashMap<PathBuf, Vec<u8>>, ctxfs_core::error::CtxfsError>;
}

/// Pure auto-gate: given a count + estimated bytes + the user's policy + the
/// configured thresholds, return the FetchPolicy to apply. Lives here so
/// providers (M3 GitHub, M4 future) all use the same algorithm.
#[must_use]
pub fn decide_policy(
    blob_count: u64,
    estimated_bytes: u64,
    policy: PrefetchPolicy,
    threshold_count: u64,
    max_bytes: u64,
) -> FetchPolicy {
    match policy {
        PrefetchPolicy::Disabled => FetchPolicy::Lazy,
        PrefetchPolicy::Force => FetchPolicy::Tarball { estimated_bytes, blob_count },
        PrefetchPolicy::Auto => {
            if blob_count < threshold_count {
                FetchPolicy::Lazy
            } else if estimated_bytes > max_bytes {
                FetchPolicy::LazyOversized {
                    estimated_bytes,
                    blob_count,
                    cap: max_bytes,
                }
            } else {
                FetchPolicy::Tarball { estimated_bytes, blob_count }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_gate_below_count_is_lazy() {
        assert_eq!(
            decide_policy(10, 1_000, PrefetchPolicy::Auto, 30, 256_000_000),
            FetchPolicy::Lazy,
        );
    }

    #[test]
    fn auto_gate_at_count_within_bytes_is_tarball() {
        let p = decide_policy(30, 1_000, PrefetchPolicy::Auto, 30, 256_000_000);
        assert!(matches!(p, FetchPolicy::Tarball { .. }));
    }

    #[test]
    fn auto_gate_above_bytes_is_lazy_oversized() {
        let p = decide_policy(1_000, 500_000_000, PrefetchPolicy::Auto, 30, 256_000_000);
        match p {
            FetchPolicy::LazyOversized { estimated_bytes, blob_count, cap } => {
                assert_eq!(estimated_bytes, 500_000_000);
                assert_eq!(blob_count, 1_000);
                assert_eq!(cap, 256_000_000);
            }
            other => panic!("expected LazyOversized, got {other:?}"),
        }
    }

    #[test]
    fn force_bypasses_byte_cap() {
        let p = decide_policy(2, 999_999_999, PrefetchPolicy::Force, 30, 1_000);
        assert!(matches!(p, FetchPolicy::Tarball { .. }));
    }

    #[test]
    fn disabled_is_always_lazy() {
        assert_eq!(
            decide_policy(1_000, 100, PrefetchPolicy::Disabled, 30, 256_000_000),
            FetchPolicy::Lazy,
        );
    }
}
```

- [ ] **Step 2: Wire into `lib.rs`**

`crates/ctxfs-provider-common/src/lib.rs`:

```rust
pub mod counters;
pub mod fetcher;
pub mod http;
pub mod mock;
pub mod observability;
pub mod rate_limit;
pub mod repo_url;
pub mod resolver;
pub mod status;
```

- [ ] **Step 3: Verify**

```bash
cargo test -p ctxfs-provider-common fetcher
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-provider-common/src/fetcher.rs \
        crates/ctxfs-provider-common/src/lib.rs

git commit -m "$(cat <<'EOF'
feat(provider-common): skeletal ContentFetcher trait + decide_policy

Introduces the type vocabulary M3's GitHubProvider uses to express its
tarball-vs-lazy decision as a value (FetchPolicy) instead of inline
control flow. M4 promotes GitHubProvider into the first concrete
ContentFetcher impl without restructuring callers.

Pure-logic decide_policy(blob_count, est_bytes, policy, threshold,
max_bytes) is the auto-gate. Provider crates re-call it; provider-common
owns the algorithm so a second native-content provider (npm tarball,
PyPI sdist) gets the same gate behavior for free.

PrefetchPolicy enum is exported here (Disabled/Auto/Force) and re-used
verbatim by ctxfs-ipc::MountOptions in Task 3 — no duplicate definition.

Skeletal — providers don't need to implement ContentFetcher in M3.
M4 lifts.
EOF
)"
```

---

## Task 3: `MountOptions` IPC + CLI flags

**Files:**
- Modify: `crates/ctxfs-ipc/src/service.rs`
- Modify: `crates/ctxfs-cli/src/main.rs`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

**Why now:** the wire shape determines what `do_mount` and `prepare_mount` will receive once they need to consult `PrefetchPolicy`. Landing it before the tarball implementation means later tasks read from the field instead of plumbing it.

**Wire-compat note (Codex other-call):** adding an argument to a `tarpc::service` method is **not** wire-compatible — older CLIs talking to a newer daemon (or vice versa) will fail. We do *not* claim back-compat from `#[serde(default)]` on the `MountOptions` field; the whole RPC sig changed. M3 ships under "client and daemon must rebuild together"; the CHANGELOG (Task 8d) flags this explicitly. Users who keep an old CLI around will need to re-install (homebrew cask + dev binary). This is acceptable for the soft-launch user base.

- [ ] **Step 1: Add the IPC types**

In `crates/ctxfs-ipc/src/service.rs`, near the top (after `MountStatus`):

```rust
pub use ctxfs_provider_common::fetcher::PrefetchPolicy;

/// User-overridable options on `mount`. Backward-compatible: a missing
/// `prefetch` field deserializes to `PrefetchPolicy::Auto` via Default.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct MountOptions {
    #[serde(default)]
    pub prefetch: PrefetchPolicy,
}
```

In the `CtxfsService` trait, change `mount`:

```rust
#[tarpc::service]
pub trait CtxfsService {
    async fn mount(
        source: String,
        mount_point: String,
        backend: ctxfs_core::Backend,
        options: MountOptions,
    ) -> Result<MountInfo, String>;
    // ...
}
```

Add a serde-roundtrip test:

```rust
    #[test]
    fn mount_options_default_is_auto() {
        let m = MountOptions::default();
        assert_eq!(m.prefetch, PrefetchPolicy::Auto);
    }

    #[test]
    fn mount_options_serde_roundtrip() {
        for policy in [PrefetchPolicy::Auto, PrefetchPolicy::Force, PrefetchPolicy::Disabled] {
            let opt = MountOptions { prefetch: policy };
            let s = serde_json::to_string(&opt).unwrap();
            let opt2: MountOptions = serde_json::from_str(&s).unwrap();
            assert_eq!(opt2.prefetch, policy);
        }
    }
```

- [ ] **Step 2: Update daemon `mount` signature**

In `crates/ctxfs-daemon/src/daemon.rs`, find:

```rust
    async fn mount(
        self,
        _: tarpc::context::Context,
        source: String,
        mount_point: String,
        backend: Backend,
    ) -> Result<MountInfo, String> {
```

Change to:

```rust
    async fn mount(
        self,
        _: tarpc::context::Context,
        source: String,
        mount_point: String,
        backend: Backend,
        options: ctxfs_ipc::service::MountOptions,
    ) -> Result<MountInfo, String> {
        info!("mount request: {source} -> {mount_point} (backend={backend}, prefetch={:?})", options.prefetch);
        let server = self.clone();
        tokio::task::spawn_blocking(move || server.do_mount(&source, &mount_point, backend, options))
            .await
            .map_err(|e| format!("mount task panicked: {e}"))?
    }
```

Update `do_mount`, `do_mount_nfs`, `do_mount_fskit`, `prepare_mount` signatures to take and thread `options: MountOptions` through. `prepare_mount` is the site that ultimately reads `options.prefetch` to drive the tarball decision in Task 7. For now, store it on the produced `MountPrep` so later tasks consume it:

```rust
struct MountPrep {
    source_spec: SourceSpec,
    github_source: SourceSpec,
    provider: Arc<GitHubProvider>,
    snapshot: Snapshot,
    subpath: Option<String>,
    options: ctxfs_ipc::service::MountOptions,  // NEW
}
```

(In M3 the tarball decision happens *inside* `provider.fetch_snapshot`, so `prepare_mount` will pass `options.prefetch` into a new `GitHubProvider::fetch_snapshot_with_options` overload — see Task 7. For Task 3, just plumb the value through; nothing reads it yet.)

- [ ] **Step 3: CLI flags**

In `crates/ctxfs-cli/src/main.rs`, in the `Commands::Mount` variant:

```rust
    /// Mount a remote source as a local directory
    Mount {
        // ... existing fields ...
        /// Force tarball prefetch (bypass byte cap). Mutually exclusive with --no-prefetch.
        #[arg(long, conflicts_with = "no_prefetch")]
        prefetch: bool,
        /// Disable tarball prefetch entirely; use lazy per-blob fetch.
        #[arg(long = "no-prefetch", conflicts_with = "prefetch")]
        no_prefetch: bool,
    },
```

In the dispatcher, derive a `PrefetchPolicy`:

```rust
        Commands::Mount {
            sources,
            mount_point,
            mount_dir,
            server_only,
            backend: backend_flag,
            prefetch,
            no_prefetch,
        } => {
            let policy = match (prefetch, no_prefetch) {
                (true, false) => PrefetchPolicy::Force,
                (false, true) => PrefetchPolicy::Disabled,
                (false, false) => PrefetchPolicy::Auto,
                (true, true) => unreachable!("conflicts_with prevents this"),
            };
            handle_mount(
                &config,
                sources,
                mount_point,
                mount_dir,
                server_only,
                backend_flag,
                policy,
            ).await?;
        }
```

(Add `use ctxfs_provider_common::fetcher::PrefetchPolicy;` at the top, or re-export via `ctxfs_ipc::service::PrefetchPolicy` and import that.)

`handle_mount` gains a `prefetch: PrefetchPolicy` parameter that it places into a `MountOptions` and passes to `client.mount(...)`. Find the call:

```rust
        let info = client
            .mount(
                long_context(),
                source.clone(),
                mp_str.clone(),
                selected_backend,
            )
            .await?
```

Change to:

```rust
        let info = client
            .mount(
                long_context(),
                source.clone(),
                mp_str.clone(),
                selected_backend,
                ctxfs_ipc::service::MountOptions { prefetch },
            )
            .await?
```

(There's a parallel call site in the `mount_dir` branch and in `DepsAction::Mount`; update them too. Pass `PrefetchPolicy::Auto` from `DepsAction::Mount` since `ctxfs deps mount` doesn't expose the flag in M3.)

- [ ] **Step 4: Add CLI smoke test**

In `crates/ctxfs-cli/tests/e2e.rs`, add a test that asserts the parser accepts both flags and errors on the conflict:

```rust
#[test]
fn mount_accepts_prefetch_flag() {
    let cli = Cli::try_parse_from(["ctxfs", "mount", "github:o/r@main", "-p", "/tmp/x", "--prefetch"]);
    assert!(cli.is_ok());
}

#[test]
fn mount_accepts_no_prefetch_flag() {
    let cli = Cli::try_parse_from(["ctxfs", "mount", "github:o/r@main", "-p", "/tmp/x", "--no-prefetch"]);
    assert!(cli.is_ok());
}

#[test]
fn mount_rejects_both_prefetch_flags() {
    let cli = Cli::try_parse_from(["ctxfs", "mount", "github:o/r@main", "-p", "/tmp/x", "--prefetch", "--no-prefetch"]);
    assert!(cli.is_err());
}
```

(If `Cli` isn't already exported for tests, gate behind `#[cfg(feature = "test-cli-parse")]` or move into an inline test in `main.rs`.)

- [ ] **Step 5: Update other callers**

`grep -rn "client.mount(\|\.mount(tarpc" crates/` to find all `mount` RPC call sites. Update each to pass `MountOptions::default()` if they don't have a user-provided policy.

- [ ] **Step 6: Verify**

```bash
cargo test
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

The pre-existing test failures (`mount_server_only_starts_nfs_and_reports_port`, `env_var_*`) remain expected.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-ipc/src/service.rs \
        crates/ctxfs-daemon/src/daemon.rs \
        crates/ctxfs-cli/src/main.rs

git commit -m "$(cat <<'EOF'
feat(ipc,cli): MountOptions { prefetch: PrefetchPolicy } on mount RPC

The mount tarpc method gains a MountOptions argument carrying the
user's prefetch preference. CLI flags --prefetch / --no-prefetch
(mutually exclusive) map to PrefetchPolicy::Force / Disabled; absent
both flags → Auto.

PrefetchPolicy is re-exported from provider-common's fetcher module
(landed in Task 2) so there's one definition shared by IPC, CLI, and
the provider-side gate.

The daemon plumbs options through prepare_mount → MountPrep without
yet reading it. Task 7 will route options.prefetch into the tarball
auto-gate inside GitHubProvider::fetch_snapshot.

Wire format change is additive: a missing `prefetch` field deserializes
to Auto via PrefetchPolicy::default(), so older clients keep working.
ctxfs deps mount keeps Auto in M3 (no per-deps prefetch override).
EOF
)"
```

---

## Task 4: `BlobCache` streaming + atomic + cleanup

**Files:**
- Modify: `crates/ctxfs-cache/src/lib.rs`
- Modify: `crates/ctxfs-cache/tests/lifecycle.rs`

**Goal:** make tarball hydration safe-by-construction. The tarball flow streams each entry into a temp file under `<cache_root>/tmp/`, hashing concurrently; on `finalize(expected_digest)` we verify, fsync, rename, and fsync the parent directory. `BlobCache::put` (existing direct-write) is unchanged for non-tarball callers.

**Two API entry points:**
1. `commit_atomic(digest, bytes)` — bytes-in-memory variant, used by callers that already have the data buffered.
2. `commit_atomic_with_writer() -> BlobTempWriter` — streaming variant. Caller writes incrementally, the writer hashes as it goes, and `BlobTempWriter::finalize(expected_digest)` does the verify-fsync-rename in one shot.

Plus `BlobCache::contains_all(&[Digest]) -> bool` for the singleflight fast path (Codex edit #6 sub-bullet).

- [ ] **Step 1: Add tests**

In `crates/ctxfs-cache/tests/lifecycle.rs`, add:

```rust
use ctxfs_cache::BlobCache;
use ctxfs_core::Digest;

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

#[test]
fn commit_atomic_with_writer_streams_and_verifies() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();

    let payload = b"streaming-content";
    let expected_digest = Digest::sha256(payload);

    let mut writer = cache.commit_atomic_with_writer().unwrap();
    writer.write_all(payload).unwrap();
    writer.finalize(&expected_digest).unwrap();

    assert!(cache.contains(&expected_digest));
    assert_eq!(cache.get(&expected_digest).unwrap(), payload);
}

#[test]
fn commit_atomic_with_writer_rejects_digest_mismatch() {
    use std::io::Write;
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();

    let actual = b"actual-content";
    let lying_digest = Digest::sha256(b"different-content");

    let mut writer = cache.commit_atomic_with_writer().unwrap();
    writer.write_all(actual).unwrap();
    let res = writer.finalize(&lying_digest);
    assert!(res.is_err(), "expected DigestMismatch error");
    assert!(!cache.contains(&lying_digest));
    // No leftover temp file.
    let tmp = std::fs::read_dir(dir.path().join("tmp")).map(|d| d.count()).unwrap_or(0);
    assert_eq!(tmp, 0);
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
    let _ = filetime::set_file_mtime(&old_file, filetime::FileTime::from_system_time(two_hours_ago));

    let cleared = cache.cleanup_orphan_temps(std::time::Duration::from_secs(3600)).unwrap();
    assert_eq!(cleared, 1);
    assert!(!old_file.exists());
    assert!(recent_file.exists(), "recent files preserved");
}

#[test]
fn cleanup_orphan_temps_handles_missing_dir() {
    let dir = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(dir.path().to_path_buf(), 1_000_000).unwrap();
    let cleared = cache.cleanup_orphan_temps(std::time::Duration::from_secs(3600)).unwrap();
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
```

(`filetime` is a tiny dev-dep; it's already in workspace as a transitive of tar — confirm with `cargo tree -i filetime` or add as a direct dev-dep.)

- [ ] **Step 2: Run, expect compile failure** — methods missing.

- [ ] **Step 3: Implement `commit_atomic`, `commit_atomic_with_writer`, `BlobTempWriter`, `cleanup_orphan_temps`, `contains_all`**

In `crates/ctxfs-cache/src/lib.rs`, on `impl BlobCache`:

```rust
    /// Atomically commit `data` under `digest`. Writes to a temp file under
    /// `<root>/tmp/<rand>`, fsyncs the file, renames into the canonical
    /// fan-out path, and fsyncs the parent directory so the rename is
    /// durable across crash. POSIX rename(2) is atomic — either the
    /// canonical path holds the full content or it doesn't exist; partial
    /// files only ever live in tmp/ and are cleaned by cleanup_orphan_temps.
    ///
    /// LRU bookkeeping mirrors `put`. Use this method (not `put`) for
    /// content where corruption is a real risk — bulk tarball hydration,
    /// concurrent prefetch, etc.
    pub fn commit_atomic(&self, digest: &Digest, data: &[u8]) -> Result<(), CtxfsError> {
        let mut writer = self.commit_atomic_with_writer()?;
        use std::io::Write;
        writer
            .write_all(data)
            .map_err(|e| CtxfsError::Cache(format!("commit write: {e}")))?;
        writer.finalize(digest)
    }

    /// Streaming variant: returns a writer the caller fills incrementally.
    /// `BlobTempWriter::finalize(expected_digest)` does fsync + verify +
    /// rename + parent-fsync in one shot.
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
            hasher: sha2::Sha256::new(),
            bytes_written: 0,
        })
    }

    /// Returns true iff every digest in `digests` is currently tracked in
    /// the LRU. Cheap (single mutex acquire) — used by the singleflight
    /// fast-path: if the manifest's blobs are all already cached, skip
    /// tarball entirely.
    pub fn contains_all<I, D>(&self, digests: I) -> bool
    where
        I: IntoIterator<Item = D>,
        D: AsRef<Digest>,
    {
        let state = self.state.lock().unwrap();
        digests
            .into_iter()
            .all(|d| state.entries.contains_key(&d.as_ref().hex))
    }

    /// Sweep `<root>/tmp/` of files older than `older_than` (mtime-based).
    /// Called by the daemon on startup to clear orphans from a crash
    /// mid-commit. Returns the count of files unlinked. Missing tmp/ dir
    /// → returns 0 without erroring.
    pub fn cleanup_orphan_temps(
        &self,
        older_than: std::time::Duration,
    ) -> Result<u64, CtxfsError> {
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
```

Streaming-writer struct:

```rust
/// Streaming writer returned by `BlobCache::commit_atomic_with_writer`.
/// Implements std::io::Write so callers can use std::io::copy or any
/// other reader pipeline directly. Hashes content as it's written; on
/// `finalize`, verifies against the expected digest, fsyncs, atomically
/// renames into the canonical cache path, and fsyncs the parent dir.
///
/// Drop without finalize → temp file is cleaned via NamedTempFile's RAII.
pub struct BlobTempWriter<'a> {
    cache_root: PathBuf,
    cache: &'a BlobCache,
    temp: Option<tempfile::NamedTempFile>,
    hasher: sha2::Sha256,
    bytes_written: u64,
}

impl std::io::Write for BlobTempWriter<'_> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        use sha2::Digest as _;
        let f = self.temp.as_mut().expect("writer used after finalize");
        let n = f.as_file_mut().write(buf)?;
        self.hasher.update(&buf[..n]);
        self.bytes_written += n as u64;
        Ok(n)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        let f = self.temp.as_mut().expect("writer used after finalize");
        f.as_file_mut().flush()
    }
}

impl BlobTempWriter<'_> {
    /// Verify SHA-256 against `expected`, fsync the temp file, rename(2)
    /// into the canonical path, fsync the parent directory.
    pub fn finalize(mut self, expected: &Digest) -> Result<(), CtxfsError> {
        use sha2::Digest as _;
        let actual_hex = hex::encode(self.hasher.clone().finalize());
        if actual_hex != expected.hex {
            // Temp file unlinks on Drop via NamedTempFile.
            return Err(CtxfsError::Cache(format!(
                "blob digest mismatch: expected {} got {}",
                expected.hex, actual_hex
            )));
        }

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
        if let Some(parent) = dest.parent() {
            // Best-effort: parent fsync isn't supported on every fs, so a
            // failure is logged but not fatal.
            if let Ok(d) = std::fs::File::open(parent) {
                if let Err(e) = d.sync_all() {
                    tracing::debug!(target: "ctxfs.cache.atomic", path = %parent.display(), error = ?e, "parent dir fsync failed (non-fatal)");
                }
            }
        }

        // LRU bookkeeping (post-rename so file is durable before tracking).
        let size = self.bytes_written;
        let key = expected.hex.clone();
        let mut evicted = Vec::new();
        {
            let mut state = self.cache.state.lock().unwrap();
            if let Some(existing) = state.entries.get(&key) {
                state.total_bytes -= existing.size;
            }
            let _ = state.entries.insert(key, CacheEntry { size });
            state.total_bytes += size;
            let limit = self.cache.max_bytes.load(Ordering::Relaxed);
            while state.total_bytes > limit && !state.entries.is_empty() {
                if let Some(entry) = state.evict_oldest() {
                    evicted.push(entry);
                }
            }
        }
        // Suppress unused warning on cache_root (only used implicitly via
        // self.cache.blob_path); kept as a struct field for future
        // extensions (e.g., reservation aware paths in M5).
        let _ = self.cache_root;
        for (k, _) in evicted {
            self.cache.remove_blob_file(&k);
        }
        Ok(())
    }
}
```

**`rebuild_index` skip `tmp/`** (Codex other-call):

In `BlobCache::rebuild_index` (the existing scan loop that reads `self.root`), add a guard before recursing into a subdirectory:

```rust
        if let Ok(algo_dirs) = fs::read_dir(&self.root) {
            for algo_entry in algo_dirs.flatten() {
                let algo_path = algo_entry.path();
                if !algo_path.is_dir() {
                    continue;
                }
                // M3: skip tmp/ — partial blobs from in-flight commits never
                // belong in the LRU. cleanup_orphan_temps removes stale ones.
                if algo_path.file_name().map_or(false, |n| n == "tmp") {
                    continue;
                }
                Self::scan_fan_out_dir(&algo_path, &mut entries)?;
            }
        }
```

Add `tempfile = { workspace = true }` to `crates/ctxfs-cache/Cargo.toml` `[dependencies]` (it's already a dev-dep — bump to direct).

Add `filetime = "0.2"` to root `[workspace.dependencies]` and `crates/ctxfs-cache/Cargo.toml` `[dev-dependencies]`.

- [ ] **Step 4: Daemon-restart cleanup wire-up (single line)**

In `crates/ctxfs-daemon/src/daemon.rs`, in `Daemon::new` (or wherever the BlobCache is constructed), after construction:

```rust
        let cleared = cache
            .cleanup_orphan_temps(std::time::Duration::from_secs(3600))
            .unwrap_or_else(|e| {
                tracing::warn!("cleanup_orphan_temps failed: {e}");
                0
            });
        if cleared > 0 {
            tracing::info!("cleared {cleared} orphan temp blob(s) from previous run");
        }
```

- [ ] **Step 5: Verify**

```bash
cargo test -p ctxfs-cache
cargo test -p ctxfs-daemon
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml \
        crates/ctxfs-cache/Cargo.toml \
        crates/ctxfs-cache/src/lib.rs \
        crates/ctxfs-cache/tests/lifecycle.rs \
        crates/ctxfs-daemon/src/daemon.rs

git commit -m "$(cat <<'EOF'
feat(cache): BlobCache atomic-commit + streaming writer + temp cleanup

Atomic temp-and-rename writes for content where corruption is a real
risk. Two API entry points:

- commit_atomic(digest, bytes) — bytes-in-memory variant.
- commit_atomic_with_writer() -> BlobTempWriter — streaming variant.
  Implements std::io::Write so std::io::copy can pipe a reader straight
  in. Hashes content as it's written; finalize(expected_digest) does
  fsync + verify + rename + parent-dir fsync in one shot.

The streaming variant is what M3 Task 6's tarball flow uses — it pipes
each tar::Archive entry through SyncIoBridge into BlobTempWriter so
memory ceiling stays per-entry, never per-archive. The bytes-in-memory
variant is for non-streaming callers (small-blob prefetch fallback).

Hardening notes:
- Parent directory fsync after rename — without this, the rename can
  be lost across crash on some filesystems even though the rename
  syscall returned. Best-effort only (some fs don't support it).
- rebuild_index skips tmp/ — partial blobs from in-flight commits
  never enter the LRU. cleanup_orphan_temps removes stale tmp/ files
  older than 1 hour (default) on daemon startup. The daemon calls it
  once at boot in Daemon::new.

contains_all(&[Digest]) is the singleflight fast path: if every
manifest blob is already cached, M3 Task 7 skips the tarball
entirely. Cheap — single LRU mutex acquire.

put() is left untouched. Non-tarball callers (manifest serialization,
small-blob prefetch fallback) keep their direct-write semantics.

Foundation for M3 Task 6 (streaming tarball hydration).
EOF
)"
```

---

## Task 5: B2 truncated-tree per-directory walk

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`

**Goal:** when GitHub returns `truncated == true` (>100k entries / >7 MB), walk the tree per-directory non-recursively to assemble a complete manifest. Without this fallback, the auto-gate's `blob_count` and `estimated_bytes` are wrong on large repos and the gate can't make correct decisions.

- [ ] **Step 1: Add a unit test for the assembly logic**

In `github.rs` `tests` module, with a stub `fetch_subtree` that returns deterministic responses:

```rust
    /// Stub trait so the assembly logic can be unit-tested without
    /// hitting the real GitHub API. Production wiring uses GitHubProvider
    /// directly.
    trait SubtreeStub {
        fn get(&self, sha: &str) -> Vec<TreeEntry>;
    }

    impl SubtreeStub for std::collections::HashMap<&'static str, Vec<TreeEntry>> {
        fn get(&self, sha: &str) -> Vec<TreeEntry> {
            self.get(sha).cloned().unwrap_or_default()
        }
    }

    #[test]
    fn assemble_walked_tree_recurses_directories() {
        let mut subtrees: std::collections::HashMap<&'static str, Vec<TreeEntry>> = std::collections::HashMap::new();
        // Root: one file + one subdir.
        let _ = subtrees.insert("root_sha", vec![
            TreeEntry { path: "README.md".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "blob_a".into(), size: Some(100) },
            TreeEntry { path: "src".into(), mode: "040000".into(), entry_type: "tree".into(), sha: "src_sha".into(), size: None },
        ]);
        // src: one file.
        let _ = subtrees.insert("src_sha", vec![
            TreeEntry { path: "lib.rs".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "blob_b".into(), size: Some(200) },
        ]);

        let assembled = GitHubProvider::assemble_walked_tree("root_sha", &subtrees);

        // Expect path-prefixed entries: README.md, src, src/lib.rs
        assert_eq!(assembled.len(), 3);
        assert!(assembled.iter().any(|e| e.path == "README.md" && e.sha == "blob_a"));
        assert!(assembled.iter().any(|e| e.path == "src" && e.entry_type == "tree"));
        assert!(assembled.iter().any(|e| e.path == "src/lib.rs" && e.sha == "blob_b"));
    }

    #[test]
    fn assemble_walked_tree_handles_deep_nesting() {
        let mut subtrees: std::collections::HashMap<&'static str, Vec<TreeEntry>> = std::collections::HashMap::new();
        let _ = subtrees.insert("a", vec![
            TreeEntry { path: "b".into(), mode: "040000".into(), entry_type: "tree".into(), sha: "b".into(), size: None },
        ]);
        let _ = subtrees.insert("b", vec![
            TreeEntry { path: "c".into(), mode: "040000".into(), entry_type: "tree".into(), sha: "c".into(), size: None },
        ]);
        let _ = subtrees.insert("c", vec![
            TreeEntry { path: "deep.txt".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "deep_blob".into(), size: Some(7) },
        ]);
        let assembled = GitHubProvider::assemble_walked_tree("a", &subtrees);
        assert!(assembled.iter().any(|e| e.path == "b/c/deep.txt"));
    }
```

- [ ] **Step 2: Run, expect compile failure** — `assemble_walked_tree` missing.

- [ ] **Step 3: Implement `assemble_walked_tree` (pure function — testable without HTTP)**

```rust
    /// Walk a tree by recursive descent over per-subtree responses, returning
    /// the entries flattened with path-prefixes (matching the recursive=1
    /// response shape). Pure: takes a callable `subtrees` that returns the
    /// entries for a given subtree SHA. The HTTP-bound caller passes a
    /// closure that performs the GitHub API call.
    ///
    /// Iterative DFS instead of recursion — bounded stack on adversarial
    /// repos (e.g., a maliciously deep symlink chain from B7).
    fn assemble_walked_tree<S: SubtreeStub>(
        root_sha: &str,
        subtrees: &S,
    ) -> Vec<TreeEntry> {
        let mut out = Vec::new();
        let mut stack: Vec<(String, String)> = Vec::new(); // (sha, path_prefix)
        stack.push((root_sha.to_string(), String::new()));

        while let Some((sha, prefix)) = stack.pop() {
            for entry in subtrees.get(&sha) {
                let prefixed_path = if prefix.is_empty() {
                    entry.path.clone()
                } else {
                    format!("{prefix}/{}", entry.path)
                };
                let mut owned = entry.clone();
                owned.path = prefixed_path.clone();
                if entry.entry_type == "tree" {
                    stack.push((entry.sha.clone(), prefixed_path));
                }
                out.push(owned);
            }
        }
        out
    }
```

(For test compatibility, define `SubtreeStub` as a non-test trait inside the `impl` block — or move the trait into the test module and have the production path use a closure-based helper. The cleaner route is closure-based:)

Actually use a closure-based version for production and write a small adapter in tests:

```rust
    fn assemble_walked_tree<F>(root_sha: &str, mut get_subtree: F) -> Vec<TreeEntry>
    where
        F: FnMut(&str) -> Vec<TreeEntry>,
    {
        let mut out = Vec::new();
        let mut stack: Vec<(String, String)> = vec![(root_sha.to_string(), String::new())];
        while let Some((sha, prefix)) = stack.pop() {
            for entry in get_subtree(&sha) {
                let prefixed = if prefix.is_empty() {
                    entry.path.clone()
                } else {
                    format!("{prefix}/{}", entry.path)
                };
                let mut owned = entry.clone();
                owned.path = prefixed.clone();
                if entry.entry_type == "tree" {
                    stack.push((entry.sha.clone(), prefixed));
                }
                out.push(owned);
            }
        }
        out
    }
```

(Test calls it with a closure that reads from the `HashMap`.)

- [ ] **Step 4: Async wiring on `GitHubProvider`**

```rust
    /// Fetch a single tree (no recursion). Used by the B2 fallback path.
    async fn fetch_subtree(
        &self,
        source: &SourceSpec,
        tree_sha: &str,
    ) -> Result<Vec<TreeEntry>, CtxfsError> {
        let (owner, repo) = owner_repo(source)?;
        let url = Self::api_url(owner, repo, &format!("git/trees/{tree_sha}"));
        let tree: TreeResponse = self.get_json(&url, "fetch subtree").await?;
        Ok(tree.tree)
    }

    /// B2 fallback: when fetch_tree returns truncated=true, walk per-directory
    /// to assemble a complete manifest. Increments truncated_tree_fallbacks
    /// counter once per fallback fire.
    async fn fetch_tree_walked(
        &self,
        source: &SourceSpec,
        root_tree_sha: &str,
    ) -> Result<Vec<TreeEntry>, CtxfsError> {
        if let Some(key) = self.counter_key.lock().unwrap().clone() {
            self.observability
                .counters_for(key)
                .record_truncated_tree_fallback();
        }
        tracing::warn!(
            target: "ctxfs.provider.tree",
            root_sha = root_tree_sha,
            "tree response was truncated; falling back to per-directory walk"
        );

        // Iterative DFS; one HTTP call per subtree.
        let mut out = Vec::new();
        let mut stack: Vec<(String, String)> = vec![(root_tree_sha.to_string(), String::new())];
        while let Some((sha, prefix)) = stack.pop() {
            let entries = self.fetch_subtree(source, &sha).await?;
            for entry in entries {
                let prefixed = if prefix.is_empty() {
                    entry.path.clone()
                } else {
                    format!("{prefix}/{}", entry.path)
                };
                let mut owned = entry.clone();
                owned.path = prefixed.clone();
                if entry.entry_type == "tree" {
                    stack.push((entry.sha.clone(), prefixed));
                }
                out.push(owned);
            }
        }
        Ok(out)
    }
```

- [ ] **Step 5: Integrate into `fetch_snapshot`**

In `fetch_snapshot`, replace `let tree = self.fetch_tree(source, &commit_sha).await?;` block with:

```rust
        // 4. Fetch tree.
        let mut tree = self.fetch_tree(source, &commit_sha).await?;
        if tree.truncated {
            // B2 fallback: per-directory walk produces a complete manifest.
            // Walk from the actual root tree SHA returned by the API, NOT
            // from commit_sha — those are different objects in git
            // (a commit and the tree it points to). The recursive=1 call
            // returns `sha` set to the root tree SHA we should walk.
            // (Codex M3-plan-v1 review #8.)
            let walked = self.fetch_tree_walked(source, &tree.sha).await?;
            tree = TreeResponse {
                sha: tree.sha.clone(),
                tree: walked,
                truncated: false,
            };
        }
        debug!("fetched tree with {} entries", tree.tree.len());
```

Make `TreeResponse.sha` no longer `#[allow(dead_code)]` (we now read it). Drop the attribute.

**Missing-size policy** (Codex edit #8 sub-bullet): downstream gate uses `e.size.unwrap_or(0)` on blob entries. With M3 the auto-gate must NOT fire on a tree with any unknown sizes — undercounting `estimated_bytes` would let an oversized repo slip past the cap. The gate code lives in Task 7's `dispatch_fetch_policy`; record the policy here as a comment in `fetch_tree_walked`:

```rust
        // Note: per-subtree responses include `size` for blob entries (per
        // GitHub Trees API docs). If an entry returns size=None, the auto-gate
        // in dispatch_fetch_policy treats the manifest as having unknown
        // total bytes and falls back to Lazy (fail-closed). Don't try to
        // estimate by file extension or cache mtimes — be honest about the
        // missing signal.
```

- [ ] **Step 6: Verify**

```bash
cargo test -p ctxfs-provider-git assemble_walked_tree
cargo test -p ctxfs-provider-git
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-provider-git/src/github.rs

git commit -m "$(cat <<'EOF'
fix(provider-git,B2): truncated-tree per-directory walk fallback

When GitHub's /git/trees/{sha}?recursive=1 response sets truncated=true
(repos > 100k entries or > 7 MB), the recursive response is incomplete.
Pre-M3, fetch_snapshot logged a warning and proceeded with the partial
tree — the resulting manifest silently dropped entries.

Auto-gate (M3 Task 6) needs an accurate blob_count and estimated_bytes
to decide tarball vs lazy. With a partial tree those signals lie, so
the gate would either misfire (skip a tarball that's profitable) or
fail-closed (lazy mode the user sees as slow). B2 makes the gate's
inputs reliable.

fetch_tree_walked is iterative DFS over /git/trees/{sha} (no
recursive=1) per subtree. assemble_walked_tree is the pure-function
path-prefixing variant unit-tested with a closure-injected stub —
no HTTP needed in tests.

Cost: N additional REST calls where N = subtree count. On a repo
hitting the truncation threshold this is the order of dozens, not
thousands; the alternative of fetching the recursive tree N times
in chunks isn't an option (no GitHub pagination on /git/trees).

Counter truncated_tree_fallbacks increments once per fallback fire.
EOF
)"
```

---

## Task 6: Streaming tarball endpoint integration with hardening

**Files:**
- Modify: `Cargo.toml` (workspace) — add `tar`, `sha1`, `flate2`; ensure `reqwest` has `stream` feature; `tokio-util` gets `io` feature
- Modify: `crates/ctxfs-provider-git/Cargo.toml` — add `tar`, `sha1`, `flate2`, `tokio-util`
- Modify: `crates/ctxfs-provider-git/src/github.rs`

**Goal:** ship the tarball download path with all the hardening the spec lists, **streaming end-to-end** (Codex edit #1). NO auto-gate logic yet (Task 7) — this task just makes the tarball *callable* and verifies it works via integration tests. The function is called `fetch_tarball_into_cache` and returns `Result<TarballOutcome, CtxfsError>` where outcome carries `(blobs_committed, blobs_skipped_invalid, blobs_skipped_digest, total_bytes)`.

Key Codex edits applied here:
- **Disable reqwest auto-redirects** (#2): provider client built with `redirect::Policy::none()`. Otherwise reqwest auto-follows the 302 with Authorization attached, defeating the strip.
- **`api_host` plumbed via constructor** (#3): `GitHubProvider::new` adds `api_host: String`. Used uniformly in `api_url`, `AuthIdentity::pat/anonymous`, and `validate_redirect_target`. No `Config::load()` inside hot path.
- **Streaming via `bytes_stream() → StreamReader → SyncIoBridge`** (#1): `flate2 + tar::Archive` runs inside `tokio::task::spawn_blocking`. Each entry pipes through `SyncIoBridge` into `BlobCache::commit_atomic_with_writer` — memory stays per-entry.
- **Path-validation type-aware** (#9): `validate_tar_entry_path(raw, entry_type)` distinguishes "directory wrapper" (no slash, return empty) from "regular file" (no slash, reject).
- **Strict UTF-8 entry paths** (#9): use `entry.path_bytes()` and reject non-UTF-8 instead of `to_string_lossy`.

- [ ] **Step 1: Workspace deps**

In root `Cargo.toml` `[workspace.dependencies]`:

```toml
tar = "0.4"
flate2 = "1"
sha1 = "0.10"
```

Update `reqwest` to ensure `stream` feature:

```toml
reqwest = { version = "0.12", default-features = false, features = ["rustls-tls", "json", "stream"] }
```

Update `tokio-util` to ensure `io` feature (needed for `StreamReader`/`SyncIoBridge`):

```toml
tokio-util = { version = "0.7", features = ["codec", "io"] }
```

In `crates/ctxfs-provider-git/Cargo.toml` `[dependencies]`:

```toml
tar = { workspace = true }
flate2 = { workspace = true }
sha1 = { workspace = true }
tokio-util = { workspace = true }
```

- [ ] **Step 2: Path validation tests**

In `github.rs` `tests` module. Path validation takes the entry's tar `EntryType` so it can distinguish "wrapper directory" from "stray file at archive root":

```rust
    use tar::EntryType;

    fn pv(raw: &str, et: EntryType) -> Result<std::path::PathBuf, CtxfsError> {
        GitHubProvider::validate_tar_entry_path(raw.as_bytes(), et)
    }

    #[test]
    fn validate_tar_entry_path_strips_top_level_prefix_for_files() {
        let p = pv("owner-repo-abc123/src/lib.rs", EntryType::Regular).unwrap();
        assert_eq!(p, std::path::PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn validate_tar_entry_path_accepts_wrapper_dir_only_for_directories() {
        // The codeload wrapper dir itself appears as a directory entry.
        // Returning empty PathBuf signals "skip — this is the wrapper".
        let dir = pv("owner-repo-abc/", EntryType::Directory).unwrap();
        assert_eq!(dir, std::path::PathBuf::new());

        // The same string with a regular-file entry is malformed.
        assert!(pv("owner-repo-abc/", EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_no_slash_regular_file() {
        // codeload always wraps; a regular file at the archive root is malformed.
        assert!(pv("README.md", EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_dotdot() {
        assert!(pv("owner-repo-abc/../escape", EntryType::Regular).is_err());
        assert!(pv("owner-repo-abc/sub/../../escape", EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_absolute() {
        assert!(pv("/etc/passwd", EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_nul_and_control() {
        assert!(pv("owner-repo-abc/foo\0bar", EntryType::Regular).is_err());
        assert!(pv("owner-repo-abc/foo\x01bar", EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_invalid_utf8() {
        // Raw bytes (not str) are passed in so we can prove the rejection.
        let mut bytes = Vec::from(b"owner-repo-abc/".as_slice());
        bytes.push(0xFFu8);
        bytes.extend_from_slice(b".rs");
        assert!(GitHubProvider::validate_tar_entry_path(&bytes, EntryType::Regular).is_err());
    }

    #[test]
    fn redirect_url_validates_codeload_only() {
        // Default github_host="api.github.com" → expected codeload host = "codeload.github.com"
        let cfg_host = "api.github.com";
        assert!(GitHubProvider::validate_redirect_target(
            "https://codeload.github.com/owner/repo/tar.gz/abc",
            cfg_host
        ).is_ok());
        assert!(GitHubProvider::validate_redirect_target(
            "https://attacker.example.com/foo",
            cfg_host
        ).is_err());
        assert!(GitHubProvider::validate_redirect_target(
            "http://codeload.github.com/foo",
            cfg_host
        ).is_err(), "http rejected even on codeload");
    }
```

- [ ] **Step 3: Implement path + redirect validators (pure)**

```rust
    /// Maximum allowed redirect chain depth when following the tarball 302.
    const TARBALL_REDIRECT_MAX_DEPTH: u8 = 3;

    /// Derive the codeload host name from `api_host`. For api.github.com →
    /// codeload.github.com. For GHE deployments the convention is `codeload.<host>`.
    pub(crate) fn codeload_host_for(api_host: &str) -> String {
        if api_host == "api.github.com" {
            "codeload.github.com".to_string()
        } else {
            format!("codeload.{api_host}")
        }
    }

    /// Validate a 302 Location target. Required: scheme=https, host equals the
    /// codeload host derived from configured api_host.
    pub(crate) fn validate_redirect_target(
        location: &str,
        api_host: &str,
    ) -> Result<reqwest::Url, CtxfsError> {
        let url = reqwest::Url::parse(location)
            .map_err(|e| CtxfsError::Provider(format!("invalid redirect URL: {e}")))?;
        if url.scheme() != "https" {
            return Err(CtxfsError::Provider(format!(
                "tarball redirect rejected: scheme={} is not https",
                url.scheme()
            )));
        }
        let expected_host = Self::codeload_host_for(api_host);
        let actual_host = url.host_str().unwrap_or("");
        if actual_host != expected_host {
            return Err(CtxfsError::Provider(format!(
                "tarball redirect rejected: host {actual_host} is not codeload host {expected_host}"
            )));
        }
        Ok(url)
    }

    /// Validate a tarball entry's path. Takes raw bytes (not str) so invalid
    /// UTF-8 cannot be silently rewritten by `to_string_lossy`. Takes the
    /// `tar::EntryType` so we can distinguish "wrapper directory at root"
    /// from "stray regular file at root" (both have no slash; only the
    /// directory case is benign).
    ///
    /// Rules:
    /// - reject invalid UTF-8 (we need to put this on disk under a UTF-8 path)
    /// - reject leading `/` (absolute)
    /// - reject NUL or any control char < 0x20
    /// - reject `..` segments anywhere
    /// - require entry to begin with the codeload top-level wrapper dir
    ///   (e.g., "owner-repo-sha/"); strip it and return the remainder
    /// - no-slash + Directory → wrapper itself; return empty PathBuf (skip)
    /// - no-slash + Regular → reject (codeload always wraps)
    pub(crate) fn validate_tar_entry_path(
        raw: &[u8],
        entry_type: tar::EntryType,
    ) -> Result<std::path::PathBuf, CtxfsError> {
        let s = std::str::from_utf8(raw)
            .map_err(|_| CtxfsError::Provider("tar entry path is not UTF-8".into()))?;
        if s.contains('\0') {
            return Err(CtxfsError::Provider("tar entry contains NUL".into()));
        }
        if s.starts_with('/') {
            return Err(CtxfsError::Provider(format!("tar entry is absolute: {s}")));
        }
        if s.chars().any(|c| (c as u32) < 0x20) {
            return Err(CtxfsError::Provider(format!("tar entry has control chars: {s}")));
        }

        let cleaned = match s.split_once('/') {
            Some((_wrapper, rest)) => rest,
            None => {
                // No '/': only a directory entry (the wrapper) is acceptable.
                return match entry_type {
                    tar::EntryType::Directory => Ok(std::path::PathBuf::new()),
                    _ => Err(CtxfsError::Provider(format!(
                        "tar entry not under wrapper dir: {s}"
                    ))),
                };
            }
        };

        for seg in cleaned.split('/') {
            if seg == ".." {
                return Err(CtxfsError::Provider(format!("tar entry contains ..: {s}")));
            }
        }
        Ok(std::path::PathBuf::from(cleaned))
    }
```

- [ ] **Step 4: Streaming tarball extraction (the meat)**

The flow:
1. `client.get(api_url).send()` — initial 302 (redirect::Policy::none on the client means reqwest does NOT auto-follow).
2. Manual redirect: parse Location, validate codeload host, fresh `Client` (no Authorization), follow up to depth 3.
3. Final response body via `bytes_stream() → StreamReader` (async Read).
4. Wrap in `SyncIoBridge` so the sync `flate2::GzDecoder + tar::Archive` can consume it.
5. Run the sync extraction inside `spawn_blocking` so we don't block the tokio runtime.
6. For each entry: path-validate, then pipe content through `BlobTempWriter` (which hashes incrementally) — call `finalize(expected_digest)` to commit.

The provider's reqwest client is built with redirect disabled in `GitHubProvider::new`:

```rust
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT_STR)
            .default_headers(default_headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build HTTP client");
```

This change applies to **all** REST calls — non-tarball callers will now see 302s as raw responses. Audit `get_json` to ensure it errors on redirect (today it relies on auto-follow). Acceptable: GitHub's REST endpoints don't redirect except for `/tarball`, `/zipball`, `/codeload` (and a few archive variants we don't use). The `get_json` path returns a clean Provider error on a 3xx — which is what we want for unhandled redirects anyway.

```rust
    /// Outcome of one tarball extraction. Returned to the caller for telemetry
    /// + auto-gate-fallback decisions.
    #[derive(Debug, Default)]
    pub(crate) struct TarballOutcome {
        pub blobs_committed: u64,
        pub blobs_skipped_invalid: u64,
        pub blobs_skipped_digest: u64,
        pub total_bytes: u64,
    }

    /// Compute the Git blob SHA-1: sha1("blob " + len + "\0" + content).
    /// Streaming variant — feed bytes via `update`, finalize to hex.
    /// (Used inside the per-entry tar pipeline so we don't buffer.)
    pub(crate) struct GitBlobSha1 {
        h: sha1::Sha1,
        size_written: u64,
        size_header_emitted: bool,
        expected_size: u64,
    }
    impl GitBlobSha1 {
        pub fn new(expected_size: u64) -> Self {
            use sha1::Digest as _;
            let h = sha1::Sha1::new();
            Self { h, size_written: 0, size_header_emitted: false, expected_size }
        }
        pub fn update(&mut self, bytes: &[u8]) {
            use sha1::Digest as _;
            if !self.size_header_emitted {
                self.h.update(format!("blob {}", self.expected_size).as_bytes());
                self.h.update(b"\0");
                self.size_header_emitted = true;
            }
            self.h.update(bytes);
            self.size_written += bytes.len() as u64;
        }
        pub fn finalize_hex(self) -> String {
            use sha1::Digest as _;
            hex::encode(self.h.finalize())
        }
    }

    /// Resolve a mount-relative path → (expected blob SHA, expected size)
    /// from the tree manifest. The size is needed for the streaming SHA-1
    /// (Git blob hash includes the size header before content).
    fn build_path_to_sha_size(
        entries: &[TreeEntry],
    ) -> std::collections::HashMap<std::path::PathBuf, (String, u64)> {
        entries
            .iter()
            .filter(|e| e.entry_type == "blob")
            .map(|e| (std::path::PathBuf::from(&e.path), (e.sha.clone(), e.size.unwrap_or(0))))
            .collect()
    }

    /// Download /repos/{o}/{r}/tarball/{ref}, follow the codeload 302
    /// (with security checks), stream-extract via tar::Archive, and commit
    /// each blob atomically into BlobCache.
    ///
    /// Streaming end-to-end:
    /// - reqwest body → bytes_stream → StreamReader (async Read)
    /// - SyncIoBridge → sync Read for tar::Archive
    /// - tar::Archive runs inside spawn_blocking
    /// - Each entry pipes through BlobTempWriter (hashes incrementally,
    ///   enforces digest, atomic rename)
    ///
    /// Memory ceiling: per-entry only. Force-mode tarballs that exceed the
    /// cache budget will commit blobs successfully (each one fits in cache
    /// briefly) and the cache LRU may evict earlier-committed entries —
    /// that's the documented "warm-cache guarantee will not hold" warning.
    async fn fetch_tarball_into_cache(
        &self,
        source: &SourceSpec,
        commit_sha: &str,
        tree_entries: &[TreeEntry],
    ) -> Result<TarballOutcome, CtxfsError> {
        let (owner, repo) = owner_repo(source)?;

        // 1. Initial API call. check_rate_limit ticks rest_calls_total.
        let api_url = self.api_url(owner, repo, &format!("tarball/{commit_sha}"));
        let initial_resp = self
            .client
            .get(&api_url)
            .send()
            .await
            .map_err(|e| CtxfsError::Provider(format!("HTTP error tarball: {e}")))?;
        self.check_rate_limit(&initial_resp)?;

        // 2. Manual redirect chain. Authorization is dropped on hop 1+.
        //    Provider's main client has redirect::Policy::none(), so we control
        //    the chain explicitly.
        let codeload_client = reqwest::Client::builder()
            .user_agent(USER_AGENT_STR)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|e| CtxfsError::Provider(format!("codeload client build: {e}")))?;
        let mut current = initial_resp;
        let mut depth = 0u8;
        let final_resp = loop {
            if !current.status().is_redirection() {
                break current;
            }
            if depth >= Self::TARBALL_REDIRECT_MAX_DEPTH {
                return Err(CtxfsError::Provider(format!(
                    "tarball redirect chain exceeded depth {}",
                    Self::TARBALL_REDIRECT_MAX_DEPTH
                )));
            }
            let location = current
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| CtxfsError::Provider("redirect without Location".into()))?
                .to_string();
            let next_url = Self::validate_redirect_target(&location, &self.api_host)?;
            current = codeload_client
                .get(next_url)
                .send()
                .await
                .map_err(|e| CtxfsError::Provider(format!("codeload GET: {e}")))?;
            depth += 1;
        };

        if !final_resp.status().is_success() {
            return Err(CtxfsError::Provider(format!(
                "tarball download failed: HTTP {}",
                final_resp.status()
            )));
        }

        // 3. Build the streaming pipeline.
        //    bytes_stream is a futures::Stream<Item = Result<Bytes, reqwest::Error>>.
        //    StreamReader makes it AsyncRead. SyncIoBridge gives us blocking Read
        //    inside the spawn_blocking thread.
        use tokio_util::io::{StreamReader, SyncIoBridge};
        let body_stream = final_resp
            .bytes_stream()
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e));
        let async_reader = StreamReader::new(body_stream);

        let path_to_sha_size = Self::build_path_to_sha_size(tree_entries);
        let cache = self.cache.clone();
        let counter_key = self.counter_key.lock().unwrap().clone();
        let observability = self.observability.clone();

        // 4. Run the sync tar+gz extraction in spawn_blocking. Per-entry work
        //    streams: tar::Entry impls Read, we copy through GitBlobSha1 +
        //    BlobTempWriter (also Write).
        let outcome = tokio::task::spawn_blocking(move || -> Result<TarballOutcome, CtxfsError> {
            let sync_reader = SyncIoBridge::new(async_reader);
            let gz = flate2::read::GzDecoder::new(sync_reader);
            let mut archive = tar::Archive::new(gz);
            let mut outcome = TarballOutcome::default();

            for entry_result in archive
                .entries()
                .map_err(|e| CtxfsError::Provider(format!("tar entries iter: {e}")))?
            {
                let mut entry = entry_result
                    .map_err(|e| CtxfsError::Provider(format!("tar entry: {e}")))?;
                let header = entry.header().clone();
                let raw_bytes = entry
                    .path_bytes()
                    .into_owned();
                let entry_type = header.entry_type();

                // Path validation. Failure → counter + skip.
                let mount_path = match Self::validate_tar_entry_path(&raw_bytes, entry_type) {
                    Ok(p) => p,
                    Err(e) => {
                        outcome.blobs_skipped_invalid += 1;
                        if let Some(ref key) = counter_key {
                            observability
                                .counters_for(key.clone())
                                .record_tarball_invalid_entries();
                        }
                        tracing::warn!(
                            target: "ctxfs.provider.tarball",
                            path = String::from_utf8_lossy(&raw_bytes).as_ref(),
                            error = format!("{e:?}").as_str(),
                            "tarball entry rejected"
                        );
                        continue;
                    }
                };

                // Skip non-regular entries (Directory, Symlink, etc. handled via manifest).
                if entry_type != tar::EntryType::Regular {
                    continue;
                }
                if mount_path.as_os_str().is_empty() {
                    continue;
                }

                // Look up expected (sha, size). If manifest has no entry for
                // this path, skip — we cannot verify orphaned tar entries.
                let (expected_sha, expected_size) = match path_to_sha_size.get(&mount_path) {
                    Some(t) => t.clone(),
                    None => continue,
                };

                // Pipe entry → hasher + writer in one std::io::copy.
                let mut hasher = GitBlobSha1::new(expected_size);
                let mut writer = cache.commit_atomic_with_writer()?;

                struct Tee<'a, W: std::io::Write> {
                    hasher: &'a mut GitBlobSha1,
                    writer: &'a mut W,
                }
                impl<W: std::io::Write> std::io::Write for Tee<'_, W> {
                    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                        let n = self.writer.write(buf)?;
                        self.hasher.update(&buf[..n]);
                        Ok(n)
                    }
                    fn flush(&mut self) -> std::io::Result<()> {
                        self.writer.flush()
                    }
                }
                let mut tee = Tee { hasher: &mut hasher, writer: &mut writer };
                std::io::copy(&mut entry, &mut tee)
                    .map_err(|e| CtxfsError::Provider(format!("tar entry stream: {e}")))?;

                // Verify SHA-1 against manifest, then commit (or discard).
                let actual_sha = hasher.finalize_hex();
                if actual_sha != expected_sha {
                    outcome.blobs_skipped_digest += 1;
                    if let Some(ref key) = counter_key {
                        observability
                            .counters_for(key.clone())
                            .record_tarball_digest_mismatch();
                    }
                    // Drop the writer without finalize — NamedTempFile RAII unlinks.
                    drop(writer);
                    tracing::warn!(
                        target: "ctxfs.provider.tarball",
                        path = mount_path.display().to_string().as_str(),
                        expected = expected_sha.as_str(),
                        actual = actual_sha.as_str(),
                        "tarball blob SHA-1 mismatch; falling back to lazy"
                    );
                    continue;
                }

                // Manifest stores Git blob SHA-1 in the digest hex; the cache
                // is keyed by the same hex via Digest::from_sha256_hex (hex is
                // hex; the field name is the M5 B3-label rename).
                let digest = Digest::from_sha256_hex(&expected_sha);
                writer.finalize(&digest)?;

                outcome.blobs_committed += 1;
                outcome.total_bytes += expected_size;
            }
            Ok(outcome)
        })
        .await
        .map_err(|e| CtxfsError::Provider(format!("spawn_blocking join: {e}")))??;

        Ok(outcome)
    }
```

**Caveat on `BlobTempWriter::finalize` digest type:** the cache today keys all blobs by `Digest::from_sha256_hex(...)`, but GitHub blob SHAs are SHA-1 (B3-label). The `Digest` field's `algo` is currently mislabeled; M5's B3-label rewrite renames `algo` to support both. M3 accepts the mislabel and uses the existing helper — this is the same trade-off M2 made.

- [ ] **Step 5: Verify**

```bash
cargo build -p ctxfs-provider-git
cargo test -p ctxfs-provider-git
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml \
        crates/ctxfs-provider-git/Cargo.toml \
        crates/ctxfs-provider-git/src/github.rs

git commit -m "$(cat <<'EOF'
feat(provider-git): streaming tarball download with hardening (no auto-gate yet)

fetch_tarball_into_cache hits /repos/{o}/{r}/tarball/{ref}, follows the
302 manually with security checks, stream-extracts entries through
SyncIoBridge → flate2::GzDecoder → tar::Archive (sync; runs inside
spawn_blocking), pipes each entry through GitBlobSha1 (incremental
hasher) AND BlobCache::commit_atomic_with_writer (streaming writer),
and verifies the SHA-1 on finalize. Auto-gate dispatch (Task 7) is
the next task; this one is pure plumbing.

Streaming end-to-end: memory ceiling is per-entry, never per-archive.
Force-mode tarballs that exceed the cache budget commit successfully
but trigger LRU evictions of earlier entries — that's the documented
"warm-cache guarantee will not hold" warning. (Codex M3-plan-v1 #1.)

Hardening lines:
- Provider's reqwest::Client built with redirect::Policy::none(): we
  control the chain manually; reqwest does NOT auto-forward Authorization
  to non-GitHub hosts. (Codex M3-plan-v1 #2.)
- api_host plumbed via GitHubProvider::new (no Config::load() in
  hot path). Used in api_url, AuthIdentity, validate_redirect_target.
  (Codex M3-plan-v1 #3.)
- Manual 302 follow: parses Location, validates scheme=https + host
  equals codeload host derived from configured api_host (default
  api.github.com → codeload.github.com), bounds chain at depth 3.
  Codeload hop uses a fresh client with no default Authorization.
- Path validation per entry: takes raw bytes (strict UTF-8 check) +
  EntryType (distinguishes wrapper-dir from stray-file-at-root); strip
  the codeload wrapper dir; reject leading `/`, `..`, NUL, control
  chars. (Codex M3-plan-v1 #9.)
- Digest verification: Git blob SHA-1 streamed alongside the writer.
  Mismatch → discard (writer dropped, NamedTempFile RAII unlinks).
- Atomic commit: BlobCache::commit_atomic_with_writer (Task 4).

Adds workspace deps: tar 0.4, flate2 1, sha1 0.10. reqwest gains
'stream' feature; tokio-util gains 'io' feature.
EOF
)"
```

---

## Task 7: Auto-gate + singleflight + `FetchPolicy` dispatch

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

**Goal:** glue Tasks 2/4/5/6 into the production `fetch_snapshot` path. Compute `FetchPolicy` via `decide_policy(...)`; dispatch to `fetch_tarball_into_cache` (Task 6) on `Tarball{...}` or stay lazy on `Lazy` / `LazyOversized`. Add daemon-side singleflight dedupe so concurrent mounts of `(host, repo, commit)` resolve to one tarball download.

- [ ] **Step 1: Daemon singleflight registry with leader semantics**

`TarballKey` lives in `provider-common::fetcher` (Task 2). The daemon imports it.

`TarballSlot` carries the OnceCell + a leader UUID so removal can be guarded with `Arc::ptr_eq` on the slot Arc:

```rust
// In ctxfs-daemon/src/daemon.rs:

use ctxfs_provider_common::fetcher::TarballKey;

#[derive(Debug)]
pub struct TarballSlot {
    /// Populated by the leader; awaited by waiters.
    /// `Result<(), String>` so error states are observable across awaiters
    /// without retry within the same flight (Codex M3-plan-v1 #6).
    pub cell: tokio::sync::OnceCell<Result<(), String>>,
}

pub type TarballSingleflightMap = dashmap::DashMap<TarballKey, Arc<TarballSlot>>;

#[derive(Debug)]
pub struct SlotClaim {
    pub key: TarballKey,
    pub slot: Arc<TarballSlot>,
    pub is_leader: bool,
    /// Reference to the registry so the leader can release the slot via
    /// `remove_if(key, |entry| Arc::ptr_eq(entry, &slot))` — guaranteeing
    /// we don't accidentally remove a *newer* slot that landed for the
    /// same key after we finished.
    pub registry: Arc<TarballSingleflightMap>,
}

impl SlotClaim {
    /// Leader-only: drop the slot from the registry when work is complete.
    /// Uses Arc pointer-equality so a stale claim cannot remove a newer slot
    /// allocated for the same key after our work finished. No-op for waiters.
    pub fn release(&self) {
        if !self.is_leader {
            return;
        }
        let target = Arc::clone(&self.slot);
        self.registry.remove_if(&self.key, |_, slot| Arc::ptr_eq(slot, &target));
    }
}
```

Add a field to `DaemonServer`:

```rust
    /// Singleflight registry for in-flight tarball prefetches.
    tarball_singleflight: Arc<TarballSingleflightMap>,
```

Construct in `DaemonServer::new`:

```rust
        tarball_singleflight: Arc::new(dashmap::DashMap::new()),
```

Pass into `GitHubProvider::new` (extending the signature):

```rust
pub fn new(
    token: Option<&str>,
    api_host: String,    // <-- NEW (Codex M3-plan-v1 #3)
    cache: Arc<BlobCache>,
    tree_cache: Option<Arc<TreeCache>>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    observability: Arc<Observability>,
    tarball_singleflight: Arc<TarballSingleflightMap>,   // <-- NEW
) -> Self {
```

(Now 7 args. M4 collapses via `ProviderContext`; M3 accepts the inflation.)

The provider claim helper:

```rust
    fn claim_singleflight_slot(&self, key: TarballKey) -> SlotClaim {
        let mut is_leader = false;
        let slot = self
            .tarball_singleflight
            .entry(key.clone())
            .or_insert_with(|| {
                is_leader = true;
                Arc::new(TarballSlot { cell: tokio::sync::OnceCell::new() })
            })
            .clone();
        SlotClaim {
            key,
            slot,
            is_leader,
            registry: self.tarball_singleflight.clone(),
        }
    }
```

(`is_leader` captured via the entry's "did we insert?" — `or_insert_with` only runs the closure on insertion, so the side-effect is well-defined.)

- [ ] **Step 2: Provider-side `dispatch_fetch_policy` orchestrator**

In `github.rs`:

```rust
    /// Run the auto-gate and, if it elects tarball, fetch it (with singleflight
    /// dedupe + cache pre-check). Tarball failure is non-fatal — the snapshot
    /// still completes; lazy reads pick up uncached blobs.
    ///
    /// Counters: prefetch_hits (committed blobs), prefetch_failures (one per
    /// failed tarball attempt), prefetch_skipped_oversized (gate said no).
    async fn dispatch_fetch_policy(
        &self,
        source: &SourceSpec,
        commit_sha: &str,
        tree_entries: &[TreeEntry],
        policy: PrefetchPolicy,
        threshold_count: u64,
        max_bytes: u64,
    ) -> Result<(), CtxfsError> {
        // Inputs to the gate. Treat any blob with size==None as "unknown",
        // which forces FetchPolicy::Lazy (the per-blob lazy path is correct
        // even for huge blobs; we just won't tarball-prefetch). Without this,
        // a tree containing a missing-size entry would undercount and we
        // could blow past the byte cap. (Codex M3-plan-v1 #8.)
        let blob_iter = tree_entries.iter().filter(|e| e.entry_type == "blob");
        let blob_count = blob_iter.clone().count() as u64;
        let any_unknown_size = blob_iter.clone().any(|e| e.size.is_none());
        let estimated_bytes: u64 = blob_iter.clone().map(|e| e.size.unwrap_or(0)).sum();

        let effective_policy = if any_unknown_size && policy == PrefetchPolicy::Auto {
            tracing::info!(
                target: "ctxfs.provider.tarball",
                "manifest has entries with unknown size; degrading auto-gate to Lazy"
            );
            PrefetchPolicy::Disabled
        } else {
            policy
        };

        let decision = decide_policy(
            blob_count,
            estimated_bytes,
            effective_policy,
            threshold_count,
            max_bytes,
        );

        match decision {
            FetchPolicy::Lazy => Ok(()),
            FetchPolicy::LazyOversized { estimated_bytes, blob_count, cap } => {
                if let Some(key) = self.counter_key.lock().unwrap().clone() {
                    self.observability
                        .counters_for(key)
                        .record_prefetch_skipped_oversized();
                }
                tracing::warn!(
                    target: "ctxfs.provider.tarball",
                    estimated_bytes,
                    blob_count,
                    cap,
                    "tarball auto-gate skipped: estimated_bytes > prefetch_max_bytes"
                );
                Ok(())
            }
            FetchPolicy::Tarball { estimated_bytes, blob_count } => {
                // Pre-claim cache check (Codex M3-plan-v1 #6 sub-bullet):
                // if every manifest blob is already in BlobCache, skip
                // the tarball entirely. Cheap.
                let blob_digests: Vec<Digest> = tree_entries
                    .iter()
                    .filter(|e| e.entry_type == "blob")
                    .map(|e| Digest::from_sha256_hex(&e.sha))
                    .collect();
                if self.cache.contains_all(blob_digests.iter()) {
                    tracing::info!(
                        target: "ctxfs.provider.tarball",
                        blob_count,
                        "all manifest blobs already cached; skipping tarball"
                    );
                    return Ok(());
                }

                // Singleflight: claim slot.
                let (owner, repo) = owner_repo(source)?;
                let key = TarballKey {
                    host: self.api_host.clone(),
                    owner: owner.to_string(),
                    repo: repo.to_string(),
                    commit_sha: commit_sha.to_string(),
                };
                let claim = self.claim_singleflight_slot(key.clone());

                // Leader populates; waiter awaits the existing cell.
                let outcome_res: Result<(), String> = claim
                    .slot
                    .cell
                    .get_or_init(|| async {
                        match self
                            .fetch_tarball_into_cache(source, commit_sha, tree_entries)
                            .await
                        {
                            Ok(out) => {
                                if let Some(k) = self.counter_key.lock().unwrap().clone() {
                                    let counters = self.observability.counters_for(k);
                                    counters.record_prefetch_hits(out.blobs_committed);
                                    counters.record_bytes_transferred(out.total_bytes);
                                    counters.record_http_transfer();
                                }
                                tracing::info!(
                                    target: "ctxfs.provider.tarball",
                                    blob_count,
                                    estimated_bytes,
                                    blobs_committed = out.blobs_committed,
                                    blobs_skipped_invalid = out.blobs_skipped_invalid,
                                    blobs_skipped_digest = out.blobs_skipped_digest,
                                    total_bytes = out.total_bytes,
                                    "tarball prefetch ok"
                                );
                                Ok(())
                            }
                            Err(e) => {
                                // Record prefetch_hits even on error — the
                                // tarball flow streams blobs into BlobCache
                                // as it goes, and partial commits are kept
                                // (Codex M3-plan-v1 #7). But we don't have a
                                // partial outcome here; we'd need to thread it
                                // back from fetch_tarball_into_cache via a
                                // returned PartialOutcome on Err. For M3,
                                // record_prefetch_failure ticks once and the
                                // committed-counter is recorded inside
                                // commit_atomic_with_writer's call site (see
                                // note in fetch_tarball_into_cache).
                                if let Some(k) = self.counter_key.lock().unwrap().clone() {
                                    self.observability
                                        .counters_for(k)
                                        .record_prefetch_failure();
                                }
                                tracing::warn!(
                                    target: "ctxfs.provider.tarball",
                                    error = format!("{e:?}").as_str(),
                                    "tarball prefetch failed; falling back to lazy"
                                );
                                Err(format!("{e}"))
                            }
                        }
                    })
                    .await
                    .clone();

                // Leader removes the slot; waiters' release() is no-op.
                claim.release();

                // Tarball failure is non-fatal.
                let _ = outcome_res;
                Ok(())
            }
        }
    }
```

**Note on partial commits + telemetry (Codex M3-plan-v1 #7):** the spec confirms partial commits are kept on mid-stream failure. To make those visible in `prefetch_hits`, `fetch_tarball_into_cache` must record the counter incrementally — i.e., increment `prefetch_hits` *inside* the per-entry loop after each `writer.finalize` succeeds, rather than only after the function returns. Update Task 6's per-entry loop to do this:

```rust
                writer.finalize(&digest)?;

                outcome.blobs_committed += 1;
                outcome.total_bytes += expected_size;
                if let Some(ref key) = counter_key {
                    observability.counters_for(key.clone()).record_prefetch_hit();
                }
```

(Then `dispatch_fetch_policy`'s success path doesn't need to call `record_prefetch_hits(out.blobs_committed)` again — drop that line.)

- [ ] **Step 3: Wire into `fetch_snapshot`**

`fetch_snapshot` currently runs `prefetch_small_blobs` then `build_directories_with_inline`. Inject the tarball dispatch *before* the small-blobs prefetch so big files are already in cache when tiny ones are fetched, OR run the small-blobs path first and skip the tarball if all blobs are already inlined. Choose: tarball first (the intent is bulk-hydrate everything in one shot).

Updated sequence:

```rust
        // 4a. Tarball auto-gate / dispatch. May commit blobs into BlobCache;
        //     does not affect the manifest. Failures are non-fatal — we fall
        //     back to lazy reads.
        self.dispatch_fetch_policy(
            source,
            &commit_sha,
            &tree.tree,
            options.prefetch,
            options.prefetch_threshold_count,
            options.prefetch_max_bytes,
        ).await?;

        // 5. Prefetch small blobs + symlink targets for B1/B7 inlining.
        //    NOTE: even after a successful tarball, we still run small-blobs
        //    prefetch — the tarball commits to BlobCache (read path) but does
        //    not populate the in-memory inline_content map (manifest path).
        //    The two prefetches are O(N) each on a tiny-file count; running
        //    both keeps M3 cleanly additive over M2's behavior.
        // ... existing M2 path continues ...
```

This requires a new method `fetch_snapshot_with_options` because the `Provider::fetch_snapshot` trait sig is `async fn fetch_snapshot(&self, source: &SourceSpec) -> Result<Vec<u8>, CtxfsError>`. M3 keeps the trait sig unchanged and adds a parallel API:

```rust
pub struct FetchOptions {
    pub prefetch: PrefetchPolicy,
    pub prefetch_threshold_count: u64,
    pub prefetch_max_bytes: u64,
}

impl GitHubProvider {
    pub async fn fetch_snapshot_with_options(
        &self,
        source: &SourceSpec,
        options: &FetchOptions,
    ) -> Result<Vec<u8>, CtxfsError> {
        // body identical to current fetch_snapshot, with the dispatch_fetch_policy
        // injected as in step 3
    }
}
```

The existing `Provider::fetch_snapshot` impl delegates by calling `fetch_snapshot_with_options(source, &FetchOptions::default())` so older callers (tests, FSKit pre-MountOptions paths) keep compiling. Default = `PrefetchPolicy::Disabled` so behavior is unchanged for callers that don't opt in.

```rust
impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            prefetch: PrefetchPolicy::Disabled,
            prefetch_threshold_count: 30,
            prefetch_max_bytes: 256 * 1024 * 1024,
        }
    }
}
```

- [ ] **Step 4: Daemon plumbing**

In `prepare_mount`, after constructing the `GitHubProvider`, change the snapshot fetch:

```rust
        let fetch_options = ctxfs_provider_git::FetchOptions {
            prefetch: options.prefetch,
            prefetch_threshold_count: self.config.prefetch_threshold_count,
            prefetch_max_bytes: self.config.prefetch_max_bytes,
        };
        let snapshot_data = self
            .rt_handle
            .block_on(provider.fetch_snapshot_with_options(&github_source, &fetch_options))
            .map_err(|e| format!("failed to fetch snapshot: {e}"))?;
```

The `GitHubProvider::new` callsite gains the `api_host` and singleflight args:

```rust
        let provider = Arc::new(GitHubProvider::new(
            self.config.github_token.as_deref(),
            self.config.github_host.clone(),               // NEW
            self.cache.clone(),
            Some(self.tree_cache.clone()),
            self.shared_tree_cache.clone(),
            self.observability.clone(),
            self.tarball_singleflight.clone(),             // NEW
        ));
```

NFS and other test callsites of `GitHubProvider::new` add `"api.github.com".to_string()` (or whatever the test's mock host is) and `Arc::new(DashMap::new())`. Mirror the M2 pattern.

**Internal helper to avoid duplicated body** (Codex other-call): the `Provider::fetch_snapshot` trait method delegates to `fetch_snapshot_with_options` via a shared private `fetch_snapshot_inner(&self, source, &FetchOptions) -> Result<Vec<u8>, CtxfsError>`. The trait sig stays unchanged; the public `fetch_snapshot_with_options` and the trait `fetch_snapshot` both call `fetch_snapshot_inner` with their respective options:

```rust
impl GitHubProvider {
    pub async fn fetch_snapshot_with_options(
        &self,
        source: &SourceSpec,
        options: &FetchOptions,
    ) -> Result<Vec<u8>, CtxfsError> {
        self.fetch_snapshot_inner(source, options).await
    }

    async fn fetch_snapshot_inner(
        &self,
        source: &SourceSpec,
        options: &FetchOptions,
    ) -> Result<Vec<u8>, CtxfsError> {
        // ... full body lives here ...
    }
}

#[async_trait]
impl Provider for GitHubProvider {
    async fn fetch_snapshot(&self, source: &SourceSpec) -> Result<Vec<u8>, CtxfsError> {
        self.fetch_snapshot_inner(source, &FetchOptions::default()).await
    }
    // ...
}
```

- [ ] **Step 5: Re-export `FetchOptions` and types**

In `crates/ctxfs-provider-git/src/lib.rs`:

```rust
pub use github::{FetchOptions, GitHubProvider, TarballKey};
```

- [ ] **Step 6: Verify**

```bash
cargo build
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

The pre-existing test failures remain expected.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-provider-git/src/github.rs \
        crates/ctxfs-provider-git/src/lib.rs \
        crates/ctxfs-daemon/src/daemon.rs

git commit -m "$(cat <<'EOF'
feat(provider-git,daemon): auto-gate + singleflight + FetchPolicy dispatch

Glues M3's pieces into fetch_snapshot:
- New fetch_snapshot_with_options(&FetchOptions) API on GitHubProvider.
  Provider::fetch_snapshot trait method delegates with FetchOptions::default
  (PrefetchPolicy::Disabled) so non-daemon callers (NFS tests, etc.)
  keep their pre-M3 behavior unchanged.
- dispatch_fetch_policy reads decide_policy(blob_count, est_bytes,
  PrefetchPolicy, threshold_count, max_bytes) → FetchPolicy. Tarball
  variant fires fetch_tarball_into_cache (Task 6); LazyOversized
  increments prefetch_skipped_oversized; Lazy is the no-op fallback.
- Singleflight via Arc<DashMap<TarballKey, Arc<OnceCell<...>>>> lives on
  the daemon (B8 constraint preserved — providers are still per-mount).
  Two concurrent mounts of (host, repo, commit) await the same OnceCell;
  first writer commits blobs to BlobCache and the slot is dropped so a
  later mount that needs re-fetch goes through fresh.

A tarball failure (network, partial extract, all entries digest-mismatch)
is non-fatal: the snapshot completes and lazy reads pick up missing blobs.
Counters distinguish prefetch_hits (committed), prefetch_failures (one
per failed tarball attempt), prefetch_skipped_oversized (gate said no
on bytes), tarball_invalid_entries (path violations), and
tarball_digest_mismatch (sha mismatches).

Note: GitHubProvider::new gains a singleflight arg, so all callsites
(daemon, NFS tests) update. M4's planned ProviderContext struct will
collapse the proliferating constructor args.
EOF
)"
```

---

## Task 8: Replay tests + carry-forwards + workspace verify + tag

**Files:**
- Create: `crates/ctxfs-provider-git/tests/replay_tarball_three_calls.rs`
- Create: `crates/ctxfs-provider-git/tests/replay_truncated_tree_walk.rs`
- Create: `crates/ctxfs-provider-git/tests/replay_singleflight_dedupe.rs`
- Create: `crates/ctxfs-provider-git/tests/replay_path_traversal_rejected.rs`
- Create: `crates/ctxfs-provider-git/tests/replay_oversized_skipped.rs`
- Create: `crates/ctxfs-provider-git/tests/common/replay_harness.rs`
- Modify: `crates/ctxfs-provider-git/tests/build_directories.rs` — assembled-path test
- Modify: `crates/ctxfs-provider-common/src/http.rs` — `bearer_header` helper
- Modify: `crates/ctxfs-provider-git/src/github.rs` — fail-strict symlinks; bearer_header sites; placeholder bucket pruning
- Modify: `crates/ctxfs-provider-common/src/observability.rs` — `merge_and_drop_placeholder` helper
- Modify: `CHANGELOG.md`

**Goal:** finish the milestone. Replay tests (the spec's exit-criteria assertions) verify `rest_calls_total == 3` etc. against an in-process HTTP server, and the M2 carry-forwards land here so the M4 starting line is clean.

### 8a. Replay tests (the milestone's exit criteria)

Replay tests live in `crates/ctxfs-provider-git/tests/` (not provider-common — Codex M3-plan-v1 #10) because they exercise the real `GitHubProvider` + `BlobCache` + tarball extraction path. The HTTP layer is stubbed with a small in-process server.

**Mock server choice:** `wiremock` is **NOT** in workspace dev-deps as of M2 (audit before adding). Use a hand-rolled `tokio::net::TcpListener` + minimal HTTP-1.1 response writer for the tests. The routes are fixed (`/repos/{o}/{r}/commits/{ref}`, `/repos/{o}/{r}/git/trees/{sha}`, `/repos/{o}/{r}/tarball/{ref}` + `codeload.<host>/owner/repo/tar.gz/{sha}`) so a 100-line scaffold suffices. The scaffold lives in `crates/ctxfs-provider-git/tests/common/replay_harness.rs`. (If audit reveals `wiremock` is already present, swap to it — the test bodies are unchanged.)

For each test: spawn the mock server on `127.0.0.1:0`, configure responses, build a real `BlobCache` in a tempdir, construct `GitHubProvider::new(token=None, api_host=host_from_listener, cache, ..., tarball_singleflight=Arc::new(DashMap::new()))`, run `fetch_snapshot_with_options(...)`, assert on the post-state.

**Codeload host override for tests:** Production derives `codeload_host = codeload_host_for(api_host)` (e.g., `codeload.github.com` for `api.github.com`). Tests need to override this so a single in-process listener can serve both API and codeload routes. The provider therefore stores `api_host: String` AND `codeload_host: String` separately. `GitHubProvider::new` derives `codeload_host` if not explicitly given:

```rust
pub fn new_with_codeload_host(
    token: Option<&str>,
    api_host: String,
    codeload_host: Option<String>,    // None → derived
    cache: Arc<BlobCache>,
    /* ... */
) -> Self {
    let codeload_host = codeload_host.unwrap_or_else(|| Self::codeload_host_for(&api_host));
    // ...
}

pub fn new(/* original args */) -> Self {
    Self::new_with_codeload_host(token, api_host, None, cache, /* ... */)
}
```

Production stays on `new(...)`. Replay tests use `new_with_codeload_host` with both set to `127.0.0.1:<port>`. No env-var, no Config field — the override is a test-only API.

Concrete tests:

1. **`replay_tarball_three_calls.rs`** — 1000-file tree, 30 MB total. `PrefetchPolicy::Auto`. Assert:
   - `counters.rest_calls_total == 3` (commit + tree + tarball)
   - `counters.prefetch_hits == 1000` (all blobs committed)
   - All blobs present in `BlobCache`
2. **`replay_truncated_tree_walk.rs`** — root tree returns `truncated: true` with one direct file + one subtree; the subtree GET returns 5 files. After fetch:
   - `counters.truncated_tree_fallbacks == 1`
   - Manifest has 6 file entries (1 + 5)
3. **`replay_singleflight_dedupe.rs`** — two concurrent `fetch_snapshot_with_options` calls for same `(repo, commit)`. Mock counts tarball requests:
   - tarball mock hit count == 1
   - both calls return Ok
4. **`replay_path_traversal_rejected.rs`** — tarball contains `owner-repo-sha/../escape`, `owner-repo-sha/legit.rs`. After fetch:
   - `counters.tarball_invalid_entries >= 1`
   - `counters.prefetch_hits >= 1` (legit.rs landed)
   - `escape` does NOT appear in cache
5. **`replay_oversized_skipped.rs`** — manifest reports `estimated_bytes = 1 GB`, `prefetch_max_bytes = 100 MB`. `PrefetchPolicy::Auto`. Assert:
   - tarball mock hit count == 0
   - `counters.prefetch_skipped_oversized == 1`

For each test:

- [ ] Step: write the test (TDD: it should fail until the matching production code is in place — most should already pass after Task 7).
- [ ] Step: run + verify.
- [ ] Step: commit.

(One commit per test for review granularity, or one combined "test(replay): M3 exit-criteria suite" commit. Suggest the combined commit since these are all telemetry-shape assertions.)

### 8b. Carry-forwards from M2

Each is its own commit so reverts are localized.

#### 8b.1 — Symlink fail-strict

In `github.rs::build_directories_inner`, change the symlink branch:

```rust
            let dir_entry = if entry.mode == MODE_SYMLINK {
                let bytes = inline.and_then(|m| m.get(&entry.sha)).ok_or_else(|| {
                    CtxfsError::Provider(format!(
                        "symlink target missing from inline map: path={} sha={}",
                        entry.path, entry.sha
                    ))
                })?;
                let target = std::str::from_utf8(bytes).map_err(|e| {
                    CtxfsError::Provider(format!(
                        "symlink target not valid UTF-8: path={} sha={}: {e}",
                        entry.path, entry.sha
                    ))
                })?.to_string();
                DirEntry::Symlink(SymlinkEntry { name, target })
            } else {
```

This requires changing `build_directories_inner` to return `Result<(Digest, HashMap<...>), CtxfsError>`. Update both public wrappers + tests. Update unit test `build_directories_without_inline_keeps_target_empty_and_no_inline_content` — under fail-strict, the no-inline path must NOT produce a symlink entry. Either the test changes shape (assert error) or the no-inline path skips symlinks entirely. Choose: skip them — backward-compat callers don't use symlinks anyway.

```rust
            let dir_entry = if entry.mode == MODE_SYMLINK {
                let bytes = match inline.and_then(|m| m.get(&entry.sha)) {
                    Some(b) => b,
                    None => {
                        // Backward-compat (build_directories without inline):
                        // skip symlinks entirely. Production callers always
                        // use build_directories_with_inline.
                        if inline.is_none() { continue; }
                        return Err(CtxfsError::Provider(format!(
                            "symlink target missing: path={} sha={}",
                            entry.path, entry.sha
                        )));
                    }
                };
                // ... UTF-8 decode as above ...
            } else {
```

Commit:

```bash
git commit -m "$(cat <<'EOF'
fix(provider-git,B7): fail-strict symlink target resolution (M2 carry-forward)

Pre-M3, build_directories_inner produced an empty target on three
edge cases: missing-from-inline-map, oversized blob excluded by the
prefetch filter, invalid UTF-8 bytes. Codex's M2-result review flagged
this — readlink has no lazy fallback, so an empty target is a silent
data-correctness regression no test catches.

M3 makes these errors explicit: missing or non-UTF-8 → CtxfsError, and
fetch_snapshot returns the error rather than producing a manifest with
broken symlinks. The strict-on-symlink prefetch policy from M2 already
ensures real-world git symlinks (always small, valid UTF-8) succeed.

Backward-compat callers of build_directories(no inline map) skip
symlinks instead of erroring — those callers are tests and FSKit's
adapter; production daemon path always uses build_directories_with_inline.
EOF
)"
```

#### 8b.2 — HeaderMap-direct refactor — **DEFERRED to Phase 5 perf work**

Codex's M3-plan-v1 review: "not load-bearing." The ~30-alloc-per-response savings are real but don't change behavior, and M3 is already substantial. Leave the carry-forward note in the M4 handoff so a future perf milestone can pick it up.

#### 8b.3 — `bearer_header` helper

In `crates/ctxfs-provider-common/src/http.rs`:

```rust
use reqwest::header::HeaderValue;

/// Build a `Bearer <token>` HeaderValue. Returns None on invalid byte input;
/// callers may unwrap or fall through to anonymous as appropriate.
#[must_use]
pub fn bearer_header(token: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(&format!("Bearer {token}")).ok()
}
```

Update `provider-git/src/github.rs` (3 sites: `new`, `token.rs:24`, `token.rs:30`) to call this helper.

Commit: `refactor(provider-common): bearer_header helper consolidates 3 sites`.

#### 8b.4 — `<resolving:ref>` placeholder bucket pruning

In `crates/ctxfs-provider-common/src/observability.rs`:

```rust
    /// Drop a placeholder counter bucket (e.g., `<resolving:ref>` set before
    /// resolve_ref runs). After the real commit SHA is known, the placeholder
    /// bucket has accumulated 1 rest_calls_total tick that should now live on
    /// the real bucket. We MERGE the placeholder counters into the resolved
    /// bucket so attribution is preserved, then remove the placeholder.
    pub fn merge_and_drop_placeholder(
        &self,
        placeholder: &CounterKey,
        resolved: &CounterKey,
    ) {
        if let Some((_, placeholder_counters)) = self.counters.remove(placeholder) {
            let resolved_counters = self.counters_for(resolved.clone());
            let snap = placeholder_counters.snapshot();
            // The only counter that should have ticked during placeholder
            // lifetime is rest_calls_total (resolve_ref API call). Merge it.
            for _ in 0..snap.rest_calls_total {
                resolved_counters.record_rest_call();
            }
        }
    }
```

In `github.rs::fetch_snapshot`, after replacing `counter_key` with the resolved version:

```rust
        let placeholder_key = CounterKey {
            source: source.provider_type.to_string(),
            repo: source.name.clone(),
            commit: format!("<resolving:{}>", source.version),
            mount_id: source.id(),
        };
        let resolved_key = CounterKey {
            source: source.provider_type.to_string(),
            repo: source.name.clone(),
            commit: commit_sha.clone(),
            mount_id: source.id(),
        };
        self.observability.merge_and_drop_placeholder(&placeholder_key, &resolved_key);
        *self.counter_key.lock().unwrap() = Some(resolved_key);
```

Commit: `refactor(observability): merge placeholder bucket on counter_key resolve (M2 carry-forward)`.

#### 8b.5 — Assembled-path fetch_snapshot test

In `crates/ctxfs-provider-git/tests/build_directories.rs`, add a wiremock-mocked test that exercises the full `fetch_snapshot` path (resolve + tree + small-blobs prefetch + tarball-disabled) and asserts:

- Manifest contains expected file entries with non-empty `inline_content` for ≤4 KB files
- Manifest contains expected symlink entries with correct `target` strings
- `rest_calls_total` counter ticked exactly 2 + N (commit + tree + N small-blob fetches)

This is a single test — see M2 carry-forward #2 in the handoff. Commit: `test(provider-git): assembled-path fetch_snapshot HTTP-mocked test (M2 carry-forward)`.

### 8c. Workspace verify

- [ ] **Step 1: Full workspace build + tests + fmt + clippy**

```bash
cargo build --release
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
cargo test 2>&1 | tail -50
```

Expected: green except documented pre-existing failures (`mount_server_only_starts_nfs_and_reports_port`, `env_var_*` race).

- [ ] **Step 2: M1 status_bench regression check**

```bash
cargo test --release -p ctxfs --test status_bench -- --ignored --nocapture
```

Expected: passes (M1 budget: p95 ≤ 100ms; M1's recorded number was 3.375µs).

### 8d. CHANGELOG + tag

- [ ] **Step 1: CHANGELOG**

Prepend to `CHANGELOG.md`:

```markdown
## v0.1.3-m3 — 2026-04-XX

### Phase 4 M3: Tarball Prefetch with Smart Gate

- New tarball prefetch path: `/repos/{o}/{r}/tarball/{ref}` is one quota-bearing
  REST call that hydrates the entire repo into BlobCache. Cold-scan cost on a
  1k-file 30 MB repo drops from "1k REST calls" to "3 REST calls" (commit + tree + tarball).
- Auto-gate decides per-mount: tarball if `blob_count >= CTXFS_PREFETCH_THRESHOLD_COUNT`
  (default 30) AND `estimated_bytes <= CTXFS_PREFETCH_MAX_BYTES` (default
  `min(cache_max/4, 256 MB)`); else lazy. Counters surface skip reasons.
- New `PrefetchPolicy { Auto, Force, Disabled }` on `MountOptions` IPC + new
  `--prefetch` / `--no-prefetch` CLI flags on `ctxfs mount`. `Auto` (default)
  consults the gate; `Force` bypasses the byte cap; `Disabled` is always lazy.
- Tarball hardening: codeload-host whitelist on the 302; `Authorization` header
  stripped before redirect; redirect chain bounded at depth 3; per-entry path
  validation rejects `..` / absolute / NUL / control chars; per-blob digest
  verification (Git SHA-1) before atomic commit; `BlobCache::commit_atomic`
  uses temp-and-rename so a daemon crash mid-commit cannot leave corrupt blobs;
  `cleanup_orphan_temps` on daemon startup unlinks orphans older than 1 hour.
- Singleflight dedupe: two concurrent mounts of the same `(host, repo, commit)`
  share one tarball download.
- B2: tree-truncation fallback walks per-directory non-recursively when
  `truncated == true` (counter `truncated_tree_fallbacks`). Auto-gate signals
  are reliable on large repos.
- Skeletal `ContentFetcher` trait + `decide_policy` lift the auto-gate
  algorithm into `ctxfs-provider-common` so M4's full `ContentFetcher`
  refactor doesn't need to relocate the logic.
- M2 carry-forwards landed: symlink-target fail-strict (no more silent
  empty targets); `ThrottleClassifier::classify` HeaderMap-direct refactor
  (~30 alloc savings per HTTP response); `bearer_header` helper consolidates
  3 sites; `<resolving:ref>` placeholder bucket merged on commit-sha resolution;
  assembled-path `fetch_snapshot` HTTP-mocked test fills the M2 coverage gap.
- New env vars: `CTXFS_PREFETCH_THRESHOLD_COUNT`, `CTXFS_PREFETCH_MAX_BYTES`,
  `CTXFS_GITHUB_HOST` (already in spec; M3 wires them).
- **Wire-format break:** the `mount` tarpc method gained an argument. Older
  CLIs cannot talk to a newer daemon (and vice versa). Rebuild both
  together; users with an old `ctxfs` CLI alongside a new daemon will see
  IPC errors until they update.
```

- [ ] **Step 2: Tag**

```bash
git add CHANGELOG.md
git commit -m "docs: CHANGELOG for v0.1.3-m3 (Phase 4 M3 tarball prefetch + B2 + skeletal ContentFetcher)"
git tag -a v0.1.3-m3 -m "Phase 4 M3: tarball prefetch with smart gate + B2 + ContentFetcher skeleton"
```

- [ ] **Step 3: Verify tag**

```bash
git tag -l v0.1.3-m3
```

(Tag is local only; not pushed until end of M5.)

---

## Self-review checklist

- [ ] **Spec coverage:** B2 (Task 5), tarball prefetch + hardening (Tasks 4 + 6 + 7), auto-gate (Tasks 2 + 7), MountOptions IPC + CLI (Task 3), Config env vars (Task 1), skeletal ContentFetcher (Task 2). All M3 spec deliverables.
- [ ] **Hardening:** redirect-host whitelist, Authorization strip via fresh client, `redirect::Policy::none()` on provider client, depth ≤ 3, path validation (UTF-8 + EntryType-aware + NUL + control chars + `..`), per-blob digest verification (streaming), atomic temp-and-rename + parent-dir fsync. All wire-level.
- [ ] **Streaming end-to-end:** `bytes_stream → StreamReader → SyncIoBridge → flate2 → tar::Archive` inside `spawn_blocking`; per-entry `Tee<GitBlobSha1, BlobTempWriter>`. Memory ceiling is per-entry, not per-archive (Codex M3-plan-v1 #1).
- [ ] **`api_host` plumbed via constructor** (Codex M3-plan-v1 #3): no `Config::load()` in hot path. Used in `api_url`, `AuthIdentity`, `validate_redirect_target`. Test-only `new_with_codeload_host` accepts an explicit codeload override.
- [ ] **Config recompute** (Codex M3-plan-v1 #4): `prefetch_max_bytes` re-derived from `cache_max_bytes` if file/env didn't explicitly set it. `PrefetchExplicit` tracker.
- [ ] **`TarballKey` in provider-common** (Codex M3-plan-v1 #5), not daemon — provider-git can't depend on daemon.
- [ ] **Singleflight leader/waiter** (Codex M3-plan-v1 #6): `claim_singleflight_slot` returns `SlotClaim { is_leader }`; only leader removes; `remove_if(key, |slot| Arc::ptr_eq(slot, &claim.slot))` so older claim cannot remove newer slot. Cache pre-check via `BlobCache::contains_all` skips tarball when manifest blobs are already cached.
- [ ] **Partial commits kept + telemetry** (Codex M3-plan-v1 #7): `prefetch_hits` recorded incrementally inside the per-entry loop, not only on success.
- [ ] **B2 walks `tree.sha`** (Codex M3-plan-v1 #8) not `commit_sha`; missing-size entries degrade Auto → Disabled (fail-closed).
- [ ] **Path validation type-aware** (Codex M3-plan-v1 #9): `(raw_bytes, EntryType)` distinguishes wrapper-dir from stray-file; strict UTF-8 instead of `to_string_lossy`.
- [ ] **Replay tests in provider-git** (Codex M3-plan-v1 #10), with hand-rolled mock listener (wiremock not in workspace).
- [ ] **Backward compat note:** `Provider::fetch_snapshot` trait sig unchanged; trait method delegates via shared `fetch_snapshot_inner` to avoid body duplication. **Tarpc wire change** (CLI ↔ daemon) is NOT compatible — both rebuild together; CHANGELOG flags this.
- [ ] **B8 constraint:** No shared/global fetcher; `tarball_singleflight` is a *registry*, not an `active_source`. Per-mount providers preserved.
- [ ] **Counter coverage:** `prefetch_hits` (per-blob, incremental), `prefetch_failures` (per attempt), `prefetch_skipped_oversized`, `tarball_digest_mismatch`, `tarball_invalid_entries`, `truncated_tree_fallbacks` — all wired.
- [ ] **Replay tests:** five tests cover the spec's M3 exit criteria.
- [ ] **M2 carry-forwards landed:** symlink fail-strict, `bearer_header` helper, placeholder bucket merge (`merge_and_drop_placeholder`), assembled-path test. **Deferred:** HeaderMap-direct refactor (Phase 5 perf).
- [ ] **BlobCache**: `commit_atomic`, `commit_atomic_with_writer`, `BlobTempWriter`, `cleanup_orphan_temps`, `contains_all`, `rebuild_index` skips `tmp/`, parent-dir fsync after rename.
- [ ] **No M4 work:** full `ContentFetcher` impl + B6 detect + B5 cache reservation are M4/M5; this plan ships only the *skeletal* trait.
- [ ] **No intentionally broken-build commits:** each task's terminal commit leaves the workspace green (clippy + test + fmt).
- [ ] **Pre-existing flakes acknowledged:** `mount_server_only_starts_nfs_and_reports_port` and `env_var_*` race remain expected failures; not blocking.

---

## Future work (M4 picks up)

- Full `ContentFetcher` lift: `GitHubProvider` becomes the first concrete impl. `daemon.rs` calls `provider.fetch_batch(...)` instead of `fetch_snapshot_with_options`. `fetch_blob` and `fetch_directory` move to dispatched methods on the trait.
- Provider-construction collapse: M3 expanded `GitHubProvider::new` to 7 args. M4 introduces `ProviderContext { api_host, observability, cache, tree_cache, shared_tree_cache, singleflight }` and reduces back to 2.
- B5 per-repo cache reservation in `ctxfs-cache` (M5 lands fully).
- B6 LFS-pointer detect-and-surface (M5).
- B3-label SHA-1 variant on `Digest` (M5).
- `<resolving:ref>` placeholder elimination (currently merged at the right moment, but ideally never seeded — M4 may collapse to a single `counter_key.set()` post-resolve if the resolve_ref bookkeeping path can be reshaped).
- HeaderMap-direct refactor (deferred from M3 carry-forwards) — Phase 5 perf milestone.
- Streaming sync-tar via tokio io::copy_buf into a pipe — M3 already streams via SyncIoBridge, so this is "make the spawn_blocking thread cheaper." Marginal; only matters if telemetry shows real concurrency contention.

---

## Out of scope (Phase 5)

- Stage 2 Git-native object store + `cat-file --batch` pipeline (gated on M6 telemetry).
- Multi-tenant cache-poisoning hardening beyond per-blob digest (full SHA-chain verification).
- LFS smudge to real bytes (B6 full resolve).
- Native-CDN content fetchers for npm tarballs / PyPI sdists / crates.io.
- B8 active_source elimination (Phase 5 — depends on Stage 2 decision).

---

## References

- Spec: `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` § Architecture, § Milestones (M3 + B2)
- M2 plan: `docs/superpowers/plans/2026-04-25-phase-4-m2-architecture-neutral-fixes.md` (style reference)
- M2 handoff: `docs/phase4-m3-handoff.md` (carry-forwards source)
- M2 result Codex review: `/tmp/counsel/20260426-064959-claude-to-codex-720b80/codex.md` (carry-forwards source)
- `crates/ctxfs-provider-git/src/github.rs` — primary implementation site
- `crates/ctxfs-cache/src/lib.rs` — `BlobCache::commit_atomic` lands here
- `crates/ctxfs-provider-common/src/fetcher.rs` — new trait + `decide_policy`
- `crates/ctxfs-ipc/src/service.rs` — `MountOptions` lands here
