# Multi-Provider Architecture: Package Registry Support

**Date:** 2026-04-06
**Status:** Draft v5 (no tarball fallback — lazy-only)

## Problem

ContextFS only supports GitHub repositories. AI agents need to inspect dependency source code — to understand how a library works, debug issues, or explore APIs — without cloning entire repos. Distributed artifacts (what's in `node_modules/` or `site-packages/`) are usually already available locally when a project is set up. The real gap is **source code access without cloning**.

## Insight: Source Code is the Primary Value

Distributed code is already accessible:
- npm: `node_modules/react/` is right there after `npm install`
- pip: `site-packages/requests/` exists after `pip install`
- cargo: source is in `~/.cargo/registry/src/` after `cargo build`

Where ctxfs adds unique value:
1. **Source code without cloning** — inspect `facebook/react`'s source without `git clone` of a 300MB monorepo
2. **No project setup needed** — agent on a fresh task can explore any dependency's internals instantly
3. **Lazy fetching** — only files the agent reads are downloaded, unlike `git clone` which pulls everything
4. **Real filesystem mount** — works with any tool (`ls`, `grep`, `cat`, IDE) without knowing about a cache directory

## Prior Art: `cevr/repo`

[github.com/cevr/repo](https://github.com/cevr/repo) solves a similar problem: fetch and cache source code from GitHub, npm, PyPI, and crates.io for AI agent consumption.

- **Source-first**: resolves to upstream source repo via `repository` field. Falls back to tarball when no repo found.
- **Where ctxfs differentiates**: `repo` does shallow git clones (downloads everything upfront). ctxfs mounts lazily via NFS — only fetches files on demand. No upfront download.

## Goal

Mount source code for any published package, lazily, as a local directory:

```sh
# Mount react-dom — resolves to facebook/react monorepo, scoped to packages/react-dom
ctxfs mount npm:react-dom@19.1.0 /mnt/react-dom
cat /mnt/react-dom/src/ReactDOM.js

# Mount requests source — resolves to github:psf/requests at exact commit
ctxfs mount pypi:requests@2.31.0 /mnt/requests
cat /mnt/requests/src/requests/api.py

# Mount serde source — resolves to github:serde-rs/serde
ctxfs mount crate:serde@1.0.0 /mnt/serde
cat /mnt/serde/serde/src/lib.rs

# Package with no source repo linked — clear error, not a silent fallback
ctxfs mount npm:some-obscure-pkg@1.0.0 /mnt/pkg
# Error: no source repository found for npm:some-obscure-pkg@1.0.0
#   The package metadata doesn't link to a GitHub repo.
#   Try mounting the source directly: ctxfs mount github:owner/repo@ref /mnt/pkg
```

## Design Decisions

| Decision | Choice | Rationale |
|----------|--------|-----------|
| Source vs distributed | **Source code first** | Dist code is already local after install. Source is the real gap. |
| Resolution strategy | Registry → source repo → GitHub provider | Reuse existing GitHub provider. Registry providers are thin resolvers. |
| No source repo found | **Error with guidance** | Downloading a tarball defeats the purpose — it's not lazy, not a virtual filesystem. If we can't resolve to a source repo, fail clearly and tell the user to mount directly via `github:`. |
| Architecture | One crate per registry + shared helpers | Each registry can evolve independently. Natural path to plugin system. |
| Registries for MVP | npm + PyPI + crates.io | Three dominant ecosystems. Dogfood with our own. |
| Spec format | `npm:react@19.1.0` | Mirrors `github:owner/repo@ref`. Simple. |
| `latest` support | Resolve before mounting | Hit registry, get exact version, then proceed. |
| Version pinning | Use exact commit when available | `gitHead` from registry metadata is more reliable than guessing tags. |
| Monorepo support | Use `repository.directory` for subpath | npm provides this field; scope the mount to the right subdirectory. |

## Architecture: Resolver + Delegation

Registry providers are **resolvers**, not full providers. They resolve a package spec to a source repo + commit, then delegate to the existing GitHub provider. Every mount is lazy — no upfront downloads.

```
User: ctxfs mount npm:react-dom@19.1.0 /mnt/react-dom

1. Daemon receives "npm:react-dom@19.1.0", detects Npm provider type
2. Constructs NpmResolver, calls resolve("react-dom", "19.1.0")
3. NpmResolver:
   a. GET registry.npmjs.org/react-dom/19.1.0
   b. Reads repository.url: "https://github.com/facebook/react"
   c. Reads repository.directory: "packages/react-dom" → becomes subpath
   d. Reads dist.gitHead: "abc123def..." → exact commit SHA
   e. Returns ResolvedSource {
        owner: "facebook", repo: "react",
        git_ref: "abc123def...",
        subpath: Some("packages/react-dom")
      }
4. Daemon constructs GitHubProvider("facebook", "react")
5. Calls fetch_snapshot with ref="abc123def..."
6. GitHubProvider fetches tree lazily, caches blobs — business as usual
7. NFS mount uses subpath to scope root to packages/react-dom/
```

**When no source repo is found:**
```
User: ctxfs mount npm:some-package@1.0.0 /mnt/pkg

1. NpmResolver: GET registry.npmjs.org/some-package/1.0.0
   → repository field missing or not a GitHub URL
   → Returns Err(CtxfsError::NoSourceRepo {
       package: "some-package@1.0.0",
       registry: "npm",
     })
2. CLI displays:
   Error: no source repository found for npm:some-package@1.0.0
     The npm metadata doesn't link to a GitHub repo.
     Try: ctxfs mount github:owner/repo@ref /mnt/pkg
```

### Daemon control flow (pseudocode):

```rust
fn do_mount(&self, source_str: &str, mount_point: &str) -> Result<MountInfo> {
    let mut source = SourceSpec::parse(source_str)?;

    // Step 1: Resolve latest if needed
    if source.version == "latest" {
        let resolver = self.make_resolver(&source)?;
        source.version = self.rt.block_on(resolver.resolve_latest(&source.name))?;
    }

    // Step 2: Get provider — always GitHubProvider
    let (owner, repo, git_ref) = match source.provider_type {
        ProviderType::GitHub => {
            let (owner, repo) = parse_owner_repo(&source.name)?;
            (owner.to_string(), repo.to_string(), source.version.clone())
        }
        ProviderType::Npm | ProviderType::PyPI | ProviderType::Crate => {
            let resolver = self.make_resolver(&source)?;
            let resolved = self.rt.block_on(
                resolver.resolve(&source.name, &source.version)
            )?;
            // Update subpath from resolver (monorepo directory)
            if source.subpath.is_none() {
                source.subpath = resolved.subpath;
            }
            (resolved.owner, resolved.repo, resolved.git_ref)
        }
    };

    let provider = Arc::new(
        GitHubProvider::new(&owner, &repo, self.token(), self.cache.clone())
    );

    // Step 3: Fetch snapshot
    let snapshot_data = self.rt.block_on(provider.fetch_snapshot(&source))?;
    let snapshot: Snapshot = serde_json::from_slice(&snapshot_data)?;

    // Step 4: Spawn NFS server with optional subpath scoping
    let port = pick_free_port()?;
    let nfs_handle = CtxfsNfs::spawn(
        provider, &snapshot, &self.cache, port, source.subpath.as_deref()
    )?;

    // ... store mount handle, return MountInfo
}
```

## Source Spec Changes

`SourceSpec` becomes provider-agnostic with structured fields:

```rust
pub struct SourceSpec {
    pub provider_type: ProviderType,  // GitHub, Npm, PyPI, Crate
    /// Provider-specific name.
    /// GitHub: "owner/repo", npm: "react" or "@scope/pkg", PyPI: "requests", crate: "serde"
    pub name: String,
    /// Provider-specific version/ref.
    /// GitHub: branch/tag/SHA, npm/PyPI/crate: exact version string (never "latest")
    pub version: String,
    /// Optional sub-path to scope the mount root.
    /// Set by resolver for monorepo packages (e.g., "packages/react-dom"),
    /// or by user for GitHub (e.g., "src/lib").
    pub subpath: Option<String>,
}
```

**Validation rules in `SourceSpec::parse()`:**
- Split on first `:` → provider type (must be known variant)
- Split remainder on last `@` → name and version (handles `@scope/pkg@version`)
- `name` must be non-empty
- `version` must be non-empty
- For `GitHub` provider: `name` must contain exactly one `/` (owner/repo)
- `id()` sanitizes characters: `/` → `_`, `@` → `_at_` → safe for filenames and cache keys

**`id()` examples:**
- `github:octocat/Hello-World@main` → `github_octocat_Hello-World_main`
- `npm:react@19.1.0` → `npm_react_19.1.0`
- `npm:@babel/core@7.24.0` → `npm__at_babel_core_7.24.0`

### `latest` Resolution

1. CLI sends `npm:react@latest` to daemon
2. Daemon detects `version == "latest"`, calls resolver's `resolve_latest("react")` → `"19.1.0"`
3. Replaces `source.version = "19.1.0"`
4. Proceeds as if user typed `npm:react@19.1.0`

## New Crates

### `ctxfs-provider-common`

Shared utilities for registry resolvers. Lightweight — no archive/tarball code.

```
ctxfs-provider-common/src/
├── resolver.rs   — ResolvedSource struct, RegistryResolver trait
├── repo_url.rs   — parse repository URLs (git+https, git://, github: shorthand, etc.)
└── http.rs       — shared reqwest client (user-agent, retry, 429 handling)
```

**`ResolvedSource` struct:**
```rust
pub struct ResolvedSource {
    pub owner: String,
    pub repo: String,
    pub git_ref: String,            // exact commit SHA or verified tag
    pub subpath: Option<String>,    // monorepo directory (e.g., "packages/react-dom")
}
```

**`RegistryResolver` trait:**
```rust
#[async_trait]
pub trait RegistryResolver: Send + Sync {
    /// Resolve a package name + version to a GitHub source repo.
    /// Returns Err(CtxfsError::NoSourceRepo { .. }) if no repo found.
    async fn resolve(&self, name: &str, version: &str) -> Result<ResolvedSource, CtxfsError>;

    /// Resolve "latest" to an exact version string.
    async fn resolve_latest(&self, name: &str) -> Result<String, CtxfsError>;
}
```

**Repository URL parsing** (`repo_url.rs`):

Handles the many URL formats registries use:
- `https://github.com/owner/repo` → `Some(("owner", "repo"))`
- `https://github.com/owner/repo.git` → strip `.git` suffix
- `git+https://github.com/owner/repo.git` → strip `git+` prefix and `.git` suffix
- `git://github.com/owner/repo.git` → strip scheme and `.git`
- `git+ssh://git@github.com/owner/repo.git` → strip scheme, user, `.git`
- `git+ssh://git@github.com:owner/repo.git` → handle `:` as path separator (SCP syntax)
- `github:owner/repo` → npm shorthand
- `https://github.com/owner/repo/tree/main/packages/foo` → extract owner/repo, ignore tree path

Returns `Option<(String, String)>` — `None` if not a recognized GitHub URL. Non-GitHub hosts (GitLab, Bitbucket) return `None` with a `tracing::info` log suggesting the user mount directly.

**HTTP helpers:**
- Shared reqwest client with `User-Agent: ctxfs/0.1`
- Rate limit handling: detect 429, parse `Retry-After`, return `CtxfsError::RateLimited`
- Retry on transient errors (5xx, connection reset) with exponential backoff, max 3 attempts
- **Registry metadata cache**: in-memory `HashMap<(registry, name, version), serde_json::Value>` to avoid redundant API calls within a session. Not persisted to disk.

### `ctxfs-provider-npm`

Implements `RegistryResolver`. ~200 lines.

**Resolution:**
- `GET https://registry.npmjs.org/{package}/{version}` (scoped: `@scope%2Fpackage`)
- `repository` field can be a string or `{ type, url, directory }` object — handle both
- Parse URL through `repo_url::parse_github_url()`
- If no GitHub URL found → `Err(CtxfsError::NoSourceRepo { .. })`
- Extract `repository.directory` → set as `subpath`

**Commit resolution (prefer exact commit):**
1. If `dist.gitHead` is present → use it directly as `git_ref` (most reliable)
2. Else try tag `v{version}` via `GET /repos/{owner}/{repo}/git/ref/tags/v{version}`
3. Else try tag `{version}`
4. Else try tag `{package}@{version}` (monorepo convention: Babel, Jest)
5. If all miss → use `v{version}` as the ref anyway (GitHub will 404 at snapshot time with a clear error)

**`latest`:** `GET https://registry.npmjs.org/{package}/latest` → `version` field. Scoped: `@scope%2Fpackage/latest`.

**Yanked:** npm unpublished versions 404 → `CtxfsError::NotFound`.

### `ctxfs-provider-pypi`

Implements `RegistryResolver`. ~200 lines.

**Resolution:**
- `GET https://pypi.org/pypi/{package}/{version}/json`
- Read `info.project_urls` (case-insensitive key lookup). Check keys in order: `Source Code`, `Source`, `GitHub`, `Repository`, `Code`, `Homepage`
- Fall back to `info.home_page` if no `project_urls` match
- Parse URL through `repo_url::parse_github_url()`
- If no GitHub URL found → `Err(CtxfsError::NoSourceRepo { .. })`

**Commit resolution:**
1. PyPI doesn't provide `gitHead`. Try tags: `v{version}`, `{version}`, `release/{version}`
2. If all miss → use `v{version}` as ref (clear error at snapshot time if wrong)

**`latest`:** `GET https://pypi.org/pypi/{package}/json` → `info.version`.

**Yanked:** If version is yanked → error.

### `ctxfs-provider-crate`

Implements `RegistryResolver`. ~200 lines.

**Resolution:**
- `GET https://crates.io/api/v1/crates/{name}` (requires `User-Agent` header)
- Read `crate.repository` → parse through `repo_url::parse_github_url()`
- If no GitHub URL → `Err(CtxfsError::NoSourceRepo { .. })`

**Commit resolution:**
1. Try tags: `v{version}`, `{version}`, `{name}-{version}`
2. If all miss → use `v{version}` as ref

**`latest`:** Read `crate.max_stable_version`, fall back to `crate.max_version` for pre-release-only crates.

**Yanked:** 403/410 → error.

**Required header:** crates.io requires `User-Agent`; requests without one get 403.

## Modified Crates

### `ctxfs-core`

- `ProviderType`: add `Npm`, `PyPI`, `Crate` variants with `Display`/`FromStr`
- `SourceSpec`: replace `owner`, `repo`, `git_ref` with generic `name`, `version`, `subpath`
- `SourceSpec::parse()`: split first `:` for provider, last `@` for version. GitHub validation: `name` must contain `/`.
- `SourceSpec::id()`: sanitize `/` → `_`, `@` → `_at_`
- `CtxfsError`: add `NoSourceRepo { package, registry }` variant for clear error messages. Ensure `RateLimited` is in core (not just git provider).

### `ctxfs-provider-git`

- Adapts to new `SourceSpec` shape:
  ```rust
  impl GitHubProvider {
      fn owner_repo(source: &SourceSpec) -> Result<(&str, &str)> {
          source.name.split_once('/')
              .ok_or_else(|| CtxfsError::InvalidSource("expected owner/repo".into()))
      }
  }
  ```
- `fetch_snapshot` uses `source.version` as the git ref (was `source.git_ref`)
- No other behavioral changes

### `ctxfs-nfs`

- **Subpath support**: when `subpath` is set, use the subpath directory as the NFS mount root instead of the snapshot root.
- Implementation: in `CtxfsNfs::new()`, if subpath provided, walk the directory tree from snapshot root to find the subpath directory's digest, use that as root inode.
- Invalid subpath → `CtxfsError::NotFound("subpath 'x' not found in snapshot")`

### `ctxfs-daemon`

- Implements control flow from pseudocode above
- `make_resolver(&source)` factory: match on `ProviderType`
- All registry paths produce a `(owner, repo, git_ref)` → construct `GitHubProvider`
- **Rate limit budget**: resolvers share the same reqwest client (with GitHub auth token) so rate limit state is shared across tag existence checks and the main GitHub provider.

### `ctxfs-manifest`

- `Snapshot.commit_sha`: always a real commit SHA now (since every mount goes through GitHubProvider). No special casing for packages.

### Untouched

`ctxfs-cache`, `ctxfs-ipc`, `ctxfs-cli` — unchanged.

## Dependency Graph

```
ctxfs-core ← ctxfs-manifest ← ctxfs-cache
                                    ↑
ctxfs-provider-common ──────────────┤
    ↑         ↑         ↑          │
    npm      pypi     crate     ctxfs-provider-git
    ↑         ↑         ↑          ↑
    └─────────┴─────────┴──────────┘
                    │
              ctxfs-daemon
```

## Testing Strategy

### Unit tests (per resolver crate)

- Parse registry metadata JSON → extract correct `repository` URL and `gitHead`
- npm: `repository` as string → parse; as `{type, url, directory}` → parse with subpath
- npm: `dist.gitHead` present → used as git_ref
- npm: scoped `@babel/core@7.24.0` → correct URL encoding
- Missing `repository` field → `CtxfsError::NoSourceRepo` with helpful message
- Non-GitHub repository URL → `CtxfsError::NoSourceRepo` + info log
- `latest` resolution with mock HTTP → exact version
- Invalid/missing version → clear error
- Yanked version → appropriate error

### Unit tests (provider-common)

- `repo_url::parse_github_url()` — all URL formats:
  - `https://github.com/owner/repo` → `Some(("owner", "repo"))`
  - `git+https://github.com/owner/repo.git` → `Some(("owner", "repo"))`
  - `git+ssh://git@github.com:owner/repo.git` → `Some(("owner", "repo"))`
  - `github:owner/repo` → `Some(("owner", "repo"))`
  - `https://gitlab.com/owner/repo` → `None`
  - `https://github.com/owner/repo/tree/main/src` → `Some(("owner", "repo"))`
- Rate limit (429 response) → `CtxfsError::RateLimited`

### Unit tests (ctxfs-nfs subpath)

- Mount with subpath → root inode is the subpath directory
- Mount with invalid subpath → `CtxfsError::NotFound`

### Unit tests (SourceSpec)

- `npm:react@19.1.0` → `SourceSpec { provider_type: Npm, name: "react", version: "19.1.0" }`
- `npm:@babel/core@7.24.0` → name=`@babel/core`, version=`7.24.0`
- `github:owner/repo@main` → name=`owner/repo`, version=`main`
- `npm:react` (no version) → parse error
- `id()` produces filename-safe strings

### Integration tests (real APIs, gated behind `CTXFS_E2E_NETWORK=1`)

- `npm:lodash@4.17.21` → resolves to `github:lodash/lodash` → snapshot has `lodash.js`
- `pypi:six@1.16.0` → resolves to `github:benjaminp/six` → snapshot has `six.py`
- `crate:itoa@1.0.11` → resolves to `github:dtolnay/itoa` → snapshot has `src/lib.rs`
- `npm:@babel/core@7.24.0` → resolves to `github:babel/babel` with subpath `packages/babel-core` → mount root has `src/` and `package.json`

### Recorded fixture tests (offline-safe)

Mock HTTP responses for CI reliability:
- Successful resolution: registry JSON with `repository` + `gitHead` → `ResolvedSource`
- Repository as string vs object → both parse correctly
- Missing repository → `NoSourceRepo` error with helpful message
- Various URL formats → correct parsing
- 404 (version not found), 429 (rate limited), yanked version
- Tag existence checks: found → use, miss → try alternatives

### E2E tests (CLI)

- `ctxfs mount --server-only npm:lodash@4.17.21 /tmp/mnt` → NFS server starts, port reported
- Same pattern as existing GitHub e2e test

## Out of Scope (Future Work)

- Private registry authentication
- Semver range resolution
- GitLab/Bitbucket as source providers (currently returns `NoSourceRepo`; adding these providers would resolve more packages)
- Plugin system for third-party providers
- Documentation site providers
- Tarball/artifact fallback (intentionally excluded — defeats lazy-mount value prop)
- `Snapshot.commit_sha` field rename to `resolved_ref`
- Persistent registry metadata cache (currently in-memory per session)
