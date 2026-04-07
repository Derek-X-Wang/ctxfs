# Tiered Caching Design

## Goal

Add multi-tier caching to ctxfs so that repeat mounts are near-instant, GitHub API calls are minimized, and popular trees can be shared across users via an optional Redis backend.

## Background

ctxfs currently has a single-tier local blob cache (`ctxfs-cache`). It stores file content and directory manifests keyed by SHA-256 digest, with LRU eviction at a configurable max size (default 512MB). This works well for repeated file reads within a single mount session, but has gaps:

- **Registry resolution is never cached.** Every `npm:react@19.1.0` mount re-queries the npm registry even though the result is immutable for pinned versions.
- **Tree manifests aren't cached across daemon restarts.** The GitHub tree API call (the most expensive single call per mount) is repeated every time.
- **No shared caching.** Two users mounting the same package both hit GitHub independently.

## Architecture

Three cache tiers, each with distinct semantics:

```
mount npm:react@19.1.0
  |
  v
[Tier 1: Resolution Cache]  -- "what GitHub repo is this?"
  |  HIT: skip npm registry
  |  MISS: query npm → cache result
  v
[Tier 2: Tree Cache]         -- "what files are in this repo?"
  |  Local HIT: skip GitHub API
  |  Local MISS → Redis HIT: populate local, skip GitHub API
  |  Redis MISS: query GitHub → cache locally + Redis
  v
[Tier 3: Blob Cache]         -- "what's in this file?" (existing, unchanged)
  |  HIT: serve from disk
  |  MISS: fetch blob from GitHub → cache
  v
[NFS serve to AI agent]
```

## Tier 1: Resolution Cache

**Purpose:** Cache the mapping from package spec to GitHub coordinates so repeat mounts skip the registry API entirely.

**Storage:** In-memory `HashMap` backed by a JSON file on disk at `{cache_dir}/resolutions.json`. Each entry is ~200 bytes.

**Key:** Source spec string (e.g., `npm:react@19.1.0`, `pypi:requests@2.31.0`).

**Value:**
```rust
struct CachedResolution {
    source: ResolvedSource,  // { owner, repo, git_ref, subpath }
    resolved_at: u64,        // unix timestamp
}
```

**TTL rules:**
- Pinned versions (`npm:react@19.1.0`): no expiry. The resolution is immutable.
- Latest (`npm:react@latest`): configurable TTL, default 1 hour. Controlled by `CTXFS_LATEST_TTL_SECS`.
- ctxfs only accepts pinned versions or `latest`. Semver ranges (`^18`, `~2.0`) and dist-tags (`next`, `canary`) are not supported — those are resolved by package managers, not ctxfs.

**TTL enforcement:** `get()` checks the `resolved_at` timestamp on every lookup. If the entry has expired (i.e., it was a `latest` resolution and `now - resolved_at > latest_ttl_secs`), `get()` returns `None` and the caller re-resolves from the registry. Expired entries are lazily replaced on the next `put()`.

**Location:** New `ResolutionCache` struct in the `ctxfs-cache` crate. Called by the daemon's `do_mount()` before invoking any registry resolver.

**Persistence:** Written to disk on every `put()` (atomic write-rename to avoid corruption). Loaded on daemon startup.

**Concurrency note:** The daemon uses a PID file to enforce single-instance per cache directory. Multiple daemons sharing the same `CTXFS_CACHE_DIR` is unsupported and may cause resolution file corruption.

## Tier 2: Tree Cache

**Purpose:** Cache directory tree manifests so repeat mounts skip the GitHub tree API call. Trees are immutable per commit SHA.

### Local Tree Cache

**Storage:** Disk-backed files at `{cache_dir}/trees/{owner}/{repo}/{commit_sha}.json`.

**Soft cap with LRU eviction.** Trees are small (~500KB for a 10,000-file repo), but unbounded growth across many repos/versions could exhaust disk. Default soft cap: 500MB (`CTXFS_TREE_CACHE_MAX_BYTES`). When exceeded, oldest entries (by last-access mtime) are evicted. Users can also manually prune via `ctxfs cache prune --trees`.

**Key:** `{owner}/{repo}@{commit_sha}` — uses the resolved commit SHA (not tag/branch) to guarantee immutability.

**Value:** Serialized `Snapshot` including all `Directory` objects for the repo, prefixed with a schema version number (currently `1`). On deserialization, if the version doesn't match the current binary's expected version, the cached entry is discarded and re-fetched. This prevents stale cache files from causing deserialization failures after upgrades.

**Location:** New `TreeCache` struct in `ctxfs-cache`. Checked by the GitHub provider before calling the tree API.

### Shared Tree Cache (Redis)

**Purpose:** Share tree manifests across users. When one user mounts a package, all subsequent users get the tree from Redis instead of GitHub.

**Scope:** Trees only. Not resolutions (too cheap to bother), not blobs (too large).

**Connection:** Configured via `CTXFS_REDIS_URL` environment variable. If unset, Redis is entirely disabled.

