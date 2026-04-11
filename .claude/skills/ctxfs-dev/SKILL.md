---
name: ctxfs-dev
description: Develop on the ContextFS (ctxfs) codebase itself. Use this skill whenever you're implementing, debugging, testing, or refactoring anything inside this repo — adding CLI commands, extending the daemon, writing a new registry resolver, touching the cache tiers, changing the NFS server, or fixing clippy/test failures. Covers the crate layout, where to put new code, how to build and test, TDD conventions, lint rules, environment variables, common gotchas, and how to commit changes. Read this before making non-trivial edits anywhere under `crates/`.
---

# ctxfs-dev — Developing on ContextFS

This skill is for agents working **inside** the ctxfs repo. It covers the crate layout, build/test workflow, conventions, and gotchas unique to this codebase.

If you're trying to **use** the ctxfs tool (mount dependencies), see the sibling `ctxfs` skill instead.

## Project Overview

ContextFS is an AI-native read-only mountable filesystem. It mounts Git repos and package-registry sources (npm, PyPI, crates.io) as local directories via an NFSv3 loopback server, fetching files lazily from GitHub. The user interacts with a `ctxfs` CLI that talks to a persistent `ctxfs-daemon` over a Unix domain socket via tarpc.

**Core flow**: `ctxfs mount npm:react@19.1.0 -p ./mnt` →
CLI (tarpc client) → Daemon (tarpc server) → Registry resolver (npm → GitHub coords) → Git provider (fetches tree manifest via GitHub REST API) → Stores in tiered cache (blob/tree/resolution) → Spawns NFS server on loopback → CLI runs `sudo mount_nfs` to kernel-mount it.

## Crate Layout (13 crates)

Read the CLAUDE.md at the repo root for the canonical dependency graph. Summary by role:

**Core types and traits (depended on by almost everything):**
- `ctxfs-core` — `Digest`, `SourceSpec`, `ProviderType`, `Config`, `CtxfsError`, the `Provider` async trait. Start here for type definitions.
- `ctxfs-manifest` — `Snapshot`, `Directory`, `DirEntry`, `InodeTable`. In-memory representation of a mounted tree.

**Caching (tiered):**
- `ctxfs-cache` — Three cache tiers: `BlobCache` (LRU content-addressable, on-disk), `TreeCache` (directory manifests per commit SHA), `ResolutionCache` (package spec → GitHub coords with TTL). Also defines the `SharedTreeCache` async trait.
- `ctxfs-cache-redis` — Optional Redis-backed `SharedTreeCache` impl, feature-gated.

**IPC and providers:**
- `ctxfs-ipc` — tarpc `CtxfsService` trait + `MountInfo`/`CacheStats` types + UDS transport helpers.
- `ctxfs-provider-common` — `RegistryResolver` trait, `ResolvedSource`, shared `parse_github_url` utility. Registry resolver crates depend on this.
- `ctxfs-provider-git` — `GitHubProvider` implementing `Provider`. Fetches tree + blobs from the GitHub REST API. Uses `BlobCache`, `TreeCache`, optional `SharedTreeCache`.
- `ctxfs-provider-npm` / `-pypi` / `-crate` — Each implements `RegistryResolver` for one ecosystem. They parse registry metadata, extract GitHub coordinates, and return a `ResolvedSource`.

**NFS backend:**
- `ctxfs-nfs` — Implements `nfsserve::NFSFileSystem` over a `Snapshot`+`InodeTable`+`BlobCache`. Spawns an NFSv3 TCP server on a loopback port per mount.

**Top-level binaries:**
- `ctxfs-daemon` — The background service. Hosts the tarpc server, manages mount lifecycle, owns the cache instances. `Daemon::run()` is the entry point.
- `ctxfs-cli` — The `ctxfs` binary (clap-based CLI). Contains `deps/` module for dependency detection. Talks to the daemon over UDS.

## Where to Add New Code

| Change | Where |
|---|---|
| New CLI command | `crates/ctxfs-cli/src/main.rs` — extend `Commands` enum and add a handler |
| New RPC method | `crates/ctxfs-ipc/src/service.rs` — add to `CtxfsService` trait; implement in `crates/ctxfs-daemon/src/daemon.rs` `DaemonServer` |
| New registry (e.g., rubygems) | New crate `crates/ctxfs-provider-rubygems/` implementing `RegistryResolver` from `ctxfs-provider-common`; wire into `ctxfs-daemon`'s resolver dispatch |
| New source type | Add variant to `ProviderType` in `crates/ctxfs-core/src/source.rs`, update `SourceSpec::parse`, add resolver |
| New cache tier | `crates/ctxfs-cache/src/` — new module, expose via `lib.rs`, wire into daemon |
| New error kind | `crates/ctxfs-core/src/error.rs` `CtxfsError` enum (uses `thiserror`) |
| CLI dependency detection for a new manifest | `crates/ctxfs-cli/src/deps/` — new parser file, register in `mod.rs` `detect_all` parser table |
| New NFS feature | `crates/ctxfs-nfs/src/` — `NFSFileSystem` trait methods |

