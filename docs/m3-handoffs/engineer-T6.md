# Engineer handoff — after Task 6

**Last commit (HEAD):** `795d1da` — `fix(provider-git): GitBlobSha1 zero-byte handling + path-validation hardening`

**Status:** Tasks 1–6 closed; both reviewers APPROVED Task 6's main commit (`3a6d3f9`) and fix-up (`795d1da`). Quality-reviewer caught one important bug (zero-byte SHA-1) + 4 minors; all addressed in the fix-up.

## Commit log since milestone start

| SHA | Subject |
|---|---|
| `5f8054b` | feat(core,config): add prefetch_threshold_count, prefetch_max_bytes, github_host (T1) |
| `939b20b` | chore(core,config): drop unused PrefetchExplicit fields; remove milestone banner (T1 carry-fwd) |
| `ea0a8f5` | feat(provider-common): skeletal ContentFetcher trait + decide_policy (T2) |
| `ad387d0` | refactor(provider-common): CostEstimate PartialEq + TarballKey field docs (T2 carry-fwd) |
| `3bbbebe` | feat(ipc,cli): MountOptions { prefetch: PrefetchPolicy } on mount RPC (T3) |
| `07a35b6` | test(cli): add mount_defaults_to_auto_prefetch parser test (T3 carry-fwd) |
| `ccc0fb5` | feat(cache): BlobCache atomic-commit + streaming writer + temp cleanup (T4) |
| `7c9b92b` | fix(cache): BlobTempWriter is content-agnostic; verify externally (T4 fix-up) |
| `33c7eae` | chore(cache): drop unused sha2 dep from ctxfs-cache (T4 carry-fwd) |
| `0ea6cbf` | fix(provider-git,B2): truncated-tree per-directory walk fallback (T5) |
| `3a6d3f9` | feat(provider-git): streaming tarball download with hardening (no auto-gate yet) (T6) |
| `795d1da` | fix(provider-git): GitBlobSha1 zero-byte handling + path-validation hardening (T6 fix-up) |

12 commits across 6 tasks. Two tasks remain: T7 (singleflight + auto-gate + dispatch) and T8 (replay tests + carry-forwards + tag).

## What's in place from Task 6 that Task 7 will use

- **`GitHubProvider` has fields:** `api_host: String`, `codeload_host: String`, `codeload_client: reqwest::Client` (auth-stripped, redirect::none), plus the existing `client`, `cache`, `tree_cache`, etc.
- **Constructors:** `pub fn new(token, api_host, cache, ...)` delegates to `pub fn new_with_codeload_host(token, api_host, codeload_override, cache, ...)`. Task 7 will add a 7th arg to `new`: `tarball_singleflight: Arc<TarballSingleflightMap>`.
- **`fetch_tarball_into_cache(source, commit_sha, tree_entries) -> Result<TarballOutcome>`** is invocable but NOT yet called from `fetch_snapshot`. Task 7's `dispatch_fetch_policy` is the call site.
- **All tarball-related helpers** (`TarballOutcome`, `GitBlobSha1`, `Tee`, `validate_tar_entry_path`, `validate_redirect_target`, `codeload_host_for`, `build_path_to_sha_size`) carry `#[allow(dead_code)]` because they're not yet called from production. Task 7 wires the call site and removes the suppressions.

## No carry-forwards from quality-reviewer this time

Both reviewers gave clean APPROVED on the fix-up. No findings to fold into Task 7.

## Critical context for Task 7 — the second-heaviest task

**Plan reference:** `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` § Task 7 (lines ~2145-2612).

**Task 7 deliverables:**
1. **Daemon-side singleflight registry** — `TarballSingleflightMap = Arc<DashMap<TarballKey, Arc<TarballSlot>>>` lives on `DaemonServer`. `TarballSlot { cell: tokio::sync::OnceCell<Result<(), String>> }`. `TarballKey` is already in `provider-common::fetcher` (Task 2).
2. **`SlotClaim`** wrapper with `is_leader: bool`, `slot: Arc<TarballSlot>`, `registry: Arc<TarballSingleflightMap>`. Leader's `release()` removes the slot via `remove_if(key, |slot| Arc::ptr_eq(slot, &claim.slot))` (Codex M3-plan-v1 #6 — prevents stale claim from removing a newer slot).
3. **`GitHubProvider::new` gets 7th arg:** `tarball_singleflight: Arc<TarballSingleflightMap>`. NFS test callsites get an `Arc::new(DashMap::new())` arg.
4. **`claim_singleflight_slot(&self, key) -> SlotClaim`** — uses `dashmap::Entry::or_insert_with` semantics so `is_leader` is true only for the inserter.
5. **`dispatch_fetch_policy(&self, source, commit_sha, tree_entries, policy, threshold_count, max_bytes) -> Result<()>`** — the orchestrator:
   - Pre-claim BlobCache::contains_all skip (if every blob is already cached, return Ok early)
   - decide_policy() based on counts/bytes/policy/threshold
   - On Tarball variant: claim slot, await OnceCell::get_or_init that calls `fetch_tarball_into_cache` (the leader runs it; waiters await the same result)
   - On LazyOversized: increment `prefetch_skipped_oversized` counter, log warn, return Ok
   - On Lazy: return Ok immediately (no-op)
   - Tarball failures are non-fatal; lazy fetch picks up missing blobs
