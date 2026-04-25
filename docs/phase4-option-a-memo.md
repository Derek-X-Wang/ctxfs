# Option A — Git-Native v2 Provider: The Case

**Author:** option-a-advocate  
**Date:** 2026-04-25  
**Scope:** ContextFS Phase 4, rate-limit / v2 provider design brainstorm  
**Counterpart:** `docs/phase4-option-b-memo.md`

---

## 1. The bound this gives you, precisely stated

The REST provider today costs **one `x-ratelimit` token per blob read** on a cold cache. A `grep -r` over a 5,000-file repo exhausts the entire 5,000-req/hr authenticated budget in a single scan. An unauthenticated user hits their 60-req/hr wall in seconds.

The Git-native v2 moves the unit of cost from _requests_ to _transport operations_:

| Operation | REST (v1) | Git-native v2 |
|---|---|---|
| Full tree manifest | 2 calls (resolve-ref + tree) | 0 network calls after first fetch (local `git ls-tree`) |
| N-file cold content scan | N blob API calls | 1 packfile fetch OR 1 tarball download, then 0 |
| Warm cache (repeat scan) | 0 calls | 0 calls (same as today) |
| Incremental update to new commit | 1 tree + M new blob calls | 1 `git fetch` (delta-compressed, only new objects) |

A packfile fetch retrieves arbitrary-sized blob sets in **O(1) round trips with delta compression**. Git's pack protocol (smart HTTP) negotiates a minimal transfer set — you do not pay per-object over the wire. Reference: [git smart HTTP protocol](https://git-scm.com/docs/http-protocol/2.19.0.html).

**The honest bound:** packfile transport is **not** rate-limit-free. GitHub's documented soft limit is [15 Git read ops/sec/repo](https://docs.github.com/en/repositories/creating-and-managing-repositories/repository-limits). There is no published authenticated hourly Git transport quota, but secondary limits (403/429 + `retry-after`) apply to Git transport just as they do to REST. The correct claim is: **"Git transport avoids per-blob primary-quota depletion. It does not eliminate rate limits."**

For bootstrap, the tarball endpoint (`GET /repos/{o}/{r}/tarball/{ref}`) costs **1 REST call** and delivers the entire repo. That call counts against the 5000/hr primary quota but leaves 4,999 remaining — a qualitatively different budget profile than the current O(N) spend.

---

## 2. Concrete component sketch

### Crate changes

**`ctxfs-provider-git`** — primary change surface. No new top-level crates needed.

Add three modules:
- `gitstore.rs` — `GitObjectStore`: manages `~/.ctxfs/git-cache/{host}/{owner}/{repo}.git/` bare clones. Responsible for `git clone --filter=blob:none --bare` on first mount and `git fetch --filter=blob:none` on subsequent mounts. Handles credential passthrough via subprocess environment (`GITHUB_TOKEN` → `GIT_ASKPASS` or standard Git credential flow).
- `catfile.rs` — `CatFilePipeline`: long-lived `git cat-file --batch` subprocess per active repo. Exposes `fetch_bytes(oid: &str) -> Vec<u8>` and `fetch_batch(oids: &[String]) -> Vec<(String, Vec<u8>)>`. Uses `--buffer` mode for throughput. The pipeline is sequential (one request, one response) — callers serialize through a `tokio::sync::Mutex`.
- `prefetch.rs` — `PrefetchScheduler`: triggered when VfsState populates a directory. Collects all file OIDs in the directory, deduplicates against BlobCache, and submits the remainder to `CatFilePipeline` in one batch.

**`ctxfs-vfs`** — `VfsState::ensure_populated` emits a prefetch hint after building the child list (one hook, ~5 lines). No structural changes.

**`ctxfs-daemon`** — holds the `GitObjectStore` instance and `CatFilePipeline` pool. No new IPC surface needed.

### Daemon process model

One long-lived `git cat-file --batch` subprocess per mounted repo, kept alive for the duration of the mount. Spawned lazily on first blob request. Bridged to async Tokio via `tokio::process::Child` + `spawn_blocking` for pipe I/O. On subprocess death (OOM kill, git crash): restart with backoff, return `CtxfsError::Provider` on failure — same behavior as today's HTTP errors.

The pool is a `DashMap<RepoKey, Arc<Mutex<CatFilePipeline>>>` in the daemon. Repos that haven't been read in >1h can have their process reaped.

### Subprocess vs `git2-rs`

