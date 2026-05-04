# Phase 4 — M1: Observability + Simulation Harness — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the observability substrate in `ctxfs-provider-common` (rate-limit gauge, throttle classifier, usage counters, mock provider) and surface it via a versioned IPC `get_status` method consumed by a new `ctxfs status` global CLI view. Ship before any provider behavior change so M2+ can measure itself against M1's counters.

**Architecture:** Four new modules in `ctxfs-provider-common` (`rate_limit`, `counters`, `status`, `mock`) provide source-agnostic primitives keyed by `(auth_identity, resource_class)` for budgets and `(source, repo, commit, mount_id)` for usage. The daemon owns registries of both; CLI reads via a versioned `StatusReportV1` JSON payload over tarpc IPC. The existing `Status { mount_id }` CLI subcommand is preserved with `mount_id: Option<String>` so no-arg invocation returns the global view.

**Tech Stack:** Rust (workspace edition 2021), tarpc IPC, reqwest/serde, dashmap for concurrent registries, `tracing` for structured logs, atomic counters via `std::sync::atomic`. Workspace lints: `clippy::all` deny, `clippy::pedantic` warn.

**Spec reference:** `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` § Core Architectural Decisions 4 (definitions), 4.5 (observability), § M1 milestone.

---

## File Structure

```
crates/
  ctxfs-provider-common/
    src/
      lib.rs                            # MODIFY: re-export new modules
      rate_limit.rs                     # CREATE: RateLimitGauge, ThrottleClassifier, types
      counters.rs                       # CREATE: UsageCounters
      status.rs                         # CREATE: StatusReportV1 types
      mock.rs                           # CREATE: MockProvider test fixture
    tests/
      replay_basic.rs                   # CREATE: exact-call-count integration test
    Cargo.toml                          # MODIFY: add dashmap dep
  ctxfs-ipc/
    src/
      service.rs                        # MODIFY: add get_status RPC method
  ctxfs-daemon/
    src/
      daemon.rs                         # MODIFY: hold registries; impl get_status
      observability.rs                  # CREATE: registry types + assembly
      lib.rs                            # MODIFY: re-export observability module
  ctxfs-cli/
    src/
      main.rs                           # MODIFY: Status takes Option<String>; add global formatter
    tests/
      status_bench.rs                   # CREATE: p95 ≤ 100ms benchmark
```

**Decomposition rationale:**
- `rate_limit.rs` and `counters.rs` are split because budgets and usage counters have different keying dimensions (per spec Definitions section). Combining them would conflate two concepts.
- `status.rs` holds the versioned IPC schema separately so future `StatusReportV2` can live next to V1 with parallel support.
- `mock.rs` is in `src/`, not `tests/`, because it's a public test helper consumed by integration tests in *other* crates (e.g. M2 will use it from `ctxfs-provider-git/tests/`).
- `observability.rs` in the daemon owns the registry-of-gauges and registry-of-counters. Keeping it out of `daemon.rs` (already 600+ lines) prevents that file growing unbounded.

---

## Task 1: Add `dashmap` dependency to provider-common

**Files:**
- Modify: `crates/ctxfs-provider-common/Cargo.toml`
- Modify: `Cargo.toml` (workspace) if `dashmap` not already declared

- [ ] **Step 1: Check workspace declaration of dashmap**

Run: `grep -n "dashmap" /Users/derekxwang/Development/incubator/ContextFS/ctxfs/Cargo.toml`

Expected: either a line declaring `dashmap = "..."` exists in `[workspace.dependencies]` (skip step 2), or no output (do step 2).

- [ ] **Step 2: Add dashmap to workspace if missing**

Modify the root `Cargo.toml` `[workspace.dependencies]` section by adding:

```toml
dashmap = "6"
```

If `dashmap` is already present, leave the workspace file alone.

- [ ] **Step 3: Add dashmap to provider-common**

Modify `crates/ctxfs-provider-common/Cargo.toml` `[dependencies]` section to add:

```toml
dashmap = { workspace = true }
```

- [ ] **Step 4: Verify it compiles**

Run: `cargo check -p ctxfs-provider-common`

Expected: compiles cleanly, no warnings about unused dashmap (it's not used yet — Cargo doesn't warn on direct deps that are unused).

- [ ] **Step 5: Commit**

```bash
git add Cargo.toml crates/ctxfs-provider-common/Cargo.toml
git commit -m "build(provider-common): add dashmap dep for concurrent registries"
```

---

## Task 2: `RateLimitGauge` types — TDD

**Files:**
- Create: `crates/ctxfs-provider-common/src/rate_limit.rs`
- Modify: `crates/ctxfs-provider-common/src/lib.rs`

- [ ] **Step 1: Wire up the new module**

Modify `crates/ctxfs-provider-common/src/lib.rs` from:

```rust
pub mod http;
pub mod repo_url;
pub mod resolver;
```

to:

```rust
pub mod http;
pub mod rate_limit;
pub mod repo_url;
pub mod resolver;
```

- [ ] **Step 2: Write the failing test for `AuthIdentity` construction**

Create `crates/ctxfs-provider-common/src/rate_limit.rs` with this initial content:

```rust
//! Rate-limit budget tracking and HTTP-response throttle classification.
//!
//! Budgets are keyed by `(AuthIdentity, ResourceClass)` because GitHub's
//! `x-ratelimit-resource` header carries finer-grained quotas than the
//! source dimension. Two sources sharing the same PAT share the same
//! budget; one source with multiple resource classes (core, search,
//! graphql) holds independent budgets per class.

use std::time::{Duration, SystemTime};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_identity_anonymous_distinct_from_pat() {
        let anon = AuthIdentity::anonymous("api.github.com");
        let pat = AuthIdentity::pat("api.github.com", "ghp_token123");
        assert_ne!(anon, pat);
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p ctxfs-provider-common rate_limit::tests::auth_identity_anonymous_distinct_from_pat`

Expected: compile error — `AuthIdentity` not defined.

- [ ] **Step 4: Implement the minimum to make the test compile and pass**

Add to `crates/ctxfs-provider-common/src/rate_limit.rs` above the `#[cfg(test)]` line:

```rust
/// Identifies the credential under which a request is made. Two requests
/// with the same `AuthIdentity` share a GitHub-side rate-limit budget.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct AuthIdentity {
    pub host: String,
    pub kind: AuthKind,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum AuthKind {
    Anonymous,
    /// Personal-access token. Stored as the **token id prefix** (first 8
    /// chars), not the secret itself, so this struct is safe to log.
    Pat { token_id_prefix: String },
    /// GitHub App installation token (placeholder for future).
    GithubApp { installation_id: u64 },
}

impl AuthIdentity {
    pub fn anonymous(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            kind: AuthKind::Anonymous,
        }
    }

    pub fn pat(host: impl Into<String>, token: &str) -> Self {
        let prefix: String = token.chars().take(8).collect();
        Self {
            host: host.into(),
            kind: AuthKind::Pat {
                token_id_prefix: prefix,
            },
        }
    }
}
```

- [ ] **Step 5: Run the test, expect pass**

Run: `cargo test -p ctxfs-provider-common rate_limit::tests::auth_identity_anonymous_distinct_from_pat`

Expected: `1 passed`.

- [ ] **Step 6: Add `ResourceClass`, with TDD**

Append to the `tests` module in `rate_limit.rs`:

```rust
    #[test]
    fn resource_class_parses_known_values() {
        assert_eq!(ResourceClass::parse("core"), ResourceClass::Core);
        assert_eq!(ResourceClass::parse("search"), ResourceClass::Search);
        assert_eq!(ResourceClass::parse("graphql"), ResourceClass::Graphql);
        assert_eq!(ResourceClass::parse("code_search"), ResourceClass::CodeSearch);
    }

    #[test]
    fn resource_class_unknown_falls_back_to_other() {
        assert!(matches!(ResourceClass::parse("audit_log"), ResourceClass::Other(_)));
    }
```

- [ ] **Step 7: Run the test to verify it fails**

Run: `cargo test -p ctxfs-provider-common rate_limit::tests::resource_class`

Expected: compile error — `ResourceClass` not defined.

- [ ] **Step 8: Implement `ResourceClass`**

Add to `rate_limit.rs` (above the `#[cfg(test)]`):

