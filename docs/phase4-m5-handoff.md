# Phase 4 — M5 handoff

**Status:** M4 fully closed. `v0.1.4-m4` annotated tag locally on commit `f6ffec5`. **No tags pushed** — they accumulate locally and push together at end of M5 (user instruction). Captured at the end of M4 for resume in a fresh Claude Code session.

---

## Where we are

### Tags (local only — none pushed)

| Tag | Commit | Milestone |
|---|---|---|
| `v0.1.1-m1` | `1c1b339` | Observability substrate + simulation harness |
| `v0.1.2-m2` | `7e70728` | B1 inline + B4 throttle propagation + B7 symlinks + simplify pass |
| `v0.1.3-m3` | `c13e41c` | Tarball prefetch + smart gate + B2 + skeletal ContentFetcher + replay suite |
| `v0.1.4-m4` | `f6ffec5` | Full ContentFetcher lift + ProviderContext refactor + B8 invariant test + post-review cleanup |

User's instruction: **don't push any tag until M5 finishes.** All tags accumulate locally; one big push at the end.

### Workspace state at `f6ffec5`

- `cargo build --release` — clean, zero warnings.
- `cargo fmt --all -- --check` — clean.
- `cargo clippy --all-targets --tests -- -D warnings` — clean.
- `cargo test` — 145 tests pass across provider-git + provider-common + daemon. Pre-existing failures unchanged:
  - `mount_server_only_starts_nfs_and_reports_port` (NFS port-bind on author's macOS).
  - `env_var_invalid_falls_through_to_config` / `env_var_overrides_config` (parallel-env-var race in `ctxfs-cli/backend`; passes with `--test-threads=1`).
- M1 status_bench: still well within p95 ≤ 100ms target.

### M4 commit log (11 commits from `v0.1.3-m3` to `v0.1.4-m4`)

| SHA | Subject | Phase |
|---|---|---|
| `2e5bce8` | feat(provider-common,provider-git): FetchBatchContext + ProviderContext | T1 |
| `62397bc` | refactor(provider-git): collapse GitHubProvider::new args via ProviderContext | T2 |
| `d06f3a3` | chore(M4): T1/T2 carry-forwards (import style, pub(crate) helper, BlobCache assert) | T2 carry-fwd |
| `bbadb07` | feat(provider-git): GitHubProvider implements ContentFetcher | T3 |
| `95d3728` | fix(provider-git): T3 follow-up — zero stale refs + lazy-mode guard test | T3 fix-up |
| `fe49b21` | test(provider-common): MockContentFetcher proves ContentFetcher implementability | T4 |
| `eb3d02e` | fix(provider-git): T3 carry-forwards — stale ref + reserved-param doc | T4 carry-fwd |
| `6fa1554` | test(daemon): B8 invariant — per-mount provider construction | T5 |
| `0a9d664` | chore(M4): CHANGELOG entry for v0.1.4-m4 | T7 |
| `e26af94` | fix(M4): post-review correctness (Disabled short-circuit + fetch_batch HashMap + counter_key thread + tempdir leak + log/doc accuracy) | post-T8 |
| `f6ffec5` | chore(M4): post-review cleanup (DRY helpers, label cleanup, dead-code removal) | post-T8 |

### Phase 4 spec + plans

- **Spec**: `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` — Phase 4 design.
- **M1 plan**: `docs/superpowers/plans/2026-04-25-phase-4-m1-observability.md` — shipped.
- **M2 plan**: `docs/superpowers/plans/2026-04-25-phase-4-m2-architecture-neutral-fixes.md` — shipped.
- **M3 plan v2**: `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` — shipped.
- **M4 plan v2**: `docs/superpowers/plans/2026-04-29-phase-4-m4-content-fetcher-lift.md` — shipped.
- **M5 plan**: TBD — write next session.
- **Predecessor handoffs**: `docs/phase4-m3-handoff.md`, `docs/phase4-m4-handoff.md`.

---

## How we work (the protocol — same as M2/M3/M4, no changes)

### Per-milestone

1. **Write the milestone plan** at `docs/superpowers/plans/2026-04-XX-phase-4-m{N}-{topic}.md`.
2. **Counsel Codex on the plan** (`counsel --deep -f`); apply edits → plan v2.
3. **Present plan v2 to the user** for approval.
4. **Spawn 3 cmux-team teammates** (engineer / spec-reviewer / quality-reviewer). Custom agent definitions at `.claude/agents/{role}.md`.
5. **Per task**: SendMessage to engineer → DONE → spec-reviewer → fix loop → quality-reviewer → fix loop → next task.
6. **Engineer rotation**: rotate engineer at clean task-boundaries before context drops below ~30%. Reviewers usually don't need to rotate.
7. **End of milestone**:
   - Counsel Codex on the milestone result (`/tmp/phase4-m{N}-result-counsel-prompt.md`).
   - Run `/simplify` skill — three parallel agents (code-reuse, code-quality, efficiency); aggregate findings; dispatch a single cleanup commit (or two: correctness + cleanup).
   - Move the tag forward to include the cleanup commit(s).
   - Shutdown the team, respawn fresh for the next milestone.
8. **End of M5**: push all tags (`v0.1.1-m1` through `v0.1.5-m5`) together.

### M4 lessons learned (worth preserving)

- **Codex M4-result counsel found a real M4 regression** (lost `Disabled` short-circuit in `fetch_snapshot_inner`). The /simplify efficiency pass also caught it independently. Run BOTH Codex counsel AND /simplify on every milestone result — they catch different things.
- **Quality-reviewer's "is this dead code or load-bearing?" lens** caught two real cleanups M4 needed: `CostEstimate.fetch_mode` (deleted) and the stale TDD-anchor test (deleted). Worth scrutinizing every YAGNI-shaped surface.
- **The MutexGuard-across-await fix in T3** was caught by the engineer themselves (post-clippy::await_holding_lock check). Good rigor.
- **`make_test_provider_context` tempdir leak** was a real bug introduced in T1 that no reviewer caught at the time. /simplify efficiency pass found it during closeout. Lesson: any `TempDir::keep()` call is a smell — scrutinize at code-review time.
- **The `M4 contract` / `Codex M4-plan-v1 #N` provenance comment pattern** is the same M3 rot pattern. Engineer correctly stripped them in the closeout cleanup. Going forward: no provenance breadcrumbs in code; the git log preserves them.

---

## What M5 ships (per spec § Milestones)

Quoting the spec verbatim:

> **M5 — Remaining bugs (B3-label, B5, B6 detect-and-surface)**
>
> - B3-label: `HashAlgorithm::Sha1` variant in `ctxfs-core`; call sites updated. (Verification done partially in M3 for tarball entries; full multi-tenant verification stays in Phase 5.)
> - B5: per-repo cache reservation in `ctxfs-cache`. **Locked invariant**: an active repo with working set ≤ its reservation receives **zero evictions** triggered by other repos' activity. (Best-effort behavior beyond reservation, with `ctxfs status` warning when a repo's working set exceeds its reservation.)
> - B6: detect LFS pointer files (file content matches GitHub LFS pointer regex `^version https://git-lfs\.github\.com/spec/v1\n...`); `tracing::warn!`; counter increment; `ctxfs status` surfaces count and sample paths under a "LFS pointer files (Phase 5: smudge)" section.
>
> **Exit criteria**:
> - B3-label: `Digest::Sha1(...)` exists and is used for GitHub blob IDs.
> - B5: regression test (described in Testing): mount A (working set ≤ reservation), mount B under cache pressure, scan A, assert `cache_hits` for A's working set unchanged. Assert `eviction_attempts_blocked_by_reservation` counter incremented when B's writes try to evict A's reserved blobs.
> - B6: `ctxfs status` shows LFS pointer count and ≤ 3 sample paths when the test corpus includes LFS-tracked files.