6. **`fetch_snapshot_with_options(&self, source, &FetchOptions) -> Result<Vec<u8>>`** — new public method on `GitHubProvider`. The trait `Provider::fetch_snapshot` impl delegates to a shared private `fetch_snapshot_inner(source, &FetchOptions)` to avoid body duplication (Codex other-call). `Provider::fetch_snapshot` calls it with `FetchOptions::default()` (= `PrefetchPolicy::Disabled` = pre-M3 behavior).
7. **`FetchOptions { prefetch, prefetch_threshold_count, prefetch_max_bytes }`** struct in github.rs. Re-exported from `crates/ctxfs-provider-git/src/lib.rs`.
8. **Daemon plumbing in `prepare_mount`** — construct a `FetchOptions` from `self.config` and the `MountOptions` already plumbed in T3, then call `provider.fetch_snapshot_with_options(&github_source, &fetch_options)`. The existing trait-method call goes away.

**Codex M3-plan-v1 #7 — partial commits + telemetry:**
The tarball flow streams blobs into BlobCache as it goes. Mid-stream failure leaves verified blobs committed (correct — they're keyed by their own SHA-1). For telemetry, **`prefetch_hits` is recorded incrementally inside the per-entry loop in `fetch_tarball_into_cache` after each successful `writer.finalize`**, NOT only on overall success. This was already in plan v2's Task 6 step 7 prose; Task 6's commit 3a6d3f9 SHOULD have it — verify by grep before Task 7 starts. If it's missing, fold it into the FIRST Task 7 commit.

**B8 constraint:** `tarball_singleflight` is a **registry** on the daemon, NOT a shared global provider. Per-mount providers are still constructed fresh in `prepare_mount`. Don't be tempted to share a single `Arc<GitHubProvider>` across mounts — that would break B8.

## Recommended commit order for Task 7

1. **Commit A — provider-common types:** define `TarballSlot`, `TarballSingleflightMap`, `SlotClaim` in `provider-common::fetcher` (or wherever — read plan; mine had them in daemon, but provider-common is cleaner because providers need to construct claims). Plus tests for SlotClaim's `release` semantics with Arc::ptr_eq.
2. **Commit B — `GitHubProvider` constructor + claim helper:** add `tarball_singleflight` field + 7th arg to `new` + `claim_singleflight_slot` method. Update daemon, NFS test callsites. (Workspace stays green throughout.)
3. **Commit C — `FetchOptions` + `fetch_snapshot_with_options` + `fetch_snapshot_inner`:** the trait-method delegation pattern. No call-site changes yet (daemon still calls `Provider::fetch_snapshot` via trait — works because of the default).
4. **Commit D — `dispatch_fetch_policy` + wire into `fetch_snapshot_inner`:** the orchestrator. Includes the BlobCache::contains_all pre-check + decide_policy + leader/waiter singleflight. This is the substantive work.
5. **Commit E — daemon prepare_mount calls `fetch_snapshot_with_options`:** routes `MountOptions.prefetch` + `Config.prefetch_*` into a `FetchOptions`. Removes any `#[allow(dead_code)]` from Task 6 helpers that are now used.

(You can merge commits if natural; the above is just a logical ordering for review granularity.)

## Read order for fresh engineer

1. `.claude/agents/engineer.md` — your role
2. This handoff (you're reading it)
3. `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` — read § Architecture (singleflight section ~line 30-40), § Task 7 (lines 2145-2612). Skim § Task 8 enough to know what NOT to do (replay tests are Task 8).
4. `crates/ctxfs-provider-git/src/github.rs` — current state. The `fetch_tarball_into_cache` you'll call is at the bottom; the `fetch_snapshot` you'll restructure is the existing method.
5. `crates/ctxfs-provider-common/src/fetcher.rs` — `TarballKey`, `decide_policy`, `PrefetchPolicy`, `FetchPolicy`. You may add `TarballSlot`/`SlotClaim` here (decide based on plan).
6. `crates/ctxfs-daemon/src/daemon.rs` — `prepare_mount` is around line 380; adds `tarball_singleflight` field; constructs FetchOptions.

## Constraints (unchanged)

- Tags local-only through M5; do NOT push.
- Pre-existing failures expected: `mount_server_only_starts_nfs_and_reports_port`, `env_var_*`, network-dependent NFS integration tests.
- B8 constraint: `tarball_singleflight` is a registry; per-mount providers preserved.
- Workspace lints: `clippy::all = deny`, `pedantic = warn`. `cargo clippy --all-targets --tests -- -D warnings` MUST pass.

## Acknowledge

Reply with `READY_FOR_TASK_7` + any clarifying questions before starting. The team lead will then send "Begin Task 7" with formal scope.