**Spawn `git`.** `libgit2` (via `git2-rs`) does not implement partial-clone filters; it cannot perform `--filter=blob:none` fetches. The subprocess path gets auth credential handling for free (Git's own credential helper chain), handles transport protocols we don't need to re-implement, and adds zero extra static binary weight. The pipeline protocol for `cat-file --batch` is trivial.

---

## 3. Migration from the existing SHA-256-keyed blob cache

The current `BlobCache` stores blobs at `~/.ctxfs/cache/sha256/{ab}/{cdef...}` keyed by the Git SHA-1 stored in a `Digest` mislabeled as `HashAlgorithm::Sha256` (B3). The bytes are correct; the label is wrong.

Migration strategy: **additive, no forced invalidation.**

1. `GitObjectStore` introduces `~/.ctxfs/git-cache/` as a new directory tree. Existing `~/.ctxfs/cache/` is untouched.
2. `BlobCache` becomes an **L1 read-through layer** in front of `CatFilePipeline`. The lookup sequence is: L1 BlobCache hit → return. Miss → `CatFilePipeline.fetch(oid)` → write to BlobCache → return.
3. Existing cache entries remain valid and warm. Users who haven't yet migrated will see BlobCache hits on their previously-fetched blobs; only new blobs take the Git path.
4. B3 fix (relabeling `HashAlgorithm::Sha256` → `HashAlgorithm::GitSha1` in `ctxfs-core`) is a separate, breaking change. Defer to Phase 4 spec; note in migration documentation.

---

## 4. Answers to the 7 open questions

**Q1 (git2-rs vs spawn git):** Spawn git. `libgit2` can't do partial clone. Subprocess gets credential helpers for free. See §2.

**Q2 (per-blob hot path vs cat-file --batch):** Long-lived `cat-file --batch` per repo, serialized through a Mutex. Concurrent reads queue up; the pipeline is fast enough that latency is negligible for filesystem access patterns. Add `--buffer` for batch prefetch paths. No per-blob subprocess spawning.

**Q3 (migration story):** BlobCache as L1 in front of GitObjectStore. Additive. No invalidation. See §3.

**Q4 (resolution layer):** Unchanged. `ctxfs-provider-npm`, `ctxfs-provider-pypi`, `ctxfs-provider-crate` all resolve to GitHub coordinates; they hand off to `GitProvider::fetch_blob`. The `Provider` trait interface is unchanged.

**Q5 (GitHub archive tarball as prefetch shortcut):** **Yes, use it for the MVP.** `GET /repos/{o}/{r}/tarball/{ref}` = 1 REST call → full repo content. Extract tarball, hydrate BlobCache. No git infrastructure needed. Use for `ctxfs mount --prefetch`. For ongoing operation (incremental updates, large repos), the packfile path amortizes better: delta compression, only-new-objects transfer. Recommend: tarball for first-mount bootstrap, `git fetch` for incremental.

**Q6 (cooperative throttling):** Required. Track Git transport op counts in `GitObjectStore`. On 403/429 with `retry-after`: emit `CtxfsError::RateLimited{retry_after_secs}` and surface the message: "GitHub rate limited Git transport. `GITHUB_TOKEN` present: {yes/no}. Retry in Ns." This also fixes B4 for the REST path as a prerequisite.

**Q7 (cache integrity in service mode):** Document the limitation explicitly. SHA-1 is not collision-resistant for multi-tenant use. For v2 MVP (single-user daemon): acceptable. For service mode: require per-tenant isolated `git-cache/{user-id}/` paths plus content-hash verification on read (SHA-256 of fetched bytes stored alongside). Do not share Git object stores across tenants.

---

## 5. Where this approach is bad — where Option B wins

**Implementation cost is real.** Subprocess lifecycle management, long-lived pipe protocol, migration plumbing — this is 600–1000 lines of new infrastructure vs a patch to the existing HTTP client. For the soft-launch user base, Option B's complexity delta is genuinely lower.

**Tarball-only users don't need it.** If the use case is "preview a repo once," the tarball endpoint already gives Option A's key win (1 REST call for everything) without the subprocess stack. Option B can add tarball prefetch to the REST provider with much less work.

**LFS behavior change.** Git-native `git cat-file` on a partial clone will return LFS pointer files for LFS-tracked content — identical to REST behavior. But a `git checkout`-style smudge operation would replace pointers with real bytes if LFS is configured. The v2 spec must explicitly state which behavior is intended. This is not a blocker, but it's an explicit choice that must be documented (B6).

**Debugging is harder.** When something goes wrong with `git cat-file --batch`, the error surface is a subprocess exit code and a dead pipe. REST errors are HTTP status codes with JSON bodies. For users who are not git-internals-fluent, subprocess diagnostics are less approachable.

**B1 and B3 are not fixed by the architectural change.** Inline content for small files (B1) still needs explicit wiring regardless of which transport wins. The SHA-1/SHA-256 mislabeling (B3) is a Digest type problem orthogonal to transport choice.

---

## 6. Minimum viable shipment

**Goal:** Demonstrate that `grep -r` on a 1,000-file repo no longer depletes the REST budget.

**Scope:** Two changes, no subprocess stack.

1. **Tarball prefetch in the existing `GitHubProvider`** (`ctxfs-provider-git/src/github.rs`):  
   `ctxfs mount --prefetch` calls `GET /repos/{o}/{r}/tarball/{ref}`, streams the gzipped tarball, extracts all regular files into `BlobCache`. Cost: 1 REST call. After this, `grep -r` reads entirely from BlobCache.

2. **Fix B4 (secondary rate limit handling)** in `check_rate_limit`: check for `retry-after` header on any 403/429 response, not just when `x-ratelimit-remaining == 0`. Emit `CtxfsError::RateLimited` correctly.

This MVP validates the bound claim — "cold scan of N files = 1 REST call with `--prefetch`" — with ~200 lines of new code, no new subprocess infrastructure, and no migration risk. The full Git-native stack (partial clone, `cat-file --batch`, incremental fetch) is a follow-on milestone once the bound improvement is proven in production.

The architectural shift to `GitObjectStore` + `CatFilePipeline` is the right long-term design. The MVP tarball path is the on-ramp that earns it.
