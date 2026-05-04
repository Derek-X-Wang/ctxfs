# Engineer handoff — after Task 1

**Last commit (HEAD):** `5f8054b` — `feat(core,config): add prefetch_threshold_count, prefetch_max_bytes, github_host`

**Status:** Task 1 closed; spec-reviewer + quality-reviewer both APPROVED. Two carry-forward minors fold into Task 2's first commit (or Task 2 dispatch can address them inline — see below).

## What landed in 5f8054b

- `crates/ctxfs-core/src/config.rs` only.
- Three new `Config` fields: `prefetch_threshold_count: u64` (default 30), `prefetch_max_bytes: u64` (default `min(cache_max/4, 256 MB)`), `github_host: String` (default `"api.github.com"`).
- Three matching `ConfigFile` Option fields for TOML.
- `pub(crate) struct PrefetchExplicit { max_bytes, threshold_count, github_host }` — Debug/Default/Clone/Copy.
- `pub fn default_prefetch_max_bytes(cache_max_bytes: u64) -> u64` (`#[must_use]`).
- `pub(crate) fn apply_prefetch_env<F>(config, explicit, read)` — closure-injected env reader.
- `pub(crate) fn recompute_derived_defaults(config, explicit)` — re-derives `prefetch_max_bytes` only if `!explicit.max_bytes`.
- `apply_file` and `apply_env` thread `&mut PrefetchExplicit`; the three M3 fields are inlined (matches plan prose: "match the existing pattern").
- `load()`, `from_env()`, `from_toml_str()` each thread one `PrefetchExplicit` and call `recompute_derived_defaults` exactly once at the tail.
- 9 plan-specified tests + 2 TOML coverage tests, all green; clippy `-D warnings` clean; fmt clean.

## Carry-forward minors from quality-reviewer (fold into Task 2's commit, or address upfront in Task 2)

1. **`PrefetchExplicit::threshold_count` and `github_host` are write-only** — only `max_bytes` is consumed by `recompute_derived_defaults`. Either drop the two unused bool fields (YAGNI) or add a doc comment on the struct saying they're M4 placeholders. **Recommendation:** drop them. Task 1 still passes; the change is one line in `apply_prefetch_env` (don't set them) + struct shrinks.
2. **`// M3 Task 1` test-module banner** in `config.rs` will rot — remove the milestone label; keep the field-list separator if a visual break is desired.

## Next: Task 2

**Plan reference:** `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` § Task 2 (lines ~333-608).

**Scope:** create `crates/ctxfs-provider-common/src/fetcher.rs` with the skeletal `ContentFetcher` trait + the type vocabulary M3's auto-gate uses. Wire `pub mod fetcher;` into `lib.rs` (alphabetical insertion).

**Types/items to create in fetcher.rs:**
- `ContentKind { File, Symlink, LfsPointer }`
- `ContentRequest { path, digest, size, kind }`
- `FetchMode { Lazy, BulkPrefetch, Forced }`
- `PrefetchPolicy { Auto, Force, Disabled }` with `Default = Auto` (Serialize/Deserialize)
- `FetchPolicy { Tarball{...}, Lazy, LazyOversized{...} }`
- `CostEstimate { total_bytes, request_count, fetch_mode }` (Default)
- `TarballKey { host, owner, repo, commit_sha }` — **lives in provider-common, NOT daemon** (Codex M3-plan-v1 #5)
- `#[async_trait::async_trait] pub trait ContentFetcher` with `estimate_cost` (sync) + `fetch_batch` (async)
- `pub fn decide_policy(blob_count, estimated_bytes, policy, threshold_count, max_bytes) -> FetchPolicy` — pure auto-gate

**Tests (5 inline, in `#[cfg(test)] mod tests`):**
- `auto_gate_below_count_is_lazy`
- `auto_gate_at_count_within_bytes_is_tarball`
- `auto_gate_above_bytes_is_lazy_oversized` (asserts the inner fields)
- `force_bypasses_byte_cap`
- `disabled_is_always_lazy`

**Skeletal trait:** providers do NOT need to implement `ContentFetcher` in M3. The trait is shipped so M3's `dispatch_fetch_policy` (Task 7) can use `FetchPolicy` as a value, and so M4 can promote `GitHubProvider` to the first concrete impl without restructuring callers.

**No call-site changes:** Task 2 only adds new types + tests. Daemon, provider-git, IPC are all untouched until Task 3.

## Workflow reminders

- TDD: write the 5 tests first, expect compile failure, then implement.
- After impl: `cargo test -p ctxfs-provider-common fetcher`, `cargo build`, `cargo fmt --all -- --check`, `cargo clippy --all-targets --tests -- -D warnings`.
- Commit message per plan's Task 2 Step 4 (the heredoc).
- Reply to team-lead: `DONE` / `DONE_WITH_CONCERNS` / `BLOCKED` / `NEEDS_CONTEXT` with commit SHA.

## Read order for fresh engineer

1. `.claude/agents/engineer.md` — your role + commit/test/clippy discipline
2. This handoff (you're reading it)
3. `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` § Task 2 only (skip Tasks 1, 3-8 for now; team lead will dispatch each)
4. `crates/ctxfs-provider-common/src/lib.rs` — see existing module list
5. `crates/ctxfs-provider-common/src/counters.rs` — see how a sibling module is structured (style reference)
6. `CLAUDE.md` for the workspace lint config

## Constraints

- B8 constraint: no shared/global fetcher code. Task 2 only ships *types* — no behavior change yet.
- Tags local-only through M5; do NOT push.
- Pre-existing test failures (`mount_server_only_starts_nfs_and_reports_port`, `env_var_*`) are expected and not your concern.
- `clippy::all = deny`, `pedantic = warn`. `cargo clippy --all-targets --tests -- -D warnings` must pass.

## Acknowledge

Reply to team-lead with `READY_FOR_TASK_2` + any clarifying questions before starting.
