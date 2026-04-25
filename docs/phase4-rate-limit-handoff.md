# Phase 4 — rate-limit gap handoff

**Status:** Brainstorming context, pre-spec. Captured at the end of the Phase 3 ship session (2026-04-25). Read this before starting the Phase 4 brainstorm so you don't re-derive what we already worked out.

**Origin:** During Phase 3 wrap, Derek raised a concern about ContextFS's GitHub API rate-limit exposure. Claude wrote an initial read; Codex challenged it via `counsel`. This file is the merged set of findings.

---

## The gap

ContextFS lazily fetches each blob via the GitHub REST API on first read (`/repos/{o}/{r}/git/blobs/{sha}`). Authenticated users get 5000 req/hr; unauthenticated 60. Cold-cache reads of many files in a single scan can burn the entire budget.

### Trigger surface (from Codex — broader than the initial read)

Anything that does cold content reads of many files trips this wire, not just `grep -r`:

- `grep -r`, `rg .`, IDE workspace indexers
- Language servers building symbol tables
- LLM agents sampling many files before answering
- `cp -R`, `head` / `wc -l` / `file` over a directory
- Spotlight, Quick Look, backup/sync tools

Pure metadata traversal (`find`, `ls`, `tree`, `getattr`, `readdir`) is fine — manifests are in memory after mount.

### Where it lands in the code

- `crates/ctxfs-provider-git/src/github.rs:144` — `resolve_ref` always calls `/commits/{ref}` before tree cache lookup
- `crates/ctxfs-provider-git/src/github.rs:155` — tree fetch via `/git/trees/{sha}?recursive=1` (one call)
- `crates/ctxfs-provider-git/src/github.rs:175` — blob fetch via `/git/blobs/{sha}` (one call **per file** on cold cache)
- `crates/ctxfs-vfs/src/state.rs:163,390` — any `read`, even one byte, loads the whole blob

---

## Bonus bugs Codex found while reading the code

These are real defects independent of the v2 redesign. Worth filing as separate GitHub issues for the Phase 4 backlog so they don't get buried under the larger architectural conversation.

### B1. Tiny-file inlining is documented but not implemented

`FileEntry.inline_content` is always `None` in current code (`crates/ctxfs-provider-git/src/github.rs:276`). The original Phase 1 design said files ≤4KB would inline during the tree walk to avoid per-blob fetches; that path was never wired up. **Result:** every file, however small, costs a blob API call on first read.

### B2. Truncated-tree fallback is documented but not implemented

GitHub's recursive tree endpoint caps at **100,000 entries or 7 MB** (https://docs.github.com/en/rest/git/trees). The code only `warn!`s and proceeds to build from the truncated response (`github.rs:163`). Repos beyond this silently produce partial mounts where some directories are missing — no fallback to per-directory walks despite Phase 2 risk-mitigation listing this as a known case.

### B3. Digest mislabeled `SHA256`; no content verification

GitHub blob IDs are **SHA-1**, but `Digest` stores them as `HashAlgorithm::Sha256`. The fetched bytes are never verified against any content hash. **Two problems:**

- The label is just wrong — confusing for future readers.
- No integrity check on cache contents — fine for a single-user local cache, but a **cache-poisoning hazard** if this ever runs as a multi-tenant service. Don't share by Git SHA-1 alone in service mode.

### B4. Secondary rate-limit handling is incomplete

Current code only flips to `RateLimited` when `x-ratelimit-remaining == 0`. GitHub's secondary rate limits return **403/429 with `retry-after` while `remaining` is still > 0** (https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api). We treat those as generic errors and cascade them as broken file reads instead of clean rate-limit signals.

### B5. LRU cache eviction breaks "second grep is free"

If a repo's blob payload exceeds `CTXFS_CACHE_MAX_BYTES` (default 512 MB), LRU eviction churns during the first scan. The "cached forever after" property only holds when the working set fits in the cache. Phase 4 should think about per-repo cache reservation, sticky pinning, or a "hot path" tier.

### B6. LFS payloads return pointer files, not real content

REST blob reads return Git LFS pointer files for LFS-tracked content, not the actual payload. We don't currently distinguish. Any v2 that goes Git-protocol-native (`git checkout` semantics) would *change* this — pointer files would smudge into real bytes — so the LFS behavior must be made explicit either way.

---

## V2 provider shape Codex sketched

Git-native, not REST-native. The framing change matters: REST API is the wrong primitive for what we're doing.

### Components

- **Per-repo object cache** under `~/.ctxfs/git-cache/`, keyed by host/repo/auth-identity
- **Fetch via Git transport** with partial-clone filters: `git clone --filter=blob:none` for blobless metadata
- **Manifests via local Git**: `git ls-tree -r -l -z <commit>` instead of REST tree JSON
- **Reads via `git cat-file --batch`** with explicit batch prefetch for bulk scans
- **`ctxfs mount --prefetch`**: one packfile fetch (or GitHub archive endpoint redirect) for snapshot eager materialization

### What this buys