### What this concretely means for M5

**B3-label (HashAlgorithm::Sha1 in ctxfs-core):**
- Today `Digest::from_sha256_hex(&sha)` stores the hex string verbatim, mislabeled as SHA-256 even when the source is a 40-char Git blob SHA-1. Engineer's review at HEAD: there's a comment somewhere explaining "B3-label rename pending — `from_sha256_hex` stores Git blob SHA-1 hex verbatim today."
- Add `pub enum HashAlgorithm { Sha256, Sha1 }` to `ctxfs-core::digest`.
- Add `Digest::from_sha1_hex(...)` constructor.
- Update GitHub-blob construction sites in `provider-git` to use `from_sha1_hex` instead of `from_sha256_hex`.
- The on-disk cache key is the hex string (unchanged by algorithm), so existing caches don't need migration.
- The `algo` field on Digest gets a non-trivial implementation; serialize/deserialize for tree-cache durable format may need versioning bump (check `TREE_SCHEMA_VERSION` in cache).
- Tests: assert that GitHub blob digests round-trip as Sha1 and that the hex equals the original 40-char string.

**B5 (per-repo cache reservation):**
- Add per-repo reservation field/policy on `BlobCache` or a new layer atop it.
- Locked invariant: an active repo with working set ≤ reservation receives ZERO evictions from other repos' activity.
- Best-effort beyond reservation: if working set > reservation, evict normally.
- New counter: `eviction_attempts_blocked_by_reservation`. Add to MountCounters.
- New `ctxfs status` field: per-repo working-set-vs-reservation. Warning when working set > reservation.
- Reservation policy default: probably `cache_max_bytes / N_active_mounts` or a per-mount config.
- Regression test: mount A (working set ≤ reservation), mount B under cache pressure, scan A, assert `cache_hits` for A's working set unchanged. Assert counter incremented.
- This is the heaviest task in M5 — design phase first; spec says "cleaner specification deferred to M5 implementation plan."

