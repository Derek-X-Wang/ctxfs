# Engineer handoff — after T3c (B5 status surfacing)

**Last commit (HEAD):** `93e01d1` — `feat(daemon,cli): B5 status surfacing — per-mount working set vs reservation`
**Status:** T3c committed and ✅ approved by both spec-reviewer and quality-reviewer with 3 cosmetic Minors (carry-fwd into T4 dispatch — see below). T3a/T3b/T3c together close B5; T1 closed B3-label; T2 closed B6.

## T3c quality-reviewer Minors (your first action: land these as a small chore commit before T4 starts)

1. `crates/ctxfs-cli/src/main.rs:946` — `// Per-mount cache usage (B5 surface)` → strip the `(B5 surface)` provenance tag. Replace with `// Per-mount cache usage:` or just remove the comment if the printed header makes it obvious.
2. `crates/ctxfs-daemon/src/daemon.rs:1431` — `/// T3c: working_set_bytes and cache_reservation_bytes are populated by ...` → strip `T3c:` prefix. Replace with `/// Verifies working_set_bytes and cache_reservation_bytes are populated by ...`.
3. `crates/ctxfs-cli/src/main.rs::tests` — add a `format_bytes` boundary table test:
   ```rust
   #[test]
   fn format_bytes_boundaries() {
       use super::format_bytes;
       assert_eq!(format_bytes(0), "0 B");
       assert_eq!(format_bytes(1023), "1023 B");
       assert_eq!(format_bytes(1024), "1.0 KiB");
       assert_eq!(format_bytes(1_048_575), "1024.0 KiB"); // visually odd but correct: still under MiB threshold
       assert_eq!(format_bytes(1_048_576), "1.0 MiB");
       assert_eq!(format_bytes(1_073_741_823), "1024.0 MiB");
       assert_eq!(format_bytes(1_073_741_824), "1.0 GiB");
   }
   ```

   (Adjust string formats to match the actual `format_bytes` impl — the spec is "GiB/MiB/KiB/B with `{:.1}` precision".)

Single small commit, e.g.:
```
chore(M5): T3c carry-forwards — provenance strips + format_bytes boundary tests

Quality-reviewer Minors on T3c (93e01d1):
- Strip "(B5 surface)" inline comment in cli/main.rs.
- Strip "T3c:" task-tracker prefix from working_set_and_reservation_appear_in_status doc.
- Pin format_bytes boundary behavior with table tests covering
  0 / 1023 / 1024 / 1MiB-1 / 1MiB / 1GiB-1 / 1GiB transitions.

No external behavior change.
```

Run gauntlet, commit, then start T4.

## What landed in M5 so far (most recent first)

| SHA | Type | Subject |
|---|---|---|
| `93e01d1` | feat | B5 status surfacing (T3c) — assemble_status_report extended with cache fields, CLI per-mount usage block + blocked counter |
| `dcca5c2` | chore | T3b carry-fwds — Arc::get_mut hardening + dead_code cleanup + over-subscribed test + parse_size_bytes overflow test + rotated-reset comment |
| `7dfe630` | fix | T3b spec-fix — unregister_mount blob_owners cleanup + 3-repo rebalance test |
| `1f53587` | feat | B5 reservation+eviction-skip+per-mount flag (T3b main) |
| `4cc8e2f` | chore | T3a carry-fwds — provenance strip + put_for race doc |
| `9a348e9` | feat | B5 foundation (T3a) — RepoKey, MountCacheView, single Mutex<CacheState> |
| `cdd0d97` | refactor | T2 carry-fwds — drop orphan record_lfs_pointer_file + comment cleanup + CRLF test |
| `1a6df22` | feat | B6 LFS detect-and-surface (T2) — manual parser + sha→path map + tarball Tee peek |
| `33fea48` | chore | T1 carry-fwds — provenance breadcrumb strip + redundant binding |
| `8972b0f` | feat | B3-label (T1) — HashAlgorithm::Sha1 + cache layout per-entry algorithm + TreeCache schema 2→3 |

Pre-M5 carry-fwds (already on main before this session): `2bd667a` env-var test race fix; `233133a` default_cost_estimate direct test.

## Next: Task 4 — Replay tests for B5 + B6

