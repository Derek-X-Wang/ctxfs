# Phase 4 — M2: Architecture-Neutral REST Fixes (B1, B4, B7) — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Plan version:** v2 (Codex-reviewed 2026-04-26 via counsel; review at `/tmp/counsel/20260426-043038-claude-to-codex-8945a1/codex.md`).

**Goal:** Fix the three architecture-neutral defects in `ctxfs-provider-git` (B1 tiny-file inlining, B4 secondary-throttle classification → clean error surface to user-space, B7 symlink target resolution) using M1's observability substrate. Adopt the M1 `ThrottleClassifier` and `RateLimitGauge` so secondary throttles surface as clean retryable errors at every layer (provider → VFS → NFS/FSKit), not as EIO. Implement a small-blobs-prefetch path during the tree walk so files ≤4 KB and symlink targets are fetched in parallel and inlined into the manifest, eliminating per-blob fetches on read. Bump the tree-cache schema version so pre-M2 cached snapshots don't bypass the new path.

**Architecture:**
- `Observability` moves from `ctxfs-daemon` to `ctxfs-provider-common` (single commit; no broken-build state) so providers can use it without a dep cycle.
- `GitHubProvider` gains `Arc<Observability>`, `AuthIdentity`, and a per-mount `CounterKey`. `check_rate_limit` is replaced with a `ThrottleClassifier`-driven path that updates the gauge AND increments `rest_calls_total` per quota-bearing call.
- A new `prefetch_small_blobs` helper runs after tree fetch, fires up to 8 concurrent (deduped) HTTP requests for ≤4 KB blobs + symlink-mode blobs. Files: best-effort with `prefetch_failures` counter on per-blob errors. **Symlinks: prefetch failures fail the snapshot** (since `readlink` has no lazy provider path).
- `build_directories_with_inline` accepts a `&HashMap<sha, Vec<u8>>` and populates `inline_content` (B1) and `target` (B7). The existing `build_directories` (no inline) stays for backward compat in tests.
- VFS gains a `VfsError::RateLimited { retry_after_secs }` variant. Adapters map it: NFS → `NFS3ERR_JUKEBOX`, FSKit → `EAGAIN` (or `NSPOSIXErrorDomain.EAGAIN` per FSKit conventions). This is what makes the spec's "zero EIO" criterion actually verifiable end-to-end.
- `TreeCache` schema version bumped from 1 → 2 to invalidate snapshots that pre-date inline_content / symlink-target population.

**Tech Stack:** Rust 2021, reqwest async HTTP, `futures::stream::FuturesUnordered` (parallel-capped), M1's `ctxfs_provider_common::{rate_limit, counters, observability}`. Workspace lints inherited.

**Spec reference:** `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` § Bug Inventory (B1, B4, B7), § Milestones (M2). Exit criteria, sharpened by Codex review:
- After B1: read-time blob calls for files ≤4 KB drops to 0 on the test corpus.
- After B4: zero EIOs (NFS3ERR_IO / FSKit EIO) surfaced under simulated 429 + Retry-After responses; the user sees a retryable signal at every layer.
- After B7: symlink targets in the test corpus equal the source repo's actual symlink targets (string equality, not just non-empty).

---

## File Structure

```
crates/
  ctxfs-provider-common/
    src/
      observability.rs                  # CREATE: moved from ctxfs-daemon
      lib.rs                            # MODIFY: pub mod observability
  ctxfs-daemon/
    src/
      observability.rs                  # DELETE
      lib.rs                            # MODIFY: pub use ctxfs_provider_common::observability
      daemon.rs                         # MODIFY: pass Arc<Observability> to GitHubProvider::new
  ctxfs-provider-git/
    src/
      github.rs                         # MODIFY: plumb Observability, classify_response, prefetch, B1/B7
    tests/
      build_directories.rs              # MODIFY: extend with B1 inline + B7 symlink target tests
    Cargo.toml                          # MODIFY: add ctxfs-provider-common, futures
  ctxfs-vfs/
    src/
      error.rs                          # MODIFY: add VfsError::RateLimited variant
      state.rs                          # MODIFY: map CtxfsError::RateLimited → VfsError::RateLimited
  ctxfs-nfs/
    src/
      fs.rs                             # MODIFY: map VfsError::RateLimited → NFS3ERR_JUKEBOX
    tests/
      medium_repo.rs                    # MODIFY: GitHubProvider::new signature change
      nfs_read_path.rs                  # MODIFY: GitHubProvider::new signature change
  ctxfs-fskit/
    src/
      adapter.rs                        # MODIFY: map VfsError::RateLimited → EAGAIN
  ctxfs-cache/
    src/
      tree.rs                           # MODIFY: bump TREE_SCHEMA_VERSION
```

---

## Task 1: Move `Observability` to `provider-common` + plumb `GitHubProvider` (single commit)

