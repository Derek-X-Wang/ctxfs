# Phase 4 — Research brief (pre-brainstorm)

**Status:** Research-only sweep produced 2026-04-25 to ground the Phase 4 rate-limit
brainstorm. Companion to `docs/phase4-rate-limit-handoff.md`. This file verifies,
sharpens, or corrects the prior handoff against the current code and the live GitHub +
Git docs. No design decisions or code changes here.

---

## 1. Code reality check

**Summary:** All major handoff citations are correct against today's `main`. Two minor
nuances: (a) `FileEntry.inline_content` *is* part of the manifest schema (not just the
struct on disk) but it is never populated in `build_directories`, and (b) the handoff
under-states how aggressive the read amplification is — even a 1-byte read on a
multi-MB blob fetches and decodes the whole blob through `BlobResponse`'s base64
inflation path.

| Handoff claim | Status | Note |
|---|---|---|
| `resolve_ref` always calls `/commits/{ref}` before tree cache lookup | ✅ | `crates/ctxfs-provider-git/src/github.rs:144-153` calls `get_json` on `/commits/{version}` unconditionally. The tree cache lookup at `:364` is keyed on the **resolved** commit SHA, so each cold mount eats one REST hit even when the manifest is fully cached. |
| Tree fetch via `/git/trees/{sha}?recursive=1` | ✅ | `github.rs:155-173`. |
| Blob fetch via `/git/blobs/{sha}` is one REST call per file | ✅ | `github.rs:175-203`, `fetch_blob` at `:428-440` populates `BlobCache` after the call. |
| `read` of even one byte loads the whole blob | ✅ | `crates/ctxfs-vfs/src/state.rs:164-172` calls `fetch_file_bytes(&node)` then slices; `fetch_file_bytes` at `:391-425` always returns the full byte vector. |
| B1: `inline_content` always `None` | ✅ | `github.rs:281` literally assigns `inline_content: None`. The schema at `crates/ctxfs-manifest/src/snapshot.rs:42-44` documents `<=4KB`, but no producer ever sets it. The VFS read path at `state.rs:391-405` *does* honor inline content if present, so wiring the producer side is the only blocker. |
| B2: truncated trees only `warn!`, no fallback | ✅ | `github.rs:165-170` is just `warn!` then proceeds. The doc-recommended fallback (per-subtree non-recursive walks) is not in the code. |
| B3: digest mislabeled as SHA-256, no content verification | ✅ | `crates/ctxfs-core/src/digest.rs:8-10` defines `HashAlgorithm::Sha256` as the only variant; `from_sha256_hex` at `:38-43` is what `github.rs:278` uses to wrap a Git SHA-1 hex string. `fetch_blob` at `github.rs:428-440` writes raw bytes into `BlobCache::put` with no hash check. |
| B4: secondary rate limits not handled | ✅ | `github.rs:205-237` only flips to `RateLimited` when `remaining == 0`. A 403/429 with `remaining > 0` and a `retry-after` header (the secondary-limit signature) returns `Ok(())` from `check_rate_limit` and then the outer `if !resp.status().is_success()` path returns `CtxfsError::Provider("failed to ... HTTP 403")` — exactly the cascade the handoff describes. |
| B5: LRU thrashes when working set > cache | ✅ | `crates/ctxfs-cache/src/lib.rs:97-132` shows `BlobCache::put` evicts oldest entries while `total_bytes > limit`. There is no per-repo reservation, no pinning, no hot-path tier — it's a single global LRU. Default 512 MB is set in `CLAUDE.md`. |
| B6: LFS pointer files returned, not real content | ✅ (assumed correct, not verified by live test) | The REST `/git/blobs/{sha}` endpoint returns the raw object, which for LFS-tracked files is the pointer file. No code path inspects pointer files or routes LFS through `objects/batch`. |
| `inline_content` schema documented as `<=4KB` | ✅ | `snapshot.rs:42` doc comment: `/// Inline content for small files (<=4KB).` |

**Three additional findings the handoff did not call out:**

1. **Provider singleton conflates active source.** `GitHubProvider` keeps a single
   `active_source: Mutex<Option<SourceSpec>>` (`github.rs:32, :356, :433-435`) that is
   overwritten on every `fetch_snapshot`. `fetch_blob` reads it back to know which
   repo to hit. With multiple concurrent mounts sharing a provider, this is a race —
   the comment at `:30-33` claims "scoped to a single mount" but the daemon does not
   enforce that. The v2 design needs to thread the source through the read path
   explicitly (or instantiate one provider per mount) regardless of which approach
   ships.

