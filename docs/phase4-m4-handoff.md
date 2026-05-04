# Phase 4 — M4 handoff

**Status:** M3 fully closed. `v0.1.3-m3` annotated tag locally on commit `c13e41c`. **No tags pushed** — they accumulate locally and push together at end of M5 (user instruction). Captured at the end of M3 for resume in a fresh Claude Code session.

---

## Where we are

### Tags (local only — none pushed)

| Tag | Commit | Milestone |
|---|---|---|
| `v0.1.1-m1` | `1c1b339` | Observability substrate + simulation harness |
| `v0.1.2-m2` | `7e70728` | B1 inline + B4 throttle propagation + B7 symlinks + simplify pass |
| `v0.1.3-m3` | `c13e41c` | Tarball prefetch + smart gate + B2 + skeletal ContentFetcher + replay suite + M2 carry-forwards |

User's instruction: **don't push any tag until M5 finishes.**

### Workspace state at `c13e41c`

- `cargo build --release` — clean, zero warnings.
- `cargo fmt --all -- --check` — clean.
- `cargo clippy --all-targets --tests -- -D warnings` — clean.
- `cargo test` — green except the 2 documented pre-existing failures:
  - `mount_server_only_starts_nfs_and_reports_port` (NFS port-bind on author's macOS).
  - `env_var_invalid_falls_through_to_config` / `env_var_overrides_config` (parallel-env-var race in `ctxfs-cli/backend`; passes with `--test-threads=1`).
- M1 status_bench: still well within p95 ≤ 100ms target.

### M3 commit log (24 commits from `v0.1.2-m2` to `v0.1.3-m3`)

| SHA | Subject | Phase |
|---|---|---|
| `5f8054b` | feat(core,config): prefetch_threshold_count + prefetch_max_bytes + github_host | T1 |
| `939b20b` | chore(core,config): drop unused PrefetchExplicit fields; remove milestone banner | T1 carry-fwd |
| `ea0a8f5` | feat(provider-common): skeletal ContentFetcher trait + decide_policy | T2 |
| `ad387d0` | refactor(provider-common): CostEstimate PartialEq + TarballKey field docs | T2 carry-fwd |
| `3bbbebe` | feat(ipc,cli): MountOptions { prefetch: PrefetchPolicy } on mount RPC | T3 |
| `07a35b6` | test(cli): add mount_defaults_to_auto_prefetch parser test | T3 carry-fwd |
| `ccc0fb5` | feat(cache): BlobCache atomic-commit + streaming writer + temp cleanup | T4 |
| `7c9b92b` | fix(cache): BlobTempWriter is content-agnostic; verify externally | T4 fix-up |
| `33c7eae` | chore(cache): drop unused sha2 dep from ctxfs-cache | T4 carry-fwd |
| `0ea6cbf` | fix(provider-git,B2): truncated-tree per-directory walk fallback | T5 |
| `3a6d3f9` | feat(provider-git): streaming tarball download with hardening (no auto-gate yet) | T6 |
| `795d1da` | fix(provider-git): GitBlobSha1 zero-byte handling + path-validation hardening | T6 fix-up |
| `11f8dd2` | feat(provider-common): TarballSlot/TarballSingleflightMap/SlotClaim | T7 |
| `6b4c0af` | feat(provider-git): tarball_singleflight field + claim_singleflight_slot | T7 |
| `34d4571` | feat(provider-git): FetchOptions + fetch_snapshot_with_options + fetch_snapshot_inner | T7 |
| `a545ff4` | feat(provider-git): dispatch_fetch_policy + wire into fetch_snapshot_inner | T7 |
| `f13e60a` | feat(daemon): wire tarball singleflight + FetchOptions into prepare_mount | T7 |
| `2022759` | test(replay): M3 exit-criteria suite + placeholder bucket merge | T8 |
| `2cba45e` | feat(M2-carry): symlink fail-strict + bearer_header helper | T8 |
| `cd73c32` | test(M2-carry): assembled-path fetch_snapshot HTTP-mock test | T8 |
| `a50f790` | chore(release): CHANGELOG for v0.1.3-m3 | T8 |
| `8c40513` | chore(provider-common,provider-git): T7 review carry-forwards | T8 fix-up |
| `eaedc7f` | fix(M3): post-review correctness (small-blob cache bypass, batch_mount policy, tree-cache Force skip) | post-T8 |
| `8347b57` | chore(M3): post-review cleanup (labels, DRY, doc quality, efficiency) | post-T8 |
| `6bda745` | fix(provider-git): effective_prefetch_policy short-circuits on non-Auto (F1 follow-up) | post-T8 |
| `c13e41c` | chore(M3): final doc cleanup (label + stale evict_oldest doc) | post-T8 |

### Phase 4 spec + plans

- **Spec**: `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` — Phase 4 design.
- **M1 plan**: `docs/superpowers/plans/2026-04-25-phase-4-m1-observability.md` — shipped.
- **M2 plan**: `docs/superpowers/plans/2026-04-25-phase-4-m2-architecture-neutral-fixes.md` — shipped.
- **M3 plan v2 (Codex-reviewed)**: `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` — shipped.
- **M4 plan**: TBD — write next session.
- **M3 handoff (the predecessor of this doc)**: `docs/phase4-m3-handoff.md`.
- **M3 per-engineer-rotation handoffs**: `docs/m3-handoffs/engineer-T1.md`, `engineer-T5.md`, `engineer-T6.md`. Document the rotation cadence used in M3 (4 fresh engineers across 8 tasks).

---

## How we work (the protocol — same as M2/M3, no changes)

### Per-milestone

1. **Write the milestone plan** at `docs/superpowers/plans/2026-04-XX-phase-4-m{N}-{topic}.md`.
2. **Counsel Codex on the plan** (`counsel --deep -f`); apply edits → plan v2.
3. **Present plan v2 to the user** for approval.
4. **Spawn 3 cmux-team teammates** (engineer / spec-reviewer / quality-reviewer). Custom agent definitions at `.claude/agents/{role}.md`.
5. **Per task**: SendMessage to engineer → DONE → spec-reviewer → fix loop → quality-reviewer → fix loop → next task.
6. **Engineer rotation**: rotate engineer at clean task-boundaries before context drops below ~30%. Write a handoff doc at `docs/m{N}-handoffs/engineer-T{n}.md`. Reviewers usually don't need to rotate.
7. **End of milestone**:
   - Counsel Codex on the milestone result (`/tmp/phase4-m{N}-result-counsel-prompt.md`).
   - Run `/simplify` skill — three parallel agents (code-reuse, code-quality, efficiency); aggregate findings; dispatch a single cleanup commit.
   - Move the tag forward to include the cleanup commit(s).
   - Shutdown the team, respawn fresh for the next milestone.
8. **End of M5**: push all tags (`v0.1.1-m1` through `v0.1.5-m5`) together.

### M3 lessons learned (worth preserving)

- **Pre-flight engineer questions are gold.** Each fresh engineer asked clarifying questions before starting — caught real ambiguities (orphan rule for `Digest: AsRef<Digest>`, `#[cfg(test)] pub fn` not visible to integration tests, `tokio-util` feature is `io-util` not `io`, etc.). Worth 2–3 min of pre-dispatch dialogue every time.
- **Quality-reviewer caught two real correctness bugs** during M3 (T4 SHA-256-vs-SHA-1 mismatch in BlobTempWriter; T6 zero-byte SHA-1 hash). Treat the quality-pass as load-bearing, not ceremonial.
- **Codex's M3-result counsel found one critical bug** (P1: `prefetch_small_blobs` cache-bypass) that BOTH reviewers had missed. Run Codex counsel on every milestone result, not just plan.
- **The `/simplify` 3-parallel-agent pass yielded ~15 actionable findings** beyond what reviewers caught. Don't skip it.
- **Wire-format break documented in CHANGELOG** when the `mount` tarpc method gained the `MountOptions` arg. CLI + daemon must rebuild together. Codex flagged this as user-visible; CHANGELOG language is now accurate.

---

## What M4 ships (per spec § Milestones)

Quoting the spec verbatim:

> **M4 — `ContentFetcher` full lift + plug-in refactor**
>
> - `ContentRequest` / `FetchPolicy` / `ContentFetcher` finalized in `provider-common` (skeletal version landed in M3).
> - `provider-git`'s fetch policy refactored to fully implement `ContentFetcher` rather than inline. The Stage 1 implementation is the first concrete trait impl.
> - `RateLimitGauge`, `ThrottleClassifier`, and `UsageCounters` already live in `provider-common` from M1; M4 does not move them.
> - **B8 constraint enforced**: M4 must preserve daemon-side per-mount `GitHubProvider` creation in `daemon.rs`. No introduction of shared/global fetchers that rely on `active_source`. CI test asserts `GitHubProvider::new` is called from the per-mount path.
> - No external behavior change; pure refactor.
>
> **Exit criteria**:
> - A trivial `MockContentFetcher` in tests can implement `ContentFetcher` and be used by a hypothetical second provider without touching `provider-git`.
> - B8 constraint test passes.

### What this concretely means for M4

1. **Promote `GitHubProvider` to first concrete `ContentFetcher` impl.** Today it has `fetch_snapshot_with_options` + `fetch_snapshot_inner` + `dispatch_fetch_policy` as inherent methods. M4 lifts the public surface into trait method impls. The trait sig is already shipped (skeletal in `provider-common::fetcher::ContentFetcher`).

2. **Daemon calls `provider.fetch_batch(...)` etc. via the trait.** Today daemon's `prepare_mount` calls `provider.fetch_snapshot_with_options(&github_source, &fetch_options)` directly on `Arc<GitHubProvider>`. M4 changes the call to go through the trait — possibly renaming (`fetch_snapshot_with_options` → `fetch_batch` or similar) — so the daemon code doesn't know it's talking to a `GitHubProvider`.

3. **`fetch_blob` and `fetch_directory` migrate to dispatched methods on the trait.** Today these are `Provider` trait methods (the older trait); M4 lifts them onto `ContentFetcher` (or the new trait shape M4 settles on).

4. **Provider construction collapse.** M3 expanded `GitHubProvider::new` to **7 args**: `token, api_host, cache, tree_cache, shared_tree_cache, observability, tarball_singleflight`. M4 introduces `ProviderContext { api_host, observability, cache, tree_cache, shared_tree_cache, singleflight }` (or similar) and reduces `GitHubProvider::new` back to ~2 args. **`#[allow(clippy::too_many_arguments)]` annotations on `new`, `new_with_codeload_host`, and `dispatch_fetch_policy` are M3 debt** — M4 removes them.

5. **MockContentFetcher** in `provider-common::tests` (or a new `provider-mock` crate) — tiny stub impl that's enough to demonstrate the trait is implementable by a hypothetical second provider (npm, PyPI, crates.io content fetcher). Doesn't ship as a real provider.

6. **B8 CI test.** New test in `crates/ctxfs-daemon/tests/` asserting `GitHubProvider::new` (the constructor — not the trait!) is called from `prepare_mount` per mount. Concrete shape: probably introspect the daemon's mount-creation path or use a test-only seam.

---

## Carry-forwards into M4 (deferred from M3)

These were noted during M3 but **do not block** the M4 start. Worth folding into M4's plan or tracking explicitly.

### From the M3-result Codex review (`/tmp/counsel/20260429-075755-claude-to-codex-470128/codex.md`)

All four Codex P1/P2/P3 items from M3-result counsel were addressed in `eaedc7f` + `8347b57`. **None are deferred.**

### From quality-reviewer's M3 closeout (the final cleanup-pair review)

Two doc-only minors landed in `c13e41c`. **None are deferred.**

### From `/simplify` pass — DEFERRED (not addressed in M3 cleanup)

These are real but lower-yield; M4's `ContentFetcher` lift is the right home for several. Don't lose track.

#### Code-quality DEFERRED:

- **M5 (`format!("{e}")` discards typed error)** in `dispatch_fetch_policy`'s OnceCell error path (`github.rs:1071`). Waiters lose typed `CtxfsError`; only get `String`. M4: consider `Result<(), Arc<CtxfsError>>` for the OnceCell, OR commit to "tarball failure is non-fatal; waiters don't need typed errors" and remove the `let _ = outcome_res;` discard ceremony.
- **M6 (`dispatch_fetch_policy` deeply-nested match)** — the Tarball arm is ~6 indent levels deep. Flatten with early returns / guards. M4 will touch this code anyway during the trait lift.
- **L2 (`expect()` panics in Client::builder)** — `reqwest::Client::builder().build().expect("...")` in `GitHubProvider::new` and `new_with_codeload_host`. A panic from constructor crashes the daemon's mount worker. M4: propagate as `Result<Self, ...>` (turns daemon startup-time mis-config into clean failure). Touches `prepare_mount` and all NFS test callsites.
- **L3 (numbered comments in `fetch_tarball_into_cache`)** — `// 1.` / `// 2.` / `// 3.` numbered prose is brittle to interior refactors. M4 trait-lift will likely reshape this code; convert the numbered headers to named sub-blocks.
- **L7 (`print_global_status` byte-slice on commit string)** — theoretical non-ASCII issue. Defer; safer pattern is `m.commit.chars().take(8).collect()`. Phase-5 polish.
- **L8 (`status_report` two-pass collection)** — minor; iterate once instead of twice over `self.counters.iter()`. Phase-5 polish.

#### Efficiency DEFERRED:

- **F2 (`BlobTempWriter` no `BufWriter`)** — `std::io::copy` uses 8 KiB stack buffer → `ceil(size/8KB)` write syscalls per blob. A `BufWriter<File>` with 64 KiB capacity would cut that 8×. Marginal; per-blob `fsync` still dominates. Phase-5 perf or M5+ if telemetry shows it matters.
- **F4 (`fetch_tree_walked` sequential)** — B2 fallback is single-threaded DFS via `await` per subtree. On truncated trees this dominates wall time. M4: replace with `FuturesUnordered` capped at `PREFETCH_CONCURRENCY = 8`. Same shape as `prefetch_small_blobs`.
- **F5 (`tarball_singleflight` slot leak on leader cancellation)** — bounded growth; one leaked slot per `(host, owner, repo, commit)` tuple. RAII-Drop fix on `SlotClaim` would cover cancellation cleanup. Bounded enough for M3; address in M4 or Phase-5 perf.
- **F6 (`update_gauge` clones `auth_identity` per response)** — DashMap entry takes by value. Add fast-path `if let Some(g) = gauges.get(&key)` to avoid clone on hit. Or `Arc`-wrap `AuthIdentity`. Marginal; per-HTTP-response allocation. Phase-5 perf.

### From M2 handoff (still deferred)

- **HeaderMap-direct refactor of `ThrottleClassifier::classify`** — Codex's M3-plan-v1 review said "not load-bearing." Defer to Phase 5 perf.
- **`env_var_invalid_falls_through_to_config` / `env_var_overrides_config` race** in `ctxfs-cli/backend`. Pre-existing flake; passes with `--test-threads=1`. Codex's M2-result review said "fix before any broader release." Probably worth picking up in M4 or M5; not blocking.
- **`<resolving:ref>` placeholder elimination** — currently merged-and-dropped via `merge_and_drop_placeholder` in M3 (improvement over M2's "filter from view"). M4 might collapse to a single `counter_key.set()` post-resolve if the resolve_ref bookkeeping path can be reshaped. Optional polish.

---

## Bugs status (B1–B8 from triage)

| Bug | Status | Where |
|---|---|---|
| B1 — tiny-file inlining | ✅ Shipped | M2 (commit `b688dd4` + dependency commits) |
| B2 — truncated-tree fallback | ✅ Shipped | M3 (commit `0ea6cbf`) |
| B3-label — Sha256 mislabeled | ⏳ Pending | M5 (Sha1 variant in core; verification = Phase 5) |
| B4 — secondary throttle classification | ✅ Shipped | M2 (provider-git → VFS → adapters: NFS JUKEBOX / FSKit EAGAIN) |
| B5 — per-repo cache reservation | ⏳ Pending | M5 |
| B6 — LFS pointer detect | ⏳ Pending | M5 (detect+warn; full smudge = Phase 5) |
| B7 — symlink target resolution | ✅ Shipped | M2 (commit `b688dd4`); fail-strict tightened in M3 (commit `2cba45e`) |
| B8 — active_source race | ⏳ Deferred | Phase 5 (M3 + M4 enforce per-mount provider constraint) |

---

## File map (where M4 will touch)

- `crates/ctxfs-provider-common/src/fetcher.rs` — `ContentFetcher` trait surface finalized; `MockContentFetcher` test impl; possibly add `ProviderContext` here for the constructor-collapse.
- `crates/ctxfs-provider-git/src/github.rs` — major: `GitHubProvider` becomes the first concrete `ContentFetcher` impl; `fetch_snapshot_with_options` / `fetch_blob` / `fetch_directory` lifted into trait method impls; constructor collapses via `ProviderContext`; `#[allow(clippy::too_many_arguments)]` annotations removed.
- `crates/ctxfs-provider-git/src/lib.rs` — re-exports updated.
- `crates/ctxfs-daemon/src/daemon.rs` — `prepare_mount` calls trait methods on `Arc<dyn ContentFetcher>` (or `Arc<GitHubProvider>` cast to the trait); B8 CI test new at `crates/ctxfs-daemon/tests/`.
- `crates/ctxfs-nfs/tests/medium_repo.rs` and `nfs_read_path.rs` — constructor signature change on `GitHubProvider::new` (probably back to ~2 args).
- `crates/ctxfs-cli/src/main.rs` — possibly no changes (CLI talks to daemon via tarpc, doesn't construct providers directly).
- `crates/ctxfs-ipc/src/service.rs` — no changes (the wire format is unchanged from M3).

---

## Recommended first actions in the new session

1. **Read this handoff** + `docs/phase4-m3-handoff.md` (predecessor) + the spec § M4 + the M3 plan (for style reference).
2. **Write the M4 plan** at `docs/superpowers/plans/2026-04-XX-phase-4-m4-content-fetcher-lift.md` (today's date).
3. **Counsel Codex on the plan** following the M1/M2/M3 pattern. Save prompt to `/tmp/phase4-m4-plan-counsel-prompt.md`.
4. **Apply Codex edits**, present plan v2 to user, get approval.
5. **Respawn the cmux team** (engineer / spec-reviewer / quality-reviewer) using `.claude/agents/` definitions and team config at `~/.claude/teams/phase4-impl/`.
6. **Execute task-by-task** per the established protocol, with engineer rotations at clean boundaries (T2 lift is heaviest; consider rotation before/after).

---

## Caveats / pitfalls

- The pre-existing test flakes (`mount_server_only_starts_nfs_and_reports_port`, `env_var_*` family) are real and persistent; don't get distracted trying to fix them mid-task. They're noted as known issues. The `env_var_*` race might land in M4 since M4 is mostly refactor with low test risk.
- `cargo test` against real GitHub will rate-limit; for M4 testing, `CTXFS_E2E_SKIP_NETWORK=1` is your friend.
- Don't push tags. The user explicitly wants all tags pushed together at end of M5. Currently 3 tags accumulated locally (`v0.1.1-m1`, `v0.1.2-m2`, `v0.1.3-m3`). M4 will tag `v0.1.4-m4` locally.
- M4 is mostly a pure refactor — exit criteria explicitly say "No external behavior change." Don't sneak in feature additions.
- B8 constraint is **load-bearing** for M4: per-mount providers MUST stay. The B8 CI test that the spec asks for is a forcing function — write it early in M4 so any drift is caught.

---

## Quick reference — key paths

- Spec: `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md`
- M3 plan v2 (Codex-reviewed): `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md`
- M3 handoff (predecessor): `docs/phase4-m3-handoff.md`
- M3 per-engineer-rotation handoffs: `docs/m3-handoffs/{engineer-T1,engineer-T5,engineer-T6}.md`
- M3 brainstorm artifacts: `docs/phase4-{rate-limit-handoff,option-a-memo,option-b-memo,bug-triage}.md`
- Cmux-team skill: `~/.claude/skills/cmux-team/SKILL.md` (updated during M3 with engineer-rotation guidance)
- Team agent definitions: `.claude/agents/{engineer,spec-reviewer,quality-reviewer}.md`
- Team config: `~/.claude/teams/phase4-impl/config.json`
- Most recent Codex M3-result review: `/tmp/counsel/20260429-075755-claude-to-codex-470128/codex.md`
- Most recent Codex M3-plan review: `/tmp/counsel/20260427-223719-claude-to-codex-79183d/codex.md`
- CHANGELOG: `CHANGELOG.md` (M1 + M2 + M3 entries)