**Files:**
- Create: `crates/ctxfs-provider-common/src/observability.rs`
- Modify: `crates/ctxfs-provider-common/src/lib.rs`
- Delete: `crates/ctxfs-daemon/src/observability.rs`
- Modify: `crates/ctxfs-daemon/src/lib.rs`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`
- Modify: `crates/ctxfs-provider-git/Cargo.toml`
- Modify: `crates/ctxfs-provider-git/src/github.rs`
- Modify: `crates/ctxfs-nfs/tests/medium_repo.rs`
- Modify: `crates/ctxfs-nfs/tests/nfs_read_path.rs`

This task is the foundational refactor — everything else builds on it. Combined into one commit (no broken-build state).

- [ ] **Step 1: Copy `observability.rs` from daemon to provider-common**

Copy the file. Update the imports inside the new copy from `use ctxfs_provider_common::counters::...` etc. to `use crate::counters::...`, `use crate::rate_limit::...`, `use crate::status::...`.

- [ ] **Step 2: Wire the module into provider-common's lib.rs**

Modify `crates/ctxfs-provider-common/src/lib.rs` to add `pub mod observability;` (alphabetical, after `mock`):

```rust
pub mod counters;
pub mod http;
pub mod mock;
pub mod observability;
pub mod rate_limit;
pub mod repo_url;
pub mod resolver;
pub mod status;
```

- [ ] **Step 3: Delete the daemon's local `observability.rs`**

```bash
git rm crates/ctxfs-daemon/src/observability.rs
```

- [ ] **Step 4: Re-export from daemon for backward compat**

Modify `crates/ctxfs-daemon/src/lib.rs`. Replace `pub mod observability;` with:

```rust
pub use ctxfs_provider_common::observability;
```

- [ ] **Step 5: Add `ctxfs-provider-common` dep to `provider-git`**

In `crates/ctxfs-provider-git/Cargo.toml`, add to `[dependencies]` (alphabetical):

```toml
ctxfs-provider-common = { workspace = true }
```

- [ ] **Step 6: Add `Arc<Observability>` + `AuthIdentity` + `counter_key` fields to `GitHubProvider`**

In `crates/ctxfs-provider-git/src/github.rs`, add imports near the top:

```rust
use ctxfs_provider_common::counters::CounterKey;
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_common::rate_limit::AuthIdentity;
```

Modify the struct:

```rust
pub struct GitHubProvider {
    client: reqwest::Client,
    cache: Arc<BlobCache>,
    tree_cache: Option<Arc<TreeCache>>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    observability: Arc<Observability>,
    auth_identity: AuthIdentity,
    /// Set in fetch_snapshot AFTER resolve_ref, using the resolved commit SHA
    /// (not source.version). Read by check_rate_limit and fetch_blob to attribute
    /// counters to the right (source, repo, commit, mount_id) bucket.
    counter_key: std::sync::Mutex<Option<CounterKey>>,
    active_source: std::sync::Mutex<Option<SourceSpec>>,
}
```

Modify `GitHubProvider::new`:

```rust
pub fn new(
    token: Option<&str>,
    cache: Arc<BlobCache>,
    tree_cache: Option<Arc<TreeCache>>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    observability: Arc<Observability>,
) -> Self {
    let auth_identity = match token {
        Some(t) => AuthIdentity::pat("api.github.com", t),
        None => AuthIdentity::anonymous("api.github.com"),
    };

    let mut default_headers = HeaderMap::new();
    let _ = default_headers.insert(ACCEPT, "application/vnd.github.v3+json".parse().unwrap());
    if let Some(token) = token {
        let _ = default_headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
    }

    let client = reqwest::Client::builder()
        .user_agent(USER_AGENT_STR)
        .default_headers(default_headers)
        .build()
        .expect("failed to build HTTP client");

    Self {
        client,
        cache,
        tree_cache,
        shared_tree_cache,
        observability,
        auth_identity,
        counter_key: std::sync::Mutex::new(None),
        active_source: std::sync::Mutex::new(None),
    }
}
```

- [ ] **Step 7: Update daemon construction site**

In `crates/ctxfs-daemon/src/daemon.rs`, find the `GitHubProvider::new(...)` call (around line 458). Append the new arg:

```rust
let provider = Arc::new(GitHubProvider::new(
    token.as_deref(),
    cache.clone(),
    Some(tree_cache.clone()),
    shared_tree_cache.clone(),
    self.observability.clone(),
));
```

- [ ] **Step 8: Update NFS test callsites**

In `crates/ctxfs-nfs/tests/medium_repo.rs` (around line 32) and `crates/ctxfs-nfs/tests/nfs_read_path.rs` (around line 36), find the `GitHubProvider::new` call. Add an `Arc::new(Observability::new())` argument:

```rust
let provider = GitHubProvider::new(
    token,
    cache,
    None,
    None,
    Arc::new(ctxfs_provider_common::observability::Observability::new()),
);
```

If these test files don't already have `ctxfs-provider-common` as a dev-dep, add it to `crates/ctxfs-nfs/Cargo.toml` `[dev-dependencies]`.

- [ ] **Step 9: Build + test the workspace**

```bash
cargo build
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

All must be green. The pre-existing test failures (`mount_server_only_starts_nfs_and_reports_port`, `env_var_invalid_falls_through_to_config`) remain expected.

- [ ] **Step 10: Commit**

```bash
git add crates/ctxfs-provider-common/src/observability.rs \
        crates/ctxfs-provider-common/src/lib.rs \
        crates/ctxfs-daemon/src/observability.rs \
        crates/ctxfs-daemon/src/lib.rs \
        crates/ctxfs-daemon/src/daemon.rs \
        crates/ctxfs-provider-git/Cargo.toml \
        crates/ctxfs-provider-git/src/github.rs \
        crates/ctxfs-nfs/Cargo.toml \
        crates/ctxfs-nfs/tests/medium_repo.rs \
        crates/ctxfs-nfs/tests/nfs_read_path.rs

git commit -m "$(cat <<'EOF'
refactor(observability): move from daemon to provider-common; plumb GitHubProvider

ctxfs-provider-git cannot depend on ctxfs-daemon (daemon already depends
on provider-git, concretely constructs GitHubProvider). Observability is
logically a provider-common primitive — it holds gauges keyed by
AuthIdentity x ResourceClass and counters keyed by CounterKey, both of
which already live in provider-common.

GitHubProvider now holds:
- Arc<Observability> for gauge/counter access
- AuthIdentity computed once from the token at construction time
- counter_key: Mutex<Option<CounterKey>> set in fetch_snapshot AFTER
  resolve_ref, using the resolved commit_sha (not source.version) so
  attribution matches the actual content fetched.

Daemon re-exports for callsite compat. NFS test callsites updated to
pass an Arc<Observability>::new(). Single commit; no broken-build state.
EOF
)"
```

---