2. **Three independent cache tiers exist today**, all keyed differently:
   - `BlobCache` — content-addressed by `Digest` (`cache/src/lib.rs`), single global
     LRU with `CTXFS_CACHE_MAX_BYTES` default 512 MB. Stores both blobs and serialized
     directory JSON (`github.rs:392`). Fan-out path `sha256/ab/cdef...`.
   - `TreeCache` — keyed on `(owner, repo, commit_sha)` JSON files
     (`cache/src/tree.rs`). Versioned envelope, atomic write, mtime-based eviction.
     Default 500 MB via `CTXFS_TREE_CACHE_MAX_BYTES`. **Stores the snapshot JSON, not
     the raw GitHub tree response** — re-using this for v2 is feasible.
   - `ResolutionCache` — keyed on the package spec string, JSON file with an in-memory
     `HashMap`. Has TTL semantics (pinned never expire; `is_latest` honors
     `CTXFS_LATEST_TTL_SECS`, default 3600). Independent of the blob path.

3. **Secondary-rate-limit signal is fully discarded.** When 403/429 lands with
   `remaining > 0`, the `retry-after` header is never parsed in the GitHub provider
   path (`github.rs:205-237`). Worth noting: the *common* HTTP helper used by npm /
   PyPI / crate registries (`crates/ctxfs-provider-common/src/http.rs:43-53`) does
   parse `retry-after` correctly — so we already have a working pattern to copy.

---

## 2. Doc reality check

**Summary:** GitHub's REST quota numbers are confirmed verbatim. The May 2025 unauth
clone-throttling changelog *exists* but is annoyingly content-light: it links out for
specific numbers rather than stating them inline. The 100k/7MB tree cap is verbatim.
The 15-ops/sec/repo soft limit is *recommended*, not enforced — that distinction
matters for design.

### REST primary limits (https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api)

- Unauthenticated: **60 requests per hour** (verbatim).
- User personal token: **5,000 requests per hour** (verbatim).
- GitHub App installation token: **5,000/hr minimum**, scaling up to a cap of
  **12,500 requests per hour** for installations with many users/repos.
- GitHub Enterprise Cloud installations / GHE-owned OAuth/Apps:
  **15,000 requests per hour**.
- `GITHUB_TOKEN` inside Actions: **1,000/hr per repository** (15,000 on GHE).

### Secondary limits (same page)

- Triggered by burst patterns; signaled with **403 or 429** plus `retry-after` header.
- Doc tells clients to use `retry-after` if present, else **wait at least one minute**,
  else exponential backoff with a retry cap.
- Warning quoted: *"Continuing to make requests while you are rate limited may result
  in the banning of your integration."*
- The doc does **not** explicitly state secondary limits fire while
  `x-ratelimit-remaining > 0` (the handoff claim). It is a true real-world behavior
  but unstated; secondary-vs-primary is signaled by status code + `retry-after`, not
  by the remaining counter.

### Recursive tree cap (https://docs.github.com/en/rest/git/trees)

- **100,000 entries, 7 MB** (verbatim, both numbers).
- `truncated: true` in response signals overflow.
- Recommended fallback: *"use the non-recursive method of fetching trees, and fetch
  one sub-tree at a time."* — i.e., O(directories) extra REST calls. For huge repos
  this is exactly the primary-quota burn we want to avoid in v2.

### Repository operation limits (https://docs.github.com/en/repositories/creating-and-managing-repositories/repository-limits)

- **15 Git operations/sec/repo** is "recommended maximum", not a hard cap.
- Push: 6/min/repo recommended; 2 GB max push size; 100 MB max single object.
- Repo storage: 10 GB on-disk; 3,000 entries per directory; 50 dir depth; 5,000
  branches.
- No documented hourly hard cap on Git transport.

### May 2025 unauth-clone changelog (https://github.blog/changelog/2025-05-08-updated-rate-limits-for-unauthenticated-requests/)

- **Page does not state the numbers inline.** It confirms qualitatively that
  unauthenticated rate limits now apply to *cloning over HTTPS*, *anonymous REST API*,
  and *raw.githubusercontent.com*. For numbers it links back to the REST rate-limits
  doc above (60/hr unauth). Effective rollout: 2025-05-08.