- Cold full-content scans become **bounded by Git transport operations and bytes**, not one REST call per file
- One packfile carries arbitrary-sized blob sets in a single round trip — same primitive `git clone` uses
- Real content addressing via Git's SHA chain solves B3

### What this does *not* buy (Codex correction)

**Packfile transport is not rate-limit-free.** I was sloppy in my initial read. GitHub has:

- Documented unauthenticated HTTPS clone rate limits (https://github.blog/changelog/2025-05-08-updated-rate-limits-for-unauthenticated-requests/)
- A repository limit of **~15 Git read ops/sec/repo**, with throttling on large read volumes (https://docs.github.com/en/repositories/creating-and-managing-repositories/repository-limits)
- Secondary limits that fire even on Git transport
- No public doc giving an exact authenticated Git transport hourly quota

The honest claim is: **"Git transport avoids REST primary-quota exhaustion from one-request-per-blob."** It is **not** "unlimited."

### Naïve-partial-clone trap

`git clone --filter=blob:none` followed by demand-driven object fetching can be worse than the REST path in some cases — Git's partial-clone docs note that dynamic object fetching invokes `fetch-pack` once per missing object, with repeated auth overhead per call (https://git-scm.com/docs/partial-clone/2.24.0). The Phase 4 spec must mandate **batched** object fetching, not naïve lazy fetch.

---

## Phase 4 framing (Codex's tightened version)

Better than the initial framing of "≤10 API calls." Be explicit about the unit of bound:

> **"Make cold full-content scans bounded by Git transport operations and bytes, not by one REST call per file, with explicit throttling and batch prefetch semantics."**

Open questions for the brainstorm:

1. **Embed `git2-rs` / spawn `git`?** Spawning `git` (with `--filter=blob:none`, `cat-file --batch`) is the boring choice — easy to reason about, free auth handling. `git2-rs` is more complex but avoids subprocess overhead.
2. **Per-blob hot path vs `cat-file --batch`?** Need to think about how the FUSE/NFS read path interfaces with `git cat-file --batch`'s pipeline-based protocol — daemon spawns a long-lived `cat-file` process per repo? Pool? Per-request?
3. **Migration story** — existing `~/.ctxfs/cache/sha256/...` blob cache vs new `~/.ctxfs/git-cache/`. Two co-existing caches, or transition the SHA-256-keyed cache to a fronting layer over the Git object cache?
4. **Resolution layer** — `ctxfs-provider-common` resolvers (npm, PyPI, crate) → GitHub coordinates. Stays the same. Only the *blob fetching* primitive changes.
5. **GitHub archive endpoint** as a `--prefetch` shortcut — `https://api.github.com/repos/{o}/{r}/tarball/{ref}` returns a single tarball. One call gets the entire repo. Worth comparing vs packfile fetch for bootstrap performance.
6. **Cooperative throttling** — even with v2, secondary rate limits exist. Need a friendly error path: "remaining: 30/5000, refusing to fetch; install GITHUB_TOKEN or wait Nm".
7. **Cache integrity in service mode** — if this ever becomes multi-tenant, per-tenant blob storage + verified hashes are non-negotiable.

---

## Recommended next session

1. **Brainstorm session** (`/brainstorm` or just continue conversation) using this file as kickoff context. Frame the problem as "make `grep -r` cheap" and let it expand from there.
2. **Decide Git-native vs improved-REST**. Codex argues Git-native; need to weigh against complexity cost for the soft-launch user base.
3. **File B1–B6 as separate GitHub issues** so they don't get tangled with the v2 architecture decision. They're real bugs regardless of which v2 shape lands.
4. **Write a Phase 4 spec** at `docs/superpowers/specs/2026-XX-XX-phase-4-rate-limit-design.md` with the brainstormed approach.
5. **Plan + implement** via the usual flow.

---

## Source pointers

Code:
- `crates/ctxfs-provider-git/src/github.rs` — current REST-based provider
- `crates/ctxfs-vfs/src/state.rs` — read path
- `crates/ctxfs-cache/src/` — blob/tree/resolution cache tiers
- `crates/ctxfs-core/src/digest.rs` — the `Sha256`-mislabeled `Digest`

Codex's full review:
- `/tmp/counsel/20260425-065429-claude-to-codex-14b9b8/codex.md` (ephemeral; copy if you want a permanent record)

GitHub docs (verbatim):
- https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api — REST quotas + secondary limits
- https://github.blog/changelog/2025-05-08-updated-rate-limits-for-unauthenticated-requests/ — unauth HTTPS clone limits
- https://docs.github.com/en/repositories/creating-and-managing-repositories/repository-limits — 15 Git ops/sec/repo
- https://docs.github.com/en/rest/git/trees — 100k entries / 7 MB recursive tree cap
- https://docs.github.com/en/rest/repos/contents — archive endpoints

Git docs:
- https://git-scm.com/docs/http-protocol/2.19.0.html — smart HTTP packfile transport
- https://git-scm.com/docs/git-clone.html — `--filter=blob:none`
- https://git-scm.com/docs/partial-clone/2.24.0 — partial-clone caveats