## Task 2: Adopt `ThrottleClassifier` + gauge updates + `rest_calls_total` (B4 part 1)

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`

This task replaces `check_rate_limit` with a path that:
- Records `rest_calls_total` per quota-bearing API call (so M1 counters can support M2's before/after claims).
- Updates the daemon-side `RateLimitGauge` from response headers (`update_gauge`).
- Sets `secondary_throttle_state` on the gauge when classifier returns `SecondaryThrottle`.
- Returns `CtxfsError::RateLimited` cleanly.

(B4 part 2 — VFS / NFS / FSKit mapping — is Task 3.)

- [ ] **Step 1: Add unit tests for `classify_response`**

In the existing `#[cfg(test)] mod tests` block in `github.rs`, add:

```rust
    #[test]
    fn classify_response_secondary_throttle_with_remaining_nonzero_returns_rate_limited() {
        use std::collections::HashMap;
        let mut headers = HashMap::new();
        let _ = headers.insert("retry-after".to_string(), "60".to_string());
        let _ = headers.insert("x-ratelimit-remaining".to_string(), "4500".to_string());
        let _ = headers.insert("x-ratelimit-resource".to_string(), "core".to_string());

        let err = GitHubProvider::classify_response(429, &headers).unwrap_err();
        match err {
            CtxfsError::RateLimited { retry_after_secs } => assert_eq!(retry_after_secs, 60),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classify_response_primary_exhausted_returns_rate_limited() {
        use std::collections::HashMap;
        let mut headers = HashMap::new();
        let _ = headers.insert("x-ratelimit-remaining".to_string(), "0".to_string());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let _ = headers.insert("x-ratelimit-reset".to_string(), (now + 120).to_string());

        let err = GitHubProvider::classify_response(403, &headers).unwrap_err();
        match err {
            CtxfsError::RateLimited { retry_after_secs } => {
                assert!(retry_after_secs > 100 && retry_after_secs <= 120);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classify_response_ok_returns_ok() {
        use std::collections::HashMap;
        let mut headers = HashMap::new();
        let _ = headers.insert("x-ratelimit-remaining".to_string(), "100".to_string());
        let _ = headers.insert("x-ratelimit-resource".to_string(), "core".to_string());
        assert!(GitHubProvider::classify_response(200, &headers).is_ok());
    }
```

- [ ] **Step 2: Run, expect compile failure**

`cargo test -p ctxfs-provider-git classify_response` → method missing.

- [ ] **Step 3: Implement `classify_response` and replace `check_rate_limit`**

Replace the existing `fn check_rate_limit(resp: &reqwest::Response) -> Result<(), CtxfsError>` (around line 205) with two methods:

```rust
    /// Pure-logic classifier on (status, headers map). Unit-testable. Used by
    /// the `check_rate_limit` adapter that operates on a real reqwest::Response.
    fn classify_response(
        status: u16,
        headers: &std::collections::HashMap<String, String>,
    ) -> Result<(), CtxfsError> {
        use ctxfs_provider_common::rate_limit::{RateLimitVerdict, ThrottleClassifier};
        match ThrottleClassifier::classify(status, headers) {
            RateLimitVerdict::Ok { .. } => Ok(()),
            RateLimitVerdict::SecondaryThrottle { retry_after, .. } => Err(CtxfsError::RateLimited {
                retry_after_secs: retry_after.as_secs(),
            }),
            RateLimitVerdict::PrimaryExhausted { reset_at, .. } => {
                let now = std::time::SystemTime::now();
                let secs = reset_at
                    .duration_since(now)
                    .map(|d| d.as_secs())
                    .unwrap_or(60);
                Err(CtxfsError::RateLimited {
                    retry_after_secs: secs,
                })
            }
            RateLimitVerdict::Other { .. } => Ok(()),
        }
    }

    /// Adapter: extracts headers from a reqwest::Response, classifies, updates the
    /// daemon-side gauge, increments rest_calls_total, and records throttle events.
    fn check_rate_limit(&self, resp: &reqwest::Response) -> Result<(), CtxfsError> {
        use ctxfs_provider_common::rate_limit::{RateLimitVerdict, ResourceClass, ThrottleClassifier};

        let status = resp.status().as_u16();
        let headers: std::collections::HashMap<String, String> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|s| (k.as_str().to_lowercase(), s.to_string()))
            })
            .collect();

        // Always increment rest_calls_total for quota-bearing GitHub API calls.
        // (Codeload tarball downloads aren't quota-bearing and don't go through here.)
        if let Some(key) = self.counter_key.lock().unwrap().clone() {
            self.observability.counters_for(key).record_rest_call();
        }

        // Update the gauge from response headers (best-effort; missing headers
        // leave the gauge unchanged per RateLimitGauge::update_from_headers).
        let resource = headers
            .get("x-ratelimit-resource")
            .map(|s| ResourceClass::parse(s))
            .unwrap_or_else(|| ResourceClass::Other("unknown".to_string()));
        self.observability
            .update_gauge(self.auth_identity.clone(), resource.clone(), &headers);

        // Classify and act on secondary throttle.
        let verdict = ThrottleClassifier::classify(status, &headers);
        if let RateLimitVerdict::SecondaryThrottle { retry_after, .. } = verdict {
            // Mark the gauge as secondary-throttled.
            // (RateLimitGauge::set_secondary_throttle takes &mut self; we need
            // to go through the DashMap entry. Add a helper on Observability.)
            self.observability.mark_secondary_throttle(
                self.auth_identity.clone(),
                resource,
                retry_after,
            );
            if let Some(key) = self.counter_key.lock().unwrap().clone() {
                self.observability.counters_for(key).record_throttle_event();
            }
            tracing::warn!(
                target: "ctxfs.provider.throttle",
                retry_after_secs = retry_after.as_secs(),
                "secondary throttle in provider-git"
            );
        }

        Self::classify_response(status, &headers)
    }
```

- [ ] **Step 4: Add `mark_secondary_throttle` helper to `Observability`**

In `crates/ctxfs-provider-common/src/observability.rs`, add a new method on `Observability`:

```rust
    /// Marks the gauge for `(auth, resource)` as secondary-throttled for `retry_after`
    /// from now. Creates the entry if absent. Used by providers when ThrottleClassifier
    /// returns SecondaryThrottle.
    pub fn mark_secondary_throttle(
        &self,
        auth: AuthIdentity,
        resource: ResourceClass,
        retry_after: std::time::Duration,
    ) {
        let mut g = self
            .gauges
            .entry((auth, resource))
            .or_insert_with(RateLimitGauge::unknown);
        g.set_secondary_throttle(retry_after);
    }
```

Add a unit test:

```rust
    #[test]
    fn mark_secondary_throttle_sets_active_state() {
        let o = Observability::new();
        let auth = AuthIdentity::anonymous("api.github.com");
        let resource = ResourceClass::Core;
        o.mark_secondary_throttle(auth.clone(), resource.clone(), std::time::Duration::from_secs(60));
        let g = o.gauge_for(&auth, &resource);
        assert!(matches!(g.secondary_throttle_state, ThrottleState::Active { .. }));
    }
```

- [ ] **Step 5: Update `Self::check_rate_limit` call site to method call**

In `get_json` (around line 130 in `github.rs`), change:

```rust
        Self::check_rate_limit(&resp)?;
```

to:

```rust
        self.check_rate_limit(&resp)?;
```

- [ ] **Step 6: Verify**

```bash
cargo test -p ctxfs-provider-common observability::tests::mark_secondary_throttle
cargo test -p ctxfs-provider-git classify_response
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

All green.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-provider-common/src/observability.rs \
        crates/ctxfs-provider-git/src/github.rs

git commit -m "$(cat <<'EOF'
fix(provider-git,B4): adopt ThrottleClassifier; record rest_calls_total + gauge updates

Replaces the homegrown check_rate_limit (which only fired on remaining=0)
with a ThrottleClassifier-driven path that:
- Records rest_calls_total per quota-bearing API call (so M2 before/after
  claims can be measured via M1 counters).
- Updates the daemon-side RateLimitGauge from response headers via
  Observability::update_gauge.
- Marks the gauge as secondary-throttled when the classifier returns
  SecondaryThrottle (new Observability::mark_secondary_throttle helper).
- Records throttle_events on the per-mount counter.
- Returns CtxfsError::RateLimited for both primary-exhausted and
  secondary-throttle verdicts.

The split between classify_response (header-map input, unit-testable)
and the check_rate_limit method (reqwest::Response adapter, performs
side effects) means classification is testable in isolation.

VFS/NFS/FSKit error mapping (so RateLimited doesn't cascade as EIO at
read time) is the next task.
EOF
)"
```

---

## Task 3: Add `VfsError::RateLimited` + EAGAIN/JUKEBOX adapter mapping (B4 part 2)

**Files:**
- Modify: `crates/ctxfs-vfs/src/error.rs` (or wherever VfsError lives)
- Modify: `crates/ctxfs-vfs/src/state.rs` (provider-error → VfsError mapping)
- Modify: `crates/ctxfs-nfs/src/fs.rs` (VfsError → NFS3ERR mapping)
- Modify: `crates/ctxfs-fskit/src/adapter.rs` (VfsError → FSKit error mapping)

This task makes the spec's "zero EIO" criterion actually verifiable end-to-end. Without it, even though provider-git returns `CtxfsError::RateLimited`, the VFS layer collapses everything into `VfsError::Io` → NFS `NFS3ERR_IO` → user sees EIO.

- [ ] **Step 1: Find current VfsError definition**

```bash
grep -rn "enum VfsError\|VfsError::Io" crates/ctxfs-vfs/src/ | head -5
```

Read the file containing `enum VfsError`. Note current variants.

- [ ] **Step 2: Add `RateLimited` variant**

Modify the `VfsError` enum to add:

```rust
    /// The provider is rate-limited; the read should be retried after
    /// `retry_after_secs`. Mapped to NFS3ERR_JUKEBOX / EAGAIN at adapter
    /// boundaries so it does NOT surface as EIO.
    RateLimited { retry_after_secs: u64 },
```

If `VfsError` has `#[derive(...)]` derives that need updating to accommodate the new variant, update them.

- [ ] **Step 3: Map provider error → VfsError in state.rs**

In `crates/ctxfs-vfs/src/state.rs`, find the conversion from `CtxfsError` to `VfsError` (around line 416, where the spec review noted "VFS currently maps all provider fetch errors to `VfsError::Io`"). Update the mapping to special-case `CtxfsError::RateLimited`:

```rust
            Err(CtxfsError::RateLimited { retry_after_secs }) => Err(VfsError::RateLimited { retry_after_secs }),
            Err(_) => Err(VfsError::Io),
```

(Adapt to the existing match-arm shape.)

- [ ] **Step 4: Map VfsError::RateLimited → NFS3ERR_JUKEBOX**

In `crates/ctxfs-nfs/src/fs.rs`, find where VfsError is mapped to NFS errors (around line 125). Add:

```rust
            VfsError::RateLimited { .. } => NFS3ERR_JUKEBOX,
```

(NFS3ERR_JUKEBOX is the "server is busy, try again" error per RFC 1813. Most NFS clients retry on this, which is exactly what we want for a rate-limited fetch.)

If the existing code uses a constant for NFS3ERR_IO, find or define the equivalent for JUKEBOX. The numeric value is 10008.

- [ ] **Step 5: Map VfsError::RateLimited → EAGAIN in FSKit adapter**

In `crates/ctxfs-fskit/src/adapter.rs`, find where VfsError is mapped (around line 83). Add:

```rust
            VfsError::RateLimited { .. } => libc::EAGAIN.into(),  // adapt to FSKit's error type
```

(FSKit returns NSError; the right pattern is `NSError(domain: NSPOSIXErrorDomain, code: Int(EAGAIN))`. Match the existing patterns in the file.)

- [ ] **Step 6: Add tests**

In `crates/ctxfs-vfs/tests/` (or inline in state.rs), add a unit test:

```rust
    #[test]
    fn provider_rate_limited_maps_to_vfs_rate_limited_not_io() {
        // Construct a VfsState with a fake provider that returns
        // CtxfsError::RateLimited { retry_after_secs: 30 } from fetch_blob.
        // Call read on a path that triggers a blob fetch.
        // Assert the result is Err(VfsError::RateLimited { retry_after_secs: 30 }),
        // NOT Err(VfsError::Io).
        // (Implementation details depend on existing VfsState test harness.)
    }
```

Add similar in `ctxfs-nfs/tests/` asserting `NFS3ERR_JUKEBOX` for a rate-limited mount, and in `ctxfs-fskit/` for `EAGAIN` (skip if FSKit tests are #[cfg(target_os = "macos")] gated and the harness is heavy).

- [ ] **Step 7: Verify**

```bash
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 8: Commit**

```bash
git add crates/ctxfs-vfs/ crates/ctxfs-nfs/ crates/ctxfs-fskit/

git commit -m "$(cat <<'EOF'
fix(vfs,nfs,fskit,B4): propagate RateLimited through VFS to retryable adapter errors

The B4 fix is incomplete without this: provider-git returns
CtxfsError::RateLimited, but VFS was mapping all provider errors to
VfsError::Io, which became NFS3ERR_IO / FSKit EIO at the user-facing
boundary. The user saw "I/O error" not "rate-limited, retry".

New VfsError::RateLimited { retry_after_secs } variant. Mapped to:
- NFS: NFS3ERR_JUKEBOX (per RFC 1813 - server is busy, retry)
- FSKit: EAGAIN (POSIX-conventional retryable)

Most NFS clients retry on JUKEBOX automatically. macOS Finder /
applications retrying after EAGAIN is also natural. The retry_after_secs
value is logged at the adapter level for diagnosis but doesn't
currently feed back to the client (NFSv3 has no retry-after); that's a
future enhancement if needed.

Now M2's "zero EIOs surfaced" exit criterion is end-to-end verifiable.
EOF
)"
```

---

## Task 4: Add `prefetch_small_blobs` helper

**Files:**
- Modify: `Cargo.toml` (workspace)
- Modify: `crates/ctxfs-provider-git/Cargo.toml`
- Modify: `crates/ctxfs-provider-git/src/github.rs`

- [ ] **Step 1: Add `futures` to workspace + provider-git deps**

In root `Cargo.toml` `[workspace.dependencies]` add (if absent):

```toml
futures = "0.3"
```

In `crates/ctxfs-provider-git/Cargo.toml` `[dependencies]` add (if absent):

```toml
futures = { workspace = true }
```

- [ ] **Step 2: Add filter test**

In `github.rs` `tests` module:

```rust
    #[test]
    fn small_blobs_filter_picks_under_4kb_files_and_symlinks() {
        let entries = vec![
            TreeEntry { path: "a.rs".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "aaa".into(), size: Some(100) },
            TreeEntry { path: "big.bin".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "bbb".into(), size: Some(10_000) },
            TreeEntry { path: "link".into(), mode: "120000".into(), entry_type: "blob".into(), sha: "ccc".into(), size: Some(20) },
            TreeEntry { path: "subtree".into(), mode: "040000".into(), entry_type: "tree".into(), sha: "ddd".into(), size: None },
            TreeEntry { path: "dup.rs".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "aaa".into(), size: Some(100) },
        ];
        let shas = GitHubProvider::small_blob_shas(&entries);
        // Sorted + deduped.
        assert_eq!(shas, vec!["aaa".to_string(), "ccc".to_string()]);
    }
```

- [ ] **Step 3: Implement filter (returns Vec<String> deduped, not iterator, so dedup is straightforward)**

```rust
    /// Threshold: files ≤ this byte size are eligible for inline prefetch.
    pub const SMALL_BLOB_THRESHOLD_BYTES: u64 = 4096;

    /// Returns deduplicated SHAs for entries that should be prefetched: any blob
    /// ≤ 4 KB, plus any mode-120000 (symlink) entry regardless of size. Trees
    /// and submodules are excluded. Result is sorted for deterministic ordering.
    fn small_blob_shas(entries: &[TreeEntry]) -> Vec<String> {
        use std::collections::BTreeSet;
        let mut seen = BTreeSet::new();
        for e in entries {
            if e.entry_type != "blob" {
                continue;
            }
            let is_symlink = e.mode == MODE_SYMLINK;
            let is_small = e.size.is_some_and(|s| s <= Self::SMALL_BLOB_THRESHOLD_BYTES);
            if is_symlink || is_small {
                let _ = seen.insert(e.sha.clone());
            }
        }
        seen.into_iter().collect()
    }