**Plan reference:** `docs/superpowers/plans/2026-04-29-phase-4-m5-remaining-bugs.md` § Task 4 (Steps 1–3).

**Scope:** Two end-to-end replay tests anchoring the M5 exit criteria. Each spins up a mock GitHub server (model after existing `replay_basic.rs` or `replay_tarball_three_calls.rs` in `crates/ctxfs-provider-git/tests/`) and validates counters/state via the StatusReportV1 path.

**Items to create:**

- `crates/ctxfs-provider-git/tests/replay_lfs_detect_surfaces_count.rs` — mock server returns one LFS pointer payload; `fetch_blob` runs detection; assert `MountCounters.lfs_pointer_files == 1` and the sample buffer carries the expected mount-relative path.

- `crates/ctxfs-provider-git/tests/replay_b5_reservation_protects_active.rs` — the canonical B5 invariant exit-criterion test:
  - Build daemon harness with small cache (e.g., 500 bytes).
  - Mount A (small corpus, working set ≤ reservation 400).
  - Mount B (cache pressure: writes that would force eviction).
  - Scan A's files; assert `cache_hits` for A's blobs unchanged after B's writes.
  - Assert `eviction_attempts_blocked_by_reservation > 0`.

**No-touch:**
- The cache eviction loop (`lru_insert_evict`) is closed; replay tests should validate behavior, not modify it.
- `assemble_status_report` (T3c) is closed; tests use it as-is.
- The carry-fwds and the spec-reviewer's findings on T3a/T3b/T3c are all closed.

**Tests:**
- The two new files ARE the tests. Each should be a single `#[tokio::test]` (or sync if appropriate).
- Run via `cargo test -p ctxfs-provider-git --test replay_lfs_detect_surfaces_count` etc.

## Read order for fresh teammate

1. `.claude/agents/engineer.md` — your role + per-task workflow
2. This handoff doc
3. `docs/superpowers/plans/2026-04-29-phase-4-m5-remaining-bugs.md` § Task 4 (Steps 1–3)
4. `crates/ctxfs-provider-git/tests/replay_basic.rs` — mock-server pattern reference
5. `crates/ctxfs-provider-git/tests/replay_tarball_three_calls.rs` — daemon-harness pattern
6. `crates/ctxfs-cache/tests/reservation.rs` — the B5 invariant unit test (T3b created it; T4's replay version is the end-to-end equivalent)
7. `crates/ctxfs-provider-common/src/lfs.rs` — the detector you're testing
8. `crates/ctxfs-cache/src/reservation.rs` + `crates/ctxfs-cache/src/lib.rs` (`register_mount`, `working_set_bytes`, `eviction_attempts_blocked_by_reservation`) — the public API your replay test exercises

## Constraints (M5-wide; carry forward)

- **Workspace lints:** `cargo clippy --all-targets --tests -- -D warnings` is a hard gate. Pedantic warnings are also denied.
- **TDD:** write failing test first, then implementation. Each task gets one commit (or carry-fwd commit + main).
- **No provenance breadcrumbs in code/comments.** No `(Codex M5-plan-v1 #N)`, no `B6:` / `T3a` prefixes, no `// added in M5 commit ...`. Commit message is the right home for those. The M3/M4/M5 reviewer cycle has flagged this 4× already.
- **No new external deps.** Manual parsers / inline helpers preferred.
- **The `Snapshot::all_blob_digests()` accessor lives on the manifest crate** (added in T3b) — use it in any replay test that needs the manifest's digest list.
- **Pre-existing flakes (don't try to fix):**
  - `mount_server_only_starts_nfs_and_reports_port` (NFS port-bind on macOS; pre-existing)
- **Tag policy:** `v0.1.5-m5` will be created in T5 after T4 lands. **No tag pushes until T6** — that's the user's milestone-end-of-Phase-4 push instruction.

## Unfinished work after T4

- T5 — CHANGELOG entry + tag `v0.1.5-m5` (small)
- T6 — Push all 5 tags (`v0.1.1-m1` through `v0.1.5-m5`) together (operational; user's milestone-end push)

## Acknowledge

Reply with `READY_FOR_T4` + any clarifying questions before starting.
