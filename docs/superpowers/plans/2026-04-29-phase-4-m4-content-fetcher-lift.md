# Phase 4 — M4: `ContentFetcher` full lift + plug-in refactor — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Plan version:** v2 (Codex-reviewed 2026-04-30 via counsel; review at `/tmp/counsel/20260430-023718-claude-to-codex-a36f5e/codex.md`). Verdict: **ship with edits**. v2 applies the 7 required edits.

**Goal:** Promote `GitHubProvider` to the first concrete implementation of the `ContentFetcher` trait that landed skeletal in M3. Collapse `GitHubProvider`'s 7+ argument constructor via a new `ProviderContext` struct so future providers (npm/PyPI/crates.io content fetchers) inherit the rate-limit / cache / observability machinery without re-deriving it. Land a `MockContentFetcher` test impl in provider-common as the spec's exit-criterion proof. Add a daemon-side CI test that asserts the B8 invariant (per-mount `GitHubProvider::new` is called from `prepare_mount`, no shared/global fetcher tied to `active_source`). **No external behavior change** — pure refactor.

**Architecture:**
- `Provider` trait (in `ctxfs-core`, 3 methods: `fetch_snapshot`, `fetch_directory`, `fetch_blob`) is the **daemon-VFS contract** and stays as-is. VFS uses `Arc<dyn Provider>` for read-time blob/directory fetches; this is unrelated to `ContentFetcher`.
- `ContentFetcher` (in `ctxfs-provider-common::fetcher`) is the **source-agnostic bulk-fetch contract**. M3 shipped it skeletal (only `estimate_cost` + `fetch_batch` method shapes; no impls). M4 makes `GitHubProvider` the first concrete impl.
- **`FetchBatchContext { source: SourceSpec, resolved_revision: String }`** is a new struct in `ctxfs-provider-common::fetcher` (Codex M4-plan-v1 #1). The trait's `fetch_batch` takes `&FetchBatchContext` so providers know what source + commit they're fetching for. Telemetry (`CounterKey`) is **not** a load-bearing data dependency for fetch logic; the context carries the resolved revision explicitly.
- `fetch_batch(ctx, requests, mode, counter_key)` is the canonical entry point for the bulk-fetch path. **Critical: `fetch_batch` is only called for `FetchPolicy::Tarball`** (Codex M4-plan-v1 #2). The auto-gate (`effective_prefetch_policy` + `decide_policy`) stays in `fetch_snapshot_inner`. `Lazy` and `LazyOversized` paths skip `fetch_batch` entirely; the latter still records `prefetch_skipped_oversized` exactly as today. **No behavior change** in M4 — only the *call shape* of the tarball path is unified through the trait.
- `ProviderContext { api_host, observability, cache, tree_cache, shared_tree_cache, singleflight }` lives in **`ctxfs-provider-git`** (Codex M4-plan-v1 #3 — confirmed). `provider-common` cannot depend on `ctxfs-cache` without inverting the existing dep direction (`cache → provider-common`). When the second concrete consumer ships (Phase 6 npm content provider), the right call (duplicate, extract, or migrate) can be made with two consumers in hand.
- `GitHubProvider::new(token: Option<&str>, ctx: ProviderContext) -> Self` shrinks from 7 args to 2. `new_with_codeload_host(token, codeload_override, ctx)` shrinks from 8 to 3. The `#[allow(clippy::too_many_arguments)]` annotations on both are removed.
- Daemon's `prepare_mount` constructs a `ProviderContext` from `self.config` + daemon-owned `Arc`s, then calls `GitHubProvider::new(token, ctx)`. **Per-mount construction preserved** — B8 invariant unchanged. The `MountPrep.provider` field stays `Arc<GitHubProvider>` (concrete); M4 doesn't introduce `Arc<dyn ContentFetcher>` in the daemon (this is about implementability, not callsite-genericization).
- `MockContentFetcher` ships in **`ctxfs-provider-common::mock`** alongside the existing `MockProvider` (Codex M4-plan-v1 #7). Trivial impl returning canned data; proves the trait is implementable from outside `provider-git`.
- B8 CI test is a **unit test in `daemon.rs`** (Codex M4-plan-v1 #4) — `DaemonServer`, `MountPrep`, and `prepare_mount` are all private; `pub(crate)` doesn't reach `tests/`. Add a private helper `build_github_provider_for_mount(&self, ...) -> Arc<GitHubProvider>` that `prepare_mount` calls and the unit test calls. Assert two helper invocations produce distinct `Arc<GitHubProvider>` instances (`Arc::ptr_eq` returns false). The helper isolates the construction concern from `prepare_mount`'s network-dependent flow.
- **`fetch_batch` return contract is "best-effort cache state"** (Codex M4-plan-v1 #7): missing paths are allowed; GitHub may only warm `BlobCache` (the trait's `HashMap<PathBuf, Vec<u8>>` return is populated for whatever bytes are available post-fetch). Documented in the trait's doc-comment. Future native-CDN providers may populate the map directly if their tarball returns bytes; M4 doesn't constrain that.

**Tech Stack:** Rust 2021, no new external deps. `async_trait` already in workspace. Workspace lints inherited.

**Spec reference:** `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` § Milestones (M4). Exit criteria:
- A trivial `MockContentFetcher` in tests can implement `ContentFetcher` and be used by a hypothetical second provider without touching `provider-git`.
- B8 constraint test passes.

---

## File Structure

```
crates/
  ctxfs-provider-common/
    src/
      fetcher.rs                         # MODIFY: add FetchBatchContext struct + extend fetch_batch sig; doc the best-effort return contract
      mock.rs                            # MODIFY: add MockContentFetcher alongside existing MockProvider
    tests/
      content_fetcher_implementable.rs   # CREATE: integration test proving the trait is implementable from outside provider-git
  ctxfs-provider-git/
    src/
      context.rs                         # CREATE: ProviderContext struct (lives in provider-git, NOT provider-common — Codex M4-plan-v1 #3)
      lib.rs                             # MODIFY: pub mod context; pub use context::ProviderContext
      github.rs                          # MAJOR MODIFY: collapse new/new_with_codeload_host signatures via ProviderContext; impl ContentFetcher with FetchBatchContext arg
    tests/
      common/
        mod.rs                           # MODIFY: update test helpers for new constructor shape (Codex M4-plan-v1 #7)
  ctxfs-daemon/
    src/
      daemon.rs                          # MODIFY: prepare_mount uses ProviderContext; private helper build_github_provider_for_mount; B8 unit test inline
  ctxfs-nfs/
    tests/
      medium_repo.rs                     # MODIFY: GitHubProvider::new callsite
      nfs_read_path.rs                   # MODIFY: GitHubProvider::new callsite
```

---

## Task 1: Add `FetchBatchContext` to provider-common; define `ProviderContext` in provider-git

**Files:**
- Create: `crates/ctxfs-provider-git/src/context.rs`
- Modify: `crates/ctxfs-provider-git/src/lib.rs`
- Modify: `crates/ctxfs-provider-common/src/fetcher.rs` (add `FetchBatchContext`; extend `fetch_batch` sig; doc the best-effort return)

**Why first:** every later task references `ProviderContext` and the new `fetch_batch` signature. Landing the types first removes a downstream blocker.

### Step 1: Audit the `ContentFetcher` trait surface (Codex M4-plan-v1 #1, #2, #7)

The skeletal trait shipped in M3 has two methods:
- `fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate` — sync
- `async fn fetch_batch(&self, requests: &[ContentRequest], mode: FetchMode, counter_key: Option<CounterKey>) -> Result<HashMap<PathBuf, Vec<u8>>, CtxfsError>`

**Codex flagged two issues with this shape:**

1. **`fetch_batch` lacks the resolved source/commit needed by the tarball path** (Codex M4-plan-v1 #1). The current `dispatch_fetch_policy` takes `(source, commit_sha, owner, repo, tree_entries, ...)` because `fetch_tarball_into_cache` needs `commit_sha` for the API URL and `source` for the blob-key derivation. Deriving these from `CounterKey` would make telemetry a load-bearing data dependency for fetch logic — wrong layering.

   **Fix:** add `pub struct FetchBatchContext { source: SourceSpec, resolved_revision: String }` to `provider-common::fetcher`. Extend `fetch_batch` signature:
   ```rust
   async fn fetch_batch(
       &self,
       ctx: &FetchBatchContext,
       requests: &[ContentRequest],
       mode: FetchMode,
       counter_key: Option<CounterKey>,
   ) -> Result<HashMap<PathBuf, Vec<u8>>, CtxfsError>;
   ```

2. **The auto-gate (`effective_prefetch_policy` + `decide_policy`) stays in `fetch_snapshot_inner`** (Codex M4-plan-v1 #2). The plan v1's "Auto/Force maps directly to BulkPrefetch/Forced" is wrong — it would lose the threshold-driven behavior and skip oversized telemetry. **Correct flow:**
   - `fetch_snapshot_inner` runs `effective_prefetch_policy(requests, options.prefetch)` → effective `PrefetchPolicy`
   - Calls `decide_policy(blob_count, est_bytes, effective_policy, threshold, cap)` → `FetchPolicy`
   - **Match on `FetchPolicy`:**
     - `Lazy` → no `fetch_batch` call; small-blob prefetch path proceeds as today
     - `LazyOversized {...}` → record `prefetch_skipped_oversized`, log warn, no `fetch_batch` call
     - `Tarball {...}` → derive `FetchMode` from policy (`Auto`→`BulkPrefetch`, `Force`→`Forced`), then `self.fetch_batch(&ctx, &requests, mode, counter_key).await`

   The trait method only handles bulk prefetch; the lazy/oversized paths are GitHub-specific orchestration that lives in `fetch_snapshot_inner`. **No behavior change in M4** — only the call shape of the tarball path is unified through the trait.

3. **Document the return contract** (Codex M4-plan-v1 #7): "best-effort cache state". Add to the trait's doc-comment:

   ```text
   ## Return contract
   
   The returned `HashMap<PathBuf, Vec<u8>>` is best-effort: it contains
   bytes for whatever paths the provider was able to fulfill in this
   call. Missing paths are NOT errors — callers should fall back to
   their per-request fetch path (e.g., `Provider::fetch_blob` for
   GitHub) for paths absent from the map.
   
   GitHub's tarball flow warms `BlobCache` by digest; it may return an
   empty map even on success. Future native-CDN providers (npm, PyPI,
   crates.io content fetchers) may populate the map directly if their
   tarball returns bytes.
   ```

### Step 2: Define `ProviderContext` in `provider-git`

`crates/ctxfs-provider-git/src/context.rs`:

```rust
//! `ProviderContext` collects the daemon-owned `Arc`s and configuration that
//! every Phase-4-shaped provider needs, so `GitHubProvider::new` shrinks to
//! `(token, ctx)`. Replaces the parameter sprawl that emerged across M1–M3
//! (api host, observability, cache, tree-cache, shared tree-cache,
//! singleflight registry).
//!
//! Lives in `ctxfs-provider-git` (not `provider-common`) because
//! `provider-common` cannot depend on `ctxfs-cache` without inverting the
//! existing dep direction (`cache → provider-common`). Future native-CDN
//! providers (npm/PyPI/crates.io) get their own context type adapted to
//! their auth/cache/network needs; the shared structural call (duplicate,
//! extract to a new crate, or migrate `ctxfs-cache` under `provider-common`)
//! is best made with a second concrete consumer in hand — Phase 6 work.

use ctxfs_cache::{BlobCache, SharedTreeCache, TreeCache};
use ctxfs_provider_common::fetcher::TarballSingleflightMap;
use ctxfs_provider_common::observability::Observability;
use std::sync::Arc;

#[derive(Clone)]
pub struct ProviderContext {
    /// API host (e.g. `api.github.com`, or a `http://127.0.0.1:PORT` test
    /// override). Used for both REST URL composition and codeload-host
    /// derivation.
    pub api_host: String,
    /// Daemon-side observability registry (gauges + per-mount counters).
    pub observability: Arc<Observability>,
    /// Blob cache (content-addressable).
    pub cache: Arc<BlobCache>,
    /// Local tree cache (per-commit manifests).
    pub tree_cache: Option<Arc<TreeCache>>,
    /// Optional shared tree cache (Redis-backed cross-process).
    pub shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    /// Singleflight registry for in-flight tarball prefetches.
    pub singleflight: Arc<TarballSingleflightMap>,
}

impl std::fmt::Debug for ProviderContext {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderContext")
            .field("api_host", &self.api_host)
            .field("observability", &"<Arc<Observability>>")
            .field("cache", &"<Arc<BlobCache>>")
            .field("tree_cache", &self.tree_cache.is_some())
            .field("shared_tree_cache", &self.shared_tree_cache.is_some())
            .field("singleflight_len", &self.singleflight.len())
            .finish()
    }
}
```

**Dependency direction confirmed (Codex M4-plan-v1 #3):** `ctxfs-provider-git` already imports `BlobCache`, `SharedTreeCache`, `TreeCache` from `ctxfs-cache` and `Observability` + `TarballSingleflightMap` from `ctxfs-provider-common`. Adding `ProviderContext` here is a no-cost composition; no new dep edges.

### Step 3: Wire module into `lib.rs`

```rust
// crates/ctxfs-provider-git/src/lib.rs
mod context;
mod github;
mod token;

pub use context::ProviderContext;
pub use github::{FetchOptions, GitBlobSha1, GitHubProvider, TreeEntry};
pub use token::{validate_github_token, TokenInfo};
```

### Step 4: Add `FetchBatchContext` to `provider-common::fetcher`

In `crates/ctxfs-provider-common/src/fetcher.rs`:

```rust
/// Source-bound context passed to `ContentFetcher::fetch_batch`. Carries the
/// resolved source spec and revision so providers don't have to derive these
/// from `CounterKey` (which is a telemetry concern, not a data dependency).
#[derive(Debug, Clone)]
pub struct FetchBatchContext {
    /// The source being fetched. Provider-specific fields like `name`
    /// (`owner/repo` for GitHub) are interpreted by the provider.
    pub source: ctxfs_core::source::SourceSpec,
    /// Resolved upstream revision (e.g., 40-char Git commit SHA for GitHub;
    /// version string for npm/PyPI/crates.io). Always concrete, never a ref.
    pub resolved_revision: String,
}
```

Update the trait method signature:

```rust
#[async_trait::async_trait]
pub trait ContentFetcher: Send + Sync {
    fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate;

    /// Fetch the given requests as a single batch. Called by orchestration
    /// code (e.g., `fetch_snapshot_inner` in GitHubProvider) when the
    /// provider's auto-gate elects bulk prefetch.
    ///
    /// ## Return contract
    ///
    /// The returned `HashMap<PathBuf, Vec<u8>>` is best-effort: it contains
    /// bytes for whatever paths the provider fulfilled in this call.
    /// Missing paths are NOT errors — callers should fall back to their
    /// per-request fetch path (e.g., `Provider::fetch_blob` for GitHub)
    /// for paths absent from the map. GitHub's tarball flow warms
    /// `BlobCache` by digest and may return an empty map even on success.
    /// Future native-CDN providers may populate the map directly.
    async fn fetch_batch(
        &self,
        ctx: &FetchBatchContext,
        requests: &[ContentRequest],
        mode: FetchMode,
        counter_key: Option<CounterKey>,
    ) -> Result<std::collections::HashMap<PathBuf, Vec<u8>>, ctxfs_core::error::CtxfsError>;
}
```

### Step 5: Inline tests on `ProviderContext` (in-line `#[cfg(test)]`)

```rust
#[test]
fn provider_context_clones_arcs_correctly() {
    let ctx = make_test_provider_context();
    let cloned = ctx.clone();
    assert!(Arc::ptr_eq(&ctx.cache, &cloned.cache));
    assert!(Arc::ptr_eq(&ctx.observability, &cloned.observability));
}

#[test]
fn provider_context_debug_redacts_arc_contents() {
    let ctx = make_test_provider_context();
    let dbg = format!("{ctx:?}");
    assert!(dbg.contains("api_host"));
    assert!(dbg.contains("<Arc<Observability>>"));
    assert!(dbg.contains("singleflight_len"));
    // Sanity: no token would appear here (none are stored on ProviderContext)
}
```

### Step 6: Verify

```bash
cargo build
cargo test -p ctxfs-provider-common fetcher
cargo test -p ctxfs-provider-git context
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

### Step 7: Commit

```bash
git add crates/ctxfs-provider-common/src/fetcher.rs \
        crates/ctxfs-provider-git/src/context.rs \
        crates/ctxfs-provider-git/src/lib.rs

git commit -m "$(cat <<'EOF'
feat(provider-common,provider-git): FetchBatchContext + ProviderContext

Two new types, one in each crate, supporting M4's full lift:

- FetchBatchContext { source, resolved_revision } in provider-common's
  fetcher module. Extends the ContentFetcher::fetch_batch signature so
  providers know what source + commit they're fetching for. Telemetry
  (CounterKey) is no longer a load-bearing data dependency for fetch
  logic.

- ProviderContext { api_host, observability, cache, tree_cache,
  shared_tree_cache, singleflight } in provider-git. Daemons build
  one and hand it to GitHubProvider::new alongside the auth token,
  shrinking that constructor from 7 args to 2.

ProviderContext lives in provider-git, not provider-common, because
provider-common cannot depend on ctxfs-cache without inverting the
existing dep direction (cache → provider-common). When future
native-CDN providers (npm/PyPI/crates.io) ship in Phase 6, the call
between (1) duplicate the struct, (2) extract to a new
ctxfs-provider-context crate, or (3) migrate ctxfs-cache under
provider-common is best made with a second concrete consumer in hand.

Debug impl on ProviderContext elides Arc contents so logs don't dump
unbounded inner state.

ContentFetcher::fetch_batch's HashMap return is documented as
"best-effort cache state" — missing paths are allowed; GitHub's
tarball flow may return an empty map while warming BlobCache by
digest. (Codex M4-plan-v1 #1 + #7.)
EOF
)"
```

---

## Task 2: Collapse `GitHubProvider::new` and `new_with_codeload_host` via `ProviderContext`

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`
- Modify: `crates/ctxfs-provider-git/tests/common/mod.rs` (Codex M4-plan-v1 #7)
- Modify: `crates/ctxfs-daemon/src/daemon.rs`
- Modify: `crates/ctxfs-nfs/tests/medium_repo.rs`
- Modify: `crates/ctxfs-nfs/tests/nfs_read_path.rs`

### Step 1: Update test for new constructor shape

Existing `github.rs` unit tests (and any inline `make_test_provider` helpers) must be updated. Add a test that uses the new signature:

```rust
#[test]
fn new_with_provider_context_compiles_with_two_args() {
    let ctx = make_test_provider_context();
    let _provider = GitHubProvider::new(None, ctx);
}
```

Update existing `make_test_singleflight()` etc. helpers to construct a `ProviderContext` and pass it.

### Step 2: Refactor `GitHubProvider::new` and `new_with_codeload_host`

Old:
```rust
pub fn new(
    token: Option<&str>,
    api_host: String,
    cache: Arc<BlobCache>,
    tree_cache: Option<Arc<TreeCache>>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    observability: Arc<Observability>,
    tarball_singleflight: Arc<TarballSingleflightMap>,
) -> Self { ... }
```

New:
```rust
pub fn new(token: Option<&str>, ctx: ProviderContext) -> Self {
    Self::new_with_codeload_host(token, None, ctx)
}

pub fn new_with_codeload_host(
    token: Option<&str>,
    codeload_host_override: Option<String>,
    ctx: ProviderContext,
) -> Self {
    // ... extract from ctx; same body as today but reading from ctx fields
}
```

Drop **both** `#[allow(clippy::too_many_arguments)]` annotations.

### Step 3: Update daemon callsite

In `crates/ctxfs-daemon/src/daemon.rs::prepare_mount`:

```rust
let ctx = ProviderContext {
    api_host: self.config.github_host.clone(),
    observability: self.observability.clone(),
    cache: self.cache.clone(),
    tree_cache: Some(self.tree_cache.clone()),
    shared_tree_cache: self.shared_tree_cache.clone(),
    singleflight: self.tarball_singleflight.clone(),
};
let provider = Arc::new(GitHubProvider::new(
    self.config.github_token.as_deref(),
    ctx,
));
```

### Step 4: Update NFS test callsites + provider-git replay-test harness

- `crates/ctxfs-nfs/tests/medium_repo.rs` and `nfs_read_path.rs`: each constructs `GitHubProvider` directly. Update each to build a `ProviderContext` first.
- `crates/ctxfs-provider-git/tests/common/mod.rs`: update `make_provider` (or whatever the existing helper is named) to construct a `ProviderContext` and pass it. Codex M4-plan-v1 #7 flagged: this file's test helpers are easy to miss; explicitly include them in this task's diff scope.
- Inline `github.rs` test helpers (`make_test_singleflight`, etc.): update to return/use `ProviderContext` where they construct `GitHubProvider`.

### Step 5: Verify

```bash
cargo build
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

All call sites updated; workspace stays green.

### Step 6: Commit

```bash
git commit -m "$(cat <<'EOF'
refactor(provider-git): collapse GitHubProvider::new args via ProviderContext

GitHubProvider::new shrinks from 7 args to 2 (token, ctx).
new_with_codeload_host shrinks from 8 to 3 (token, codeload_override, ctx).
Drops both clippy::too_many_arguments allows.

Daemon's prepare_mount and the two NFS integration tests are the only
external callers; each now builds a ProviderContext once and passes it.
The internal struct fields are unchanged; this is a constructor-API
refactor only.

B8 invariant unchanged: prepare_mount still builds a fresh provider
per mount; ctx is cloned from daemon-owned Arcs.
EOF
)"
```

---

## Task 3: Implement `ContentFetcher` for `GitHubProvider`

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`

### Step 1: Add `impl ContentFetcher for GitHubProvider`

```rust
use ctxfs_provider_common::fetcher::{
    ContentFetcher, ContentRequest, CostEstimate, FetchBatchContext, FetchMode,
    FetchPolicy, decide_policy, PrefetchPolicy,
};

#[async_trait::async_trait]
impl ContentFetcher for GitHubProvider {
    fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate {
        let total_bytes: Option<u64> = if requests.iter().any(|r| r.size.is_none()) {
            None
        } else {
            Some(requests.iter().map(|r| r.size.unwrap_or(0)).sum())
        };
        CostEstimate {
            total_bytes,
            request_count: requests.len(),
            fetch_mode: None, // M4 doesn't speculate; M5+ may refine
        }
    }

    async fn fetch_batch(
        &self,
        ctx: &FetchBatchContext,
        requests: &[ContentRequest],
        mode: FetchMode,
        counter_key: Option<CounterKey>,
    ) -> Result<HashMap<PathBuf, Vec<u8>>, CtxfsError> {
        // M4 contract: fetch_batch is only invoked for FetchPolicy::Tarball.
        // The auto-gate (effective_prefetch_policy + decide_policy) lives in
        // fetch_snapshot_inner; Lazy and LazyOversized paths skip this call
        // entirely. Mode tells us why we're here:
        //   - BulkPrefetch: Auto-gate fired; tarball within byte cap
        //   - Forced: Force policy; tarball regardless of byte cap
        //   - Lazy: should not happen here (caller bug if it does — error)
        //
        // ContentRequest entries have path/digest/size/kind; we extract
        // (digest, size, kind) into the (TreeEntry-shaped) inputs the
        // existing fetch_tarball_into_cache + path_to_sha_size code wants.
        //
        // The returned HashMap is best-effort: populated by reading
        // self.cache.get(&request.digest) for each request after the
        // tarball commits land. Missing entries are allowed.
        //
        // ... (impl reuses fetch_tarball_into_cache; full code in Step 2)
    }
}
```

### Step 2: Add `to_request()` and `from_request()` helpers (TreeEntry ↔ ContentRequest)

Inside `impl GitHubProvider`:

```rust
/// Map a tree manifest entry into a ContentRequest. Used by
/// `fetch_snapshot_inner` to bridge GitHub-specific tree shape into the
/// source-agnostic trait surface.
pub(crate) fn tree_entry_to_request(entry: &TreeEntry) -> Option<ContentRequest> {
    if entry.entry_type != "blob" {
        return None;
    }
    let kind = match entry.mode.as_str() {
        "120000" => ContentKind::Symlink,
        // M5: detect LFS pointers; for M3/M4 they pass through as File
        _ => ContentKind::File,
    };
    Some(ContentRequest {
        path: PathBuf::from(&entry.path),
        digest: Some(Digest::from_sha256_hex(&entry.sha)),
        size: entry.size,
        kind,
    })
}
```

### Step 3: Reshape `fetch_snapshot_inner` to call `fetch_batch` only for `FetchPolicy::Tarball`

**Key refinement (Codex M4-plan-v1 #2): the auto-gate stays in `fetch_snapshot_inner`.** `fetch_batch` is only called for `FetchPolicy::Tarball`. Match arms:

```rust
// In fetch_snapshot_inner, after tree fetch + B2 walk:

// Build requests once. Used by both estimate_cost (auto-gate inputs) and
// fetch_batch (tarball execution).
let requests: Vec<ContentRequest> = tree.tree
    .iter()
    .filter_map(Self::tree_entry_to_request)
    .collect();

let blob_count = requests.len() as u64;
let estimated_bytes: u64 = requests.iter().filter_map(|r| r.size).sum();
let effective_policy = Self::effective_prefetch_policy(&requests, options.prefetch);
let decision = decide_policy(
    blob_count,
    estimated_bytes,
    effective_policy,
    options.prefetch_threshold_count,
    options.prefetch_max_bytes,
);

match decision {
    FetchPolicy::Lazy => {
        // No fetch_batch call. Existing small-blob prefetch path proceeds.
    }
    FetchPolicy::LazyOversized { estimated_bytes, blob_count, cap } => {
        if let Some(ref key) = counter_key_clone {
            self.observability
                .counters_for(key.clone())
                .record_prefetch_skipped_oversized();
        }
        tracing::warn!(
            target: "ctxfs.provider.tarball",
            estimated_bytes, blob_count, cap,
            "tarball auto-gate skipped: estimated_bytes > prefetch_max_bytes"
        );
        // No fetch_batch call.
    }
    FetchPolicy::Tarball { .. } => {
        let mode = match effective_policy {
            PrefetchPolicy::Force => FetchMode::Forced,
            _ => FetchMode::BulkPrefetch,
        };
        let batch_ctx = FetchBatchContext {
            source: source.clone(),
            resolved_revision: commit_sha.clone(),
        };
        // Trait dispatch: ContentFetcher::fetch_batch
        let _outcome = self
            .fetch_batch(&batch_ctx, &requests, mode, counter_key_clone.clone())
            .await?;
        // The HashMap return is best-effort; we don't act on it. Bytes
        // landed in BlobCache via fetch_tarball_into_cache's atomic-commit.
    }
}
```

The body of `fetch_batch` reuses the existing tarball flow:

```rust
async fn fetch_batch(
    &self,
    ctx: &FetchBatchContext,
    requests: &[ContentRequest],
    mode: FetchMode,
    counter_key: Option<CounterKey>,
) -> Result<HashMap<PathBuf, Vec<u8>>, CtxfsError> {
    if mode == FetchMode::Lazy {
        return Err(CtxfsError::Provider(
            "fetch_batch called with FetchMode::Lazy; expected BulkPrefetch or Forced".into()
        ));
    }
    let (owner, repo) = owner_repo(&ctx.source)?;
    // Use existing dispatch_fetch_policy machinery (renamed/restructured
    // as needed to take ContentRequest). The body of dispatch_fetch_policy
    // becomes this method's body, with TreeEntry → ContentRequest swap.
    self.dispatch_tarball_for_requests(
        &ctx.source,
        &ctx.resolved_revision,
        owner,
        repo,
        requests,
        mode,
        counter_key,
    ).await?;
    
    // Best-effort return: read BlobCache for each request with a digest.
    let mut bytes_map = HashMap::new();
    for req in requests {
        if let Some(digest) = &req.digest {
            if let Some(bytes) = self.cache.get(digest) {
                let _ = bytes_map.insert(req.path.clone(), bytes);
            }
        }
    }
    Ok(bytes_map)
}
```

`dispatch_fetch_policy` is **renamed** to `dispatch_tarball_for_requests` since it no longer dispatches policy (that's done by the caller); it just executes the tarball path for a request set.

### Step 4: Verify trait works

Add an inline test:
```rust
#[test]
fn estimate_cost_aggregates_request_sizes() {
    let provider = make_test_provider();
    let requests = vec![
        ContentRequest { path: "a.rs".into(), digest: None, size: Some(100), kind: ContentKind::File },
        ContentRequest { path: "b.rs".into(), digest: None, size: Some(200), kind: ContentKind::File },
    ];
    let estimate = provider.estimate_cost(&requests);
    assert_eq!(estimate.total_bytes, Some(300));
    assert_eq!(estimate.request_count, 2);
}

#[test]
fn estimate_cost_returns_none_total_when_any_size_unknown() {
    let provider = make_test_provider();
    let requests = vec![
        ContentRequest { path: "a.rs".into(), digest: None, size: Some(100), kind: ContentKind::File },
        ContentRequest { path: "b.rs".into(), digest: None, size: None, kind: ContentKind::File },
    ];
    let estimate = provider.estimate_cost(&requests);
    assert_eq!(estimate.total_bytes, None);
}
```

### Step 5: Verify

```bash
cargo build
cargo test --workspace
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

### Step 6: Commit

```bash
git commit -m "$(cat <<'EOF'
feat(provider-git): GitHubProvider implements ContentFetcher

Stage 1 GitHub becomes the first concrete impl of the source-agnostic
ContentFetcher trait that landed skeletal in M3. Future native-CDN
providers (npm tarballs, PyPI sdists, crates.io .crate files) will
implement the same trait without re-deriving the auto-gate /
singleflight / counter machinery.

estimate_cost aggregates request sizes (None total when any size is
unknown — same fail-closed semantics as the auto-gate's any_unknown
detection).

fetch_batch reshapes the existing dispatch_fetch_policy logic to take
&[ContentRequest] instead of &[TreeEntry]. fetch_snapshot_inner does
the TreeEntry → ContentRequest mapping once at the GitHub-specific
boundary. The HashMap return contract is "best-effort post-fetch
cache state" — VFS still calls Provider::fetch_blob for read-time
retrieval; this matches the existing tarball-as-cache-warmer flow.
EOF
)"
```

---

## Task 4: `MockContentFetcher` test impl + integration test

**Files:**
- Modify: `crates/ctxfs-provider-common/src/mock.rs` (Codex M4-plan-v1 #7 — alongside existing `MockProvider`)
- Create: `crates/ctxfs-provider-common/tests/content_fetcher_implementable.rs`

### Step 1: Define `MockContentFetcher` in `mock.rs` (alongside `MockProvider`)

In `crates/ctxfs-provider-common/src/mock.rs` (extend the existing module):

```rust
use crate::fetcher::{
    ContentFetcher, ContentRequest, CostEstimate, FetchBatchContext, FetchMode,
};
use crate::counters::CounterKey;
use std::collections::HashMap;
use std::path::PathBuf;

/// Trivial `ContentFetcher` impl for tests. Returns canned bytes for paths
/// in `canned_bytes`; missing paths return None per the trait's
/// best-effort contract. Used to prove the trait is implementable from
/// outside `provider-git` (M4 spec exit-criterion).
#[derive(Debug, Default)]
pub struct MockContentFetcher {
    pub canned_bytes: HashMap<PathBuf, Vec<u8>>,
}

#[async_trait::async_trait]
impl ContentFetcher for MockContentFetcher {
    fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate {
        let total_bytes: Option<u64> = if requests.iter().any(|r| r.size.is_none()) {
            None
        } else {
            Some(requests.iter().filter_map(|r| r.size).sum())
        };
        CostEstimate {
            total_bytes,
            request_count: requests.len(),
            fetch_mode: None,
        }
    }

    async fn fetch_batch(
        &self,
        _ctx: &FetchBatchContext,
        requests: &[ContentRequest],
        _mode: FetchMode,
        _counter_key: Option<CounterKey>,
    ) -> Result<HashMap<PathBuf, Vec<u8>>, ctxfs_core::error::CtxfsError> {
        Ok(requests
            .iter()
            .filter_map(|r| {
                self.canned_bytes
                    .get(&r.path)
                    .map(|b| (r.path.clone(), b.clone()))
            })
            .collect())
    }
}
```

### Step 2: Integration test that proves trait usability

`crates/ctxfs-provider-common/tests/content_fetcher_implementable.rs`:

```rust
//! Proves the ContentFetcher trait can be implemented from a crate that
//! does NOT depend on provider-git. Spec exit-criterion: a hypothetical
//! second provider can plug in without touching provider-git.

use ctxfs_core::source::{ProviderType, SourceSpec};
use ctxfs_provider_common::fetcher::{
    ContentFetcher, ContentKind, ContentRequest, FetchBatchContext, FetchMode,
};
use ctxfs_provider_common::mock::MockContentFetcher;
use std::path::PathBuf;

fn test_ctx() -> FetchBatchContext {
    FetchBatchContext {
        source: SourceSpec {
            provider_type: ProviderType::GitHub,
            name: "owner/repo".to_string(),
            version: "main".to_string(),
            subpath: None,
        },
        resolved_revision: "abcdef0123456789abcdef0123456789abcdef01".to_string(),
    }
}

#[tokio::test]
async fn mock_fetcher_returns_canned_bytes() {
    let mut canned = std::collections::HashMap::new();
    let _ = canned.insert(PathBuf::from("a.rs"), b"contents".to_vec());

    let fetcher = MockContentFetcher { canned_bytes: canned };
    let requests = vec![ContentRequest {
        path: PathBuf::from("a.rs"),
        digest: None,
        size: Some(8),
        kind: ContentKind::File,
    }];

    let bytes_map = fetcher
        .fetch_batch(&test_ctx(), &requests, FetchMode::BulkPrefetch, None)
        .await
        .unwrap();
    assert_eq!(bytes_map.get(&PathBuf::from("a.rs")), Some(&b"contents".to_vec()));
}

#[tokio::test]
async fn mock_fetcher_missing_path_returns_empty_map_not_error() {
    let fetcher = MockContentFetcher { canned_bytes: Default::default() };
    let requests = vec![ContentRequest {
        path: PathBuf::from("absent.rs"),
        digest: None,
        size: Some(0),
        kind: ContentKind::File,
    }];

    let bytes_map = fetcher
        .fetch_batch(&test_ctx(), &requests, FetchMode::BulkPrefetch, None)
        .await
        .unwrap();
    assert!(bytes_map.is_empty(), "missing paths produce empty map, not error");
}

#[test]
fn mock_fetcher_estimate_cost_sums_sizes() {
    let fetcher = MockContentFetcher { canned_bytes: Default::default() };
    let requests = vec![
        ContentRequest { path: PathBuf::from("a.rs"), digest: None, size: Some(100), kind: ContentKind::File },
        ContentRequest { path: PathBuf::from("b.rs"), digest: None, size: Some(200), kind: ContentKind::File },
    ];
    let est = fetcher.estimate_cost(&requests);
    assert_eq!(est.request_count, 2);
    assert_eq!(est.total_bytes, Some(300));
}
```

### Step 3: Verify

```bash
cargo test -p ctxfs-provider-common
cargo build
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

### Step 4: Commit

```bash
git commit -m "$(cat <<'EOF'
test(provider-common): MockContentFetcher proves ContentFetcher implementability

Spec exit-criterion (M4): a hypothetical second provider can implement
ContentFetcher without touching provider-git. MockContentFetcher is
that proof — a trivial impl in provider-common that returns canned
bytes from a HashMap. The integration test in
content_fetcher_implementable.rs exercises it.

Future npm/PyPI/crates.io content providers will follow this shape.
EOF
)"
```

---

## Task 5: B8 invariant test (UNIT test inside `daemon.rs`)

**Files:**
- Modify: `crates/ctxfs-daemon/src/daemon.rs` — add private helper `build_github_provider_for_mount`; add inline `#[cfg(test)]` B8 test

### Step 1: Test design (Codex M4-plan-v1 #4)

**Critical revision from plan v1:** an integration test in `crates/ctxfs-daemon/tests/` will NOT compile cleanly because `DaemonServer`, `MountPrep`, and `prepare_mount` are private. `pub(crate)` doesn't reach `tests/` (separate compilation unit).

**Correct approach:** UNIT test inside `daemon.rs` plus a private helper `build_github_provider_for_mount` that:
- Takes the source / token / config inputs needed to construct the provider
- Returns `Arc<GitHubProvider>`
- Is called by `prepare_mount`
- Is callable from the inline `#[cfg(test)] mod tests` block

This isolates the construction concern from the network-dependent flow of `prepare_mount`. The test asserts: two consecutive helper calls produce distinct `Arc<GitHubProvider>` instances. Even if they share the singleflight registry (they should — that's its purpose), the provider Arc itself must differ — sharing a single provider would re-introduce the `active_source` race.

### Step 2: Extract `build_github_provider_for_mount` helper

In `crates/ctxfs-daemon/src/daemon.rs`:

```rust
impl DaemonServer {
    /// Construct a fresh `GitHubProvider` for a single mount.
    ///
    /// **B8 invariant** (M4): every mount must get its own provider Arc.
    /// Sharing a provider across mounts re-introduces the `active_source`
    /// race. The singleflight registry IS shared (passed by Arc-clone via
    /// ProviderContext); the provider itself is not.
    fn build_github_provider_for_mount(&self) -> Arc<GitHubProvider> {
        let ctx = ProviderContext {
            api_host: self.config.github_host.clone(),
            observability: self.observability.clone(),
            cache: self.cache.clone(),
            tree_cache: Some(self.tree_cache.clone()),
            shared_tree_cache: self.shared_tree_cache.clone(),
            singleflight: self.tarball_singleflight.clone(),
        };
        Arc::new(GitHubProvider::new(
            self.config.github_token.as_deref(),
            ctx,
        ))
    }
}
```

`prepare_mount` calls this helper instead of inlining the construction.

### Step 3: Add the B8 unit test in `#[cfg(test)] mod tests`

```rust
#[cfg(test)]
mod tests {
    use super::*;
    
    fn make_test_daemon() -> DaemonServer {
        // Reuse existing test-construction helpers if any; otherwise build
        // a minimal DaemonServer with tempdir-backed BlobCache, default
        // observability, empty singleflight registry.
        // ...
    }

    /// B8: every call to `build_github_provider_for_mount` returns a fresh
    /// `Arc<GitHubProvider>`. Sharing a provider across mounts would
    /// re-introduce the active_source race.
    #[test]
    fn b8_per_mount_provider_arcs_are_distinct() {
        let daemon = make_test_daemon();
        let p1 = daemon.build_github_provider_for_mount();
        let p2 = daemon.build_github_provider_for_mount();
        assert!(
            !Arc::ptr_eq(&p1, &p2),
            "B8 violation: build_github_provider_for_mount returned the same Arc twice"
        );
    }

    /// Stronger: even a third call still produces a fresh Arc. Catches a
    /// hypothetical regression where someone caches the first Arc.
    #[test]
    fn b8_three_mount_provider_arcs_all_distinct() {
        let daemon = make_test_daemon();
        let p1 = daemon.build_github_provider_for_mount();
        let p2 = daemon.build_github_provider_for_mount();
        let p3 = daemon.build_github_provider_for_mount();
        assert!(!Arc::ptr_eq(&p1, &p2));
        assert!(!Arc::ptr_eq(&p2, &p3));
        assert!(!Arc::ptr_eq(&p1, &p3));
    }

    /// The singleflight registry IS shared (so two concurrent mounts of
    /// the same (repo, commit) dedupe to one tarball download). Verify
    /// this complement of B8 — sharing the registry is intentional.
    #[test]
    fn singleflight_registry_arc_is_shared_across_providers() {
        let daemon = make_test_daemon();
        let registry_initial = daemon.tarball_singleflight.clone();
        let _p1 = daemon.build_github_provider_for_mount();
        let _p2 = daemon.build_github_provider_for_mount();
        // After two provider builds, the registry Arc count should be ≥ 3
        // (daemon + 2 providers). This proves the registry was cloned-by-Arc,
        // not copied or replaced.
        assert!(
            Arc::strong_count(&registry_initial) >= 3,
            "singleflight registry must be Arc-cloned into every provider"
        );
    }
}
```

### Step 4: Verify

```bash
cargo test -p ctxfs-daemon b8
cargo test -p ctxfs-daemon singleflight
```

### Step 3: Verify

```bash
cargo test -p ctxfs-daemon b8
```

### Step 4: Commit

```bash
git commit -m "$(cat <<'EOF'
test(daemon): B8 invariant — per-mount provider construction

Spec exit-criterion (M4): a CI test asserts GitHubProvider::new is
called from the per-mount path; no shared/global fetcher leaks across
mounts.

Two prepare_mount calls produce distinct Arc<GitHubProvider> instances.
The singleflight tarball registry CAN be shared across mounts (it's a
DashMap of OnceCell slots — that's its purpose), but the provider
itself MUST stay per-mount because GitHubProvider holds active_source
and counter_key state that would race if shared.
EOF
)"
```

---

## Task 6: Carry-forward cleanups (DEFER L2 and F5 per Codex review)

**Codex M4-plan-v1 #5 + #6: defer both L2 and F5.**

- **L2 (panic-as-Result on `Client::builder`)**: deferred. Changing `GitHubProvider::new` to return `Result<Self, CtxfsError>` changes daemon mount-failure semantics versus the current panic path. That's useful but **not** a no-behavior-change M4 refactor. Land in M5 (lighter test risk) or Phase-5 perf.
- **F5 (`SlotClaim` Drop impl)**: deferred. The naive Drop impl interacts badly with `OnceCell::get_or_init` cancellation: a waiter can become the de-facto initializer on the *old* slot while Drop removes the registry entry, causing duplicate fetches. A *guarded* Drop impl is feasible (private `released` flag, `Drop` cleanup only when `is_leader && !released && slot.cell.get().is_none()`), but the design needs care. Land in a future milestone with explicit cancellation-cleanup focus.

**M4 ships ZERO cleanup-pass carry-forwards.** This keeps the milestone tightly scoped to its spec deliverables (full ContentFetcher lift + ProviderContext + B8 test + MockContentFetcher). Cleaner story for review.

### Defer to Phase 5 perf or M5:
- L2 (panic-as-Result) — M5 candidate
- F5 (SlotClaim Drop) — needs guarded design; defer
- M5 quality (format!("{e}") in dispatch OnceCell) — M5 cleanup
- M6 quality (dispatch_fetch_policy nested match) — M4 trait lift partially flattens this naturally; remaining residue is M5 cleanup
- L3 (numbered comments) — M4 trait lift will reshape `dispatch_tarball_for_requests`
- F2 (BlobTempWriter BufWriter) — perf, low yield
- F4 (fetch_tree_walked FuturesUnordered) — perf
- F6 (update_gauge auth_identity clone) — perf
- HeaderMap-direct refactor — Phase-5 perf
- env_var_* test race — M5

**No commit for this task.** Section retained as documentation of scope decisions.

---

## Task 7: Workspace verify + CHANGELOG + tag `v0.1.4-m4`

### Step 1: Full release build + tests + fmt + clippy

```bash
cargo build --release
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
cargo test 2>&1 | tail -50
```

Expected: green except documented pre-existing failures.

### Step 2: M1 status_bench regression check

```bash
cargo test --release -p ctxfs --test status_bench -- --ignored --nocapture
```

### Step 3: CHANGELOG

Prepend to `CHANGELOG.md`:

```markdown
## v0.1.4-m4 — 2026-04-XX

### Phase 4 M4: ContentFetcher full lift + ProviderContext refactor

- `ContentFetcher` trait in `ctxfs-provider-common` now has its first
  concrete impl: `GitHubProvider`. Future native-CDN content providers
  (npm tarballs, PyPI sdists, crates.io `.crate` files) implement the
  same trait without re-deriving the auto-gate / singleflight /
  observability machinery.
- `MockContentFetcher` test impl proves the trait is implementable from
  outside `provider-git`.
- `GitHubProvider::new` constructor collapsed from 7 args to 2 via a new
  `ProviderContext` struct that bundles the daemon-owned `Arc`s
  (cache, tree-cache, observability, singleflight, api_host).
  `new_with_codeload_host` shrinks from 8 to 3.
- B8 invariant test: per-mount provider construction in `prepare_mount`
  is now CI-asserted.
- No external behavior change (pure refactor); replay tests + existing
  test suite green.
```

### Step 4: Tag

```bash
git tag -a v0.1.4-m4 -m "Phase 4 M4: ContentFetcher full lift + ProviderContext refactor"
```

(Tag is local only; not pushed until end of M5 per user instruction.)

### Step 5: Verify tag

```bash
git tag -l v0.1.4-m4
git rev-list -n 1 v0.1.4-m4
```

---

## Self-review checklist

- [ ] **Spec coverage:** ContentFetcher full lift (Tasks 1, 3, 4); ProviderContext (Tasks 1, 2); MockContentFetcher (Task 4); B8 CI test (Task 5). All M4 spec deliverables.
- [ ] **`Provider` trait unchanged:** the daemon-VFS contract (snapshot/blob/directory) is untouched; M4 only adds the `ContentFetcher` layer.
- [ ] **VFS untouched:** `VfsState` still uses `SharedProvider = Arc<dyn Provider>`; no migration to `dyn ContentFetcher` in M4.
- [ ] **B8 constraint preserved:** `prepare_mount` still constructs `Arc<GitHubProvider>` per mount via `build_github_provider_for_mount`; **unit** test in daemon.rs asserts the invariant. Singleflight-registry-shared complement test included.
- [ ] **No external behavior change:** existing tests (replay suite, build_directories, NFS integration) all pass. **The auto-gate stays in `fetch_snapshot_inner`; `fetch_batch` is only called for `FetchPolicy::Tarball`** (Codex M4-plan-v1 #2).
- [ ] **No new public API surface beyond:** `ProviderContext` (in provider-git), `FetchBatchContext` (in provider-common), `MockContentFetcher` (in provider-common::mock), `ContentFetcher` impl on `GitHubProvider` (the trait already existed).
- [ ] **`fetch_batch` return contract documented:** "best-effort cache state"; missing paths allowed (Codex M4-plan-v1 #7).
- [ ] **Lints clean:** `clippy::all = deny`, `pedantic = warn`; both `#[allow(clippy::too_many_arguments)]` annotations on `GitHubProvider::new` constructors removed.
- [ ] **Workspace deps:** no new external deps. `ctxfs-provider-git`'s `Cargo.toml` unchanged; `ProviderContext` uses existing types.
- [ ] **No M5 work:** B3-label, B5 cache reservation, B6 LFS detect, full constructor-Result propagation, `SlotClaim` Drop impl are M5/Phase-5.
- [ ] **Test helpers updated:** `tests/common/mod.rs`, inline `github.rs` test helpers, NFS integration tests all build `ProviderContext` first (Codex M4-plan-v1 #7).

---

## Future work (M5 picks up)

- **B3-label**: `HashAlgorithm::Sha1` variant in `ctxfs-core`; rename `from_sha256_hex` callsites that store SHA-1 hex.
- **B5**: Per-repo cache reservation in `ctxfs-cache`. Locked invariant: active repo with working set ≤ reservation receives zero evictions from other repos.
- **B6**: LFS pointer detect-and-surface in `ctxfs status`. (Full LFS smudge stays in Phase 5.)
- **L2 (panic-as-Result)**: if M4 deferred this, M5's lighter constructor changes are a natural place.
- **`env_var_*` test race**: pick up in M5.

---

## References

- Spec: `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` § M4 (lines 334-344)
- M3 plan v2 (Codex-reviewed): `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md`
- M3 handoff: `docs/phase4-m3-handoff.md`
- M4 handoff (predecessor of this plan): `docs/phase4-m4-handoff.md`
- M3 per-engineer-rotation handoffs: `docs/m3-handoffs/{engineer-T1,engineer-T5,engineer-T6}.md`
- Cmux-team skill: `~/.claude/skills/cmux-team/SKILL.md`
- Most recent Codex M3-result review: `/tmp/counsel/20260429-075755-claude-to-codex-470128/codex.md`
- `ContentFetcher` trait current state: `crates/ctxfs-provider-common/src/fetcher.rs`
- `Provider` trait current state: `crates/ctxfs-core/src/provider.rs`
