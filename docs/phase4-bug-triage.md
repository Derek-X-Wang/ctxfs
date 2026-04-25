# Phase 4 Bug Triage — ctxfs-provider-git

**Author:** bug-triage agent  
**Date:** 2026-04-25  
**Status:** Read-only investigation; no code changed.  
**Scope:** B1–B6 from the Phase 4 handoff, plus additional defects found via code inspection and git history.

---

## Method

Primary inputs:

1. `docs/phase4-rate-limit-handoff.md` (B1–B6 definitions)
2. `crates/ctxfs-provider-git/src/github.rs` (657 lines, full read)
3. `crates/ctxfs-vfs/src/state.rs` (full read)
4. `crates/ctxfs-core/src/digest.rs` (full read)
5. `crates/ctxfs-manifest/src/snapshot.rs` (full read)
6. `crates/ctxfs-cache/src/lib.rs` (eviction mechanics)
7. `crates/ctxfs-provider-common/src/http.rs` (rate-limit reference implementation)
8. `crates/ctxfs-daemon/src/daemon.rs` (provider lifetime / active_source scope)
9. `git log --grep='active_source'` → commit 869ca44 (NFS backend PR)
10. grep for `TODO`, `FIXME`, `XXX` across all crates (two results, both unrelated to provider-git)

---

## Key: architecture-coupling definitions

| Term | Meaning |
|------|---------|
| **Option A** | Git-native v2: `git clone --filter=blob:none`, `git cat-file --batch` |
| **Option B** | Improved REST + tarball `--prefetch` (current REST path, evolved) |

For each bug: does the correct fix look different under Option A vs Option B?  
- **Yes → fold into v2** (or both, if a tracking issue is useful)  
- **No → file now** (fix is architecture-neutral)

---

## Recommendations at a glance