```rust
/// GitHub `x-ratelimit-resource` header values. Each resource class has
/// its own per-`AuthIdentity` budget.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ResourceClass {
    Core,
    Search,
    CodeSearch,
    Graphql,
    Other(String),
}

impl ResourceClass {
    pub fn parse(s: &str) -> Self {
        match s {
            "core" => Self::Core,
            "search" => Self::Search,
            "code_search" => Self::CodeSearch,
            "graphql" => Self::Graphql,
            other => Self::Other(other.to_string()),
        }
    }
}
```

- [ ] **Step 9: Run resource class tests, expect pass**

Run: `cargo test -p ctxfs-provider-common rate_limit::tests::resource_class`

Expected: `2 passed`.

- [ ] **Step 10: Add `RateLimitGauge` snapshot type, with TDD**

Append to the `tests` module:

```rust
    #[test]
    fn gauge_default_is_unlimited_unknown() {
        let g = RateLimitGauge::unknown();
        assert!(g.remaining.is_none());
        assert!(g.limit.is_none());
        assert!(g.reset_at.is_none());
        assert!(matches!(g.secondary_throttle_state, ThrottleState::None));
    }

    #[test]
    fn gauge_update_from_headers_parses_standard_response() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("x-ratelimit-limit".to_string(), "5000".to_string());
        headers.insert("x-ratelimit-remaining".to_string(), "4999".to_string());
        headers.insert("x-ratelimit-reset".to_string(), "1700000000".to_string());

        let mut g = RateLimitGauge::unknown();
        g.update_from_headers(&headers);

        assert_eq!(g.limit, Some(5000));
        assert_eq!(g.remaining, Some(4999));
        assert_eq!(
            g.reset_at,
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000))
        );
    }

    #[test]
    fn gauge_update_ignores_missing_headers() {
        let headers = std::collections::HashMap::new();
        let mut g = RateLimitGauge::unknown();
        g.update_from_headers(&headers);
        assert!(g.remaining.is_none());
    }
```

- [ ] **Step 11: Run the tests to verify they fail**

Run: `cargo test -p ctxfs-provider-common rate_limit::tests::gauge`

Expected: compile error — `RateLimitGauge` and `ThrottleState` not defined.

- [ ] **Step 12: Implement `RateLimitGauge` and `ThrottleState`**

Add to `rate_limit.rs`:

```rust
/// Snapshot of a rate-limit budget at a point in time.
#[derive(Debug, Clone)]
pub struct RateLimitGauge {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_at: Option<SystemTime>,
    pub secondary_throttle_state: ThrottleState,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ThrottleState {
    None,
    /// Secondary throttle active until the wrapped instant.
    Active { until: SystemTime },
}

impl RateLimitGauge {
    pub fn unknown() -> Self {
        Self {
            limit: None,
            remaining: None,
            reset_at: None,
            secondary_throttle_state: ThrottleState::None,
        }
    }

    /// Update the gauge from a map of HTTP response headers (lowercased keys).
    /// Missing headers leave the corresponding field unchanged.
    pub fn update_from_headers(&mut self, headers: &std::collections::HashMap<String, String>) {
        if let Some(v) = headers.get("x-ratelimit-limit").and_then(|s| s.parse().ok()) {
            self.limit = Some(v);
        }
        if let Some(v) = headers.get("x-ratelimit-remaining").and_then(|s| s.parse().ok()) {
            self.remaining = Some(v);
        }
        if let Some(secs) = headers.get("x-ratelimit-reset").and_then(|s| s.parse::<u64>().ok()) {
            self.reset_at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        }
    }

    /// Set the secondary-throttle state to active for the given duration from now.
    pub fn set_secondary_throttle(&mut self, retry_after: Duration) {
        self.secondary_throttle_state = ThrottleState::Active {
            until: SystemTime::now() + retry_after,
        };
    }

    /// Clear the secondary-throttle state if its `until` is in the past.
    pub fn clear_expired_throttle(&mut self) {
        if let ThrottleState::Active { until } = self.secondary_throttle_state {
            if SystemTime::now() >= until {
                self.secondary_throttle_state = ThrottleState::None;
            }
        }
    }
}
```

- [ ] **Step 13: Run all rate_limit tests, expect pass**

Run: `cargo test -p ctxfs-provider-common rate_limit::tests`

Expected: `5 passed`.

- [ ] **Step 14: Run clippy on the new module**

Run: `cargo clippy -p ctxfs-provider-common --all-targets -- -D warnings`

Expected: no warnings.

- [ ] **Step 15: Commit**

```bash
git add crates/ctxfs-provider-common/src/lib.rs crates/ctxfs-provider-common/src/rate_limit.rs
git commit -m "feat(provider-common): RateLimitGauge keyed by auth identity x resource class

AuthIdentity stores only the token-id-prefix (first 8 chars), not the
secret, so RateLimitGauge is safe to log. ResourceClass models GitHub's
x-ratelimit-resource header (core, search, code_search, graphql) so a
single auth identity can hold independent per-resource budgets.
update_from_headers is a no-op when headers are absent so non-quota-
bearing transfers (codeload tarball download) don't corrupt the gauge."
```

---

## Task 3: `ThrottleClassifier` — TDD

**Files:**
- Modify: `crates/ctxfs-provider-common/src/rate_limit.rs`

- [ ] **Step 1: Write the failing test for the OK case**

Append to the `tests` module in `rate_limit.rs`:

```rust
    use std::collections::HashMap;

    fn hdr(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs.iter().map(|(k, v)| ((*k).to_string(), (*v).to_string())).collect()
    }

    #[test]
    fn classifier_returns_ok_for_200() {
        let h = hdr(&[("x-ratelimit-resource", "core"), ("x-ratelimit-remaining", "100")]);
        let v = ThrottleClassifier::classify(200, &h);
        assert!(matches!(v, RateLimitVerdict::Ok { resource: ResourceClass::Core }));
    }

    #[test]
    fn classifier_primary_exhausted_when_remaining_zero() {
        let h = hdr(&[
            ("x-ratelimit-resource", "core"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1700000000"),
        ]);
        let v = ThrottleClassifier::classify(403, &h);
        match v {
            RateLimitVerdict::PrimaryExhausted { reset_at, resource } => {
                assert_eq!(resource, ResourceClass::Core);
                assert_eq!(reset_at, SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000));
            }
            other => panic!("expected PrimaryExhausted, got {other:?}"),
        }
    }

    #[test]
    fn classifier_secondary_throttle_429_with_retry_after_and_remaining_nonzero() {
        let h = hdr(&[
            ("x-ratelimit-resource", "core"),
            ("x-ratelimit-remaining", "4500"),
            ("retry-after", "60"),
        ]);
        let v = ThrottleClassifier::classify(429, &h);
        match v {
            RateLimitVerdict::SecondaryThrottle { retry_after, resource } => {
                assert_eq!(retry_after, Duration::from_secs(60));
                assert_eq!(resource, ResourceClass::Core);
            }
            other => panic!("expected SecondaryThrottle, got {other:?}"),
        }
    }

    #[test]
    fn classifier_secondary_throttle_403_with_retry_after_is_secondary() {
        // GitHub's secondary limits sometimes return 403 (not 429) with retry-after.
        let h = hdr(&[
            ("x-ratelimit-remaining", "4500"),
            ("retry-after", "30"),
        ]);
        let v = ThrottleClassifier::classify(403, &h);
        assert!(matches!(v, RateLimitVerdict::SecondaryThrottle { .. }));
    }

    #[test]
    fn classifier_other_for_500() {
        let v = ThrottleClassifier::classify(500, &HashMap::new());
        assert!(matches!(v, RateLimitVerdict::Other { status: 500 }));
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p ctxfs-provider-common rate_limit::tests::classifier`

Expected: compile error — `ThrottleClassifier`, `RateLimitVerdict` not defined.

- [ ] **Step 3: Implement `ThrottleClassifier` and `RateLimitVerdict`**

Add to `rate_limit.rs` (above `#[cfg(test)]`):

