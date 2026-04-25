# Phase 4 — Rate-Limit & Efficient-Fetch Design Spec

**Date**: 2026-04-25
**Status**: Design validated via brainstorming session 2026-04-25 (cmux agent team — option-a-advocate, option-b-advocate, bug-triage). Codex second-opinion review applied 2026-04-25.
**Scope**: Make ContextFS read paths efficient enough that "user mounts a dependency and doesn't think about storage or quotas" holds for typical workloads, *measurably*. Build the efficiency machinery once at the right abstraction layer (`ctxfs-provider-common`) so the second native-content provider inherits it. Stop short of a full Git-native rewrite — that work moves to Phase 5, gated on telemetry from this phase.

## Motivation

After Phase 3, ContextFS v0.1.0 ships through Homebrew + Sparkle. The single user is the author. During wrap-up, Codex (via `counsel`) flagged a structural gap in `ctxfs-provider-git`: every blob fetched on first read is its own GitHub REST call (`/git/blobs/{sha}`), so a cold `rg .` over a 5,000-file repo can torch the entire authenticated 5,000 req/hr budget — and that's just for the *current* user; the UX promise of "users mount, don't worry about storage or maintenance" pulls toward aggressive prefetching, which makes the structural problem worse.

A team brainstorm produced two competing memos (`docs/phase4-option-a-memo.md`, `docs/phase4-option-b-memo.md`) and a triage report (`docs/phase4-bug-triage.md`). Both memos converged on a near-term Stage 1 (tarball prefetch + bug fixes) and diverged only on whether to *also* build a full Git-native object store + `cat-file --batch` pipeline (Stage 2). Phase 4 commits to Stage 1 — measured against telemetry that will be the entire justification (or not) for Stage 2 in Phase 5.

GitHub is one source of many in the long-term plan (npm, PyPI, crates.io content fetchers will follow). Phase 4's machinery lives in `ctxfs-provider-common` so each future provider plugs in rather than re-deriving.

The kickoff context for this phase is `docs/phase4-rate-limit-handoff.md`. The Codex review that produced the v2 of this spec is at `/tmp/counsel/20260425-171401-claude-to-codex-05ae29/codex.md`.

---

## Definitions

These terms appear throughout the spec; pin them once so milestone exit criteria are unambiguous.

- **REST call** — a *quota-bearing* GitHub API request: any HTTP request to `api.github.com` whose response carries `x-ratelimit-*` headers. The 302 redirect from `/repos/{o}/{r}/tarball/{ref}` to its `codeload.github.com` URL counts as **one** REST call (the initial API request); the redirect target download is a non-quota-bearing transfer and does not count. Counters distinguish `rest_calls_total` from `http_transfers_total`.
- **HTTP transaction** — any HTTP request/response pair, including codeload tarball downloads. Used in transfer-volume counters but not in rate-limit budgets.
- **Auth identity** — the credential under which a request is made: `(host, token_kind, token_id)` where `token_kind ∈ {Anonymous, PAT, GitHubApp}`. Each auth identity has its own GitHub-side quota.
- **Resource class** — GitHub's `x-ratelimit-resource` header value (e.g., `core`, `search`, `code_search`, `graphql`). Each resource has its own bucket per auth identity.
- **Source** — the registry namespace a content fetch was issued through: `github`, `npm`, `pypi`, `crates`. A given `(auth_identity, resource_class)` may be exercised by multiple sources (e.g., crates.io resolves to a GitHub repo, both share the GitHub `core` resource for that PAT).
- **Mount** — a single `ctxfs mount` invocation; identified by a `mount_id` UUID. Multiple mounts can target the same source/repo/commit.

---

## Core Architectural Decisions

### 1. Stage 1 only; Stage 2 deferred to Phase 5 gated on telemetry

The team brainstorm rejected the option of committing to a full Git-native rewrite up front. Reasoning:

1. **"Solve the root cause" is compatible with Stage 1** if observability proves Stage 1 was sufficient. The root cause — "1 REST call per blob" — is structurally killed by tarball prefetch on the bulk-scan path; the lazy per-blob path remains for one-off reads (where it is the right primitive, not the wrong one).
2. **The abstraction lift to `provider-common` means Stage 2 composes with Stage 1's work.** Stage 2 becomes a new `FetchPolicy` plugged into the same `ContentFetcher` trait + throttle + observability primitives. It is not a rewrite of Phase 4 work.
3. **Stage 2 carries known unknowns** — partial-clone naïve trap (per-object `fetch-pack` calls with repeated auth handshake), `cat-file --batch` lifecycle (per-repo? pool? per-request?), LFS smudge change. These are best designed from telemetry, not first principles.
4. **v0.1.0 just shipped to a single-user audience.** Stage 2 is multi-week implementation cost (subprocess management, pipe-protocol parsing, migration from the SHA-256-keyed cache). Hard to justify before measurement.

Phase 4 ends with an M6 go/no-go memo for Phase 5 grounded in real numbers.

### 2. Efficiency machinery lives in `ctxfs-provider-common`, not `ctxfs-provider-git`

GitHub is one source. Future native-CDN content providers (npm tarballs, PyPI sdists, crates.io `.crate` files) will hit the same rate-limit / throttle / observability concerns. The brainstorm chose to lift the cross-cutting machinery up front rather than refactor later.