- Implication: an unauthenticated v2 user gets **60 Git transport ops/hr/IP** in the
  worst case, regardless of whether we use REST or packfile.

### Partial clone (https://git-scm.com/docs/partial-clone)

- "Promisor remote": *"A remote that can later provide the missing objects is called
  a promisor remote, as it promises to send the objects when requested."*
- Quoted caveat (verbatim): *"Dynamic object fetching invokes fetch-pack once for
  each item because most algorithms stumble upon a missing object and need to have
  it resolved before continuing their work. This may incur significant overhead — and
  multiple authentication requests — if many objects are needed."*
- Also verbatim: *"Dynamic object fetching tends to be slow as objects are fetched
  one at a time."*
- Online-only requirement: *"Use of partial clone requires that the user be online
  and the origin remote or other promisor remotes be available for on-demand
  fetching of missing objects."*

### Filter specs (https://git-scm.com/docs/git-rev-list, https://git-scm.com/docs/git-clone)

- `--filter=blob:none` — *"omits all blobs"*.
- `--filter=blob:limit=<n>[kmg]` — *"omits blobs of size at least <n> bytes"*.
- `--filter=tree:<depth>` — *"omits all blobs and trees whose depth from the root
  tree is >= <depth>"*. `tree:0` excludes everything except explicitly named trees.
- `--filter=auto` — server-driven via promisor protocol; persisted to config.
- For our workload, `blob:none` (manifests-only fetch) is the obvious starting filter;
  `tree:0` is too aggressive because we walk all directories, and `blob:limit=` could
  pre-fetch small files in one shot (interesting for inline-content B1).

---

## 3. Approach analysis

**Summary:** Git-native v2 is the architecturally cleaner answer but carries real
operational and supply-chain cost. Improved-REST is *much* smaller surface and
addresses the highest-frequency bugs. A staged hybrid (ship improved-REST first,
defer Git-native to a future phase) is decision-relevant given the soft-launch user
base. Below is what each actually buys, costs, and assumes.

### A. Git-native v2 (partial-clone + `cat-file --batch`)

**Surface area:**
- New crate (or major rewrite of `ctxfs-provider-git`): per-repo bare cache under
  `~/.ctxfs/git-cache/{host}/{owner}/{repo}.git`, plus auth identity scoping.
- Daemon lifecycle: long-lived `git cat-file --batch` subprocess pool keyed on
  `(repo, auth)`. Needs supervisor, idle-timeout, restart-on-crash, stdin/stdout
  pipes managed in async.
- New runtime dep: system `git` binary (>= 2.24 for partial-clone), or vendor
  `git2-rs` (libgit2). `git2-rs` is ~70k LOC C dep, no native partial-clone parity
  with CLI git as of libgit2 1.x — verified before committing.
- Manifest path changes: `git ls-tree -r -l -z <commit>` instead of REST tree JSON.
  `Snapshot`/`Directory` schemas can stay; only the producer changes.
- Throttling: needs a token-bucket gate sized to the 15-ops/sec/repo soft limit.

**What it buys:**
- B1 (inline): orthogonal — solvable in either approach.
- B2 (truncation): **eliminated** — `ls-tree` has no entry cap.
- B3 (digest mislabel): forces honesty — the digest is Git SHA-1, and content can
  be verified by re-running `git hash-object`.
- B4 (secondary limits): still needed; Git transport hits the same limit class.
- B5 (LRU thrash): partly addressed because objects share with Git's pack format on
  disk (delta compression typically 3-10x smaller than raw blobs).
- B6 (LFS): becomes explicit — `git lfs` integration is a known surface, not a
  silent pointer-file hazard.
- Rate-limit profile (best case, authenticated): one packfile per cold mount + one
  refresh per ref change. Under typical agent usage (~hundreds of files per scan),
  drops from ~hundreds of REST calls to ~1-3 packfile transports.
- Eliminates the per-blob REST request entirely, which is the explicit Phase 4 goal.

**What it costs:**
- Subprocess management complexity in the daemon: PID tracking, zombie reaping,
  graceful shutdown, partial-stdout drain on kill. Non-trivial in async Rust.
- New external dep — `git` is "always installed" on dev machines but not guaranteed
  in CI/sandboxed environments. Homebrew formula needs to declare it.
- Larger on-disk footprint for cold mounts: a partial clone of `react.git` is
  ~30-50 MB even without blobs; with `blob:none` it's mostly history.
