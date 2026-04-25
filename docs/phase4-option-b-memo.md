# Phase 4 Option B: REST + Targeted Fixes + Tarball Prefetch

**Author:** Option-B advocate  
**Date:** 2026-04-25  
**Status:** Brainstorm input — not a final decision

---

## 1. The Core Claim

REST + B1+B2+B4+B5 fixes + tarball `--prefetch` eliminates the rate-limit problem for the overwhelming majority of real-world ctxfs workloads at an implementation cost that is a fraction of a Git-native rewrite. This is not an argument that REST is architecturally ideal — it isn't — but that the pragmatic path is clearly cheaper than the visionary one at this stage of the project.

**The quantification:**

A typical cold scan of a 5,000-file repo today costs:

- 1 `resolve_ref` call (`/commits/{ref}`)
- 1 tree fetch (`/git/trees/{sha}?recursive=1`)
- 5,000 blob fetches (`/git/blobs/{sha}`) — one per file, however small

Total: ~5,002 REST calls. Against a 5,000 req/hr budget, a single cold `rg .` nearly exhausts the quota.

With B1 inlining (≤4 KB files fetched inline during the tree walk): most repositories skew heavily toward small files — READMEs, config files, `.rs`, `.ts`, `.py` sources, lock files. A conservative estimate is 65-75% of blobs in a typical project are under 4 KB. Inlining those: **≤1,750 blob calls** for the same repo, a **65% reduction with a single focused fix**.

With `--prefetch` via `/tarball/{ref}`: **1 call** gets every byte. The cache is warm. `rg .` is free for the lifetime of that cache entry. For cold-scan workloads, this reduces per-scan REST consumption by **~99.98%**.

These numbers are achievable with under 300 lines of Rust changes to two files. The Git-native rewrite requires a new subprocess model, per-repo object store, packfile protocol implementation (or `git2-rs` binding), auth-forwarding, and a cache migration path. That's weeks of work with new failure modes. The ratio of improvement per implementation hour is not close.

---

## 2. What Concretely Changes — File-Level Surgery

All changes are within `crates/ctxfs-provider-git/src/github.rs` and `crates/ctxfs-cache/src/lib.rs`. No new crates, no Provider trait changes, no VfsState changes (it already handles `inline_content: Some(...)` at `state.rs:391-405`).

**`github.rs`**

- **B1 fix** (`build_directories`, line ~276): When `entry.size <= 4096` in the tree walk, include the blob content inline using the `content` field GitHub already returns in the Contents API, or by fetching it eagerly in a batched subrequest during tree construction. Set `FileEntry.inline_content` instead of leaving it `None`. The VfsState fast-path at `state.rs:393` already handles this without modification.

- **B2 fix** (`fetch_tree`, lines 163-170): When `tree.truncated == true`, fall back to per-directory recursive tree fetches (`/git/trees/{dir_sha}`) instead of warning and continuing with a partial tree. Cap recursion depth. The `warn!` stays, but silent partial mounts become an error with a clear message.

- **B4 fix** (`check_rate_limit`, lines 205-237): GitHub secondary rate limits return 403 or 429 with a `retry-after` header while `x-ratelimit-remaining` is still positive. The current check only fires on `remaining == 0`. Fix: on any 403/429, check for `retry-after` header first; if present, emit `CtxfsError::RateLimited { retry_after_secs }` regardless of the primary-quota counter. This prevents secondary-rate-limit responses from cascading as broken file reads.

- **B3 fix** (`build_directories`, lines ~278-286): GitHub blob SHAs are Git SHA-1, not SHA-256. Rename `Digest::from_sha256_hex` calls here to a new `Digest::from_git_sha1` constructor that labels the algorithm correctly, eliminating the misleading `Sha256` annotation in cache keys. Verification on fetch: after decoding the blob response, compare `sha1(content)` against the stored SHA. Not collision-resistant in the cryptographic sense, but catches corruption and accidental misdelivery.

- **Tarball `--prefetch`**: New async method `prefetch_via_tarball(&self, source, dest_cache)` that fetches `/repos/{owner}/{repo}/tarball/{ref}`, streams the `.tar.gz`, and for each entry: (a) computes SHA-1 of the decompressed bytes, (b) calls `self.cache.put(&digest, &bytes)` to warm the blob cache. Called once from a new `ctxfs mount --prefetch` CLI flag before the filesystem is served. See §3 for design detail.

- **B6 optional**: In `fetch_blob_content`, if the returned `content` after base64-decode starts with `version https://git-lfs.github.com/spec/v1`, log a specific warning: "file is an LFS pointer; real content not available without LFS support." This surfaces LFS gracefully rather than silently serving pointer bytes.

**`crates/ctxfs-cache/src/lib.rs`**