**B6 (LFS pointer detect-and-surface):**
- File content matching LFS pointer regex `^version https://git-lfs\.github\.com/spec/v1\n...` (and SHA + size lines below).
- Detect at blob-fetch time? Or at manifest-build time? Spec says "detect LFS pointer files" — probably scan content on blob fetch.
- `tracing::warn!` on detection.
- Counter increment: `lfs_pointer_files` already in MountCounters from M1.
- `ctxfs status` global view: LFS pointer count + ≤ 3 sample paths under "LFS pointer files (Phase 5: smudge)" section.
- Phase 5 ships full LFS smudge to real bytes; M5 only surfaces the issue.

### Suggested task structure (rough — refine in plan v1)

- T1: B3-label — add `HashAlgorithm::Sha1` variant + `from_sha1_hex` + call-site updates
- T2: B6 — LFS pointer detect-and-surface (lighter than B5; warm up before B5)
- T3: B5 — per-repo cache reservation (heaviest; could be 2-3 sub-tasks)
- T4: Replay tests for B5 + B6
- T5: CHANGELOG + tag `v0.1.5-m5`
- T6: **Push all tags together** (`v0.1.1-m1` through `v0.1.5-m5`) — the user's milestone-end push instruction

---

## Carry-forwards into M5 (deferred from M4 + Codex/simplify)

These accumulated from /simplify and Codex reviews. Worth folding into M5 plan or tracking explicitly.

### From quality-reviewer's M4 cleanup-chain review (Minor, NOT blocking M4)

- **`default_cost_estimate` direct unit test missing** — function is in `provider-common::fetcher` but only exercised transitively by `estimate_cost_aggregates_request_sizes` and `estimate_cost_returns_none_total_when_any_size_unknown` in provider-git. Both tests catch breakage only when provider-git compiles. Adding a 2-case test directly in `fetcher.rs::tests` (all-known-sizes → `Some(sum)`, any-unknown → `None`) would give the helper isolated coverage. **5-line addition; can land as a T1 carry-forward in M5 or as a one-shot small commit before M5 starts.**

### From Codex M4-plan-v1 review and /simplify deferred items