New abstractions in `ctxfs-provider-common`:

- **`RateLimitGauge`** — budget tracker keyed by `(auth_identity, resource_class)`. Holds `limit`, `remaining`, `reset_at`, `secondary_throttle_state`. Updated from response headers on every quota-bearing call. **Not** keyed by source — multiple sources may share a budget (crates.io → GitHub on the same PAT).
- **`ThrottleClassifier`** — given an HTTP response, classifies as `Ok | PrimaryExhausted{reset_at} | SecondaryThrottle{retry_after} | Other(StatusCode)`. Centralizes the secondary-throttle handling that B4 currently misses. Returns the parsed `x-ratelimit-resource` so the caller can update the right gauge.
- **`ContentRequest` + `FetchPolicy` + `ContentFetcher` trait** — the source-agnostic content-fetching contract. Generalized from "fetch N blob SHAs" to a request shape that fits npm tarballs, PyPI sdists, and crates.io `.crate` files as well as Git blobs:
  ```rust
  pub struct ContentRequest {
      pub path: PathBuf,        // mount-relative path (semantic key)
      pub digest: Option<Digest>, // content hash if the source provides one
      pub size: Option<u64>,    // estimated bytes if known from manifest
      pub kind: ContentKind,    // File | Symlink | LfsPointer
  }
  pub enum FetchMode { Lazy, BulkPrefetch, Forced }
  pub trait ContentFetcher {
      fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate;
      fn fetch_batch(&self, requests: &[ContentRequest], mode: FetchMode) -> Result<...>;
  }
  ```
  A skeletal version of this trait lands in **M3** (not M4) so M3's tarball-vs-lazy decision can be expressed *as* a `FetchPolicy`, not baked into `GitHubProvider`.
- **`UsageCounters`** — atomic counters keyed by `(source, repo, commit, mount_id)`. Tracks `rest_calls_total`, `http_transfers_total`, `bytes_total`, `throttle_events`, `prefetch_hits`, `prefetch_failures`, `truncated_tree_fallbacks`, `cache_hits`, `cache_misses`, `lfs_pointer_files`. Daemon owns the registry; CLI reads via IPC. Note: counters are usage-side; budgets are auth-side. The two are separate dimensions for a reason — see Definitions.

`ctxfs-provider-common` may **not** depend on `ctxfs-cache` or `ctxfs-daemon`. The dependency direction is `cache, daemon → provider-common → core`. This is preserved by today's `Cargo.toml`s and must continue.

`ctxfs-provider-git` becomes the first consumer.

### 3. Tarball prefetch with smart auto-gate

GitHub publishes `/repos/{o}/{r}/tarball/{ref}` which returns the entire repo as a single tarball — one quota-bearing API call (302 to codeload) replaces N blob calls. The naïve "always tarball" hurts the "I just want one file" case; the naïve "never tarball" leaves the bulk-scan budget exposed.

The brainstorm chose an **auto-gate** with two dimensions:

- **`blob_count >= CTXFS_PREFETCH_THRESHOLD_COUNT`** (default `30`)
- **`estimated_bytes <= CTXFS_PREFETCH_MAX_BYTES`** (default `min(CTXFS_CACHE_MAX_BYTES / 4, 256 MB)`)

Both conditions must hold for auto-prefetch to fire. Bytes are computed from the `size` field already present in the recursive tree response (no extra API call). A 30-blob repo of 100 MB binaries will skip the gate; a 30-blob repo of source files will trigger it.

When `estimated_bytes > CTXFS_PREFETCH_MAX_BYTES`, the gate logs `tracing::warn!`, increments `prefetch_skipped_oversized`, and falls back to lazy mode. `ctxfs status` surfaces the skip with the byte estimate so the user sees why.

CLI overrides via `MountOptions { prefetch: PrefetchPolicy }`:
- `ctxfs mount <ref>` → `PrefetchPolicy::Auto` (gate decides)
- `ctxfs mount <ref> --prefetch` → `PrefetchPolicy::Force` (bypass byte cap, with warning if estimated_bytes > cache budget)
- `ctxfs mount <ref> --no-prefetch` → `PrefetchPolicy::Disabled`

`PrefetchPolicy::Force` with `estimated_bytes > CTXFS_CACHE_MAX_BYTES` issues a stronger warning that the warm-cache guarantee will not hold (the prefetched tarball will be partially evicted by the time the scan finishes).

Threshold defaults are env-tunable via `CTXFS_PREFETCH_THRESHOLD_COUNT` and `CTXFS_PREFETCH_MAX_BYTES`. Both are M3 deliverables in `ctxfs-core::config` (currently no field for either; see [`config.rs`](../../../crates/ctxfs-core/src/config.rs)). Defaults will be re-tuned from M1 telemetry before M3 ships.

### 4. Tarball hydration is atomic and verified

Codex's review caught that today's `BlobCache::put` writes blob files directly (not temp-and-rename). On daemon crash mid-prefetch, the cache index reconstructs from whatever bytes exist on disk, which can include corrupt half-written blobs. M3 makes hydration safe by construction:

1. **Streaming extraction.** Tarball entries are streamed from the HTTP response and decoded with `tar::Archive` without buffering the full tarball to disk. Memory ceiling: per-entry size, not full archive.
2. **Path normalization.** Each tar entry's path is canonicalized; `..` traversal, absolute paths, and entries outside the repo namespace are rejected with a counter increment (`tarball_invalid_entries`).
3. **Per-blob temp file + verify + atomic rename.** Each entry is written to `<cache_dir>/tmp/<random>`, then the in-progress file is hashed (Git blob SHA-1: `sha1("blob <size>\0" || content)`) and compared against the manifest digest. Mismatch → discard, increment `tarball_digest_mismatch`, fall through to lazy fetch for that blob. Match → atomic rename into the canonical cache path.
4. **Concurrent-mount dedupe (singleflight).** Two simultaneous `ctxfs mount` calls for the same `(host, repo, commit)` collapse to a single tarball download. Subsequent mounts wait on the in-flight prefetch via a daemon-side `dashmap<TarballKey, Arc<Notify>>`. Cancelling the first mount cancels the prefetch only if no waiters remain.
5. **Daemon restart cleanup.** On startup, the daemon scans `<cache_dir>/tmp/` and unlinks anything older than 1 hour. Counter `temp_orphans_cleared`.
6. **Redirect security.** When following the 302 from `/tarball/{ref}`:
   - `Location` is parsed and required to have scheme `https` and host `codeload.github.com` (or the host configured via `CTXFS_GITHUB_HOST` for self-hosted). Other hosts → reject, error.
   - The `Authorization` header is **stripped** before following (the codeload domain doesn't need it; leaking GHE PATs is a real CVE pattern in OSS Git tooling).
   - The redirect chain is bounded at depth 3.

### 5. Observability ships first, as the measurement tool for everything else

M1 (observability + simulation harness) is intentionally the first milestone. Counter machinery in `provider-common`, `ctxfs status` CLI surface (renamed and re-shaped — see below), structured `tracing` log lines, JSON-over-IPC payload. A `MockProvider` test fixture records every call so workload-replay tests can measure provider call counts without hitting the real API.

This means M2+ (architecture-neutral fixes, tarball prefetch, etc.) ride the new counters, so we can produce concrete before/after numbers — not anecdotes — for each milestone.

**Naming collision resolution.** The existing `ctxfs status` subcommand in `ctxfs-cli` already takes a `mount_id` and returns mount-specific status. The new "budget + counters" surface needs a different shape. Resolution:
- `ctxfs status` (no args) — global view: rate-limit budgets per `(auth_identity, resource_class)`, recent throttle events, top-N mounts by `rest_calls_total`, prefetch hit rate.
- `ctxfs status --mount <id>` — preserves existing behavior; per-mount detail.
- IPC payload uses a versioned schema `StatusReportV1 { budgets: Vec<BudgetEntry>, counters: Vec<CounterEntry>, mounts: Vec<MountSummary> }` with explicit `schema_version` field so future versions evolve safely.

### 6. All bugs fold into the spec; no separate GitHub issues

The bug-triage report identified eight defects in `ctxfs-provider-git` (B1–B8). With a single user (the author), there is no community value in GitHub-issue overhead. Each bug is a milestone item below; resolution lands as part of Phase 4 work.

---

## Bug Inventory (folded into milestones)

| ID | One-line | Milestone |
|---|---|---|
| **B1** | Tiny-file inlining (`FileEntry.inline_content` always `None`) | M2 |
| **B2** | Truncated-tree fallback (>100k entries / 7 MB) silently produces partial mounts | **M3** *(moved from M5: auto-gate depends on a complete tree)* |
| **B3-label** | Digest mislabeled `HashAlgorithm::Sha256` (it's SHA-1) | M5 |
| **B3-verify** | No content verification against any hash | **Partial in M3** (tarball-hydration verify) / Phase 5 (full multi-tenant) |
| **B4** | Secondary rate-limit responses cascade as EIO instead of clean `RateLimited` | M2 |
| **B5** | LRU cache eviction breaks "second grep is free" for large repos | M5 |
| **B6** | LFS payloads return pointer files | **M5 detect + surface in `ctxfs status`** / Phase 5 (full smudge) |
| **B7** | Symlinks always read as empty string (`target: String::new()`) | M2 |
| **B8** | `active_source` race / design hazard | Phase 5 — **constraint**: M4 must not introduce shared fetchers relying on `active_source` (preserve per-mount provider creation in `daemon.rs`) |

---

## Architecture

### Crate-level changes

| Crate | Change |
|---|---|
| `ctxfs-provider-common` | **New code.** `RateLimitGauge`, `ThrottleClassifier`, `ContentRequest`/`FetchPolicy`/`ContentFetcher` trait family, `UsageCounters`, `MockProvider` test helper. May depend only on `ctxfs-core` (no cycle into cache/daemon). |
| `ctxfs-provider-git` | Adopts `provider-common` machinery. Wires B1, B4, B7 (M2), tarball prefetch + B2 truncated-tree fallback (M3), implements `ContentFetcher` trait (M4). Adds B6 detect-and-surface (M5). |
| `ctxfs-cache` | Per-repo cache reservation policy (B5 mitigation). Atomic temp-and-rename writes for tarball hydration (M3). Restart-time temp orphan cleanup. |
| `ctxfs-core` | Add `HashAlgorithm::Sha1` variant (B3-label). Add `prefetch_threshold_count` and `prefetch_max_bytes` fields to `Config` and env parsing. |
| `ctxfs-cli` | New `ctxfs status` global view (no args) + preserved `ctxfs status --mount <id>` per-mount detail. New `--prefetch` / `--no-prefetch` flags on `ctxfs mount` (mapped to `MountOptions::prefetch`). |
| `ctxfs-ipc` | Mount RPC extended with `MountOptions { prefetch: PrefetchPolicy }`. New `GetStatus` IPC method returning versioned `StatusReportV1` JSON. |
| `ctxfs-daemon` | Holds `RateLimitGauge` + `UsageCounters` registry. Implements `GetStatus`. Singleflight tarball-prefetch dedupe map. Daemon-restart temp cleanup. **Continues to spawn a fresh `GitHubProvider` per mount** (B8 constraint). |

### Component diagram

```
┌────────────────────────────────────────────────────────────────────┐
│                          ctxfs-cli                                 │
│   ctxfs mount <ref> [--prefetch|--no-prefetch]                     │
│   ctxfs status               ctxfs status --mount <id>             │
│        │                            │                              │
│        │ IPC: Mount(MountOptions)   │ IPC: GetStatus               │
└────────┼────────────────────────────┼──────────────────────────────┘
         │                            │
┌────────▼────────────────────────────▼──────────────────────────────┐
│                         ctxfs-daemon                               │
│   ┌────────────────────────────┐ ┌─────────────────────────────┐   │
│   │ RateLimitGauges (auth      │ │ UsageCounters (per          │   │
│   │ identity × resource class) │ │ source × repo × commit ×    │   │
│   │  limit, remain, reset_at,  │ │ mount_id)                   │   │
│   │  secondary_throttle_state  │ │  rest_calls_total,          │   │
│   └────────────────────────────┘ │  http_transfers_total, ...  │   │
│   Singleflight tarball dedupe map└─────────────────────────────┘   │
│   Per-mount GitHubProvider (B8 constraint)                          │
└────────┼─────────────────────────────────────────────────────────┘ │
         │                                                          │
┌────────▼──────────────────────────────────────────────────────┐   │
│                ctxfs-provider-common (NEW)                    │   │
│   trait ContentFetcher (ContentRequest, FetchPolicy)          │   │
│   ThrottleClassifier::classify(response) -> RateLimitVerdict  │   │
│   RateLimitGauge / UsageCounters / MockProvider               │   │
└────────┼──────────────────────────────────────────────────────┘   │
         │ implements                                                │
┌────────▼──────────────────────────────────────────────────────┐   │
│                ctxfs-provider-git                              │   │
│   Stage 1 ContentFetcher impl:                                 │   │
│     auto-gate (count + bytes) → tarball or lazy                │   │
│     B1+B2+B4+B7 fixes wired                                    │   │
│     B6 detect-and-surface                                      │   │
└────────┼──────────────────────────────────────────────────────┘   │
         │ writes via                                                │
┌────────▼──────────────────────────────────────────────────────┐   │
│                ctxfs-cache                                     │   │
│   Atomic temp-and-rename + verify (M3)                         │   │
│   Per-repo reservation slices (M5, B5 mitigation)              │   │
│   Restart-time temp-orphan cleanup (M3)                        │   │
└───────────────────────────────────────────────────────────────┘   │
                                                                    │
                            (reads via VFS) ──────────────────────────
```

### Data flow — cold mount of a 1k-file repo, 30 MB total (typical)

1. User runs `ctxfs mount github.com/foo/bar@v1.0.0`.
2. CLI sends `Mount { source, mount_point, backend, options: MountOptions { prefetch: Auto } }` RPC to daemon.
3. `provider-git` calls `/repos/foo/bar/commits/v1.0.0` → resolves SHA. **(1 REST call.)** `RateLimitGauge` for `(github_pat_xxx, core)` updated from `x-ratelimit-*` headers.
4. `provider-git` calls `/repos/foo/bar/git/trees/{sha}?recursive=1` → manifest. **(1 REST call.)** Sums `size` of all blob entries: `estimated_bytes = 30 MB`.
5. Truncation check: `truncated == false` → manifest is complete. (If `truncated == true`, B2 fallback fires; see below.)
6. Auto-gate: `blob_count = 1000 >= 30` AND `estimated_bytes = 30 MB <= prefetch_max_bytes (256 MB)` → fire.
7. Singleflight check: no in-flight prefetch for `(github.com, foo/bar, sha)` → claim the slot.
8. `provider-git` calls `/repos/foo/bar/tarball/{sha}` → 302 to `codeload.github.com/...`. **(1 REST call recorded against `core` budget.)** Redirect security check (host=codeload, scheme=https, depth ≤ 3); strip `Authorization`; follow.
9. Streaming tar extraction with per-blob temp-and-verify-and-rename. `prefetch_hits` counter incremented.
10. Manifest resolved with all blob bytes already cached. Subsequent reads are pure cache hits.
11. **Total: 3 quota-bearing REST calls + 1 codeload transfer** for any subsequent volume of reads against this mount until eviction.

### Data flow — cold mount of a 10-file repo (below count gate)

1. Steps 1–5 same as above. `blob_count = 10 < 30` → gate skips tarball.
2. Reads happen lazily.
3. Files ≤ 4 KB are returned from `inline_content` in the manifest — but B1 only inlines content already provided by the tree response, which **doesn't include blob content** (see Codex correction). The `size` field is in the tree; the bytes are not. So B1's contribution is *deferring* the small-blob fetches until first read, where they're served from inline-content if a separate small-blobs-prefetch fired during tree walk. **Practically: B1's "inline tiny files" only saves API calls if implementation bundles a small-blob-prefetch into the tree-walk path.** The implementation plan must address this explicitly; see M2 below.
4. Files > 4 KB require a `/git/blobs/{sha}` call each on first read.
5. **Total: 2 REST calls + (count of files needing read) blob calls.**

### Data flow — truncated-tree response (B2)

1. Step 4 returns a tree response with `truncated == true`. Counter `truncated_tree_fallbacks` incremented; structured-log `tree_truncated` event.
2. `provider-git` walks the tree per-directory: starting from root, calls `/repos/foo/bar/git/trees/{sha}?recursive=0` (1 call), then for each subtree-typed entry, recurses with the same non-recursive call. **(N REST calls where N = directory count.)**
3. With a complete manifest, the auto-gate evaluates as in steps 5–11.

### Data flow — secondary throttle hit mid-fetch

1. `provider-git` issues a fetch.
2. GitHub returns `429 Too Many Requests` with `Retry-After: 60` and `x-ratelimit-remaining: 4500` (still nonzero), `x-ratelimit-resource: core`.
3. `ThrottleClassifier::classify` returns `SecondaryThrottle { retry_after: 60s, resource: "core" }`.
4. `RateLimitGauge` for `(auth_identity, core)` flags `secondary_throttle_state = Active(60s)`. `throttle_events` counter incremented.
5. `provider-git` returns a clean `RateLimited` provider error to the VFS layer. **B4 fixed: no EIO surface.**
6. `ctxfs status` shows: "Throttled (core), wait 60s. Remaining 4500/5000."

### Error handling

| Scenario | Behavior |
|---|---|
| Tarball download fails partway | Per-blob temp files discarded. Counter `prefetch_failures` incremented. Mount falls back to `PrefetchPolicy::Disabled` for this session. Subsequent reads use lazy per-blob. |
| Primary rate-limit exhausted (`x-ratelimit-remaining == 0`) | `RateLimited { reset_at }` returned to VFS. Read paths return `EAGAIN`-equivalent (provider-defined). User-facing error from `ctxfs status` shows reset time. |
| Secondary throttle (429 / 403 + `Retry-After`) | `RateLimited { retry_after }`. `RateLimitGauge.secondary_throttle_state = Active(retry_after)`. New fetches deferred. |
| Truncated-tree response (>100k entries / >7 MB) | (B2) Per-directory non-recursive walk fallback. Counter `truncated_tree_fallbacks`. |
| LFS pointer file detected | (B6) Read returns the pointer-file bytes (truthful behavior — pointer is what's there). Counter `lfs_pointer_files`. `ctxfs status` shows count + sample paths under "LFS pointer files (smudge in Phase 5)". |
| Tarball entry fails digest verification | Per-blob entry discarded. Counter `tarball_digest_mismatch`. Blob falls through to lazy fetch on first read. Other entries in the tarball are unaffected. |
| Tarball entry has invalid path (`..`, absolute, escapes repo namespace) | Entry rejected. Counter `tarball_invalid_entries`. Logged with offending path at `tracing::warn!`. |
| Tarball larger than `CTXFS_PREFETCH_MAX_BYTES` (auto mode) | Skip prefetch; `prefetch_skipped_oversized` counter; `ctxfs status` shows skip reason. Mount continues with lazy reads. |
| Tarball larger than `CTXFS_CACHE_MAX_BYTES` (force mode) | Strong warning logged + surfaced via `ctxfs status`: "warm-cache guarantee will not hold (tarball X MB exceeds cache budget Y MB)". Prefetch proceeds; user opted in via `--prefetch`. |
| Daemon crash mid-prefetch | Per-blob temp files orphaned. On daemon restart, `<cache_dir>/tmp/` swept of files older than 1 hour. Counter `temp_orphans_cleared`. No partial blobs ever land in canonical cache (atomic rename). |
| Concurrent mount of same `(host, repo, commit)` | Singleflight dedupe: second mount waits on the first's prefetch via `Arc<Notify>`. No double-fetch. If first mount is cancelled, second takes ownership of the prefetch. |
| Tarball redirect to non-codeload host | Reject. Error: "Refusing tarball redirect to <host>: not codeload.github.com". User can override via `CTXFS_GITHUB_HOST` env (for GHE). |

### Testing

- **Unit tests**: inline `#[cfg(test)]` per module. `RateLimitGauge` state transitions, `ThrottleClassifier` cases, gate logic (count + bytes), redirect security checks, path-normalization rejection cases.
- **Integration tests**: `crates/ctxfs-provider-common/tests/` — `MockProvider` records every call; workload-replay tests assert exact call counts:
  - `scan_5k_files_with_prefetch.rs` — assert `rest_calls_total == 3` (commit + tree + tarball).
  - `scan_5k_files_no_prefetch_after_b1.rs` — assert `read_time_blob_calls` decreased by ≥50% vs pre-B1 baseline (counts measured after mount completes; tree-walk-time fetches don't count).
  - `scan_below_count_threshold.rs` — assert lazy path used.
  - `scan_below_byte_threshold_oversized.rs` — assert prefetch skipped, `prefetch_skipped_oversized == 1`.
  - `scan_force_prefetch_oversized.rs` — assert prefetch fires with warning.
  - `secondary_throttle_recovery.rs` — assert clean `RateLimited` surfacing, no EIO.
  - `truncated_tree_fallback.rs` — assert per-directory walk fires when tree response stub has `truncated: true`.
  - `tarball_partial_failure_falls_back.rs` — assert lazy mode after partial tarball failure.
  - `tarball_digest_mismatch.rs` — assert mismatched blob discarded, others land.
  - `tarball_path_traversal_rejected.rs` — assert `..`-containing entries rejected.
  - `concurrent_mount_singleflight.rs` — assert two simultaneous mounts of same `(repo, commit)` produce one tarball call.
  - `redirect_to_non_codeload_rejected.rs` — assert tarball rejected when `Location` host isn't codeload.
  - `daemon_restart_clears_temp.rs` — assert orphan temp files cleared on startup.
- **Cache-reservation regression test (M5)**: mount repo A with 100 MB working set; mount repo B with 500 MB (exceeds remaining cache budget); scan A; assert `cache_hits` for A's blobs unchanged (no eviction of A's working set while ≤ A's reservation).
- **Existing**: `ctxfs-provider-git/tests/build_directories.rs` extended to cover B7 symlink resolution and B1 inline path.

---

## Milestones

Each milestone is independently releasable. Suggested release-tagging cadence: M2 lands as `v0.1.1`, M3 as `v0.1.2`, M4 as `v0.2.0`, M5 as `v0.2.1`, M6 produces a memo (no release).

### M1 — Observability + simulation harness *(ships first)*

- `RateLimitGauge`, `ThrottleClassifier`, `UsageCounters` in `ctxfs-provider-common`.
- `MockProvider` test fixture in `ctxfs-provider-common`.
- `ctxfs status` (no-arg global view) + preserved `ctxfs status --mount <id>`.
- IPC `GetStatus` method returning versioned `StatusReportV1` JSON.
- Structured `tracing` log lines for every fetch, throttle, prefetch.
- Workload-replay test scaffolding under `crates/ctxfs-provider-common/tests/`.

**Exit criteria**:
- `ctxfs status` benchmark: **p95 ≤ 100ms over 100 sequential calls**, on author's M-series Mac, with one mounted 1k-file repo and zero concurrent read load. Recorded via a benchmark in `crates/ctxfs-cli/tests/`.
- `MockProvider` records every call; one workload-replay test asserts an exact call count.
- No behavior change in `provider-git` yet (this milestone is observability-only).

### M2 — Architecture-neutral REST fixes (B1, B4, B7)

- B1: tiny-file inlining (≤4 KB) wired into `build_directories`. Implementation note: requires a small-blobs-prefetch during tree walk (one batched `/git/blobs/{sha}` call per ≤4 KB file, fired in parallel up to a cap). Without this, B1 only changes *when* the calls happen, not whether. Implementation plan must address.
- B4: `ThrottleClassifier` adopted in `provider-git`; secondary throttle responses produce `RateLimited` errors.
- B7: symlink target resolution during the tree walk; `target: String::new()` removed; symlink targets fetched as part of B1's small-blobs-prefetch (mode-120000 blobs).
- Workload-replay tests assert before/after numbers using M1 counters.

**Exit criteria**:
- After B1: `read_time_blob_calls` for files ≤ 4 KB drops to 0 on the test corpus (they're already in inline_content).
- After B4: zero EIOs surfaced under simulated 429 + `Retry-After` responses.
- After B7: symlink targets in the test corpus are non-empty strings matching the source repo's symlink targets.

### M3 — Tarball prefetch with smart gate (+ B2, + tarball hardening, + skeletal `ContentFetcher`)

- `provider-git` integrates `/repos/{o}/{r}/tarball/{ref}` endpoint with full hardening:
  - Streaming tar extraction (no full-tarball buffering).
  - Per-blob temp-and-verify-and-rename in `ctxfs-cache`.
  - Path normalization rejection of `..` / absolute / escaping entries.
  - Redirect security: codeload-host whitelist, `Authorization` strip, depth ≤ 3.
  - Singleflight dedupe for concurrent mounts of same `(repo, commit)`.
  - Daemon-restart temp-orphan cleanup.
- Auto-gate logic on `blob_count >= CTXFS_PREFETCH_THRESHOLD_COUNT` AND `estimated_bytes <= CTXFS_PREFETCH_MAX_BYTES`.
- `MountOptions { prefetch: PrefetchPolicy }` in `ctxfs-ipc` Mount RPC; `--prefetch` / `--no-prefetch` CLI flags.
- `CTXFS_PREFETCH_THRESHOLD_COUNT` and `CTXFS_PREFETCH_MAX_BYTES` fields added to `Config` and env parsing in `ctxfs-core`.
- **B2 truncated-tree fallback** — per-directory non-recursive walk when `truncated == true`. Required for the gate to make correct decisions on large repos.
- **Skeletal `ContentFetcher` trait** introduced in `provider-common`. The tarball-vs-lazy decision is implemented as a `FetchPolicy` value, not inline `if`/`else` in `GitHubProvider`. M4 expands this without restructuring.
- M1 counter `prefetch_hits`, `prefetch_failures`, `prefetch_skipped_oversized`, `tarball_digest_mismatch`, `tarball_invalid_entries`, `truncated_tree_fallbacks` all reporting.

**Exit criteria**:
- Cold scan of a 1k-file 30MB repo: `rest_calls_total == 3` (replay test).
- Truncated-tree replay test: per-directory walk fires; manifest is complete.
- Concurrent-mount replay test: two mounts of same `(repo, commit)` produce one tarball call.
- Path-traversal replay test: malicious tarball rejects offending entries; legitimate entries still land.
- Daemon-restart replay test: orphaned temp files cleared on startup.

### M4 — `ContentFetcher` full lift + plug-in refactor

- `ContentRequest` / `FetchPolicy` / `ContentFetcher` finalized in `provider-common` (skeletal version landed in M3).
- `provider-git`'s fetch policy refactored to fully implement `ContentFetcher` rather than inline. The Stage 1 implementation is the first concrete trait impl.
- `RateLimitGauge`, `ThrottleClassifier`, and `UsageCounters` already live in `provider-common` from M1; M4 does not move them.
- **B8 constraint enforced**: M4 must preserve daemon-side per-mount `GitHubProvider` creation in `daemon.rs`. No introduction of shared/global fetchers that rely on `active_source`. CI test asserts `GitHubProvider::new` is called from the per-mount path.
- No external behavior change; pure refactor.

**Exit criteria**:
- A trivial `MockContentFetcher` in tests can implement `ContentFetcher` and be used by a hypothetical second provider without touching `provider-git`.
- B8 constraint test passes.

### M5 — Remaining bugs (B3-label, B5, B6 detect-and-surface)

- B3-label: `HashAlgorithm::Sha1` variant in `ctxfs-core`; call sites updated. (Verification done partially in M3 for tarball entries; full multi-tenant verification stays in Phase 5.)
- B5: per-repo cache reservation in `ctxfs-cache`. **Locked invariant**: an active repo with working set ≤ its reservation receives **zero evictions** triggered by other repos' activity. (Best-effort behavior beyond reservation, with `ctxfs status` warning when a repo's working set exceeds its reservation.)
- B6: detect LFS pointer files (file content matches GitHub LFS pointer regex `^version https://git-lfs\.github\.com/spec/v1\n...`); `tracing::warn!`; counter increment; `ctxfs status` surfaces count and sample paths under a "LFS pointer files (Phase 5: smudge)" section.

**Exit criteria**:
- B3-label: `Digest::Sha1(...)` exists and is used for GitHub blob IDs.
- B5: regression test (described in Testing): mount A (working set ≤ reservation), mount B under cache pressure, scan A, assert `cache_hits` for A's working set unchanged. Assert `eviction_attempts_blocked_by_reservation` counter incremented when B's writes try to evict A's reserved blobs.
- B6: `ctxfs status` shows LFS pointer count and ≤ 3 sample paths when the test corpus includes LFS-tracked files.

### M6 — Stage-2 gate decision memo

- Aggregate M1 telemetry from author's daily use over 2–4 weeks post-M5.
- Identify whether residual rate-limit pain exists: are below-threshold scans still expensive? Are secondary throttles fired in real use? Are tarball-prefetched workloads still hitting per-blob fetches via mid-session reads?
- Produce `docs/phase5-stage2-decision.md` with go/no-go recommendation grounded in numbers.
- **Phase 4 ends with this memo, not a code change.**

**Exit criteria**: memo committed; decision documented; if go, Phase 5 spec brainstorm scheduled.

---

## Out of Scope (Phase 5+ / Future Work)

- **Stage 2 — Git-native object store + `cat-file --batch` pipeline.** Phase 5, gated on M6. Composes with Stage 1 via the `ContentFetcher` trait — not a rewrite.
- **Mirror websites / external GitHub mirrors** as alternative content sources. Phase 5+, evaluated after Stage 2 decision.
- **Multi-tenant cache integrity (B3-verification full).** Per-tenant blob storage, full SHA chain verification, cache-poisoning hardening. Phase 5+, only relevant if ContextFS ever runs as a service. (M3 ships per-blob digest verification on tarball hydration as a partial step; that work composes forward.)
- **Native-CDN content fetchers for npm tarballs / PyPI sdists / crates.io `.crate`.** Phase 6+. Phase 4's `provider-common` abstraction is the foundation they plug into.
- **B8 active_source race/design hazard.** Phase 5 — the right shape depends on whether Stage 2 lands. M4 enforces a constraint (preserve per-mount providers) so Phase 5 can pick up cleanly.
- **LFS smudge to real bytes (B6 full resolve).** Phase 5 — detect-and-surface is the M5 deliverable.
- **`ctxfs status` predicted-cost-of-mount estimates.** Captured here as a soft post-M3 enhancement; cuttable.

---

## Risks / Open Questions

| Risk | Mitigation |
|---|---|
| `CTXFS_PREFETCH_THRESHOLD_COUNT = 30` and `CTXFS_PREFETCH_MAX_BYTES = 256 MB` are guesses | M1 telemetry collected pre-M3 lets us pick data-driven defaults before M3 ships. |
| Tarball endpoint has its own undocumented secondary limits | Tarball failures fall back to lazy mode; counter surfaces failure rate; M6 memo evaluates. |
| GitHub may rate-limit tarballs differently from blob calls | Same gauge / throttle pipeline catches both. M1 telemetry distinguishes via `x-ratelimit-resource`. |
| Per-repo cache reservation may starve small repos when one big one is mounted | Reservation invariant only protects working sets ≤ reservation; status surfaces overage warnings. Cleaner specification deferred to M5 implementation plan. |
| Author is the only user; M6 telemetry is N=1 | Telemetry from synthetic workload-replay tests supplements real use. M6 explicitly notes data sparsity. |
| Phase 4 work overlaps with FSKit work in progress | `provider-git` and `provider-common` are upstream of FSKit; changes are additive. Coordination check at M2 cut. |
| Tarball redirect leaks `Authorization` to non-GitHub host | M3 redirect security: strip header before following; reject non-codeload hosts; bound depth. |
| Daemon crash mid-prefetch leaves corrupt cache state | M3 atomic temp-and-rename + restart-time temp-orphan sweep. Canonical cache only contains verified blobs. |
| Memory pressure during 1 GB tarball extraction | Streaming `tar::Archive` with per-entry processing; no full-tarball buffering. |
| Concurrent mounts of same `(repo, commit)` double-fetch | Singleflight dedupe map in daemon. |
| Path-traversal in tarball entries hydrates unexpected cache locations | Path normalization rejects `..` / absolute / repo-namespace-escaping entries. |
| Tarball entry corruption | Per-blob digest verification before atomic rename; mismatches discarded with counter. |
| B1 implementation regresses to "fetch one blob per tiny file" without batching | M2 implementation plan must include batched small-blobs-prefetch during tree walk. Reviewed at M2 cut. |
| `StatusReportV1` schema needs to evolve | Versioned schema with explicit `schema_version` field. New fields are additive; breaking changes bump to V2 with parallel support. |

---

## Success Criteria (measurable)

Verified by M1 counters + workload-replay tests. **"REST call" means quota-bearing GitHub API call** (excludes codeload tarball downloads following redirect; see Definitions).

- Cold `rg .` on a 1,000-file 30 MB repo with `PrefetchPolicy::Auto`: **`rest_calls_total == 3`** (commit + tree + tarball).
- Cold `rg .` on a 100-file repo (below count gate): **`read_time_blob_calls ≤ count(files > 4 KB)`**, with B1 inlining absorbing ≤ 4 KB files (assuming M2 implements the small-blobs-prefetch correctly).
- Cold `rg .` on a 30-blob 500 MB binary repo: **prefetch skipped**; `prefetch_skipped_oversized == 1`; `ctxfs status` shows skip reason.
- Secondary throttle responses produce **zero EIOs** in the read path.
- `ctxfs status` p95 latency ≤ **100ms** over 100 sequential calls under defined load.
- `MockProvider` exists; at least one workload-replay test per milestone asserts an exact call count.
- B7 symlink targets are non-empty for symlinks in the test corpus.
- Daemon crash mid-prefetch leaves no corrupt blobs in canonical cache; restart sweeps temp orphans.
- Concurrent mounts of same `(repo, commit)` produce exactly one tarball call.
- B5 invariant: active repo with working set ≤ reservation receives zero evictions from other repos' activity.

---

## References

- `docs/phase4-rate-limit-handoff.md` — phase kickoff context, Codex's correction on packfile rate limits, original problem statement.
- `docs/phase4-option-a-memo.md` — Option A (Git-native v2) advocate's case.
- `docs/phase4-option-b-memo.md` — Option B (REST + tarball prefetch) advocate's case.
- `docs/phase4-bug-triage.md` — full B1–B8 triage report.
- `/tmp/counsel/20260425-171401-claude-to-codex-05ae29/codex.md` — Codex review of v1 of this spec; v2 (this document) applies the review.
- `crates/ctxfs-provider-git/src/github.rs` — current REST provider being modified (`build_directories` at L228+, `check_rate_limit` etc.).
- `crates/ctxfs-vfs/src/state.rs` — read path that calls into the provider.
- `crates/ctxfs-provider-common/` — destination crate for new abstractions.
- `crates/ctxfs-cache/src/lib.rs` — `BlobCache::put` (current direct-write; M3 makes atomic).
- `crates/ctxfs-core/src/config.rs` — destination for `prefetch_threshold_count`, `prefetch_max_bytes` fields.
- `crates/ctxfs-ipc/src/service.rs` — Mount RPC; M3 adds `MountOptions`.
- `crates/ctxfs-cli/src/main.rs` — existing `ctxfs status --mount` path; M1 adds no-arg global view.
- `crates/ctxfs-daemon/src/daemon.rs` — per-mount `GitHubProvider::new` site (B8 constraint anchor).
- `CLAUDE.md` — project shape, env vars, lints.

External docs:
- <https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api> — REST quotas + secondary limits + `x-ratelimit-resource` semantics.
- <https://docs.github.com/en/rest/repos/contents> — archive endpoints (`/tarball/{ref}` with documented 302 to codeload).
- <https://docs.github.com/en/rest/git/trees> — recursive-tree caps + truncation handling.
- <https://github.blog/changelog/2025-05-08-updated-rate-limits-for-unauthenticated-requests/> — unauth HTTPS clone limits.