```

- [ ] **Step 4: Run filter test, expect pass**

```bash
cargo test -p ctxfs-provider-git small_blobs_filter
```

- [ ] **Step 5: Implement prefetch HTTP path**

Add to `impl GitHubProvider`:

```rust
    /// Maximum concurrent in-flight blob requests during prefetch.
    /// 8 is the GitHub-best-practices recommendation for batched fetches;
    /// higher concurrencies can trip secondary rate limits per
    /// https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api
    const PREFETCH_CONCURRENCY: usize = 8;

    /// Identifies blob SHAs that come from symlink (mode-120000) entries.
    /// Used by prefetch_small_blobs to apply the strict-failure policy to
    /// symlinks (which have no lazy fallback in the read path).
    fn symlink_shas(entries: &[TreeEntry]) -> std::collections::HashSet<String> {
        entries
            .iter()
            .filter(|e| e.entry_type == "blob" && e.mode == MODE_SYMLINK)
            .map(|e| e.sha.clone())
            .collect()
    }

    /// Fetches blob SHAs in `shas` in parallel (capped at PREFETCH_CONCURRENCY)
    /// and returns a map sha → bytes.
    ///
    /// Failure policy:
    /// - Files (non-symlink): per-blob errors logged + counter; SHA omitted from
    ///   the map; caller falls back to lazy fetch on read.
    /// - Symlinks (SHA in `symlink_shas`): per-blob errors **fail the entire
    ///   prefetch** and propagate as the returned error. Symlinks have no
    ///   lazy provider path (readlink returns the stored target string).
    async fn prefetch_small_blobs(
        &self,
        source: &SourceSpec,
        shas: Vec<String>,
        symlink_shas: &std::collections::HashSet<String>,
    ) -> Result<std::collections::HashMap<String, Vec<u8>>, CtxfsError> {
        use futures::stream::{FuturesUnordered, StreamExt};

        let mut results: std::collections::HashMap<String, Vec<u8>> = std::collections::HashMap::new();
        let mut in_flight = FuturesUnordered::new();
        let mut iter = shas.into_iter();

        for _ in 0..Self::PREFETCH_CONCURRENCY {
            if let Some(sha) = iter.next() {
                in_flight.push(self.fetch_blob_with_sha(source, sha));
            }
        }

        while let Some((sha, result)) = in_flight.next().await {
            match result {
                Ok(bytes) => {
                    let _ = results.insert(sha, bytes);
                }
                Err(e) => {
                    // Symlink: fail the prefetch.
                    if symlink_shas.contains(&sha) {
                        return Err(CtxfsError::Provider(format!(
                            "symlink prefetch failed for sha {sha}: {e}"
                        )));
                    }
                    // File: log + counter + skip.
                    if let Some(key) = self.counter_key.lock().unwrap().clone() {
                        self.observability
                            .counters_for(key)
                            .record_prefetch_failure();
                    }
                    tracing::warn!(
                        target: "ctxfs.provider.fetch",
                        sha = sha.as_str(),
                        error = format!("{e:?}").as_str(),
                        "prefetch_small_blobs: per-file fetch failed; falling back to lazy"
                    );
                }
            }
            if let Some(next_sha) = iter.next() {
                in_flight.push(self.fetch_blob_with_sha(source, next_sha));
            }
        }
        Ok(results)
    }

    async fn fetch_blob_with_sha(
        &self,
        source: &SourceSpec,
        sha: String,
    ) -> (String, Result<Vec<u8>, CtxfsError>) {
        let result = self.fetch_blob_content(source, &sha).await;
        (sha, result)
    }
```

- [ ] **Step 6: Verify**

```bash
cargo build -p ctxfs-provider-git
cargo test -p ctxfs-provider-git
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 7: Commit**

```bash
git add Cargo.toml crates/ctxfs-provider-git/Cargo.toml crates/ctxfs-provider-git/src/github.rs

git commit -m "$(cat <<'EOF'
feat(provider-git): small_blob_shas + prefetch_small_blobs (deduped, 8-way concurrency)

Filter picks ≤4KB blobs and mode-120000 symlinks; deduplicates via
BTreeSet so duplicate refs to the same blob aren't refetched.

Prefetcher fires up to 8 concurrent requests via FuturesUnordered.
GitHub's REST best-practices page recommends serializing requests to
avoid secondary throttles; 8 is a balance between throughput and
burst-tolerance. PREFETCH_CONCURRENCY is a const for trivial future
tunability.

Failure policy is split:
- Files: best-effort. Per-blob errors increment prefetch_failures,
  log a warning, and the SHA is omitted from the result map. The
  caller falls back to lazy fetch on read.
- Symlinks: strict. A symlink prefetch failure fails the entire
  snapshot. Reasoning: VFS::readlink has no lazy provider path
  (it just returns the stored target string), so an empty target
  is a silent data-correctness regression.

The fail-strict-on-symlink policy is the safety guarantee that B7
needs to actually be correct end-to-end.
EOF
)"
```

---

## Task 5: `build_directories_with_inline` + wire B1/B7

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`

- [ ] **Step 1: Add B1 + B7 unit tests**

In `tests` module:

```rust
    #[test]
    fn build_directories_inlines_small_files_when_map_provided() {
        let source = make_test_source();
        let entries = vec![
            TreeEntry { path: "small.txt".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "abc".into(), size: Some(10) },
            TreeEntry { path: "big.bin".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "def".into(), size: Some(99_999) },
        ];
        let mut inline = std::collections::HashMap::new();
        let _ = inline.insert("abc".to_string(), b"hello!".to_vec());

        let (_, dirs) = GitHubProvider::build_directories_with_inline(&entries, &source, &inline);
        let root = dirs.get("").unwrap();
        let small_entry = root.entries.iter().find(|e| e.name() == "small.txt").unwrap();
        let big_entry = root.entries.iter().find(|e| e.name() == "big.bin").unwrap();

        match small_entry {
            DirEntry::File(f) => assert_eq!(f.inline_content, Some(b"hello!".to_vec())),
            _ => panic!("expected file"),
        }
        match big_entry {
            DirEntry::File(f) => assert_eq!(f.inline_content, None),
            _ => panic!("expected file"),
        }
    }

    #[test]
    fn build_directories_resolves_symlink_target_from_inline_map() {
        let source = make_test_source();
        let entries = vec![
            TreeEntry { path: "link".into(), mode: "120000".into(), entry_type: "blob".into(), sha: "lnk".into(), size: Some(13) },
        ];
        let mut inline = std::collections::HashMap::new();
        let _ = inline.insert("lnk".to_string(), b"path/to/target".to_vec());

        let (_, dirs) = GitHubProvider::build_directories_with_inline(&entries, &source, &inline);
        let root = dirs.get("").unwrap();
        let link_entry = root.entries.iter().find(|e| e.name() == "link").unwrap();
        match link_entry {
            DirEntry::Symlink(s) => assert_eq!(s.target, "path/to/target"),
            _ => panic!("expected symlink"),
        }
    }

    #[test]
    fn build_directories_without_inline_keeps_target_empty_and_no_inline_content() {
        // Backward-compat: existing build_directories(...) with no inline map
        // produces empty target and no inline_content.
        let source = make_test_source();
        let entries = vec![
            TreeEntry { path: "small.txt".into(), mode: "100644".into(), entry_type: "blob".into(), sha: "abc".into(), size: Some(10) },
            TreeEntry { path: "link".into(), mode: "120000".into(), entry_type: "blob".into(), sha: "lnk".into(), size: Some(13) },
        ];
        let (_, dirs) = GitHubProvider::build_directories(&entries, &source);
        let root = dirs.get("").unwrap();
        let small_entry = root.entries.iter().find(|e| e.name() == "small.txt").unwrap();
        let link_entry = root.entries.iter().find(|e| e.name() == "link").unwrap();

        match small_entry {
            DirEntry::File(f) => assert!(f.inline_content.is_none()),
            _ => panic!("expected file"),
        }
        match link_entry {
            DirEntry::Symlink(s) => assert_eq!(s.target, ""),  // backward-compat
            _ => panic!("expected symlink"),
        }
    }