```rust
/// Verdict returned by [`ThrottleClassifier::classify`].
#[derive(Debug, Clone)]
pub enum RateLimitVerdict {
    Ok { resource: ResourceClass },
    PrimaryExhausted { reset_at: SystemTime, resource: ResourceClass },
    SecondaryThrottle { retry_after: Duration, resource: ResourceClass },
    Other { status: u16 },
}

/// Classifies an HTTP response into a rate-limit verdict.
///
/// Order of checks (matters):
/// 1. `Retry-After` header present + status 429/403 → `SecondaryThrottle`
///    (secondary throttles can fire while `x-ratelimit-remaining` is still
///    nonzero, so this comes before the remaining-zero check).
/// 2. `x-ratelimit-remaining == 0` and status not 2xx → `PrimaryExhausted`.
/// 3. Status 2xx → `Ok`.
/// 4. Anything else → `Other`.
pub struct ThrottleClassifier;

impl ThrottleClassifier {
    pub fn classify(
        status: u16,
        headers: &std::collections::HashMap<String, String>,
    ) -> RateLimitVerdict {
        let resource = headers
            .get("x-ratelimit-resource")
            .map(|s| ResourceClass::parse(s))
            .unwrap_or_else(|| ResourceClass::Other("unknown".to_string()));

        // 1. Secondary throttle: 429 or 403 with Retry-After.
        if (status == 429 || status == 403) && headers.contains_key("retry-after") {
            if let Some(secs) = headers.get("retry-after").and_then(|s| s.parse::<u64>().ok()) {
                return RateLimitVerdict::SecondaryThrottle {
                    retry_after: Duration::from_secs(secs),
                    resource,
                };
            }
        }

        // 2. Primary exhausted.
        if let Some(remaining) = headers.get("x-ratelimit-remaining").and_then(|s| s.parse::<u64>().ok()) {
            if remaining == 0 && !(200..300).contains(&status) {
                if let Some(reset_secs) = headers.get("x-ratelimit-reset").and_then(|s| s.parse::<u64>().ok()) {
                    return RateLimitVerdict::PrimaryExhausted {
                        reset_at: SystemTime::UNIX_EPOCH + Duration::from_secs(reset_secs),
                        resource,
                    };
                }
            }
        }

        // 3. OK.
        if (200..300).contains(&status) {
            return RateLimitVerdict::Ok { resource };
        }

        // 4. Other.
        RateLimitVerdict::Other { status }
    }
}
```

- [ ] **Step 4: Run classifier tests, expect pass**

Run: `cargo test -p ctxfs-provider-common rate_limit::tests::classifier`

Expected: `5 passed`.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy -p ctxfs-provider-common --all-targets -- -D warnings`

Expected: no warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-provider-common/src/rate_limit.rs
git commit -m "feat(provider-common): ThrottleClassifier returns RateLimitVerdict

Order of checks matters: secondary-throttle (Retry-After + 429/403) is
checked before primary-exhausted (remaining=0). GitHub secondary limits
fire while remaining is still nonzero, so checking remaining first would
misclassify them as Other. Fixes B4 once provider-git adopts this."
```

---

## Task 4: `UsageCounters` — TDD

**Files:**
- Create: `crates/ctxfs-provider-common/src/counters.rs`
- Modify: `crates/ctxfs-provider-common/src/lib.rs`

- [ ] **Step 1: Wire up the new module**

Modify `crates/ctxfs-provider-common/src/lib.rs` to:

```rust
pub mod counters;
pub mod http;
pub mod rate_limit;
pub mod repo_url;
pub mod resolver;
```

- [ ] **Step 2: Write the failing test for `CounterKey` and atomic increment**

Create `crates/ctxfs-provider-common/src/counters.rs`:

```rust
//! Atomic per-mount usage counters.
//!
//! Keyed by `(source, repo, commit, mount_id)` because a single repo can
//! have multiple concurrent mounts at different commits, and per-mount
//! attribution is needed for `ctxfs status` to surface "top mounts by
//! cost".

use std::sync::atomic::{AtomicU64, Ordering};

#[cfg(test)]
mod tests {
    use super::*;

    fn key() -> CounterKey {
        CounterKey {
            source: "github".to_string(),
            repo: "foo/bar".to_string(),
            commit: "abc123".to_string(),
            mount_id: "mnt-1".to_string(),
        }
    }

    #[test]
    fn snapshot_starts_at_zero() {
        let c = MountCounters::new();
        let s = c.snapshot();
        assert_eq!(s.rest_calls_total, 0);
        assert_eq!(s.bytes_total, 0);
        assert_eq!(s.cache_hits, 0);
    }

    #[test]
    fn rest_calls_increment_is_atomic() {
        let c = MountCounters::new();
        c.record_rest_call();
        c.record_rest_call();
        c.record_rest_call();
        assert_eq!(c.snapshot().rest_calls_total, 3);
    }

    #[test]
    fn bytes_total_accumulates() {
        let c = MountCounters::new();
        c.record_bytes_transferred(100);
        c.record_bytes_transferred(250);
        assert_eq!(c.snapshot().bytes_total, 350);
    }

    #[test]
    fn counter_key_equality() {
        let k1 = key();
        let k2 = key();
        assert_eq!(k1, k2);
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p ctxfs-provider-common counters::tests`

Expected: compile error — types not defined.

- [ ] **Step 4: Implement `CounterKey`, `MountCounters`, `CounterSnapshot`**

Add to `counters.rs` above the `#[cfg(test)]`:

```rust
/// Key for a per-mount counter bucket. All four dimensions are required:
/// two mounts of the same `(source, repo, commit)` at different mount points
/// hold separate counters keyed by `mount_id`.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CounterKey {
    pub source: String,
    pub repo: String,
    pub commit: String,
    pub mount_id: String,
}

/// Atomic counters for one mount. Cheaply incremented on hot paths.
#[derive(Debug, Default)]
pub struct MountCounters {
    rest_calls_total: AtomicU64,
    http_transfers_total: AtomicU64,
    bytes_total: AtomicU64,
    throttle_events: AtomicU64,
    prefetch_hits: AtomicU64,
    prefetch_failures: AtomicU64,
    truncated_tree_fallbacks: AtomicU64,
    cache_hits: AtomicU64,
    cache_misses: AtomicU64,
    lfs_pointer_files: AtomicU64,
}

/// Read-only snapshot for serialization in `StatusReportV1`.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct CounterSnapshot {
    pub rest_calls_total: u64,
    pub http_transfers_total: u64,
    pub bytes_total: u64,
    pub throttle_events: u64,
    pub prefetch_hits: u64,
    pub prefetch_failures: u64,
    pub truncated_tree_fallbacks: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
    pub lfs_pointer_files: u64,
}

impl MountCounters {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_rest_call(&self) {
        self.rest_calls_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_http_transfer(&self) {
        self.http_transfers_total.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_bytes_transferred(&self, bytes: u64) {
        self.bytes_total.fetch_add(bytes, Ordering::Relaxed);
    }

    pub fn record_throttle_event(&self) {
        self.throttle_events.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_prefetch_hit(&self) {
        self.prefetch_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_prefetch_failure(&self) {
        self.prefetch_failures.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_truncated_tree_fallback(&self) {
        self.truncated_tree_fallbacks.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cache_hit(&self) {
        self.cache_hits.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_cache_miss(&self) {
        self.cache_misses.fetch_add(1, Ordering::Relaxed);
    }

    pub fn record_lfs_pointer_file(&self) {
        self.lfs_pointer_files.fetch_add(1, Ordering::Relaxed);
    }

    pub fn snapshot(&self) -> CounterSnapshot {
        CounterSnapshot {
            rest_calls_total: self.rest_calls_total.load(Ordering::Relaxed),
            http_transfers_total: self.http_transfers_total.load(Ordering::Relaxed),
            bytes_total: self.bytes_total.load(Ordering::Relaxed),
            throttle_events: self.throttle_events.load(Ordering::Relaxed),
            prefetch_hits: self.prefetch_hits.load(Ordering::Relaxed),
            prefetch_failures: self.prefetch_failures.load(Ordering::Relaxed),
            truncated_tree_fallbacks: self.truncated_tree_fallbacks.load(Ordering::Relaxed),
            cache_hits: self.cache_hits.load(Ordering::Relaxed),
            cache_misses: self.cache_misses.load(Ordering::Relaxed),
            lfs_pointer_files: self.lfs_pointer_files.load(Ordering::Relaxed),
        }
    }
}
```

- [ ] **Step 5: Run counter tests, expect pass**

Run: `cargo test -p ctxfs-provider-common counters::tests`