**Key format:** `ctxfs:tree:{owner}/{repo}@{commit_sha}`

**Value:** zstd-compressed JSON of the tree manifest. Typical compressed size: 50-200KB for large repos.

**TTL:** No Redis-level expiry. Data is immutable. Server-side memory management via Redis `maxmemory-policy allkeys-lru`.

**Trait:**
```rust
#[async_trait]
pub trait SharedTreeCache: Send + Sync {
    async fn get_tree(&self, owner: &str, repo: &str, commit_sha: &str) -> Option<Vec<u8>>;
    async fn put_tree(&self, owner: &str, repo: &str, commit_sha: &str, data: &[u8]);
}
```

`RedisTreeCache` implements this trait. The trait exists so that other backends (HTTP, S3) can be added later without changing consumers.

**Graceful degradation:** If Redis is unreachable or returns an error, log a warning and fall back to GitHub API. A mount must never fail because Redis is down.

**Lookup order:**
1. Local `TreeCache` on disk
2. Redis `SharedTreeCache` (if configured) — on hit, populate local cache
3. GitHub tree API — on fetch, store in both local and Redis

### Compression

Compression is applied **only in the Redis layer**. Trees are zstd-compressed before storing in Redis and decompressed on retrieval. This saves network bandwidth and Redis memory.

Local caches store uncompressed data. File blobs remain uncompressed for fast reads — AI agents read the same files repeatedly in tight loops and CPU overhead from decompression on every read is undesirable.

## Tier 3: Blob Cache (existing, unchanged)

The existing `BlobCache` in `ctxfs-cache` continues to handle file content blobs. No changes to its behavior, eviction strategy, or storage format.

## Configuration

### New environment variables

| Variable | Default | Description |
|----------|---------|-------------|
| `CTXFS_REDIS_URL` | (unset) | Redis connection URL. Unset = Redis disabled |
| `CTXFS_LATEST_TTL_SECS` | `3600` | TTL in seconds for `@latest` resolution cache |
| `CTXFS_TREE_CACHE_MAX_BYTES` | `536870912` (500MB) | Soft cap for local tree cache |

### Existing variables (unchanged)

| Variable | Default | Description |
|----------|---------|-------------|
| `CTXFS_CACHE_DIR` | `~/.ctxfs/cache` | Local cache root directory |
| `CTXFS_CACHE_MAX_BYTES` | `536870912` (512MB) | Max blob cache size |

### Config struct additions

```rust
// Added to ctxfs_core::Config
pub redis_url: Option<String>,
pub latest_ttl_secs: u64,           // default: 3600
pub tree_cache_max_bytes: u64,      // default: 536_870_912 (500MB)
```

## Crate Structure

| Crate | Change |
|-------|--------|
| `ctxfs-cache` | Add `ResolutionCache`, `TreeCache`, `SharedTreeCache` trait |
| `ctxfs-cache-redis` (new) | `RedisTreeCache` impl. Deps: `redis` (async, tokio), `zstd` |
| `ctxfs-core` | Add `redis_url`, `latest_ttl_secs` to `Config` |
| `ctxfs-provider-git` | Check `TreeCache` before GitHub tree API in `fetch_snapshot()` |
| `ctxfs-daemon` | Wire `ResolutionCache` into `do_mount()`, pass `SharedTreeCache` to provider |
| `ctxfs-cli` | Extend `ctxfs cache stats` and `ctxfs cache prune` for new tiers |

### Optional Redis dependency

`ctxfs-cache-redis` is a separate crate with an optional cargo feature flag on the daemon. Users who don't need Redis don't pull in `redis` or `zstd` dependencies. Default build: Redis disabled.

```toml
# In ctxfs-daemon/Cargo.toml
[features]
default = []
redis = ["ctxfs-cache-redis"]
```

**Runtime detection:** If `CTXFS_REDIS_URL` is set but the binary was compiled without the `redis` feature, the daemon emits a warning at startup: `"CTXFS_REDIS_URL is set but Redis support is not compiled in. Build with --features redis to enable shared tree caching."` The mount proceeds without Redis (no error, no silent ignore).

## CLI Changes

- `ctxfs cache stats` — now reports blob count/size, tree count/size, and resolution count
- `ctxfs cache prune` — gains `--trees` and `--resolutions` flags to clear specific tiers
- Existing `ctxfs cache prune --max-size` continues to target blob cache only

## Out of Scope

- **No public Redis instance.** "Bring your own" only. No URL shipped in code, config, or docs.
- **No local blob compression.** Blobs stay uncompressed for fast reads.
- **No HTTP shared cache backend.** Redis only for now. `SharedTreeCache` trait enables HTTP later.
- **No cache invalidation API.** Pinned versions and commit SHAs are immutable. `@latest` uses TTL.
- **No blob sharing via Redis.** Blobs are too large and access patterns are per-user.
- **No per-mount cache isolation.** All mounts share the same local cache.