```

- [ ] **Step 2: Run, expect compile failure**

```bash
cargo test -p ctxfs-provider-git build_directories_inlines_small_files
```

- [ ] **Step 3: Refactor existing `build_directories` to share with new `build_directories_with_inline`**

Add public functions:

```rust
    pub fn build_directories(
        entries: &[TreeEntry],
        source: &SourceSpec,
    ) -> (Digest, HashMap<String, Directory>) {
        Self::build_directories_inner(entries, source, None)
    }

    pub fn build_directories_with_inline(
        entries: &[TreeEntry],
        source: &SourceSpec,
        inline: &std::collections::HashMap<String, Vec<u8>>,
    ) -> (Digest, HashMap<String, Directory>) {
        Self::build_directories_inner(entries, source, Some(inline))
    }
```

Rename the existing implementation body to `build_directories_inner` taking an optional reference. In the symlink branch:

```rust
            let dir_entry = if entry.mode == MODE_SYMLINK {
                let target = inline
                    .and_then(|m| m.get(&entry.sha))
                    .and_then(|bytes| std::str::from_utf8(bytes).ok())
                    .map(String::from)
                    .unwrap_or_default();
                DirEntry::Symlink(SymlinkEntry { name, target })
            } else {
```

In the file branch:

```rust
                    "blob" => {
                        let executable = entry.mode == MODE_EXECUTABLE;
                        let size = entry.size.unwrap_or(0);
                        let inline_content = inline
                            .filter(|_| size <= Self::SMALL_BLOB_THRESHOLD_BYTES)
                            .and_then(|m| m.get(&entry.sha))
                            .cloned();
                        DirEntry::File(FileEntry {
                            name,
                            digest: Digest::from_sha256_hex(&entry.sha),
                            size,
                            executable,
                            inline_content,
                        })
                    }
```

- [ ] **Step 4: Verify**

```bash
cargo test -p ctxfs-provider-git build_directories
cargo test -p ctxfs-provider-git
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-provider-git/src/github.rs

git commit -m "$(cat <<'EOF'
feat(provider-git,B1,B7): build_directories_with_inline populates inline_content + symlink target

Two public functions sharing a private inner implementation:
- build_directories(...) preserves the prior signature; passes None.
- build_directories_with_inline(..., &inline_map) populates from the map.

Files ≤4KB whose SHA appears in the map get inline_content set (B1).
The size guard prevents a misbuilt map from accidentally inlining a
large blob.

Symlinks (mode 120000) decode bytes as UTF-8 into target (B7). Strict
prefetch failure policy (Task 4) ensures the inline map always has
the symlink's target before this function runs in production paths;
backward-compat tests verify build_directories(no inline) still
produces empty target.
EOF
)"
```

---

## Task 6: Wire prefetch into `fetch_snapshot`

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`

- [ ] **Step 1: Update `fetch_snapshot`**

Find `fetch_snapshot` (around line 352). Update to (a) set `counter_key` AFTER `resolve_ref` using the resolved commit_sha, (b) call `prefetch_small_blobs` after tree fetch, (c) use `build_directories_with_inline`:

```rust
    async fn fetch_snapshot(&self, source: &SourceSpec) -> Result<Vec<u8>, CtxfsError> {
        // 1. Resolve the ref to a concrete commit sha.
        let commit_sha = self.resolve_ref(source).await?;

        // 2. Record the source + the commit-attributed counter key.
        *self.active_source.lock().unwrap() = Some(source.clone());
        *self.counter_key.lock().unwrap() = Some(CounterKey {
            source: "github".to_string(),
            repo: source.name.clone(),
            commit: commit_sha.clone(),
            mount_id: source.id().to_string(),
        });

        // 3. Fetch tree.
        let tree = self.fetch_tree(source, &commit_sha).await?;

        // 4. Prefetch small blobs + symlink targets.
        let symlink_set = Self::symlink_shas(&tree.tree);
        let small_shas = Self::small_blob_shas(&tree.tree);
        let inline = if small_shas.is_empty() {
            std::collections::HashMap::new()
        } else {
            self.prefetch_small_blobs(source, small_shas, &symlink_set)
                .await?
        };

        // 5. Record prefetch_hits per inlined blob.
        if let Some(key) = self.counter_key.lock().unwrap().clone() {
            let counters = self.observability.counters_for(key);
            for _ in 0..inline.len() {
                counters.record_prefetch_hit();
            }
        }

        // 6. Build directories with the inline content.
        let (root_digest, directories) =
            Self::build_directories_with_inline(&tree.tree, source, &inline);

        // ... existing snapshot serialization continues here ...
```

(Preserve the rest of the existing `fetch_snapshot` body — manifest serialization, tree-cache write, etc.)

- [ ] **Step 2: Verify build + tests**

```bash
cargo build -p ctxfs-provider-git
cargo test -p ctxfs-provider-git
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 3: Commit**

```bash
git add crates/ctxfs-provider-git/src/github.rs

git commit -m "$(cat <<'EOF'
feat(provider-git): wire prefetch into fetch_snapshot; counter_key uses resolved commit_sha

After resolve_ref completes, set counter_key with the *resolved*
commit_sha (not source.version) so attribution matches the actual
content fetched. Then run small-blobs prefetch, then build directories
with the resulting inline map.