**Golden rule**: follow existing crate boundaries. Don't cross-import between unrelated crates. If something would require it, look for the right shared abstraction or add one in `ctxfs-core`.

## Build, Test, Lint

```bash
# Full workspace build
cargo build --release

# Run everything (all unit + integration tests)
cargo test

# Single crate
cargo test -p ctxfs-cli

# Single test by name
cargo test -p ctxfs-cache lru_eviction

# Lint (required before commit — clippy::all = deny)
cargo clippy --all-targets --tests

# Format
cargo fmt --all
```

## Lint Rules (important)

Workspace lints live in the root `Cargo.toml` under `[workspace.lints]`. All crates inherit via `[lints] workspace = true`.

- **Rust**: `unsafe_code = warn`, `unused_results = warn`, `missing_debug_implementations = warn`
- **Clippy**: `all = deny` (hard errors), `pedantic = warn`
- **Pedantic overrides** (allowed): `module_name_repetitions`, `must_use_candidate`, `missing_errors_doc`, `missing_panics_doc`, `return_self_not_must_use`, `wildcard_imports`, `cast_possible_truncation`, `cast_sign_loss`, `cast_lossless`

**Common fixes when clippy yells:**
- `too_many_lines` on a long function → add `#[allow(clippy::too_many_lines)]` above the fn (already done for `main()`)
- `type_complexity` → extract a `type Foo = ...` alias at module scope (not inside the function — `items_after_statements` will then warn)
- `items_after_statements` → move `type`/`const` declarations to module scope
- New `pub struct` without `Debug` → add `#[derive(Debug)]` (lint is `missing_debug_implementations`)
- `ignore_without_reason` on a `#[ignore]` test → use `#[ignore = "reason"]`

## Testing Conventions (TDD)

The project uses a TDD workflow: **write the failing test first**, then implement until it passes. This is enforced by convention, not tooling.

**Unit tests** live inline in `#[cfg(test)] mod tests { ... }` blocks at the bottom of each source file. Prefer these for pure logic.

**Integration tests** live in `crates/*/tests/*.rs`:
- `ctxfs-core/tests/cross_crate.rs` — SourceSpec/Digest/Config interop across crates
- `ctxfs-cache/tests/lifecycle.rs` — cache restart persistence, concurrency, eviction under pressure
- `ctxfs-cache/tests/tiered_cache.rs` — resolution and tree cache lifecycle
- `ctxfs-ipc/tests/rpc_roundtrip.rs` — real tarpc client/server over UDS
- `ctxfs-provider-git/tests/build_directories.rs` — snapshot construction → cache → resolution
- `ctxfs-nfs/tests/medium_repo.rs` — mounted-FS reads against a real repo (rate-limited)
- `ctxfs-cli/tests/e2e.rs` — end-to-end daemon + CLI smoke tests

**Tests that require network/rate-limited resources** should be `#[ignore = "reason"]`'d by default or gated on `GITHUB_TOKEN` being set. Two currently-ignored tests:
- `mount_npm_server_only_resolves_and_starts_nfs` — requires matching tag conventions (lodash uses `4.17.21`, not `v4.17.21`)
- `mount_server_only_starts_nfs_and_reports_port` — fails when rate-limited (runs fine in CI with token, fails locally without)

When writing new tests that hit the real GitHub API, add a `GITHUB_TOKEN` check at the top or mark them `#[ignore]`.

## Environment Variables

Read in `ctxfs-core::config::Config::from_env`:

- `GITHUB_TOKEN` — 5000 req/hr vs 60 unauthenticated. Set this for real testing.
- `CTXFS_SOCKET` — override daemon UDS path (default: `~/.ctxfs/ctxfs.sock`)
- `CTXFS_CACHE_DIR` — override blob cache dir (default: `~/.ctxfs/cache`)
- `CTXFS_CACHE_MAX_BYTES` — max blob cache size (default: 512MB)
- `CTXFS_LOG_LEVEL` — tracing filter (default: `info`)
- `CTXFS_REDIS_URL` — optional shared tree cache backend (requires `--features redis`)
- `CTXFS_LATEST_TTL_SECS` — TTL for `@latest` resolution cache (default: 3600)
- `CTXFS_TREE_CACHE_MAX_BYTES` — max local tree cache size (default: 500MB)

## Running the Daemon Locally