- **B5 fix**: Add a `pin(&self, digests: &[Digest])` method and a pinned-entry set (e.g., `HashSet<String>` behind the `Mutex`). Pinned entries are excluded from LRU eviction. `fetch_snapshot` pins all blob digests for the mounted repo on successful warm-cache entry. `ctxfs umount` unpins. This provides the "second grep is free" guarantee even when multiple repos are mounted and the global LRU would otherwise churn the first repo's blobs out.

---

## 3. Tarball Prefetch Design

**Invocation:** Explicit opt-in only. `ctxfs mount github:owner/repo@main --prefetch`. Not automatic; auto-prefetch on every mount would surprise users with large upfront fetches. A future `ctxfs cache warm` subcommand could expose this independently of mount.

**What it does:** Sends one authenticated GET to `https://api.github.com/repos/{owner}/{repo}/tarball/{ref}`. GitHub returns a redirect to an S3-signed URL; `reqwest` follows it. Streams the `application/x-gzip` body, decompresses with `flate2`, and walks the `tar` archive with the `tar` crate. For each regular file entry, computes its Git SHA-1 (`sha1("blob {size}\0{content}")`) and writes to `BlobCache`.

**Failure modes:** If the tarball fetch fails mid-stream, the partially-warmed cache is left intact (no rollback needed — the blobs that made it in are valid). Mount proceeds normally; files not yet in cache are fetched lazily by the existing blob path. The user gets a warning: "prefetch incomplete after N bytes; falling back to on-demand fetching." A future `--prefetch-require` flag can make this fatal.

**Caveats:** The tarball endpoint has undocumented secondary rate limits. For very large repos or automated pipelines, it may 429. B4's improved rate-limit handling ensures this surfaces cleanly. The tarball also lacks LFS content — if the repo uses LFS, `--prefetch` warms only the pointer files. This should be documented clearly.

---

## 4. The Soft-Launch Argument

v0.1.0 shipped four weeks ago. The user base is small and self-selected — developers who found ContextFS via its GitHub release, who have a PAT configured, and who are mostly browsing moderate-sized repos. They are not running 50,000-file monorepos through LLM agents at scale.

For this population, B1+B4 alone likely resolves 80% of the rate-limit complaints they'll encounter. B3 prevents the confusing SHA mislabel from proliferating further into cache files on disk. B2 prevents silent partial mounts that produce mysterious "file not found" errors in large repos.

These are not trade-offs against a future architectural ideal — they are unambiguous correctness improvements that should ship regardless of which v2 direction is chosen. Filing them as separate issues (as the handoff recommends) and resolving them now buys time to make the Option A vs B decision carefully without user-visible degradation in the interim.

---

## 5. Where Option A Genuinely Wins

Option B is pragmatic, not visionary. Be honest about the ceiling:

- **Extreme cold scans (50k+ files, mostly large):** Even with B1, a repository like the Linux kernel has tens of thousands of blobs over 4 KB. Inlining helps but doesn't eliminate the per-blob quota drain. The Git packfile transport fetches arbitrary sets of blobs in a single round trip; REST fundamentally cannot match this for bulk content.

- **Multi-tenant service mode:** If ctxfs ever runs as a shared service where multiple users' PATs pool against a common quota, the per-PAT limit becomes a hard architectural constraint. Git transport, with its transport-level (not per-object) quota, is the right primitive there. B3's SHA-1 fix improves integrity but doesn't eliminate the need for tenant-isolated blob stores.

- **Offline / disconnected use:** A Git object store (blobless partial clone) can satisfy reads from local objects without network. REST is inherently online-only.

- **Architectural coherence:** Codex's framing — "REST is the wrong primitive for a filesystem" — has real merit. Fetching JSON-wrapped base64-encoded blobs individually is a design smell. The 33% base64 overhead alone adds up. A Git-native provider is the more honest long-term shape. Option B is buying time, not solving the problem permanently.

---

## 6. Sunset Clause

Option B stops being adequate when any of the following conditions arrive:

1. **A single user's typical workflow exceeds ~3,000 REST calls per session** after B1 inlining — indicating workloads that are fundamentally blob-heavy (AI agents, IDEs with deep indexing, `cp -R` of large repos).

2. **ctxfs adds a server/sharing mode** where multiple users share a single daemon. PAT-per-user quotas require per-tenant blob stores; REST becomes untenable and the Git transport's repository-level (not per-user) semantics become necessary.

3. **Tarball prefetch secondary limits become a bottleneck** — if GitHub starts rate-limiting `/tarball/{ref}` aggressively, the escape hatch for bulk prefetch closes and packfile transport becomes the only sane option.

4. **The user base grows and bug reports shift from "rate limited on large repos"** (B1 addressable) **to "rate limited on normal repos"** (indicates the quota is being shared with too many other integrations, and per-object REST fundamentally cannot compete with batched packfile).

At that point, Option A should be prioritized. The Option B work is not wasted — B3, B4, B5 are useful in any provider, and the tarball prefetch logic could be reused as a bootstrap for a Git object cache. But the `GitHubProvider` REST core would need to be retired or relegated to a fallback.