Symlink prefetch failures propagate (per Task 4 policy) so the snapshot
fails fast rather than producing a manifest with empty symlink targets.
File prefetch failures are absorbed; missing entries fall back to
lazy fetch on read.

prefetch_hits counter incremented per successfully prefetched blob.
EOF
)"
```

---

## Task 7: Bump `TreeCache` schema version (invalidate pre-M2 snapshots)

**Files:**
- Modify: `crates/ctxfs-cache/src/tree.rs`

Pre-M2 cached snapshots have `inline_content: None` and `target: ""` baked into the serialized manifest. If we don't invalidate them, mounts that hit the tree cache will silently bypass the M2 prefetch path and serve old broken manifests.

- [ ] **Step 1: Find the schema version**

```bash
grep -n "TREE_SCHEMA_VERSION\|schema_version" crates/ctxfs-cache/src/tree.rs
```

- [ ] **Step 2: Bump the constant**

If `TREE_SCHEMA_VERSION = 1` exists, change to `2`. If a different mechanism is used (e.g., a `version` field in the cache file format), bump that. Add a comment explaining why M2 forces invalidation.

If no version mechanism exists at all, **add one**: a const `TREE_SCHEMA_VERSION: u32 = 2`, written as the first 4 bytes of the cache file, checked on read; mismatched-version reads return `None` (cache miss).

- [ ] **Step 3: Add a regression test**

```rust
    #[test]
    fn old_schema_version_is_treated_as_cache_miss() {
        // Write a cache file with schema_version=1 (or whatever the prior was).
        // Read; assert None (treated as cache miss, not a corrupt cache).
    }
```

- [ ] **Step 4: Verify**

```bash
cargo test -p ctxfs-cache
cargo test
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
```

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-cache/src/tree.rs

git commit -m "$(cat <<'EOF'
fix(cache,B1+B7): bump TreeCache schema version to invalidate pre-M2 snapshots

Pre-M2 snapshots have inline_content=None and target="" baked into the
serialized manifest. Without a version bump, mounts that hit the tree
cache would silently bypass the new M2 prefetch path and serve old
broken manifests.

Old-version reads return None (treated as cache miss), forcing a fresh
fetch via the M2 path. The user pays one cold-mount cost per repo
post-upgrade; thereafter the v2-format snapshot is cached.
EOF
)"
```

---

## Task 8: Whole-workspace verification + tag `v0.1.2-m2`

- [ ] **Step 1: Full release build + tests + fmt + clippy**

```bash
cargo build --release
cargo fmt --all -- --check
cargo clippy --all-targets --tests -- -D warnings
cargo test 2>&1 | tail -50
```

Expected: green except the documented pre-existing failures.

- [ ] **Step 2: Run M1 benchmark to confirm no regression**

```bash
cargo test --release -p ctxfs --test status_bench -- --ignored --nocapture
```

Expected: passes.

- [ ] **Step 3: CHANGELOG**

Prepend to `CHANGELOG.md`:

```markdown
## v0.1.2-m2 — 2026-04-26

### Phase 4 M2: Architecture-neutral REST fixes

- Fix B1: `FileEntry.inline_content` populated for ≤4KB blobs via
  parallel-capped (8-way) prefetch during the tree walk. Read-time blob
  calls drop to zero for tiny files.
- Fix B4: ThrottleClassifier adoption in provider-git produces clean
  `CtxfsError::RateLimited` for primary AND secondary throttles. New
  `VfsError::RateLimited` variant + adapter mappings (NFS3ERR_JUKEBOX,
  EAGAIN) so the user-facing error is "retry" not "I/O error".
  Rate-limit gauge now updates from response headers; `rest_calls_total`
  counter increments per quota-bearing call.
- Fix B7: Symlink targets resolved from the same prefetch path
  (mode-120000 blobs decoded as UTF-8 into `SymlinkEntry::target`).
  Symlink prefetch failures fail the snapshot (no silent empty-target
  regression).
- Refactor: `Observability` moved from `ctxfs-daemon` to
  `ctxfs-provider-common` so providers can use it without a dep cycle.
- Cache: `TreeCache` schema version bumped to invalidate pre-M2
  snapshots; users pay one cold-mount cost per repo on upgrade.
```

- [ ] **Step 4: Tag**

```bash
git add CHANGELOG.md
git commit -m "docs: CHANGELOG for v0.1.2-m2"
git tag -a v0.1.2-m2 -m "Phase 4 M2: architecture-neutral REST fixes (B1, B4, B7)"
```

- [ ] **Step 5: Verify tag**

```bash
git tag -l v0.1.2-m2
```

(Tag is local only; not pushed until end of M5.)

---

## Self-review checklist

- [x] **Spec coverage:** B1 (Tasks 4–6), B4 (Tasks 2–3), B7 (Tasks 4–6) all addressed.
- [x] **End-to-end EIO elimination:** Task 3 covers the VFS→adapter mapping that makes "zero EIO" verifiable, not just "zero CtxfsError::Provider".
- [x] **Tree-cache invalidation:** Task 7 prevents silent regression from cached pre-M2 snapshots.
- [x] **Symlink failure policy:** Strict (snapshot fails) per Codex review — no silent empty-target.
- [x] **Counter coverage:** `rest_calls_total`, `prefetch_hits`, `prefetch_failures`, `throttle_events` all incremented at the right points; gauge updates wired.
- [x] **B8 constraint:** Per-mount provider creation in `daemon.rs` preserved.
- [x] **No tarball / smart-gate / ContentFetcher work:** Those are M3/M4.
- [x] **Test callsite updates:** NFS test files updated for new `GitHubProvider::new` signature.
- [x] **No intentionally-broken-build commit:** Tasks 1–7 each leave the workspace green.

---

## Future work (M3 picks up)

- Tarball prefetch via `/tarball/{ref}`, atomic temp-and-rename, redirect security, singleflight dedupe.
- Smart auto-gate (count + bytes thresholds).
- Skeletal `ContentFetcher` trait + plug-in refactor.
- B2 truncated-tree fallback (required because the auto-gate uses `blob_count` from the tree response).