- **L2 (panic-as-Result on `Client::builder`)**: `GitHubProvider::new` still uses `expect("failed to build HTTP client")`. Changing to `Result<Self, CtxfsError>` propagates through `prepare_mount`. M5 has lighter test risk than M4 — natural home.
- **F5 (`SlotClaim` Drop impl)**: bounded growth of leaked slots on leader cancellation. Needs guarded design (private `released` flag + Drop only when `is_leader && !released && cell.get().is_none()`). M5 candidate.
- **M5 quality (`format!("{e}")` in dispatch_fetch_policy OnceCell)**: typed error loss. Address in M5 if SlotClaim cleanup happens.
- **M6 quality (dispatch_tarball_for_requests nested match)**: M4 trait lift partially flattened this; remaining residue is M5 cleanup if you want it.
- **L3 (numbered comments in `fetch_tarball_into_cache`)**: `// 1.` / `// 2.` / `// 3.` brittle. M5 cleanup if you touch the function.
- **F2 (BlobTempWriter BufWriter)** — perf, low yield. Phase-5 perf or skip.
- **F4 (fetch_tree_walked sequential DFS → FuturesUnordered)** — only fires on truncated trees. Phase-5 perf.
- **F6 (update_gauge auth_identity clone)** — perf, low yield.
- **HeaderMap-direct refactor** — Phase-5 perf.

### Pre-existing flake (still unaddressed)

- **`env_var_invalid_falls_through_to_config` / `env_var_overrides_config` race** in `ctxfs-cli/backend`. Pre-existing flake; passes with `--test-threads=1`. Codex's M2-result review said "fix before any broader release." User's instruction: **don't release until M6 / Phase 5**, so fixing this is on the critical path BEFORE the v0.1.5-m5 tag pushes publicly. M5 candidate or a one-off cleanup commit.

### From /simplify code-reuse pass (M4 deferred items, low priority)

- **R3**: `(blob_count, estimated_bytes)` aggregation computed in two sites — small DRY miss. M5+ candidate if the aggregation needs to live in provider-common (`pub fn aggregate_request_sizes`).
- **R4**: Auto-gate inline in `fetch_snapshot_inner` doesn't use `estimate_cost` — could unify via `decide_policy(estimate, ...)`. Subjective; current split is intentional per docstring.
- **R6**: `tree_entry_to_request` filter parallels `small_blob_shas`/`symlink_shas` — could derive from requests vector. Marginal.

---

## Bugs status (B1–B8 from triage)

| Bug | Status | Where |
|---|---|---|
| B1 — tiny-file inlining | ✅ Shipped | M2 (commit `b688dd4` + dependency commits) |
| B2 — truncated-tree fallback | ✅ Shipped | M3 (commit `0ea6cbf`) |
| B3-label — Sha256 mislabeled | ⏳ Pending | **M5** (Sha1 variant in core) |
| B4 — secondary throttle classification | ✅ Shipped | M2 (provider-git → VFS → adapters: NFS JUKEBOX / FSKit EAGAIN) |
| B5 — per-repo cache reservation | ⏳ Pending | **M5** |
| B6 — LFS pointer detect | ⏳ Pending | **M5** (detect+warn; full smudge = Phase 5) |
| B7 — symlink target resolution | ✅ Shipped | M2 (commit `b688dd4`); fail-strict tightened in M3 (commit `2cba45e`) |
| B8 — active_source race | ⏳ Deferred | Phase 5 (M3 + M4 enforce per-mount provider constraint; CI test in M4) |

After M5 ships: B3-label + B5 + B6 closed. B8 stays in Phase 5.

---

## File map (where M5 will touch)

