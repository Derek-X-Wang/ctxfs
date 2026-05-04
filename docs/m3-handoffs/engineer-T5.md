# Engineer handoff — after Task 5

**Last commit (HEAD):** `0ea6cbf` — `fix(provider-git,B2): truncated-tree per-directory walk fallback`

**Status:** Tasks 1–5 closed; both reviewers APPROVED Tasks 1, 2, 3, 4 (with one fix-up commit), and 5. One micro carry-forward from quality-reviewer's T5 review folds into Task 6's first commit.

## Commit log since you joined / since milestone start

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

10 commits across 5 tasks.

## Carry-forward minor for your first Task 6 commit

**Add a reverse-tether doc note on `assemble_walked_tree`** in `crates/ctxfs-provider-git/src/github.rs`. Quality-reviewer flagged: the test-only `assemble_walked_tree` and the production async `fetch_tree_walked` have byte-for-byte-mirror DFS bodies. `fetch_tree_walked` has a forward-pointing comment ("Mirrors `assemble_walked_tree`..."). `assemble_walked_tree`'s doc should add the reverse tether: "**If you change the DFS loop body, mirror the change in `fetch_tree_walked`.**" One sentence; folds into a tiny prerequisite commit before Task 6's substantive work.

## Critical context for Task 6 — read this BEFORE you touch streaming code

**Task 6 is the heaviest task in the milestone.** It's the streaming tarball download path with all the hardening. The plan covers:
- `redirect::Policy::none()` on the provider client (so reqwest doesn't auto-follow with Authorization attached)
- Manual 302 follow with Authorization-stripped fresh client + codeload-host whitelist + depth ≤ 3
- Streaming via `bytes_stream() → StreamReader → SyncIoBridge → flate2::GzDecoder → tar::Archive` inside `tokio::task::spawn_blocking`
- Per-entry `Tee<GitBlobSha1, BlobTempWriter>` (so we hash + write in one std::io::copy)
- Type-aware path validation (raw bytes + `EntryType` distinguishes wrapper-dir from stray-file-at-root)
- Strict UTF-8 path checking (`std::str::from_utf8` on `entry.path_bytes()`, NOT `to_string_lossy`)
- Per-blob digest verification BEFORE `writer.finalize`

**Plan reference:** `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` § Task 6 (lines ~1580-2144).

**Critical Codex/quality-review fix already in plan v2:**
The original Task 4 plan had `BlobTempWriter` verify content via internal SHA-256, which would always reject GitHub manifest digests (40-char SHA-1 hex ≠ 64-char SHA-256 hex). The fix landed in commit `7c9b92b`: **`BlobTempWriter` is content-agnostic now**. It does NOT verify on `finalize`. Verification is the caller's job. Task 6's tarball flow MUST verify externally via `GitBlobSha1` + `Tee` BEFORE calling `writer.finalize`. The plan's Task 6 step 4 already shows this pattern — follow it.

**`Digest::from_sha256_hex` is mislabeled** (B3 — fixed in M5). It just stores the hex string verbatim. For Git blob SHA-1s (40-char), it stores the 40-char hex. Don't be confused by the name.

## Task 6 quick-reference

**Files:**
- `Cargo.toml` (workspace root) — add `tar = "0.4"`, `flate2 = "1"`, `sha1 = "0.10"`. Update `reqwest` features to include `stream`. Update `tokio-util` features to include `io`.
- `crates/ctxfs-provider-git/Cargo.toml` — add `tar`, `flate2`, `sha1`, `tokio-util` workspace deps.
- `crates/ctxfs-provider-git/src/github.rs` — major modify

**API to land** (skim plan, then implement):
- `validate_redirect_target(location, api_host) -> Result<reqwest::Url, CtxfsError>` (pure)
- `validate_tar_entry_path(raw: &[u8], entry_type: tar::EntryType) -> Result<PathBuf, CtxfsError>` (pure, EntryType-aware)
- `codeload_host_for(api_host: &str) -> String` helper
- `fetch_tarball_into_cache(source, commit_sha, tree_entries) -> Result<TarballOutcome, CtxfsError>` — async, streams via SyncIoBridge inside spawn_blocking
- `pub(crate) struct GitBlobSha1` — incremental hasher with size-prefix-first behavior
- `pub(crate) struct TarballOutcome { blobs_committed, blobs_skipped_invalid, blobs_skipped_digest, total_bytes }`
- `GitHubProvider` gains `api_host: String` field; `new` and `new_with_codeload_host` constructors

**Test-only constructor:**
`GitHubProvider::new_with_codeload_host(token, api_host, codeload_host: Option<String>, cache, ...)` — production stays on `new(...)` which derives codeload from api_host. Tests use the override variant to point both at `127.0.0.1:<port>`. M3 replay tests in Task 8 will use this.

**Tests in this task** (Task 6 is plumbing — the heavy replay tests are Task 8):
- 6 path-validation tests
- 1 redirect-target validation test
- (No HTTP-mocked tests yet — those are Task 8's replay-test scaffolding)

**Verify after:**
- `cargo build` (workspace) — clean
- `cargo test -p ctxfs-provider-git` — green
- `cargo fmt --all -- --check` clean
- `cargo clippy --all-targets --tests -- -D warnings` clean

**No call-site changes outside provider-git in Task 6.** `dispatch_fetch_policy` and the daemon wiring is Task 7. `GitHubProvider::new` callsites in daemon + NFS tests will need a +api_host arg — but the plan defers that signature change to Task 7's commit (see plan § Task 7 step 4). For Task 6, just add the `api_host` field; the existing callers will break the build. **Two acceptable paths:**
1. Make Task 6 the moment we update all callers (daemon + NFS tests gain `"api.github.com".to_string()` arg). Keeps each commit green.
2. Use a temporary `Default` for `api_host` ("api.github.com") so Task 6's commit doesn't need to touch callers; remove it in Task 7. Slightly uglier but isolates change scope.

**Recommendation: option 1.** Update callers in Task 6's commit so the workspace stays green. Plan's File Structure already lists this. The carry-forward complexity isn't worth saving 3 lines.

## Read order for fresh engineer

1. `.claude/agents/engineer.md` — your role
2. This handoff (you're reading it)
3. `docs/superpowers/plans/2026-04-27-phase-4-m3-tarball-prefetch.md` — read § Architecture (top), § Task 6 (lines ~1580-2144). Skim § Task 7 enough to know what NOT to do (no singleflight in Task 6).
4. `crates/ctxfs-provider-git/src/github.rs` — current state. Note: existing `client` field will need `redirect::Policy::none()`; existing `auth_identity` derivation logic moves around `api_host`.
5. `crates/ctxfs-cache/src/lib.rs` — `commit_atomic_with_writer` and `BlobTempWriter` are the entry points you'll use. **Do NOT call `writer.finalize` without external verification.**
6. `Cargo.toml` (root) — current workspace deps; you'll add tar/sha1/flate2 + extend reqwest + tokio-util features.

## Constraints (unchanged from earlier handoffs)

- Tags local-only through M5; do NOT push.
- Pre-existing failures (`mount_server_only_starts_nfs_and_reports_port`, `env_var_*`, network-dependent NFS integration tests) are expected.
- B8 constraint: NO shared/global fetcher; provider-git stays per-mount; tarball_singleflight is Task 7. **Task 6 should NOT introduce any daemon-side state.**
- Workspace lints: `clippy::all = deny`, `pedantic = warn`. `cargo clippy --all-targets --tests -- -D warnings` must pass.

## Acknowledge

Reply with `READY_FOR_TASK_6` + any clarifying questions before starting. The team lead will then send "Begin Task 6" with formal scope.