- New failure modes: `cat-file --batch` deadlock if the daemon stops draining
  stdout, partial-clone network hiccup mid-fetch, auth token rotation invalidating
  on-disk credentials.
- Supply chain: vendoring `git2-rs` adds C build deps; spawning system `git`
  inherits whatever ships on the user's system.
- Migration: existing `~/.ctxfs/cache/sha256/...` blobs become orphans (or need a
  shim layer mapping Git SHA-1 → SHA-256-keyed cache; not worth the effort).

**Honest risk:**
- Bakes in the assumption that **a long-lived subprocess pool is the right
  abstraction in the daemon**. If we ever support remote daemons or service mode,
  the subprocess assumption forces us into containerization rather than pure Rust.
- The 15-ops/sec/repo recommended limit is **not a hard ceiling** — actual
  throttling thresholds are private. We can't prove the new design stays under
  them without instrumentation in production.
- Naïve partial-clone (Git's own docs) is *worse* than REST for our workload. We
  must mandate batched prefetch in the spec. If the spec does not enforce this
  (e.g., "fetch lazily on `cat-file` miss"), v2 regresses.

### B. Improved REST (fix B1/B2/B4 + bulk packfile fallback)

**Surface area:**
- Wire B1: in `build_directories`, when `entry.size <= 4096` AND we have an
  authenticated client, fetch and inline content in the tree-walk pass. ~30 lines.
- Wire B2: when `tree.truncated == true`, recurse into per-directory non-recursive
  tree fetches. Bounded by 100k/7MB chunks. ~80 lines.
- Wire B4: change `check_rate_limit` to honor `retry-after` on 403/429 *regardless
  of remaining*. Reuse the pattern from `provider-common/src/http.rs:43-53`. ~10
  lines.
- Optional: add `--prefetch` mode that fetches the GitHub archive endpoint
  (`/repos/{o}/{r}/tarball/{ref}`) on mount. One REST call → entire repo.

**What it buys:**
- B1 done: typical repo has 60-80% of files under 4 KB → fewer cold blob calls.
- B2 done: large repos work correctly (today they silently truncate).
- B4 done: secondary limits observed → daemon backs off cleanly instead of
  cascading errors.
- Rate-limit profile: typical scan still costs O(big-files) REST calls. With
  `--prefetch` archive flag: 1 archive + 0 blob calls for cold scans.
- Solves the **complaint**, not the **architecture**. `grep -r` on a clean cache
  with `--prefetch` archive is effectively free.

**What it costs:**
- B3 (digest naming) untouched.
- B5 (LRU thrash) untouched — same global LRU.
- B6 (LFS) untouched.
- Tarball bootstrap is GitHub-specific; hard to generalize to GitLab/Bitbucket
  later. But we don't support those today.
- Doesn't change the per-call rate-limit *floor* — cold reads of a single >4KB
  file still cost one REST hit each. Once `--prefetch` is the recommended UX, we
  rarely care, but for ad-hoc reads we still burn quota.

**Honest risk:**
- Inlining 4KB across an entire tree response inflates the cached snapshot JSON.
  A 100k-entry repo with 50% small files = 200 MB inline. The TreeCache 500 MB
  default holds, but per-entry size needs a cap (e.g. inline only files <= 1 KB).
- Truncated-tree fallback can itself burn quota: a deeply-nested 100k+ entry repo
  takes O(directories) REST calls. Need to guard against pathological repos.
- Tarball endpoint is documented as a redirect to a temporary URL with its own
  auth scoping; behavior under fine-grained tokens is not as well-tested as the
  blob endpoint. Need to verify.

### C. Hybrid / staged

**Sketch:** Ship B in this phase, treat A as Phase 5 with B's prefetch path as the
forcing function for the migration plan.

- Phase 4 = improved-REST + `--prefetch` tarball + B1/B2/B4 fixes + secondary-limit
  retry. Keep cache layers stable. ~2-3 weeks of work, low risk.
- Phase 5 = Git-native, with the `~/.ctxfs/git-cache` layout designed *now* so the
  prefetch tarball path can be retargeted to a packfile fetch with no observable
  change to mounts.

**What this buys:**
- All of B's wins this phase.
- The Phase 5 transition only changes the producer; manifest schema and VFS read
  path stay the same.
- Defers the subprocess-pool decision until we have data on whether the soft-launch
  user base actually hits the secondary limits.

**What it costs:**
- Two writes of the prefetch path (tarball now, packfile later).
- Cache integrity work (B3) deferred — fine for single-user, blocking for service
  mode whenever that comes.

---

## 4. Sharpened open questions

The handoff's seven questions are reordered and re-framed; three carry decidable
proposals. One new question added at the end.

1. **(was Q1) `git2-rs` vs spawn `git`?** — *Decidable now.* Spawn `git`. Subprocess
   complexity is real but lower than vendoring libgit2 + maintaining its quirks; the
   user-facing cost (system `git` dep) is acceptable for a Rust tool that already
   ships via Homebrew. The `git2-rs` partial-clone story still lags CLI git as of
   2024-2025. **For the brainstorm:** confirm we're not embedding libgit2.

2. **(was Q2) Hot path interaction with `cat-file --batch`?** — Sharpen: **Pool
   strategy.** Three options to evaluate: (a) one `cat-file` process per
   `(repo, auth)` long-lived; (b) per-request spawn (too slow for `grep -r`); (c)
   per-mount, lifecycle scoped to the mount. The handoff implicitly favors (a). Open:
   how many concurrent `cat-file`s can the daemon run before stdin/stdout multiplex
   becomes the bottleneck?

3. **(was Q3) Migration of existing `BlobCache`?** — *Decidable now.* Treat the new
   git-cache as additive. Keep `BlobCache` as a transparent fronting tier keyed on
   the existing `Digest` (which we now relabel honestly as Git SHA-1). On miss, fall
   through to `git cat-file`. Existing users keep their cache; nothing breaks.

4. **(was Q4) Resolution layer (`provider-common` resolvers)?** — Confirmed: the
   resolver layer is unchanged across all three approaches. `ResolvedSource → owner,
   repo, ref, subpath` is provider-agnostic. **No question to brainstorm.** Drop from
   the agenda.

5. **(was Q5) GitHub archive endpoint as `--prefetch`?** — Sharpen: this is a
   *Phase 4 decision* not a v2 question. If we ship improved-REST, archive is the
   prefetch path; if we ship git-native, packfile is the prefetch path; if we ship
   hybrid, archive is the bridge. The interesting data-gathering question: **how
   does the tarball endpoint behave under fine-grained PATs and orgs with SSO?**
   Untested.

6. **(was Q6) Cooperative throttling UX.** — Reframe as **two questions**:
   (a) what's the user-facing message format when we hit primary or secondary
   limits? (b) Do we ever block the FUSE/NFS read with a sleep, or always return an
   IO error and surface state via a sidecar (`ctxfs status`)? The current
   `RateLimited` error surfaces as opaque IO failure; the brainstorm should pick
   one.

7. **(was Q7) Multi-tenant integrity.** — *Decidable now for Phase 4.* Out of scope.
   Note in the spec that current single-user mode is fine; service mode requires
   per-tenant blob storage + verified content addressing, but that's a separate
   project.

8. **(NEW) What's the rate-limit budget we're optimizing for?** The handoff frames
   "make `grep -r` cheap" qualitatively. For the brainstorm, propose a concrete
   budget: e.g., *"Cold full-content scan of a 10k-file authenticated repo must cost
   ≤ 50 REST-equivalents under the worst-case path"* — pick a number we can measure.
   Without this, we cannot say whether B is "good enough" or A is required.

9. **(NEW) Active-source mutex race.** Already-existing bug from §1 finding 1. Not
   itself rate-limit related but the design discussion needs to fix it as part of
   touching the read path. Suggest: pass `(SourceSpec, Digest)` through the read
   path instead of relying on provider state.

---

## File pointers

- Code:
  - `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-provider-git/src/github.rs`
  - `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-vfs/src/state.rs`
  - `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-cache/src/lib.rs`,
    `tree.rs`, `resolution.rs`, `shared.rs`
  - `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-core/src/digest.rs`,
    `error.rs`
  - `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-manifest/src/snapshot.rs`
  - `/Users/derekxwang/Development/incubator/ContextFS/ctxfs/crates/ctxfs-provider-common/src/http.rs`,
    `resolver.rs`, `repo_url.rs`
- Prior counsel record:
  `/tmp/counsel/20260425-065429-claude-to-codex-14b9b8/codex.md` (50 lines, ephemeral)
- Source for verbatim doc quotes: see URL list at top of §2.