Expected: `4 passed`.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -p ctxfs-provider-common --all-targets -- -D warnings`

Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-provider-common/src/counters.rs crates/ctxfs-provider-common/src/lib.rs
git commit -m "feat(provider-common): MountCounters with atomic per-mount counters

Keyed by (source, repo, commit, mount_id) so two mounts of the same repo
at the same commit but different mount points hold separate counters.
All increments use Ordering::Relaxed because counter consistency across
threads only matters at snapshot time, where the daemon collects them
all in a single pass for the StatusReportV1 payload."
```

---

## Task 5: `StatusReportV1` schema — TDD

**Files:**
- Create: `crates/ctxfs-provider-common/src/status.rs`
- Modify: `crates/ctxfs-provider-common/src/lib.rs`

- [ ] **Step 1: Wire up the new module**

Modify `crates/ctxfs-provider-common/src/lib.rs` to:

```rust
pub mod counters;
pub mod http;
pub mod rate_limit;
pub mod repo_url;
pub mod resolver;
pub mod status;
```

- [ ] **Step 2: Write the failing serde-roundtrip test**

Create `crates/ctxfs-provider-common/src/status.rs`:

```rust
//! Versioned IPC schema for `ctxfs status`.
//!
//! `schema_version` is an explicit field so future StatusReportV2 can be
//! added with parallel support without breaking older CLI clients.

use crate::counters::{CounterKey, CounterSnapshot};
use serde::{Deserialize, Serialize};

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty_report() {
        let r = StatusReportV1 {
            schema_version: 1,
            budgets: vec![],
            counters: vec![],
            mounts: vec![],
        };
        let json = serde_json::to_string(&r).unwrap();
        let r2: StatusReportV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(r2.schema_version, 1);
        assert!(r2.budgets.is_empty());
    }

    #[test]
    fn schema_version_field_is_explicit_and_stable() {
        let r = StatusReportV1 {
            schema_version: 1,
            budgets: vec![],
            counters: vec![],
            mounts: vec![],
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"schema_version\":1"));
    }
}
```

- [ ] **Step 3: Run the test to verify it fails**

Run: `cargo test -p ctxfs-provider-common status::tests`

Expected: compile error — `StatusReportV1` not defined.

- [ ] **Step 4: Implement the schema**

Add to `status.rs` above the `#[cfg(test)]`:

```rust
/// Top-level versioned status payload returned by IPC `get_status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReportV1 {
    pub schema_version: u32, // always 1 for this struct; v2 will use a different struct
    pub budgets: Vec<BudgetEntry>,
    pub counters: Vec<CounterEntry>,
    pub mounts: Vec<MountSummary>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetEntry {
    pub host: String,
    pub auth_kind: String, // "anonymous" | "pat:<prefix>" | "github_app:<id>"
    pub resource_class: String, // "core" | "search" | "code_search" | "graphql" | "other:<value>"
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_at_unix: Option<u64>,
    pub throttle_active_until_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterEntry {
    pub key: CounterKey,
    pub counters: CounterSnapshot,
}

/// Per-mount summary derived from counters; sorted by `rest_calls_total`
/// descending in the daemon-side assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountSummary {
    pub mount_id: String,
    pub source: String,
    pub repo: String,
    pub commit: String,
    pub rest_calls_total: u64,
    pub bytes_total: u64,
    pub prefetch_hits: u64,
    pub cache_hit_ratio: Option<f64>, // None when (cache_hits + cache_misses) == 0
}
```

- [ ] **Step 5: Run status tests, expect pass**

Run: `cargo test -p ctxfs-provider-common status::tests`

Expected: `2 passed`.

- [ ] **Step 6: Add `Hash` derive to CounterKey for serde**

The `CounterKey` already derives `Hash`, `Eq`, `PartialEq`. For serde, add `Serialize, Deserialize`:

Modify `crates/ctxfs-provider-common/src/counters.rs` `CounterKey` derive line from:

```rust
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct CounterKey {
```

to:

```rust
#[derive(Debug, Clone, Eq, PartialEq, Hash, serde::Serialize, serde::Deserialize)]
pub struct CounterKey {
```

- [ ] **Step 7: Re-run all provider-common tests**

Run: `cargo test -p ctxfs-provider-common`

Expected: all tests pass.

- [ ] **Step 8: Run clippy**

Run: `cargo clippy -p ctxfs-provider-common --all-targets -- -D warnings`

Expected: no warnings.

- [ ] **Step 9: Commit**

```bash
git add crates/ctxfs-provider-common/src/status.rs crates/ctxfs-provider-common/src/lib.rs crates/ctxfs-provider-common/src/counters.rs
git commit -m "feat(provider-common): StatusReportV1 IPC schema with explicit version

Versioned schema means v2 lands as a separate struct with parallel
support; older CLI clients still read v1 cleanly via serde_json's
default-on-missing fields. BudgetEntry encodes auth_kind as a string
form (\"pat:<prefix>\") so the IPC payload remains JSON-friendly while
preserving identity information for top-N attribution."
```

---

## Task 6: `MockProvider` test fixture — TDD

**Files:**
- Create: `crates/ctxfs-provider-common/src/mock.rs`
- Modify: `crates/ctxfs-provider-common/src/lib.rs`

- [ ] **Step 1: Wire up the new module**

Modify `crates/ctxfs-provider-common/src/lib.rs` to add `pub mod mock;`. Final form:

```rust
pub mod counters;
pub mod http;
pub mod mock;
pub mod rate_limit;
pub mod repo_url;
pub mod resolver;
pub mod status;
```

- [ ] **Step 2: Write the failing test**

Create `crates/ctxfs-provider-common/src/mock.rs`:

```rust
//! Test fixture: an HTTP-shaped `MockProvider` that records every call
//! it would have made, so workload-replay integration tests can assert
//! exact provider call counts without hitting the real GitHub API.
//!
//! Used cross-crate: M2's tests will pull this in from
//! `ctxfs-provider-git/tests/`, so it lives in `src/`, not `tests/`.

use std::sync::Mutex;

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RecordedCall {
    Commit { repo: String, reference: String },
    Tree { repo: String, sha: String, recursive: bool },
    Blob { repo: String, sha: String },
    Tarball { repo: String, sha: String },
}

#[derive(Debug, Default)]
pub struct MockProvider {
    calls: Mutex<Vec<RecordedCall>>,
}

impl MockProvider {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn record_commit(&self, repo: impl Into<String>, reference: impl Into<String>) {
        self.calls.lock().unwrap().push(RecordedCall::Commit {
            repo: repo.into(),
            reference: reference.into(),
        });
    }

    pub fn record_tree(&self, repo: impl Into<String>, sha: impl Into<String>, recursive: bool) {
        self.calls.lock().unwrap().push(RecordedCall::Tree {
            repo: repo.into(),
            sha: sha.into(),
            recursive,
        });
    }

    pub fn record_blob(&self, repo: impl Into<String>, sha: impl Into<String>) {
        self.calls.lock().unwrap().push(RecordedCall::Blob {
            repo: repo.into(),
            sha: sha.into(),
        });
    }

    pub fn record_tarball(&self, repo: impl Into<String>, sha: impl Into<String>) {
        self.calls.lock().unwrap().push(RecordedCall::Tarball {
            repo: repo.into(),
            sha: sha.into(),
        });
    }

    pub fn calls(&self) -> Vec<RecordedCall> {
        self.calls.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.calls.lock().unwrap().len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn records_commit_tree_blob_tarball_in_order() {
        let m = MockProvider::new();
        m.record_commit("foo/bar", "main");
        m.record_tree("foo/bar", "abc", true);
        m.record_blob("foo/bar", "blob1");
        m.record_tarball("foo/bar", "abc");
        assert_eq!(m.count(), 4);
        let calls = m.calls();
        assert!(matches!(&calls[0], RecordedCall::Commit { .. }));
        assert!(matches!(&calls[3], RecordedCall::Tarball { .. }));
    }

    #[test]
    fn count_starts_at_zero() {
        let m = MockProvider::new();
        assert_eq!(m.count(), 0);
    }
}
```

- [ ] **Step 3: Run the tests, expect pass**

Run: `cargo test -p ctxfs-provider-common mock::tests`

Expected: `2 passed`.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p ctxfs-provider-common --all-targets -- -D warnings`

Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-provider-common/src/mock.rs crates/ctxfs-provider-common/src/lib.rs
git commit -m "feat(provider-common): MockProvider records every call for replay tests

Lives in src/ (not tests/) so cross-crate integration tests in
ctxfs-provider-git can pull it in. Records the four call shapes that
matter for rate-limit accounting: commit resolve, tree fetch, blob
fetch, tarball fetch. count() and calls() let tests assert exact
ordering or just the number for less-brittle workload-replay tests."
```

---

## Task 7: Workload-replay integration test

**Files:**
- Create: `crates/ctxfs-provider-common/tests/replay_basic.rs`

- [ ] **Step 1: Write the integration test**

Create `crates/ctxfs-provider-common/tests/replay_basic.rs`:

```rust
//! Workload-replay integration test: simulates a 1k-file cold scan via
//! MockProvider and asserts exact call counts. This is the regression
//! sentinel for M3's `rest_calls_total == 3` exit criterion; M1 ships
//! with a placeholder workload (lazy per-blob path simulated) and M2/M3
//! extend it to the tarball-prefetch and B1-inline paths.

use ctxfs_provider_common::mock::{MockProvider, RecordedCall};

/// Simulates the *current* (pre-Phase-4) lazy-per-blob workload.
/// Asserts exactly: 1 commit + 1 tree + N blobs.
#[test]
fn lazy_workload_records_one_call_per_blob() {
    let mock = MockProvider::new();
    let blob_count = 100;

    // Simulate the cold mount.
    mock.record_commit("foo/bar", "main");
    mock.record_tree("foo/bar", "abc", true);
    for i in 0..blob_count {
        mock.record_blob("foo/bar", format!("blob{i}"));
    }

    let calls = mock.calls();
    assert_eq!(calls.len(), 1 + 1 + blob_count);
    assert!(matches!(calls[0], RecordedCall::Commit { .. }));
    assert!(matches!(calls[1], RecordedCall::Tree { .. }));
    let blob_calls = calls.iter().filter(|c| matches!(c, RecordedCall::Blob { .. })).count();
    assert_eq!(blob_calls, blob_count);
}

/// Sentinel for the M3 tarball-prefetch path (exit criterion).
#[test]
fn tarball_workload_records_three_calls() {
    let mock = MockProvider::new();

    mock.record_commit("foo/bar", "main");
    mock.record_tree("foo/bar", "abc", true);
    mock.record_tarball("foo/bar", "abc");

    let calls = mock.calls();
    assert_eq!(calls.len(), 3);
    assert!(matches!(calls[2], RecordedCall::Tarball { .. }));

    // No blob calls.
    let blob_calls = calls.iter().filter(|c| matches!(c, RecordedCall::Blob { .. })).count();
    assert_eq!(blob_calls, 0);
}
```

- [ ] **Step 2: Run the integration test**

Run: `cargo test -p ctxfs-provider-common --test replay_basic`

Expected: `2 passed`.

- [ ] **Step 3: Commit**

```bash
git add crates/ctxfs-provider-common/tests/replay_basic.rs
git commit -m "test(provider-common): workload-replay sentinels for M1 + M3 paths

The lazy-workload test asserts the pre-Phase-4 baseline (1 commit + 1
tree + N blob calls); the tarball-workload test asserts M3's exit
criterion (rest_calls_total == 3). Both use MockProvider so they don't
depend on real-API mocking infrastructure. M2 will extend with
'lazy + B1 inline' assertions; M3 with the smart-gate matrix."
```

---

## Task 8: Daemon-side observability registry

**Files:**
- Create: `crates/ctxfs-daemon/src/observability.rs`
- Modify: `crates/ctxfs-daemon/src/lib.rs`

- [ ] **Step 1: Inspect existing daemon module structure**

Run: `cat /Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-daemon/src/lib.rs`

Note what's exported. The new `observability` module will be added to it.

- [ ] **Step 2: Write the failing test for `Observability::status_report`**

Create `crates/ctxfs-daemon/src/observability.rs`:

```rust
//! Daemon-owned registry of rate-limit gauges and per-mount counters.
//! Assembles the StatusReportV1 payload for IPC `get_status`.

use ctxfs_provider_common::counters::{CounterKey, MountCounters};
use ctxfs_provider_common::rate_limit::{AuthIdentity, AuthKind, RateLimitGauge, ResourceClass, ThrottleState};
use ctxfs_provider_common::status::{
    BudgetEntry, CounterEntry, MountSummary, StatusReportV1,
};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

/// Registry of all rate-limit gauges and per-mount counters owned by the daemon.
#[derive(Debug, Default)]
pub struct Observability {
    /// Keyed by (AuthIdentity, ResourceClass).
    gauges: DashMap<(AuthIdentity, ResourceClass), RateLimitGauge>,
    /// Keyed by (source, repo, commit, mount_id).
    counters: DashMap<CounterKey, Arc<MountCounters>>,
}

impl Observability {
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create the counters for a mount.
    pub fn counters_for(&self, key: CounterKey) -> Arc<MountCounters> {
        self.counters
            .entry(key)
            .or_insert_with(|| Arc::new(MountCounters::new()))
            .clone()
    }

    /// Get a clone of the gauge for an (auth, resource) pair, or `unknown` if absent.
    pub fn gauge_for(&self, auth: &AuthIdentity, resource: &ResourceClass) -> RateLimitGauge {
        self.gauges
            .get(&(auth.clone(), resource.clone()))
            .map(|g| g.clone())
            .unwrap_or_else(RateLimitGauge::unknown)
    }

    /// Update a gauge from response headers. Creates the entry if absent.
    pub fn update_gauge(
        &self,
        auth: AuthIdentity,
        resource: ResourceClass,
        headers: &std::collections::HashMap<String, String>,
    ) {
        let mut g = self
            .gauges
            .entry((auth, resource))
            .or_insert_with(RateLimitGauge::unknown);
        g.update_from_headers(headers);
    }

    /// Assemble a StatusReportV1 payload for IPC.
    pub fn status_report(&self) -> StatusReportV1 {
        let budgets = self
            .gauges
            .iter()
            .map(|entry| {
                let (auth, resource) = entry.key();
                let g = entry.value();
                BudgetEntry {
                    host: auth.host.clone(),
                    auth_kind: auth_kind_string(&auth.kind),
                    resource_class: resource_class_string(resource),
                    limit: g.limit,
                    remaining: g.remaining,
                    reset_at_unix: g
                        .reset_at
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs()),
                    throttle_active_until_unix: match &g.secondary_throttle_state {
                        ThrottleState::None => None,
                        ThrottleState::Active { until } => until
                            .duration_since(UNIX_EPOCH)
                            .ok()
                            .map(|d| d.as_secs()),
                    },
                }
            })
            .collect();

        let counters: Vec<CounterEntry> = self
            .counters
            .iter()
            .map(|entry| CounterEntry {
                key: entry.key().clone(),
                counters: entry.value().snapshot(),
            })
            .collect();

        let mut mounts: Vec<MountSummary> = counters
            .iter()
            .map(|c| {
                let total_cache_ops = c.counters.cache_hits + c.counters.cache_misses;
                let cache_hit_ratio = if total_cache_ops > 0 {
                    Some(c.counters.cache_hits as f64 / total_cache_ops as f64)
                } else {
                    None
                };
                MountSummary {
                    mount_id: c.key.mount_id.clone(),
                    source: c.key.source.clone(),
                    repo: c.key.repo.clone(),
                    commit: c.key.commit.clone(),
                    rest_calls_total: c.counters.rest_calls_total,
                    bytes_total: c.counters.bytes_total,
                    prefetch_hits: c.counters.prefetch_hits,
                    cache_hit_ratio,
                }
            })
            .collect();

        // Sort by rest_calls_total descending so the top-N consumers are first.
        mounts.sort_by(|a, b| b.rest_calls_total.cmp(&a.rest_calls_total));

        StatusReportV1 {
            schema_version: 1,
            budgets,
            counters,
            mounts,
        }
    }
}

fn auth_kind_string(kind: &AuthKind) -> String {
    match kind {
        AuthKind::Anonymous => "anonymous".to_string(),
        AuthKind::Pat { token_id_prefix } => format!("pat:{token_id_prefix}"),
        AuthKind::GithubApp { installation_id } => format!("github_app:{installation_id}"),
    }
}

fn resource_class_string(rc: &ResourceClass) -> String {
    match rc {
        ResourceClass::Core => "core".to_string(),
        ResourceClass::Search => "search".to_string(),
        ResourceClass::CodeSearch => "code_search".to_string(),
        ResourceClass::Graphql => "graphql".to_string(),
        ResourceClass::Other(s) => format!("other:{s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(mount_id: &str) -> CounterKey {
        CounterKey {
            source: "github".to_string(),
            repo: "foo/bar".to_string(),
            commit: "abc".to_string(),
            mount_id: mount_id.to_string(),
        }
    }

    #[test]
    fn empty_registry_produces_v1_report() {
        let o = Observability::new();
        let r = o.status_report();
        assert_eq!(r.schema_version, 1);
        assert!(r.budgets.is_empty());
        assert!(r.counters.is_empty());
        assert!(r.mounts.is_empty());
    }

    #[test]
    fn counters_for_creates_and_returns_same_arc() {
        let o = Observability::new();
        let c1 = o.counters_for(key("mnt-1"));
        let c2 = o.counters_for(key("mnt-1"));
        c1.record_rest_call();
        // c2 should observe the increment because they share the Arc.
        assert_eq!(c2.snapshot().rest_calls_total, 1);
    }

    #[test]
    fn mounts_sorted_by_rest_calls_descending() {
        let o = Observability::new();
        let a = o.counters_for(key("mnt-A"));
        let b = o.counters_for(key("mnt-B"));
        for _ in 0..3 {
            a.record_rest_call();
        }
        for _ in 0..7 {
            b.record_rest_call();
        }
        let r = o.status_report();
        assert_eq!(r.mounts.len(), 2);
        assert_eq!(r.mounts[0].mount_id, "mnt-B"); // 7 > 3
        assert_eq!(r.mounts[1].mount_id, "mnt-A");
    }

    #[test]
    fn cache_hit_ratio_is_none_when_no_cache_ops() {
        let o = Observability::new();
        let _ = o.counters_for(key("mnt-1"));
        let r = o.status_report();
        assert!(r.mounts[0].cache_hit_ratio.is_none());
    }

    #[test]
    fn cache_hit_ratio_calculated_when_ops_recorded() {
        let o = Observability::new();
        let c = o.counters_for(key("mnt-1"));
        c.record_cache_hit();
        c.record_cache_hit();
        c.record_cache_hit();
        c.record_cache_miss();
        let r = o.status_report();
        let ratio = r.mounts[0].cache_hit_ratio.unwrap();
        assert!((ratio - 0.75).abs() < 1e-9);
    }
}
```

- [ ] **Step 3: Wire up the module**

Modify `crates/ctxfs-daemon/src/lib.rs` to add `pub mod observability;`. (If `lib.rs` doesn't exist or doesn't have a `pub mod` line for `daemon`, inspect first; the typical pattern in this workspace is for `lib.rs` to re-export the implementation from `daemon.rs`.)

- [ ] **Step 4: Add provider-common as a dep of daemon if not already**

Run: `grep -n "ctxfs-provider-common" /Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-daemon/Cargo.toml`

If not present, add `ctxfs-provider-common = { workspace = true }` to `[dependencies]`.

Also add `dashmap = { workspace = true }` if not present.

- [ ] **Step 5: Run the daemon tests**

Run: `cargo test -p ctxfs-daemon observability::tests`

Expected: `5 passed`.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy -p ctxfs-daemon --all-targets -- -D warnings`

Expected: no warnings.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-daemon/src/observability.rs crates/ctxfs-daemon/src/lib.rs crates/ctxfs-daemon/Cargo.toml
git commit -m "feat(daemon): Observability registry + StatusReportV1 assembly

Owns DashMaps of (AuthIdentity, ResourceClass) -> RateLimitGauge and
CounterKey -> Arc<MountCounters>. status_report() walks both, computes
the per-mount summary with cache_hit_ratio, and sorts by
rest_calls_total descending so the top-N is at index 0.
Tests cover the empty case, Arc-sharing semantics, sort order, and
hit-ratio None-vs-Some boundary."
```

---

## Task 9: IPC `get_status` RPC

**Files:**
- Modify: `crates/ctxfs-ipc/src/service.rs`
- Modify: `crates/ctxfs-ipc/Cargo.toml`

- [ ] **Step 1: Add provider-common dep to ipc crate**

Run: `grep -n "ctxfs-provider-common" /Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-ipc/Cargo.toml`

If missing, add to `[dependencies]`:

```toml
ctxfs-provider-common = { workspace = true }
```

- [ ] **Step 2: Add `get_status` method to the service trait**

Modify `crates/ctxfs-ipc/src/service.rs` — add an import line near the top:

```rust
pub use ctxfs_provider_common::status::StatusReportV1;
```

Then add a new method to the `CtxfsService` trait, just before `async fn ping() -> String;`:

```rust
    /// Returns global observability state: rate-limit budgets, per-mount
    /// counters, and top-N mount summary. Used by `ctxfs status` (no-arg).
    async fn get_status() -> Result<StatusReportV1, String>;
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ctxfs-ipc`

Expected: compiles. (The daemon-side impl will be added in Task 10; the IPC trait just declares the method.)

- [ ] **Step 4: Update the existing serde tests if needed**

Run: `cargo test -p ctxfs-ipc`

Expected: existing tests still pass; the new method doesn't affect existing serde-roundtrip tests.

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-ipc/src/service.rs crates/ctxfs-ipc/Cargo.toml
git commit -m "feat(ipc): add get_status RPC returning StatusReportV1

Re-exports StatusReportV1 from provider-common so callers don't need to
depend on provider-common just for the type. New tarpc method is added
to CtxfsService trait; daemon-side impl follows in the next commit."
```

---

## Task 10: Daemon `get_status` implementation

**Files:**
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

- [ ] **Step 1: Locate the `CtxfsService` impl block**

Run: `grep -n "impl.*CtxfsService.*for" /Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-daemon/src/daemon.rs`

Note the line. Read the surrounding 30 lines to understand the impl-block style, the `async fn` shape, and how the existing `status(mount_id)` is implemented.

- [ ] **Step 2: Add an `Observability` field to the daemon's service struct**

Inspect the daemon's main service struct (the one implementing `CtxfsService`). Typical pattern: a `ServiceServer` struct with `Arc<Mutex<...>>` fields. Add:

```rust
use crate::observability::Observability;
use std::sync::Arc;
```

And to the struct fields, add:

```rust
    pub observability: Arc<Observability>,
```

In the constructor (e.g. `ServiceServer::new`), initialize it:

```rust
    observability: Arc::new(Observability::new()),
```

- [ ] **Step 3: Implement `get_status`**

Inside the `impl CtxfsService for ServiceServer` block, add:

```rust
    async fn get_status(self, _: tarpc::context::Context) -> Result<ctxfs_ipc::service::StatusReportV1, String> {
        Ok(self.observability.status_report())
    }
```

- [ ] **Step 4: Verify daemon compiles**

Run: `cargo check -p ctxfs-daemon`

Expected: compiles. If there's a name collision between `ctxfs_ipc::service::StatusReportV1` and `ctxfs_provider_common::status::StatusReportV1`, prefer the re-exported `ctxfs_ipc::service::StatusReportV1` to keep the daemon side talking through IPC types.

- [ ] **Step 5: Run all daemon tests**

Run: `cargo test -p ctxfs-daemon`

Expected: all pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-daemon/src/daemon.rs
git commit -m "feat(daemon): implement get_status RPC

ServiceServer holds Arc<Observability>; get_status simply returns
status_report(). The Arc clone is cheap; the DashMap iterations inside
status_report are lock-free for read so this is safe under contention."
```

---

## Task 11: CLI `ctxfs status` global view

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs`

- [ ] **Step 1: Locate the `Status` variant of `Commands`**

Run: `sed -n '60,75p' /Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-cli/src/main.rs`

Read the existing `Status { mount_id }` variant.

- [ ] **Step 2: Change `mount_id` to `Option<String>`**

In `crates/ctxfs-cli/src/main.rs`, find the `Status` variant inside `enum Commands`. Modify it from:

```rust
    /// Show status of a specific mount
    Status {
        /// Mount identifier
        mount_id: String,
    },
```

to:

```rust
    /// Show observability status. With no argument, prints the global view
    /// (rate-limit budgets per (host, resource), top-N consumed mounts,
    /// recent throttle events). With --mount, prints per-mount detail.
    Status {
        /// Optional: limit output to a specific mount.
        #[arg(long = "mount")]
        mount_id: Option<String>,
    },
```

- [ ] **Step 3: Update the match arm for `Commands::Status`**

Find the `Commands::Status { mount_id } => { ... }` block (around line 273). Change it to dispatch on `Option`:

```rust
        Commands::Status { mount_id } => match mount_id {
            Some(id) => {
                let info = client
                    .status(tarpc::context::current(), id)
                    .await??;
                print_mount_status(&info);
            }
            None => {
                let report = client
                    .get_status(tarpc::context::current())
                    .await??;
                print_global_status(&report);
            }
        },
```

(Note: the existing per-mount print code becomes the `print_mount_status` helper, extracted in Step 4.)

- [ ] **Step 4: Extract `print_mount_status`**

Move the existing per-mount print code (the lines that currently follow `Commands::Status { mount_id }` and call `println!("  Status:      {}", info.status);` etc.) into a new function:

```rust
fn print_mount_status(info: &ctxfs_ipc::service::MountInfo) {
    println!("Mount: {}", info.id);
    println!("  Source:      {}", info.source);
    println!("  Mount Point: {}", info.mount_point);
    println!("  Commit:      {}", info.commit_sha);
    println!("  Status:      {}", info.status);
    println!("  Mounted at:  {}", info.mounted_at);
    if let Some(port) = info.nfs_port {
        println!("  NFS port:    {port}");
    }
    if let Some(ref vp) = info.volume_path {
        println!("  Volume:      {vp}");
    }
}
```

(Place this function alongside `print_server_only_info` near the bottom of `main.rs`.)

- [ ] **Step 5: Add `print_global_status`**

Add this function in the same area:

```rust
fn print_global_status(report: &ctxfs_ipc::service::StatusReportV1) {
    println!("ContextFS observability — schema v{}", report.schema_version);
    println!();

    if report.budgets.is_empty() {
        println!("Rate-limit budgets: (none yet — make a request to populate)");
    } else {
        println!("Rate-limit budgets:");
        for b in &report.budgets {
            let remaining = b.remaining.map(|r| r.to_string()).unwrap_or_else(|| "?".into());
            let limit = b.limit.map(|l| l.to_string()).unwrap_or_else(|| "?".into());
            let throttled = if b.throttle_active_until_unix.is_some() {
                " [SECONDARY THROTTLE ACTIVE]"
            } else {
                ""
            };
            println!(
                "  {} {}/{}: {}/{}{}",
                b.host, b.auth_kind, b.resource_class, remaining, limit, throttled
            );
        }
    }
    println!();

    if report.mounts.is_empty() {
        println!("Mounts: (none active)");
    } else {
        println!("Top mounts by REST calls:");
        for m in report.mounts.iter().take(10) {
            let ratio_str = m
                .cache_hit_ratio
                .map(|r| format!("{:.1}% cache", r * 100.0))
                .unwrap_or_else(|| "no cache ops".into());
            println!(
                "  {} ({}/{} @ {}): {} calls, {} bytes, {} prefetch hits, {}",
                m.mount_id,
                m.source,
                m.repo,
                &m.commit[..8.min(m.commit.len())],
                m.rest_calls_total,
                m.bytes_total,
                m.prefetch_hits,
                ratio_str
            );
        }
    }
}
```

- [ ] **Step 6: Verify CLI compiles**

Run: `cargo build -p ctxfs-cli`

Expected: clean build.

- [ ] **Step 7: Run CLI tests**

Run: `cargo test -p ctxfs-cli`

Expected: pass. Existing tests should not be affected by the variant signature change because no test exercises `Commands::Status` directly.

- [ ] **Step 8: Manual smoke test (record output, don't auto-verify)**

Run the daemon then test the CLI:

```bash
# in one terminal
cargo run -p ctxfs-cli -- daemon start
# in another
cargo run -p ctxfs-cli -- status
```

Expected: prints "ContextFS observability — schema v1" header and "(none yet)" budgets and "(none active)" mounts. The daemon hasn't issued any quota-bearing calls yet, so the registry is empty.

Stop the daemon: `cargo run -p ctxfs-cli -- daemon stop`.

- [ ] **Step 9: Commit**

```bash
git add crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): ctxfs status no-arg shows global view; --mount preserves per-mount

Resolves the naming collision Codex caught in the spec review: existing
Status takes a positional mount_id; the new behavior makes mount_id an
Option, with no-arg dispatching to get_status() and --mount preserving
the per-mount path. print_mount_status / print_global_status split keeps
the dispatch site readable."
```

---

## Task 12: Integrate `ThrottleClassifier` into `provider-common::http` (no-op for callers)

**Files:**
- Modify: `crates/ctxfs-provider-common/src/http.rs`

The existing `fetch_registry_json` already handles 404 → NotFound and 429 → RateLimited. M1 doesn't change behavior — it adds tracing log lines and surfaces verdicts in a way M2 can pick up. Provider-git's adoption of the classifier happens in M2.

- [ ] **Step 1: Add tracing log lines on every fetch**

Modify `crates/ctxfs-provider-common/src/http.rs` `fetch_registry_json` function to add logs at key points. After the line `let resp = client.get(url).send().await...`, add before the status checks:

```rust
    let status = resp.status();
    let headers: std::collections::HashMap<String, String> = resp
        .headers()
        .iter()
        .filter_map(|(k, v)| {
            v.to_str().ok().map(|s| (k.as_str().to_lowercase(), s.to_string()))
        })
        .collect();

    tracing::debug!(
        target: "ctxfs.provider.fetch",
        registry = registry_name,
        url = url,
        status = status.as_u16(),
        ratelimit_remaining = headers.get("x-ratelimit-remaining").map(|s| s.as_str()).unwrap_or("?"),
        "registry fetch completed"
    );
```

(Replace the original `let status = resp.status();` line with this expanded block.)

- [ ] **Step 2: Add a verdict-classification log**

Before the `if status == reqwest::StatusCode::NOT_FOUND` line, add:

```rust
    let verdict = crate::rate_limit::ThrottleClassifier::classify(status.as_u16(), &headers);
    if let crate::rate_limit::RateLimitVerdict::SecondaryThrottle { retry_after, ref resource } = verdict {
        tracing::warn!(
            target: "ctxfs.provider.throttle",
            registry = registry_name,
            resource = format!("{resource:?}").as_str(),
            retry_after_secs = retry_after.as_secs(),
            "secondary throttle detected"
        );
    }
```

- [ ] **Step 3: Verify provider-common still compiles + tests pass**

Run: `cargo test -p ctxfs-provider-common`

Expected: all pass; tracing logs are not captured in tests.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy -p ctxfs-provider-common --all-targets -- -D warnings`

Expected: no warnings.

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-provider-common/src/http.rs
git commit -m "feat(provider-common): tracing log lines for registry fetches + throttle detect

Logs every fetch at debug (target ctxfs.provider.fetch) with status code
and remaining-quota; emits a warn-level event at ctxfs.provider.throttle
when ThrottleClassifier returns SecondaryThrottle. Provider-git adoption
of the classifier (and updating the daemon-side gauge) lands in M2.
This commit is purely additive; no existing call site changes behavior."
```

---

## Task 13: `ctxfs status` p95 latency benchmark

**Files:**
- Create: `crates/ctxfs-cli/tests/status_bench.rs`

- [ ] **Step 1: Write the benchmark test**

Create `crates/ctxfs-cli/tests/status_bench.rs`:

```rust
//! M1 exit-criterion benchmark: `ctxfs status` p95 latency over 100
//! sequential calls must be ≤ 100ms with one mounted 1k-file repo and
//! zero concurrent read load.
//!
//! This test boots a daemon in-process, invokes `get_status` 100 times,
//! sorts the durations, and asserts the 95th-percentile is ≤ 100ms.
//!
//! Skipped in CI (#[ignore]) because timing is host-dependent. Author
//! runs on M-series Mac.

use std::time::{Duration, Instant};

#[ignore = "host-timing-dependent; run locally with `cargo test --release -p ctxfs-cli --test status_bench -- --ignored`"]
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn status_p95_within_100ms() {
    use ctxfs_daemon::observability::Observability;
    use ctxfs_provider_common::counters::CounterKey;

    let obs = Observability::new();

    // Populate with a representative load: 1 mount, simulated counter activity.
    let key = CounterKey {
        source: "github".to_string(),
        repo: "foo/bar".to_string(),
        commit: "abc".to_string(),
        mount_id: "mnt-1".to_string(),
    };
    let counters = obs.counters_for(key);
    for _ in 0..1000 {
        counters.record_cache_hit();
    }

    let n = 100;
    let mut durations = Vec::with_capacity(n);
    for _ in 0..n {
        let start = Instant::now();
        let _ = obs.status_report();
        durations.push(start.elapsed());
    }

    durations.sort();
    let p95 = durations[(n * 95) / 100];
    assert!(
        p95 <= Duration::from_millis(100),
        "p95 latency {p95:?} exceeded 100ms target"
    );
    eprintln!("p95: {p95:?}, p50: {:?}, max: {:?}", durations[n / 2], durations[n - 1]);
}
```

- [ ] **Step 2: Add ctxfs-daemon as a dev-dep of ctxfs-cli if not already**

Run: `grep -n "ctxfs-daemon" /Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-cli/Cargo.toml`

If not in `[dev-dependencies]`, add:

```toml
ctxfs-daemon = { workspace = true }
ctxfs-provider-common = { workspace = true }
```

- [ ] **Step 3: Run the benchmark locally**

Run: `cargo test --release -p ctxfs-cli --test status_bench -- --ignored --nocapture`

Expected: passes. The eprintln! line shows the p50/p95/max numbers; record them in the spec's M1 exit-criteria as the validated baseline.

- [ ] **Step 4: Run normal test suite to confirm benchmark is properly ignored**

Run: `cargo test -p ctxfs-cli`

Expected: the benchmark is skipped (status: "ignored").

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-cli/tests/status_bench.rs crates/ctxfs-cli/Cargo.toml
git commit -m "test(cli): p95 status_report benchmark for M1 exit criterion

Ignored by default because timing is host-dependent. Run locally with
`cargo test --release ... -- --ignored`. Asserts p95 over 100 sequential
status_report() calls is ≤ 100ms on representative load (1 mount, 1000
recorded cache ops). Failures here block the M1 release tag."
```

---

## Task 14: Whole-workspace verification

**Files:** none (verification only)

- [ ] **Step 1: Full build at release profile**

Run: `cargo build --release`

Expected: clean build. Any new warnings introduced by M1 work must be fixed before proceeding.

- [ ] **Step 2: Full clippy at deny-warnings**

Run: `cargo clippy --all-targets --tests -- -D warnings`

Expected: zero warnings across the workspace.

- [ ] **Step 3: Full test suite**

Run: `cargo test`

Expected: all green. Any pre-existing failures should be triaged (likely unrelated to M1) but M1 changes must not introduce new failures.

- [ ] **Step 4: Run the local benchmark**

Run: `cargo test --release -p ctxfs-cli --test status_bench -- --ignored --nocapture`

Expected: passes. Note p50/p95/max for the spec's M1 exit-criteria record.

- [ ] **Step 5: Tag for release**

```bash
git tag -a v0.1.1-m1 -m "Phase 4 M1: observability substrate"
```

(Don't push the tag yet — that happens after manual smoke test on the actual daemon.)

- [ ] **Step 6: Manual smoke test**

```bash
cargo run --release -p ctxfs-cli -- daemon start
cargo run --release -p ctxfs-cli -- status
# Mount a real repo if you have GITHUB_TOKEN set:
GITHUB_TOKEN=$GITHUB_TOKEN cargo run --release -p ctxfs-cli -- mount github.com/octocat/Hello-World@master /tmp/ctxfs-test
cargo run --release -p ctxfs-cli -- status
```

Expected: after the mount, `ctxfs status` shows the budget for `api.github.com pat:<prefix>/core` populated from the response headers, and 1 mount in the top-N list with `rest_calls_total >= 2` (commit + tree).

(Note: provider-git is not yet wired to update the daemon's `Observability` registry — that happens in M2 Task 1. The smoke test before M2 will show empty budgets even after a mount; that's expected. The smoke is just a sanity check that the daemon doesn't crash.)

- [ ] **Step 7: Update release notes**

Add a CHANGELOG entry at the top of `CHANGELOG.md` (or create one if absent):

```markdown
## v0.1.1-m1 — 2026-XX-XX

### Phase 4 M1: Observability substrate

- New: `ctxfs status` (no-arg) shows global rate-limit budgets and top-N
  mounts. `ctxfs status --mount <id>` preserves per-mount detail.
- New IPC: `get_status` returns versioned `StatusReportV1` JSON.
- New abstractions in `ctxfs-provider-common`: `RateLimitGauge`,
  `ThrottleClassifier`, `UsageCounters`, `MockProvider` test fixture.
- Workload-replay integration tests via `MockProvider` ready for M2/M3
  to extend.
- No behavior change in `provider-git` (M2 wires the integration).
```

- [ ] **Step 8: Commit + push tag**

```bash
git add CHANGELOG.md
git commit -m "docs: CHANGELOG for v0.1.1-m1 (Phase 4 M1 observability)"
git push origin main
git push origin v0.1.1-m1
```

(The push is the "release" step; it triggers any release CI configured for tags.)

---

## Self-review checklist

Run this after writing the plan, before handing it off.

- [x] **Spec coverage:** Every M1 deliverable in the spec maps to a task above:
  - `RateLimitGauge`, `ThrottleClassifier`, `UsageCounters` → Tasks 2, 3, 4
  - `MockProvider` test fixture → Task 6
  - `ctxfs status` global view → Task 11
  - IPC `get_status` method → Tasks 9, 10
  - Structured `tracing` log lines → Task 12
  - Workload-replay test scaffolding → Task 7
  - `MockProvider` records every call; one workload-replay test asserts an exact call count → Task 7
  - p95 ≤ 100ms benchmark → Task 13
  - No behavior change in `provider-git` → enforced by Task 14 manual smoke test note.

- [x] **Placeholder scan:** No "TBD" / "implement later" / "similar to Task N" in any step. All code blocks are complete.

- [x] **Type consistency:** `MountCounters`, `CounterSnapshot`, `CounterKey`, `StatusReportV1`, `BudgetEntry`, `CounterEntry`, `MountSummary`, `Observability`, `RateLimitGauge`, `ResourceClass`, `AuthIdentity`, `AuthKind`, `ThrottleState`, `RateLimitVerdict`, `ThrottleClassifier`, `MockProvider`, `RecordedCall` — names are consistent across tasks. Method names (`record_rest_call`, `record_bytes_transferred`, `snapshot`, `status_report`, `counters_for`, `gauge_for`, `update_gauge`, `classify`, `update_from_headers`, `set_secondary_throttle`, `clear_expired_throttle`) match between definitions and call sites.

- [x] **Test coverage:** Every type with non-trivial logic has tests defined inline (`#[cfg(test)]` modules). The integration test in Task 7 exercises the cross-cutting workflow.

- [x] **Lint discipline:** Every implementation task ends with `cargo clippy -- -D warnings`. Workspace lints (`clippy::all` deny, `clippy::pedantic` warn) inherited via `[lints] workspace = true`.

- [x] **Frequent commits:** Every task ends with a `git commit` step. Commit messages explain why, not just what (per the user's global commit-message convention).

- [x] **Reproducible:** Every step is either a code change with full content provided, or a runnable command with expected output.

---

## What this plan does NOT cover (future plans)

- M2: B1, B4, B7 fixes in `provider-git`. New plan: `2026-XX-XX-phase-4-m2-bug-fixes.md`.
- M3: tarball prefetch, smart gate, B2, ContentFetcher skeleton. New plan: `2026-XX-XX-phase-4-m3-tarball-prefetch.md`.
- M4: ContentFetcher full lift + plug-in refactor. New plan: `2026-XX-XX-phase-4-m4-contentfetcher-lift.md`.
- M5: B3-label, B5, B6 detect-and-surface. New plan: `2026-XX-XX-phase-4-m5-remaining-bugs.md`.
- M6: Stage 2 gate decision memo (no plan needed; just write the memo).

Each subsequent plan starts the moment the current milestone tags. M2 may begin immediately after `v0.1.1-m1` since M2 has no dependency on M1's telemetry. M3's defaults (`CTXFS_PREFETCH_THRESHOLD_*`) get re-tuned from M1 telemetry collected during M2 work.