For dev testing of mount flows:

```bash
# Foreground (for logs)
cargo run --release -p ctxfs -- daemon start

# Background (detach)
nohup ./target/release/ctxfs daemon start > /tmp/ctxfs-daemon.log 2>&1 &

# Stop
./target/release/ctxfs daemon stop
```

For testing a mount without needing sudo (NFS kernel mount), use `--server-only`:

```bash
./target/release/ctxfs mount github:octocat/Hello-World@master /tmp/mnt --server-only
```

This exercises the daemon → provider → cache → NFS-server path without requiring a kernel mount. Useful for iterating on provider/cache/daemon code.

## Key Architectural Constraints

1. **Daemon is authoritative** — all mount state lives in the daemon. The CLI is a thin tarpc client. Don't add state to the CLI.
2. **Read-only filesystem** — the NFS server rejects writes at the FUSE/NFS layer. Don't add write paths.
3. **Lazy fetch** — tree manifests fetch when a mount starts; individual blobs fetch on first read. Don't eagerly fetch all blobs at mount time.
4. **Content-addressable storage** — blobs are keyed by SHA-256 hex, in a fan-out directory structure (`sha256/<prefix>/<rest>`). Never key by filename.
5. **tarpc over UDS with postcard wire** — don't introduce new IPC mechanisms. New RPCs go into the existing service trait.
6. **Three-tier caching** — blobs (content), trees (directory manifests), resolutions (package→GitHub). Each tier has different eviction rules. Check `crates/ctxfs-cache/src/` before adding a fourth tier.
7. **Tests first** — TDD convention. Write a failing test, run it, then implement.

## Common Gotchas

**"My clippy fix passes locally but CI fails"** — CI runs `cargo clippy --all-targets --tests`. Make sure you passed `--all-targets` locally too; `cargo clippy` alone skips test targets.

**"My test passes alone but fails with `cargo test`"** — Check for shared state: cache dirs, sockets, environment variables. Use `tempfile::tempdir()` per test. The cache crate has a `BlobCache::new(tempdir, 1024)` pattern — follow it.

**"My integration test gets rate-limited"** — Set `GITHUB_TOKEN` in your shell, or mark the test `#[ignore]`. Never commit real API keys.

**"Daemon won't stop"** — Stale PID file at `~/.ctxfs/ctxfs.pid`. Delete it. The daemon startup checks for live processes via signal 0 and skips stale files, but if something went wrong you may have to clean up manually.

**"Mount succeeds but files look empty"** — NFS cache timing. If it persists across retries, check the daemon log for fetch errors. Blob cache at `~/.ctxfs/cache/sha256/...` should have the data.

**"I added a feature flag but cargo still compiles the default path"** — Features are additive. Make sure you're passing `--features <name>` when testing, or check that the flag is in the crate's `[features]` table and the code is gated with `#[cfg(feature = "...")]`.

**"My new crate isn't recognized"** — Add it to the `[workspace] members = [...]` list in the root `Cargo.toml`, and declare it in `[workspace.dependencies]` so other crates can use `{ workspace = true }`.

## Commit Style

The repo follows "git commits as project memory" — commit messages should explain the *why*, not just the *what*. Example from recent history:

```
feat(cli): restructure Commands enum for multi-mount, unmount --all, and deps

Adds variadic sources with auto-derived mount points via --mount-dir,
--all flag on unmount, and a new deps subcommand group. The Mount
variant's mount_point became a flag (-p) instead of a positional
because clap can't disambiguate Vec<String> + trailing positional.
```

Subject: `type(scope): short description` — types: `feat`, `fix`, `refactor`, `test`, `docs`, `chore`, `style`. Keep subject under ~70 chars. Body explains constraints, trade-offs, or context that future-you will want.

Never commit without running `cargo clippy --all-targets --tests` and `cargo test` first (unless you're committing WIP and will clean up before merging).

## When To Escalate

Stop and ask the user (or report `BLOCKED`) if:
- A change would require breaking the tarpc service contract (existing clients would fail)
- A change would require `unsafe` code beyond the existing `libc::kill` use
- You need to add a new external dependency that isn't already in `[workspace.dependencies]`
- The plan calls for modifying crates you weren't asked to touch
- You've tried the same clippy/test fix three times and it keeps failing differently each time

Don't improvise architecture decisions — the 13-crate boundary is deliberate.

## References

- **Root `CLAUDE.md`** — canonical project overview, dependency graph, env vars. Read this before anything else.
- **`docs/superpowers/specs/`** — design specs for recent features (tiered caching, multi-mount, deps)
- **`docs/superpowers/plans/`** — TDD implementation plans
- **`crates/*/README.md`** — per-crate READMEs (where they exist)
