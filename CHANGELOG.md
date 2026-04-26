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