| Bug | Summary | Effort | Recommendation |
|-----|---------|--------|----------------|
| B1 | Tiny-file inlining never wired up | S | **File now** |
| B2 | Truncated-tree fallback never wired up | M | **Both** |
| B3 | Digest mislabeled SHA-256 (it's SHA-1) | M | **Both** |
| B4 | Secondary rate-limit handling incomplete | S | **File now** |
| B5 | LRU eviction breaks "second grep is free" | L | **Both** |
| B6 | LFS payloads return pointer files | M | **Both** |
| B7 ★ | Symlink targets always empty string | S | **File now** |
| B8 ★ | `active_source` design hazard (not a current race) | M | **Fold into v2** |

★ = found during this triage, not in the original B1–B6 list.

---

## B1 — Tiny-file inlining never wired up

### One-line summary
`FileEntry.inline_content` is always `None` in the provider; the ≤4 KB inlining path documented in Phase 1 was never implemented.

### Where it lives
- `crates/ctxfs-provider-git/src/github.rs:281` — `inline_content: None, // filled lazily`
- `crates/ctxfs-vfs/src/state.rs:30` — comment: "Small files (<=4 KB) are inlined in the manifest and don't require a separate fetch."
- `crates/ctxfs-vfs/src/state.rs:391–405` — VfsState.fetch_file_bytes checks `inline_content: Some(content)` and short-circuits if present; the path is correct and fully implemented.
- `crates/ctxfs-manifest/src/snapshot.rs:42` — `FileEntry.inline_content: Option<Vec<u8>>` with `#[serde(default, skip_serializing_if = "Option::is_none")]`

### User-visible impact
Every file, regardless of size, costs one blob API call on first read. A repo with 200 config files of 10–100 bytes each burns 200 rate-limit credits. Users with unauthenticated access (60/hr) can exhaust their quota reading a modest project.

### Effort estimate (fix on current REST provider)
**S** — During `build_directories`, check `entry.size <= 4096`; if true, queue the blob SHA for a batch fetch after the tree walk and populate `inline_content`. VfsState already handles it correctly. No manifest schema changes needed.

### Architecture coupling
The mechanism (`inline_content` field) is architecture-neutral — both Option A and Option B would populate the same field, just via different fetching primitives (REST batch vs `git cat-file --batch`). Fix shape is essentially identical under both options.

### **Recommendation: File now**

**Suggested issue:**

> **Title:** `ctxfs-provider-git: inline small files during tree walk to avoid per-blob API calls`
>
> **Body:**
>
> `FileEntry.inline_content` was designed for files ≤ 4 KB to be embedded directly in the tree manifest, avoiding a separate blob API call per file on first read. The field is fully supported by `ctxfs-vfs` (`VfsState::fetch_file_bytes` short-circuits when `inline_content` is `Some`), but `build_directories` in `github.rs:281` always sets `inline_content: None`.
>
> **Current behavior:** Every file costs one `GET /repos/{o}/{r}/git/blobs/{sha}` on cold read, regardless of size.
>
> **Expected behavior:** Files whose tree-entry size ≤ 4096 bytes have their content fetched during the tree walk and embedded in the manifest. This eliminates ~50–80% of blob API calls in typical projects where most files are small (config, markdown, Rust source files).
>
> **Fix sketch:** In `build_directories` (or its caller `fetch_snapshot`), after the tree walk, collect all blob SHAs with `size <= 4096`. Fetch their content in parallel (up to some concurrency limit). Populate `FileEntry.inline_content` before serializing the manifest.
>
> **Files:** `crates/ctxfs-provider-git/src/github.rs` (build_directories, fetch_snapshot)
>
> **Tests:** Add a unit test to `github.rs` asserting that a FileEntry with size ≤ 4096 has `inline_content: Some(...)` after build_directories when content is fetched.

---

## B2 — Truncated-tree fallback never wired up

### One-line summary
When GitHub's recursive tree endpoint returns `truncated: true` (repos > 100,000 entries or > 7 MB), the code logs a warning and continues building from the partial response — resulting in silently incomplete mounts.

### Where it lives
- `crates/ctxfs-provider-git/src/github.rs:163–170` — `fetch_tree` logs `warn!` on truncation but returns the truncated `TreeResponse` without error.
- GitHub REST docs: `/git/trees?recursive=1` caps at 100,000 entries or 7 MB; see https://docs.github.com/en/rest/git/trees.

### User-visible impact
Mounts of large repos (e.g., Linux kernel, CPython) silently produce partial directory trees. Files deep in the repo are missing from `ls` output with no error. Users have no indication the mount is incomplete.

### Effort estimate (fix on current REST provider)
**M** — The fallback is a per-directory recursive traversal: on `truncated: true`, walk each directory entry's SHA with non-recursive tree fetches. This is N additional API calls where N is the number of subdirectories. Implementation is moderate complexity but no architectural changes.

### Architecture coupling
**Yes — fix path diverges significantly.**

- **Option A (Git-native):** `git ls-tree -r` (or `git clone --filter=blob:none`) has no entry cap; the Git packfile protocol handles arbitrarily large trees in a single operation. This bug disappears entirely.
- **Option B (improved REST):** The fix requires per-directory recursive fallback, which adds complexity and API call volume. A tarball `--prefetch` shortcut also sidesteps this limitation.

The truncated-tree scenario is one of the strongest arguments for Option A (or Option B's tarball path), so its resolution should inform — and be documented in — the Phase 4 architecture decision.

### **Recommendation: Both**
File a tracking issue now (user-facing silent data loss), but mark it blocked on Phase 4 architecture decision. Document in the issue that Option A and Option B have fundamentally different resolution paths.

**Suggested issue title:** `ctxfs-provider-git: truncated GitHub tree responses cause silent partial mounts for large repos`

---

## B3 — Digest mislabeled SHA-256 (it's SHA-1); no content verification

### One-line summary
`Digest::from_sha256_hex(entry.sha)` stores GitHub's SHA-1 blob IDs with `algorithm: HashAlgorithm::Sha256` — a factually wrong label. Fetched bytes are never verified against any hash.

### Where it lives
- `crates/ctxfs-provider-git/src/github.rs:277–278` — `Digest::from_sha256_hex(&entry.sha)` — entry.sha is a 40-char SHA-1
- `crates/ctxfs-core/src/digest.rs:8–10` — `HashAlgorithm` has only one variant: `Sha256`
- `crates/ctxfs-core/src/digest.rs:38–43` — `from_sha256_hex` is the only constructor from an existing hex string; naming implies SHA-256

#### Concrete consequence
The blob cache stores SHA-1-addressed content under the path prefix `sha256/`. Digests serialize as `{"algorithm":"sha256","hex":"<40-char-sha1>"}`. Any consumer trying to verify integrity (e.g., re-fetching and comparing) would compute a SHA-256 hash of the content and compare it against a SHA-1 value — they will never match.

#### Verification gap
The blob fetch at `github.rs:175–201` returns raw bytes decoded from base64. Nothing in the provider, VfsState, or BlobCache verifies that the content hashes to the stored digest. This is acceptable for a single-user local cache (trust GitHub's transport), but a **cache-poisoning hazard** if the daemon is ever exposed as a multi-tenant service.

### User-visible impact
Today: none observable in practice (single-user, local cache, GitHub's transport is trusted). In future: incorrect type label makes debugging confusing; any integrity-checking tooling (future work) would silently malfunction.

### Effort estimate (fix on current REST provider)
**M** — Add `HashAlgorithm::Sha1` variant to `ctxfs-core/digest.rs`. Rename `from_sha256_hex` to `from_git_sha`. Update all call sites (two in provider, several in tests). This changes the serialized JSON format of `Digest` objects (algorithm field changes from `"sha256"` to `"sha1"`), which **invalidates existing caches**. A migration shim may be needed.

### Architecture coupling
**Partially yes.**

- **Label fix** (`HashAlgorithm::Sha1` variant): architecture-neutral. Should be done regardless of which v2 shape lands. The cache path change (existing `sha256/ab/...` files become `sha1/ab/...`) needs a migration strategy but is not architecturally bound.
- **Content verification**: Fundamentally different under each option. Under Option A (Git-native), object content is verified by the Git object store (Git's SHA-1 hash chain is the integrity primitive). Under Option B (REST), you'd need to compute a SHA-1 of fetched bytes and compare against `entry.sha`. Neither approach can reuse the current `HashAlgorithm::Sha256` content-hashing logic.

### **Recommendation: Both (split into two issues)**
1. **File now:** Label fix — `HashAlgorithm::Sha1`, rename `from_sha256_hex`, update cache path prefix. Closes a correctness issue that will confuse future contributors and debugging sessions.
2. **Fold into v2:** Content verification — the right hash and verification strategy depends on which v2 provider shape is chosen.

**Suggested issue title (part 1):** `ctxfs-core: Digest mislabels GitHub SHA-1 blob IDs as SHA-256 — add HashAlgorithm::Sha1 variant`

---

## B4 — Secondary rate-limit handling incomplete

### One-line summary
`check_rate_limit` only fires `CtxfsError::RateLimited` when `x-ratelimit-remaining == 0`. GitHub's secondary rate limits return **403 or 429 with a `retry-after` header while `remaining` is still nonzero**; those responses fall through as generic provider errors, breaking file reads.

### Where it lives
- `crates/ctxfs-provider-git/src/github.rs:205–237` — `check_rate_limit`
- `crates/ctxfs-provider-git/src/github.rs:130` — call site in `get_json`

#### Exact failure mode
```
check_rate_limit(resp)?;        // remaining=30 → passes through (BUG)
if !resp.status().is_success()  // 403 caught here
    → Err(CtxfsError::Provider("failed to fetch blob …: HTTP 403"))
```
This propagates to VfsState as `VfsError::Io("fetch_blob: provider error: …")`, then to the filesystem as EIO. The user sees a corrupt read — not a "rate limited, try again in 30 s" error.

#### Reference implementation already exists
`crates/ctxfs-provider-common/src/http.rs:43–52` correctly reads `retry-after` on 429. The GitHub provider should adopt equivalent logic for both 403 and 429.

### User-visible impact
Any secondary-rate-limited request (common during large cold scans) silently fails as a broken file read (EIO), rather than surfacing a clear "rate limited" error with a retry window. Users have no way to distinguish transient throttling from a real data error.

### Effort estimate (fix on current REST provider)
**S** — Two-part change in `check_rate_limit`:
1. On 403 or 429, check for `retry-after` header first.
2. If `retry-after` is present and parseable, return `RateLimited { retry_after_secs }` regardless of `x-ratelimit-remaining` value.

### Architecture coupling
**No** — Both Option A and Option B still make REST calls for reference resolution (`/commits/{ref}`). Secondary rate limits can hit any REST endpoint. This fix applies equally under any v2 shape.

### **Recommendation: File now**

**Suggested issue:**

> **Title:** `ctxfs-provider-git: secondary rate-limit responses (403/429 with retry-after) cascade as broken file reads instead of RateLimited errors`
>
> **Body:**
>
> GitHub's secondary rate limits fire when automated requests come in too fast, even when `x-ratelimit-remaining > 0`. They return HTTP 403 or 429 with a `retry-after: N` header (seconds to wait). Reference: https://docs.github.com/en/rest/using-the-rest-api/rate-limits-for-the-rest-api
>
> **Current behavior in `check_rate_limit` (github.rs:205):** Only triggers `RateLimited` when `x-ratelimit-remaining == 0`. If remaining is nonzero, the 403/429 passes through and is caught by the generic `!resp.status().is_success()` check, producing `CtxfsError::Provider("failed to fetch blob …: HTTP 403")`. This reaches the VFS as EIO — a broken file read, not a rate-limit signal.
>
> **Expected behavior:** On any 403 or 429, check for `retry-after` header first. If present, return `CtxfsError::RateLimited { retry_after_secs }`. Only fall through to the `remaining == 0` path if `retry-after` is absent.
>
> **Fix location:** `crates/ctxfs-provider-git/src/github.rs` → `check_rate_limit`. Reference implementation: `crates/ctxfs-provider-common/src/http.rs:43–52` (correct 429 handling for registry providers).
>
> **Tests:** Add a unit test to `check_rate_limit` asserting that a mock response with status=403, `retry-after: 30`, `x-ratelimit-remaining: 100` returns `Err(RateLimited { retry_after_secs: 30 })`.

---

## B5 — LRU cache eviction breaks "second grep is free" for large repos

### One-line summary
When a repo's total blob payload exceeds `CTXFS_CACHE_MAX_BYTES` (default 512 MB), LRU eviction churns during the first `grep -r`, evicting earlier-fetched blobs before the scan completes. Repeated reads don't avoid network calls.

### Where it lives
- `crates/ctxfs-cache/src/lib.rs:28–37` — global LRU state (`evict_oldest` is FIFO within the LRU; no per-repo carve-out)
- `crates/ctxfs-provider-git/src/github.rs:428–439` — blob cache put after fetch
- `crates/ctxfs-vfs/src/state.rs:411–424` — VfsState also calls `cache.put` redundantly after provider fetch (double-write, not a bug but worth knowing)

### User-visible impact
For repos larger than 512 MB (e.g., many open-source monorepos), the first `grep -r` triggers repeated network fetches throughout the scan. Performance degrades unexpectedly compared to the documented guarantee that the first full-content scan warms the cache permanently.

### Effort estimate (fix on current REST provider)
**L** — Per-repo cache reservation, sticky pinning, or a hot-path tier requires significant cache redesign. The global LRU doesn't support per-repo budgets.

### Architecture coupling
**Yes — fix strategy depends entirely on v2 cache shape.**

- **Option A (Git-native):** The unit of storage shifts from individual blobs (fetched per-file) to packfile objects stored in per-repo Git object directories under `~/.ctxfs/git-cache/{host}/{owner}/{repo}/`. The blob cache is no longer the primary storage primitive. Per-repo reservation becomes natural (each repo has its own object store). The 512 MB global LRU may not be the right abstraction at all under Option A.
- **Option B (improved REST + tarball prefetch):** A tarball `--prefetch` fetches all blobs in one round trip into a dedicated archive. A per-repo pinning strategy on top of the current cache could work, but requires API additions to `BlobCache`.

### **Recommendation: Both**
File a tracking issue now so Phase 4 planning accounts for this when sizing the cache strategy. Resolution waits on architecture decision.

**Suggested issue title:** `ctxfs-cache: no per-repo cache reservation; LRU eviction breaks warm-cache guarantee for repos > CTXFS_CACHE_MAX_BYTES`

---

## B6 — LFS payloads return pointer files, not real content

### One-line summary
For Git LFS-tracked files, the GitHub REST blob API returns LFS pointer text (`version https://git-lfs.github.com/spec/v1\noid sha256:…\nsize …`), not the actual file content. The provider does not detect or handle this.

### Where it lives
- `crates/ctxfs-provider-git/src/github.rs:175–201` — `fetch_blob_content` decodes base64 blob response; no LFS pointer detection

### User-visible impact
Reading any LFS-tracked file (common in repos with large binaries, model weights, dataset files) returns the pointer text rather than the real content. Programs trying to use these files receive corrupt data with no error.

### Effort estimate (fix on current REST provider)
**M** — Detect LFS pointer prefix in fetched content, then fetch from the LFS API (`https://github.com/{owner}/{repo}.git/info/lfs/objects/batch`). The LFS batch API requires separate authentication handling.

### Architecture coupling
**Yes — behavior change under v2.**

- **Option A (Git-native, subprocess `git`):** If the local Git config has an LFS smudge filter (`git lfs smudge`), `git cat-file` followed by a smudge step would transparently return real content. However, ctxfs doesn't need LFS content to be visible to users if they're reading source code; the current behavior at least surfaces pointer metadata. Whether smudging should happen depends on use-case decisions made during Phase 4 spec.
- **Option B (improved REST):** Detection + LFS API fetch must be implemented explicitly; the REST path has no automatic smudge semantics.

Either way, the current behavior must be made explicit. Users should at minimum see a clear error or know they're reading a pointer file, not silently receive corrupt content.

### **Recommendation: Both**
File now to document current behavior and ensure it's on the Phase 4 spec radar. Actual fix approach (smudge vs LFS API client vs explicit error) should be decided during Phase 4.

**Suggested issue title:** `ctxfs-provider-git: LFS-tracked files return pointer text, not real content — no LFS fetch or detection`

---

## B7 ★ — Symlink targets always empty string (not in original B1–B6)

### One-line summary
The provider sets `SymlinkEntry.target = String::new()` with a comment saying it will be "resolved lazily via blob fetch," but no lazy resolution is implemented anywhere. All symlinks in all mounted repos return empty targets.

### Where it lives
- `crates/ctxfs-provider-git/src/github.rs:268–270`:
  ```rust
  DirEntry::Symlink(SymlinkEntry {
      name,
      target: String::new(), // target resolved lazily via blob fetch
  })
  ```
- `crates/ctxfs-vfs/src/state.rs:194–199` — `readlink` returns `target.clone()` directly; no fetch
- `crates/ctxfs-vfs/src/state.rs:391–424` — `fetch_file_bytes` only handles `NodeKind::File`; no symlink blob fetch path

#### What the fix requires
In Git's object model, a symlink's target is stored as the blob content of the mode-`120000` tree entry. The correct fix:
1. Either: during `build_directories`, for all entries with `mode == "120000"`, fetch blob content and use it as `target`. This adds one API call per symlink during tree walk.
2. Or: in `VfsState::readlink`, when `target.is_empty()`, fetch via `provider.fetch_blob(digest)` and update the node. (Requires carrying the blob digest alongside symlink entries, which `SymlinkEntry` does not currently do.)

The `SymlinkEntry` struct in `ctxfs-manifest/src/snapshot.rs:53–57` only has `name` and `target`; it does not carry a `digest`. Adding a digest field is needed for option 2.

### User-visible impact
**Critical.** Any repo with symlinks (package.json → ../package.json, CMakeLists.txt linking, etc.) produces broken symlink behavior: `readlink` returns empty string, `ls -la` shows `link -> `, and any program following symlinks gets ENOENT or EINVAL. This is a data-correctness regression for any affected mount.

### Effort estimate (fix on current REST provider)
**S–M** — Option 1 (fetch during tree walk) is the simpler implementation: detect `mode == "120000"`, fetch blob immediately, use content as target. No struct changes needed. One network call per symlink at mount time.

### Architecture coupling
**No** — Symlink target content is a blob in both REST and Git-native v2. The mechanism (REST blob fetch vs `git cat-file --batch`) differs, but the fix shape is essentially the same under both options. The `SymlinkEntry` struct change (if option 2 is chosen) is neutral.

### **Recommendation: File now**

**Suggested issue:**

> **Title:** `ctxfs-provider-git: symlink targets always empty — "lazy blob fetch" path never implemented`
>
> **Body:**
>
> In `build_directories` (github.rs:268), symlinks are created with `target: String::new()` and a comment `// target resolved lazily via blob fetch`. No lazy resolution is implemented: `VfsState::readlink` (state.rs:194) returns `target.clone()` directly, and `fetch_file_bytes` has no symlink handling path.
>
> **Current behavior:** All symlinks in all mounted repos return an empty target string. `readlink(2)` returns `""`. Any process following a symlink gets an error.
>
> **Expected behavior:** Symlink targets should contain the actual target path stored as the blob content of the mode-120000 tree entry.
>
> **Fix sketch (Option 1 — fetch at tree walk time):**
> In `build_directories`, when `entry.mode == "120000"`, call `fetch_blob_content(source, &entry.sha)` and use the returned bytes (UTF-8 string) as `target`. This adds one REST call per symlink during `fetch_snapshot`, but eliminates the need for a lazy path.
>
> **Fix sketch (Option 2 — lazy at readlink time):**
> Add a `digest: Option<Digest>` field to `SymlinkEntry`. Populate it during `build_directories`. In `VfsState::readlink`, when `target.is_empty()` and `digest.is_some()`, fetch via provider and update the node in place.
>
> **Files:** `crates/ctxfs-provider-git/src/github.rs` (build_directories), `crates/ctxfs-vfs/src/state.rs` (readlink), optionally `crates/ctxfs-manifest/src/snapshot.rs` (SymlinkEntry)
>
> **Tests:** End-to-end test that mounts a repo with a symlink and asserts `readlink` returns the correct non-empty target.

---

## B8 ★ — `active_source` design hazard (not in original B1–B6)

### One-line summary
`GitHubProvider` stores repo context in a `Mutex<Option<SourceSpec>>` field (`active_source`) as a workaround for the Provider trait not carrying source context through `fetch_blob`. This is not currently a live race (each mount creates its own provider), but is a latent API design hazard.

### Where it lives
- `crates/ctxfs-provider-git/src/github.rs:32` — `active_source: std::sync::Mutex<Option<SourceSpec>>`
- `crates/ctxfs-provider-git/src/github.rs:356` — set in `fetch_snapshot`
- `crates/ctxfs-provider-git/src/github.rs:433` — read in `fetch_blob`
- `crates/ctxfs-daemon/src/daemon.rs:455` — each `prepare_mount` creates a **fresh** `Arc::new(GitHubProvider::new(...))`, so `active_source` is never shared across mounts
- git commit 869ca44 documents the origin: "Added `active_source: Mutex<Option<SourceSpec>>` populated by `fetch_snapshot` so later blob reads know which repo to hit."

#### Why it's a hazard, not a current bug
The sequence within a single mount is:
1. `prepare_mount` creates provider, calls `fetch_snapshot` → sets `active_source`
2. VfsState receives provider; only `fetch_blob` is ever called after this

Since (1) completes before (2) begins, there is no concurrent write risk today. The hazard is:
- If `fetch_snapshot` is ever called a second time on the same provider (e.g., in a future refactor where a provider serves multiple mounts), a concurrent `fetch_blob` in mount #1 could use the wrong source from mount #2's `fetch_snapshot` call.
- The Provider trait (`fn fetch_blob(&self, digest: &Digest)`) is `Send + Sync`, implying it can be called concurrently; the implicit source-context dependency violates this contract.

### User-visible impact
None today. Future refactors that share a provider across mounts or call fetch_snapshot concurrently would produce confusing failures (blob fetches hitting the wrong repo).

### Effort estimate (fix on current REST provider)
**M** — The cleanest fix is to change the Provider trait's `fetch_blob` signature to accept source context, or split the provider into a factory + per-repo instance. Either is a cross-crate API change.

### Architecture coupling
**Yes.**

- **Option A (Git-native):** A per-repo subprocess (`git cat-file --batch`) is inherently repo-scoped; `active_source` as a concept disappears. The provider becomes a factory that returns per-repo handles.
- **Option B (improved REST):** The Provider trait API needs to be revisited anyway; the `fetch_blob(digest)` signature needs augmentation to carry at minimum repo identity.

The right fix is entangled with the v2 provider API design.

### **Recommendation: Fold into v2**
Document in the Phase 4 spec that `fetch_blob` must carry source/repo context explicitly. The current `active_source` workaround should be noted as a known design debt when the Provider trait is redesigned.

---

## Additional efficiency note (not a bug, for completeness)

### `resolve_ref` before tree cache — always burns one API call per mount

`fetch_snapshot` calls `resolve_ref` (→ `GET /repos/{o}/{r}/commits/{ref}`) before consulting the tree cache, because the tree cache is keyed by commit SHA, not by ref name. This means every mount setup, even on a warm tree cache, costs at least one REST call.

For repos mounted by pinned SHA (version is a 40-char hex), this call is redundant — `version` is already the commit SHA. A simple guard (`if source.version.len() == 40 && source.version.chars().all(|c| c.is_ascii_hexdigit())`) would skip the resolution call for pinned SHA mounts.

This is an optimization, not a bug, and is part of the broader Phase 4 rate-limit framing. Not recommended as a standalone issue — the v2 architecture may change this path entirely.

---

## Summary

### Top three findings (by severity and actionability)

1. **B7 (Symlinks always broken)** — A correctness defect that silently breaks all repos with symlinks. All users. Every mount. The fix is small, architecture-neutral, and independent of Phase 4. Should be filed and fixed before Phase 4 ships.

2. **B4 (Secondary rate-limit cascades as EIO)** — Users hitting secondary rate limits see corrupt file reads, not a clean "rate limited" error. The fix is two lines in `check_rate_limit`; the provider-common crate already has a correct reference implementation. File now, fix independently.

3. **B3 (SHA-1 mislabeled as SHA-256)** — Affects correctness of any future integrity-checking tooling and will confuse contributors debugging cache issues. The label fix can be filed and resolved independently; the content-verification aspect correctly waits on v2 architecture.

### Phase 4 architecture signal

The bugs with the strongest architecture coupling are B2 (truncated tree) and B5 (LRU eviction). Both are fundamentally addressed by Option A (Git-native) and only partially by Option B (tarball prefetch helps B2 and B5 for the initial load, but per-repo pinning is still needed for B5). This is not a recommendation for either option — it is a factual input for the architecture decision.

### Tracking recommendation

File GitHub issues now for: **B1, B4, B7** (architecture-neutral, scoped, fixable independently).

File tracking issues (blocked on Phase 4) for: **B2, B3 (label half), B5, B6** — so the Phase 4 spec author knows to address them.

Fold into v2 spec without a standalone issue: **B3 (verification half), B8**.