- `crates/ctxfs-core/src/digest.rs` — `HashAlgorithm::Sha1` variant; `Digest::from_sha1_hex`; serde compat for the new variant.
- `crates/ctxfs-provider-git/src/github.rs` — `tree_entry_to_request` and other Digest-construction sites switch to `from_sha1_hex`. Possibly LFS pointer detection site (in `fetch_blob` or post-tarball).
- `crates/ctxfs-cache/src/lib.rs` — major: B5 per-repo cache reservation. New struct(s) to track per-repo working sets + reservation policy. Probably a `ReservationPolicy` enum or builder.
- `crates/ctxfs-provider-common/src/counters.rs` — add `eviction_attempts_blocked_by_reservation` counter (B5). `lfs_pointer_files` already exists from M1.
- `crates/ctxfs-cli/src/main.rs` — `ctxfs status` global view: per-repo reservation status (B5); LFS pointer count + sample paths (B6).
- `crates/ctxfs-provider-git/tests/` — new replay tests for B5 (mount A + B + scan + counter assertion) and B6 (LFS pointer detection).
- `CHANGELOG.md` — M5 entry.

---

## Recommended first actions in the new session

1. **Read this handoff** + `docs/phase4-m4-handoff.md` (predecessor) + the spec § M5.
2. **Address the `default_cost_estimate` direct test gap** as a one-line carry-forward commit before M5 plan starts (5-line addition; tag stays at f6ffec5 since it's a test-only change).
3. **Optionally address `env_var_*` test race** as a separate small commit. Documented as critical-path-before-public-release.
4. **Write the M5 plan** at `docs/superpowers/plans/2026-04-XX-phase-4-m5-remaining-bugs.md`.
5. **Counsel Codex on the plan** following the M2/M3/M4 pattern. Save prompt to `/tmp/phase4-m5-plan-counsel-prompt.md`.
6. **Apply Codex edits**, present plan v2 to user, get approval.
7. **Respawn the cmux team** (engineer / spec-reviewer / quality-reviewer) using `.claude/agents/` definitions and team config at `~/.claude/teams/phase4-impl/`.
8. **Execute task-by-task** per the established protocol, with engineer rotations at clean boundaries.
9. **At end of M5**: push all tags (`v0.1.1-m1` through `v0.1.5-m5`) together via `git push --tags` after final tag freeze.

---

## Caveats / pitfalls

- The pre-existing test flakes (`mount_server_only_starts_nfs_and_reports_port`, `env_var_*` family) are real and persistent. The `env_var_*` race becomes a critical-path item before M5's tags go public.
- `cargo test` against real GitHub will rate-limit; for M5 testing, `CTXFS_E2E_SKIP_NETWORK=1` is your friend.
- **Don't push tags until M5 finishes.** Currently 4 tags accumulated locally (`v0.1.1-m1` through `v0.1.4-m4`). M5 will tag `v0.1.5-m5` locally, then **push all 5 together at the end**.
- B5 design is non-trivial — the spec says "Cleaner specification deferred to M5 implementation plan." Plan brainstorm needed before TDD starts. May warrant an Option A vs Option B advocate session if the design has real degrees of freedom.
- B6 LFS detection runs at blob-read time; verify it doesn't measurably slow the read path (per-blob regex match on first ~50 bytes is fast, but worth measuring).

---

## Quick reference — key paths

- Spec: `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md`
- M4 plan v2 (Codex-reviewed): `docs/superpowers/plans/2026-04-29-phase-4-m4-content-fetcher-lift.md`
- M4 handoff (predecessor): `docs/phase4-m4-handoff.md`
- M3 brainstorm artifacts: `docs/phase4-{rate-limit-handoff,option-a-memo,option-b-memo,bug-triage}.md`
- Cmux-team skill: `~/.claude/skills/cmux-team/SKILL.md`
- Team agent definitions: `.claude/agents/{engineer,spec-reviewer,quality-reviewer}.md`
- Team config: `~/.claude/teams/phase4-impl/config.json`
- Most recent Codex M4-result review: `/tmp/counsel/20260430-045236-claude-to-codex-27a8ad/codex.md`
- Most recent Codex M4-plan review: `/tmp/counsel/20260430-023718-claude-to-codex-a36f5e/codex.md`
- CHANGELOG: `CHANGELOG.md` (M1 + M2 + M3 + M4 entries)
