## v0.1.3-m3 — 2026-04-28

### Phase 4 M3: Tarball prefetch with smart gate (B2)

- **Tarball prefetch**: Cold scan of a repo with ≥ `prefetch_threshold_count`
  blobs (default: 30) and total estimated bytes ≤ `prefetch_max_bytes` (default:
  `min(cache_max_bytes / 4, 256 MB)`) triggers a single
  `GET /repos/{o}/{r}/tarball/{sha}` call instead of one REST call per blob.
  Files stream directly into `BlobCache` atomically: temp-file → hash-verify
  (Git blob SHA-1) → rename → LRU update. Per-entry memory ceiling; no full-
  archive buffer.
- **Fix B2 / truncated-tree fallback**: When GitHub returns `truncated=true`
  on the recursive tree fetch, `fetch_tree_walked` drives a per-directory DFS
  (`fetch_subtree` calls) to assemble a complete manifest. Counter
  `truncated_tree_fallbacks` increments once per fallback. Manifest entries
  with `size=None` force `Lazy` on the auto-gate (fail-closed).
- **Tarball hardening**: Path-traversal entries (`..`, absolute, NUL/control
  bytes) are rejected (`tarball_invalid_entries` counter). Codeload redirect
  target is validated against `codeload_host` (exact match; GHE support is
  planned for a later milestone). Redirect depth capped at 3. Digest mismatches
  skip the entry (`tarball_digest_mismatch` counter). LFS pointer bytes are
  stored verbatim (LFS-aware handling is a future milestone).
- **Singleflight dedupe**: Daemon-level `Arc<TarballSingleflightMap>` shared
  across all per-mount providers prevents duplicate tarball downloads for the
  same `(host, owner, repo, commit)`. Leader downloads; waiters get the
  result for free. Cache pre-check skips download entirely if all blobs are
  already present.
- **Auto-gate** (`PrefetchPolicy::Auto`): `blob_count >= threshold_count`
  AND `estimated_bytes <= max_bytes` → tarball; below threshold → lazy
  (REST per-blob); above byte cap → `prefetch_skipped_oversized` (lazy).
  Force/Disabled modes available via `--prefetch` / `--no-prefetch` CLI flags.
- **`FetchOptions`** passed into `GitHubProvider::fetch_snapshot_with_options`.
  Daemon constructs from `Config` + `MountOptions`. Non-daemon callers use
  the `Provider` trait `fetch_snapshot` which delegates via `FetchOptions::default()`
  (Disabled).
- **M2 carry-forwards landed**:
  - Symlink fail-strict: `build_directories_with_inline` returns `Err` when a
    symlink SHA is missing from the inline map or its bytes are not valid UTF-8.
    No more silent empty-target regression.
  - `bearer_header(token)` helper in `ctxfs-provider-common::http`; eliminates
    3 duplicated `format!("Bearer …").parse().unwrap()` sites.
  - `merge_and_drop_placeholder`: after ref resolution the `<resolving:ref>`
    counter bucket is merged into the real commit bucket so `rest_calls_total`
    == 3 (commit + tree + tarball) is observable from the resolved-SHA key.
  - Assembled-path integration test in `tests/build_directories.rs`.
- **BlobCache additions**: `commit_atomic` (in-memory), `commit_atomic_with_writer`
  (streaming via `BlobTempWriter`), `cleanup_orphan_temps` (daemon startup
  sweep of `tmp/`), `contains_all` (singleflight cache pre-check). Rebuild
  index skips `tmp/` so partial blobs never enter LRU.
- **IPC wire format**: `MountOptions { prefetch: PrefetchPolicy }` added to
  the `mount` RPC. Wire format is not backward-compatible (rebuild daemon +
  CLI together).
- **Replay test suite** (5 integration tests against hand-rolled HTTP mock):
  three-REST-calls, truncated-tree walk, singleflight dedupe, path-traversal
  rejection, oversized-manifest skip.

### Breaking changes

- `tarpc` mount method wire format changed: old CLI + new daemon (or vice versa)
  will fail at connection time. Rebuild both.

## v0.1.2-m2 — 2026-04-26

### Phase 4 M2: Architecture-neutral REST fixes (B1, B4, B7)

- **Fix B1**: `FileEntry.inline_content` now populated for ≤4 KB blobs via
  parallel-capped (8-way) `prefetch_small_blobs` during the tree walk.
  Read-time blob calls drop to zero for tiny files. Files >4 KB still go
  through the lazy per-read path.
- **Fix B4**: ThrottleClassifier adoption in `provider-git` produces clean
  `CtxfsError::RateLimited` for primary AND secondary throttles
  (the latter previously cascaded as generic provider errors). New
  `VfsError::RateLimited` variant; adapter mappings: NFS → `NFS3ERR_JUKEBOX`
  (per RFC 1813 retryable), FSKit → `EAGAIN` (POSIX retryable). The
  user-facing error is now "retry" not "I/O error" through every layer.
  Rate-limit gauge updates from response headers; `rest_calls_total`
  counter increments per quota-bearing call.
- **Fix B7**: Symlink targets resolved from the same prefetch path
  (mode-120000 blobs decoded as UTF-8 into `SymlinkEntry::target`).
  Symlink prefetch failures fail the snapshot (no silent empty-target
  regression). UTF-8 decode failures log a structured warning.
- **Refactor**: `Observability` moved from `ctxfs-daemon` to
  `ctxfs-provider-common` so providers can use it without a dep cycle.
  `GitHubProvider::new` now takes `Arc<Observability>`.
- **Cache invalidation**: `TreeCache` schema version bumped (1 → 2);
  redis cache gains a 4-byte LE prefix mechanism. Pre-M2 cached
  snapshots are dropped on first read post-upgrade (single cold-mount
  cost per repo). Both tiers log a warning when invalidating.
- **Defensive**: `small_blob_shas` now applies the size threshold to
  symlinks too, blocking adversarial 5MB "symlink" blobs.
- **Counter attribution**: `fetch_snapshot` pre-seeds `counter_key`
  with a `<resolving:ref>` placeholder before `resolve_ref`, then
  replaces with the resolved commit SHA — closes the M2.T2 attribution
  gap so every quota-bearing call ticks `rest_calls_total`.
- Status p95 latency: 4.042µs (target: ≤100ms).

## v0.1.1-m1 — 2026-04-25

### Phase 4 M1: Observability substrate

- New: `ctxfs status` (no-arg) shows global rate-limit budgets and top-N
  mounts. `ctxfs status --mount <id>` preserves per-mount detail.
- New IPC: `get_status` returns versioned `StatusReportV1` JSON.
- New abstractions in `ctxfs-provider-common`: `RateLimitGauge`,
  `ThrottleClassifier`, `UsageCounters`, `MockProvider` test fixture.
- Workload-replay integration tests via `MockProvider` ready for M2/M3
  to extend.
- No behavior change in `provider-git` (M2 wires the integration).
- Status p95 latency: 3.708µs (target: ≤100ms).
