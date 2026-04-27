# Phase 4 — M3 handoff

**Status:** M2 fully closed (`v0.1.2-m2` tagged locally, not pushed). Ready to start M3 (tarball prefetch + smart gate + B2 + skeletal `ContentFetcher`). Captured at the end of the M2 implementation session for resume in a fresh Claude Code session.

---

## Where we are

### Tags (local only — none pushed)

| Tag | Commit | Milestone |
|---|---|---|
| `v0.1.1-m1` | `1c1b339` | Observability substrate + simulation harness |
| `v0.1.2-m2` | `7e70728` | B1 inline + B4 throttle propagation + B7 symlinks + simplify pass |

User's instruction: **don't push any tag until M5 finishes.** All tags accumulate locally; one big push at the end.

### Workspace state

- `cargo build --release` — clean, zero warnings.
- `cargo fmt --all -- --check` — clean.
- `cargo clippy --all-targets --tests -- -D warnings` — clean.
- `cargo test` — green except 2 documented pre-existing failures:
  - `mount_server_only_starts_nfs_and_reports_port` (NFS port-bind on author's macOS).
  - `env_var_invalid_falls_through_to_config` / `env_var_overrides_config` (parallel-env-var race in `ctxfs-cli/backend`; passes with `--test-threads=1`).
- M1 status_bench: p95 = 3.375µs (target ≤100ms; ~30,000× margin).

### Phase 4 spec + plans

- **Spec**: `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md` — Phase 4 design (Codex-reviewed, 14 edits applied to v2). Locked decisions: Stage 1 only; provider-common as the abstraction layer; tarball auto-gate (count + bytes); observability-first; bugs fold into milestones.
- **M1 plan**: `docs/superpowers/plans/2026-04-25-phase-4-m1-observability.md` — shipped.
- **M2 plan**: `docs/superpowers/plans/2026-04-25-phase-4-m2-architecture-neutral-fixes.md` — shipped.
- **M3 plan**: TBD — write next session.

### Brainstorm artifacts (from Phase 4 kickoff)

- `docs/phase4-rate-limit-handoff.md` — original Phase 4 brainstorm context (Codex's v2 sketch + B1–B6 finds).
- `docs/phase4-option-a-memo.md` — Git-native v2 advocate memo.
- `docs/phase4-option-b-memo.md` — REST + tarball prefetch advocate memo.
- `docs/phase4-bug-triage.md` — B1–B8 triage (B7 + B8 added beyond original handoff's B1–B6).

---

## How we work (the protocol)

The user established this orchestration pattern across M1 → M2:

### Per-milestone

1. **Write the milestone plan** at `docs/superpowers/plans/2026-04-XX-phase-4-m{N}-{topic}.md`.
2. **Counsel Codex on the plan** with the `counsel` CLI:
   - Write a self-contained prompt to `/tmp/phase4-m{N}-plan-counsel-prompt.md`.
   - Invoke `counsel --deep -f /tmp/phase4-m{N}-plan-counsel-prompt.md`.
   - Read Codex's review at `/tmp/counsel/<slug>/codex.md`.
3. **Apply Codex's edits** to plan v2.
4. **Present plan v2 to the user** for approval.
5. **Spawn 3 cmux-team teammates** (persistent across the milestone) — `engineer`, `spec-reviewer`, `quality-reviewer`. Custom agent definitions live at `.claude/agents/{role}.md` (already created at the start of M2).
6. **Per task**: SendMessage to `engineer` → DONE → SendMessage to `spec-reviewer` → fix loop → SendMessage to `quality-reviewer` → fix loop → next task.
7. **End of milestone**:
   - Counsel Codex on the milestone result (`/tmp/phase4-m{N}-result-counsel-prompt.md`).
   - Run `/simplify` skill — three parallel agents (code-reuse, code-quality, efficiency); aggregate findings; dispatch a single cleanup commit.
   - Move the tag forward to include the simplify commit.
   - Shutdown the team, respawn fresh for the next milestone (cleaner context).
8. **End of M5**: push all tags (`v0.1.1-m1` through `v0.1.5-m5`) together.

### cmux-team specifics

- Skill: `cmux-team` (lives at `~/.claude/skills/cmux-team/SKILL.md`; written during the brainstorm session). Use it.
- Agent role definitions: `.claude/agents/engineer.md`, `.claude/agents/spec-reviewer.md`, `.claude/agents/quality-reviewer.md`. Reusable across all M2–M5 milestones.
- Team config: `~/.claude/teams/phase4-impl/config.json` (still exists; respawn drops new pane IDs).
- One quirk hit in M2: cmux didn't free the `engineer` agent_id after a hung-pane shutdown, so the respawn became `engineer-2`. If you respawn after the M2 team's clean shutdown (pre-M3), the slot should be free and you can re-use `engineer`. If not, accept the auto-rename.

### Codex review pattern

The counsel-Codex prompts follow this shape:
- Lead with "you reviewed the milestone plan (or X) earlier; here's what's locked".
- Tell Codex what to evaluate (numbered list of areas).
- Tell Codex explicitly what NOT to do (re-litigate locked decisions, etc.).
- Ask for a verdict at the top: ship-as-is / ship-with-edits / don't-ship-return-to-plan.

This pattern works. Reuse it.

---

## What M3 ships (per spec § Milestones)

Quoting the spec verbatim so the next session has it inline:

> **M3 — Tarball prefetch with smart gate (+ B2, + tarball hardening, + skeletal `ContentFetcher`)**
>
> - `provider-git` integrates `/repos/{o}/{r}/tarball/{ref}` endpoint with full hardening:
>   - Streaming tar extraction (no full-tarball buffering).
>   - Per-blob temp-and-verify-and-rename in `ctxfs-cache`.
>   - Path normalization rejection of `..` / absolute / escaping entries.
>   - Redirect security: codeload-host whitelist, `Authorization` strip, depth ≤ 3.
>   - Singleflight dedupe for concurrent mounts of same `(repo, commit)`.
>   - Daemon-restart temp-orphan cleanup.
> - Auto-gate logic on `blob_count >= CTXFS_PREFETCH_THRESHOLD_COUNT` AND `estimated_bytes <= CTXFS_PREFETCH_MAX_BYTES`.
> - `MountOptions { prefetch: PrefetchPolicy }` in `ctxfs-ipc` Mount RPC; `--prefetch` / `--no-prefetch` CLI flags.
> - `CTXFS_PREFETCH_THRESHOLD_COUNT` and `CTXFS_PREFETCH_MAX_BYTES` fields added to `Config` and env parsing in `ctxfs-core`.
> - **B2 truncated-tree fallback** — per-directory non-recursive walk when `truncated == true`. Required for the gate to make correct decisions on large repos.
> - **Skeletal `ContentFetcher` trait** introduced in `provider-common`. The tarball-vs-lazy decision is implemented as a `FetchPolicy` value, not inline `if`/`else` in `GitHubProvider`. M4 expands this without restructuring.
> - M1 counter `prefetch_hits`, `prefetch_failures`, `prefetch_skipped_oversized`, `tarball_digest_mismatch`, `tarball_invalid_entries`, `truncated_tree_fallbacks` all reporting.
>
> **Exit criteria**:
> - Cold scan of a 1k-file 30MB repo: `rest_calls_total == 3` (replay test).
> - Truncated-tree replay test: per-directory walk fires; manifest is complete.
> - Concurrent-mount replay test: two mounts of same `(repo, commit)` produce one tarball call.
> - Path-traversal replay test: malicious tarball rejects offending entries; legitimate entries still land.
> - Daemon-restart replay test: orphaned temp files cleared on startup.

---

## Carry-forwards into M3

These accumulated from per-task quality reviews and the M2-end Codex review. Worth folding into the M3 plan or tracking explicitly:

### From Codex's M2-result review

1. **Symlink hardening edge cases**. Currently:
   - Excluded-by-`small_blob_shas`-size → empty target.
   - Missing `size` field → empty target.
   - Invalid UTF-8 bytes → empty target with `tracing::warn!`.
   
   None of these fail the snapshot. Codex recommended fail-strict for M3. Decide: tighten the policy, or document as Phase 5 territory?

2. **Missing assembled-path test**. There's no provider-git HTTP-mocked `fetch_snapshot` test that asserts the manifest has inline tiny files + exact symlink targets + zero read-time blob calls. Per-component tests cover the pieces; the assembled path doesn't have a green-light test. M3 candidate.

### From the /simplify pass

3. **HeaderMap-direct refactor of `ThrottleClassifier::classify`**. Today it takes `&HashMap<String, String>`. Per-response we materialize ~30 heap allocations to build that map. Switching to `&HeaderMap` (or a small struct of just the 4 headers we read) eliminates the allocations. Signature change cascades to tests. M3 candidate.

4. **`bearer_header(&str) -> String` helper**. Three sites in `provider-git` (`github.rs::new`, `token.rs:24`, `token.rs:30`) duplicate `format!("Bearer {token}")`. Trivial; can be folded into M3 if convenient.

5. **`<resolving:ref>` placeholder bucket pruning**. Today: filtered from user view via `status_report().mounts`. Per-key counter still accumulates. M3+: prune the bucket entirely after counter_key is replaced (or merge into the real bucket). Telemetry cleanup, not behavior.

### Pre-existing flakes

6. **`env_var_invalid_falls_through_to_config` / `env_var_overrides_config` race**. Confirmed pre-existing on M1's tag. Codex flagged: "fix the env-var race before any broader release because it can mask real CLI regressions, but it should not block starting M3." Decide whether to fix in M3 or punt.

---

## Bugs status (B1–B8 from triage)

| Bug | Status | Where |
|---|---|---|
| B1 — tiny-file inlining | ✅ Shipped | M2 (commit `b688dd4` + dependency commits) |
| B2 — truncated-tree fallback | ⏳ Pending | M3 |
| B3-label — Sha256 mislabeled | ⏳ Pending | M5 (Sha1 variant in core; verification = Phase 5) |
| B4 — secondary throttle classification | ✅ Shipped | M2 (provider-git → VFS → adapters: NFS JUKEBOX / FSKit EAGAIN) |
| B5 — per-repo cache reservation | ⏳ Pending | M5 |
| B6 — LFS pointer detect | ⏳ Pending | M5 (detect+warn; full smudge = Phase 5) |
| B7 — symlink target resolution | ✅ Shipped | M2 (commit `b688dd4`) |
| B8 — active_source race | ⏳ Deferred | Phase 5 (M4 enforces per-mount provider constraint) |

---

## File map (where M3 will touch)

- `crates/ctxfs-provider-git/src/github.rs` — most changes; tarball endpoint integration, auto-gate logic, `ContentFetcher` skeletal impl, B2 fallback.
- `crates/ctxfs-cache/src/blob.rs` (or wherever `BlobCache::put` lives) — atomic temp-and-verify-and-rename writes for tarball hydration.
- `crates/ctxfs-cache/src/lib.rs` — daemon-restart temp-orphan cleanup helper.
- `crates/ctxfs-core/src/config.rs` — `prefetch_threshold_count` and `prefetch_max_bytes` env parsing.
- `crates/ctxfs-ipc/src/service.rs` — `MountOptions { prefetch: PrefetchPolicy }` on Mount RPC.
- `crates/ctxfs-cli/src/main.rs` — `--prefetch` / `--no-prefetch` flags on `ctxfs mount`.
- `crates/ctxfs-provider-common/src/` — new `ContentFetcher` / `FetchPolicy` / `ContentRequest` types.
- `crates/ctxfs-daemon/src/daemon.rs` — singleflight tarball-prefetch dedupe map; restart-time temp cleanup.
- `crates/ctxfs-provider-common/src/counters.rs` — add the new prefetch / tarball / truncated-tree counter fields the spec lists (some may already exist from M1).

---

## Recommended first actions in the new session

1. **Read this handoff** + the spec § M3 + the M2 plan (for style reference).
2. **Write the M3 plan** at `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` (today's date — adjust to actual date).
3. **Counsel Codex on the plan** following the M1/M2 pattern. Save prompt to `/tmp/phase4-m3-plan-counsel-prompt.md`.
4. **Apply Codex edits**, present plan v2 to user, get approval.
5. **Respawn the cmux team** (engineer / spec-reviewer / quality-reviewer) using the existing `.claude/agents/` definitions and team config at `~/.claude/teams/phase4-impl/`.
6. **Execute task-by-task** per the established protocol.

---

## Caveats / pitfalls

- The `engineer-2` rename happened in M2 because the prior `engineer` slot wasn't released after a hung-pane shutdown. After M2's clean shutdown, the slot should be free; if cmux still rejects the name, accept `engineer-2` again.
- The pre-existing test flakes are real and persistent; don't get distracted trying to fix them mid-task. They're noted as a known issue and any failure on `mount_server_only_starts_nfs_and_reports_port` or the `env_var_*` family is the documented pre-existing one until proven otherwise (rerun with `--test-threads=1` or `CTXFS_E2E_SKIP_NETWORK=1`).
- `cargo test` against real GitHub will rate-limit; the spec-reviewer hit this in M2.T3 and observed the M2 chain working correctly (RateLimited → JUKEBOX). For M3 testing, `CTXFS_E2E_SKIP_NETWORK=1` is your friend.
- Don't push tags. The user explicitly wants all tags pushed together at end of M5.

---

## Quick reference — key paths

- Spec: `docs/superpowers/specs/2026-04-25-phase-4-rate-limit-design.md`
- M1 plan: `docs/superpowers/plans/2026-04-25-phase-4-m1-observability.md`
- M2 plan: `docs/superpowers/plans/2026-04-25-phase-4-m2-architecture-neutral-fixes.md`
- M2 brainstorm artifacts: `docs/phase4-{rate-limit-handoff,option-a-memo,option-b-memo,bug-triage}.md`
- Cmux-team skill: `~/.claude/skills/cmux-team/SKILL.md`
- Team agent definitions: `.claude/agents/{engineer,spec-reviewer,quality-reviewer}.md`
- Team config: `~/.claude/teams/phase4-impl/config.json`
- Most recent Codex M2-result review: `/tmp/counsel/20260426-064959-claude-to-codex-720b80/codex.md`
- CHANGELOG: `CHANGELOG.md` (M1 + M2 entries)
