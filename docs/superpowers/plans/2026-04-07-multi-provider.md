# Multi-Provider Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add npm, PyPI, and crates.io support as thin registry resolvers that map package specs to GitHub source repos, then delegate to the existing GitHubProvider for lazy NFS mounting.

**Architecture:** Registry providers implement a `RegistryResolver` trait with a single `resolve(name, version)` method that returns `ResolvedSource { owner, repo, git_ref, subpath }`. The daemon dispatches based on `ProviderType`, calls the resolver, then constructs a `GitHubProvider` with the resolved coordinates. No tarball fallback — if no GitHub repo is found, we error with guidance.

**Tech Stack:** Rust, reqwest (HTTP), serde_json (registry API parsing), async-trait, existing ctxfs workspace crates.

**Spec:** `docs/superpowers/specs/2026-04-06-multi-provider-design.md`

---

## File Structure

### New crates
- `crates/ctxfs-provider-common/` — `ResolvedSource` struct, `RegistryResolver` trait, `repo_url` parser, shared HTTP helpers
- `crates/ctxfs-provider-npm/` — npm registry resolver (~200 lines)
- `crates/ctxfs-provider-pypi/` — PyPI registry resolver (~200 lines)
- `crates/ctxfs-provider-crate/` — crates.io registry resolver (~200 lines)

### Modified files
- `Cargo.toml` — add 4 new workspace members + deps
- `crates/ctxfs-core/src/source.rs` — `SourceSpec` becomes `{ provider_type, name, version, subpath }`, `ProviderType` gains 3 variants
- `crates/ctxfs-core/src/error.rs` — add `NoSourceRepo` variant
- `crates/ctxfs-provider-git/src/github.rs` — adapt to new `SourceSpec` (extract owner/repo from `name` field)
- `crates/ctxfs-nfs/src/fs.rs` — subpath support (scope mount root to subdirectory)
- `crates/ctxfs-daemon/src/daemon.rs` — resolver dispatch in `do_mount`
- `crates/ctxfs-daemon/Cargo.toml` — add new provider deps
- `CLAUDE.md` — update architecture docs

---

### Task 1: Refactor `SourceSpec` to be provider-agnostic

**Files:**
- Modify: `crates/ctxfs-core/src/source.rs`
- Modify: `crates/ctxfs-core/src/error.rs`

This task changes `SourceSpec` from `{ provider_type, owner, repo, git_ref, subpath }` to `{ provider_type, name, version, subpath }` and adds `Npm`, `PyPI`, `Crate` to `ProviderType`. Also adds `NoSourceRepo` error variant.

- [ ] **Step 1: Write failing tests for new SourceSpec shape**

Add these tests to the existing `#[cfg(test)] mod tests` in `crates/ctxfs-core/src/source.rs`:

```rust
#[test]
fn parse_npm_basic() {
    let s = SourceSpec::parse("npm:react@19.1.0").unwrap();
    assert_eq!(s.provider_type, ProviderType::Npm);
    assert_eq!(s.name, "react");
    assert_eq!(s.version, "19.1.0");
    assert_eq!(s.subpath, None);
}

#[test]
fn parse_npm_scoped() {
    let s = SourceSpec::parse("npm:@babel/core@7.24.0").unwrap();
    assert_eq!(s.provider_type, ProviderType::Npm);
    assert_eq!(s.name, "@babel/core");
    assert_eq!(s.version, "7.24.0");
}

#[test]
fn parse_pypi() {
    let s = SourceSpec::parse("pypi:requests@2.31.0").unwrap();
    assert_eq!(s.provider_type, ProviderType::PyPI);
    assert_eq!(s.name, "requests");
    assert_eq!(s.version, "2.31.0");
}

#[test]
fn parse_crate() {
    let s = SourceSpec::parse("crate:serde@1.0.0").unwrap();
    assert_eq!(s.provider_type, ProviderType::Crate);
    assert_eq!(s.name, "serde");
    assert_eq!(s.version, "1.0.0");
}

#[test]
fn parse_npm_no_version_fails() {
    assert!(SourceSpec::parse("npm:react").is_err());
}

#[test]
fn parse_npm_empty_version_fails() {
    assert!(SourceSpec::parse("npm:react@").is_err());
}

#[test]
fn id_sanitizes_special_chars() {
    let s = SourceSpec::parse("npm:@babel/core@7.24.0").unwrap();
    let id = s.id();
    assert!(!id.contains('/'));
    assert!(!id.contains('@'));
    assert!(id.contains("babel"));
    assert!(id.contains("core"));
    assert!(id.contains("7.24.0"));
}

#[test]
fn display_npm() {
    let s = SourceSpec::parse("npm:react@19.1.0").unwrap();
    assert_eq!(s.to_string(), "npm:react@19.1.0");
}

#[test]
fn display_npm_scoped() {
    let s = SourceSpec::parse("npm:@babel/core@7.24.0").unwrap();
    assert_eq!(s.to_string(), "npm:@babel/core@7.24.0");
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p ctxfs-core -- parse_npm`
Expected: compilation errors (no `Npm`/`PyPI`/`Crate` variants, `SourceSpec` still has old fields)

- [ ] **Step 3: Implement new SourceSpec and ProviderType**

Replace `ProviderType` and `SourceSpec` in `crates/ctxfs-core/src/source.rs`:

```rust
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    GitHub,
    Npm,
    PyPI,
    Crate,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderType::GitHub => write!(f, "github"),
            ProviderType::Npm => write!(f, "npm"),
            ProviderType::PyPI => write!(f, "pypi"),
            ProviderType::Crate => write!(f, "crate"),
        }
    }
}

impl std::str::FromStr for ProviderType {
    type Err = CtxfsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github" => Ok(ProviderType::GitHub),
            "npm" => Ok(ProviderType::Npm),
            "pypi" => Ok(ProviderType::PyPI),
            "crate" => Ok(ProviderType::Crate),
            other => Err(CtxfsError::InvalidSource(format!(
                "unsupported provider '{other}', expected 'github', 'npm', 'pypi', or 'crate'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpec {
    pub provider_type: ProviderType,
    pub name: String,
    pub version: String,
    pub subpath: Option<String>,
}

impl SourceSpec {
    /// Parse a source string.
    ///
    /// Formats:
    /// - `github:owner/repo@ref`
    /// - `github:owner/repo@ref:subpath`
    /// - `npm:react@19.1.0`
    /// - `npm:@scope/package@19.1.0`
    /// - `pypi:requests@2.31.0`
    /// - `crate:serde@1.0.0`
    pub fn parse(s: &str) -> Result<Self, CtxfsError> {
        let (provider_str, rest) = s.split_once(':').ok_or_else(|| {
            CtxfsError::InvalidSource(format!("missing provider prefix in '{s}'"))
        })?;

        let provider_type: ProviderType = provider_str.parse()?;

        match provider_type {
            ProviderType::GitHub => Self::parse_github(rest),
            _ => Self::parse_registry(provider_type, rest),
        }
    }

    fn parse_github(rest: &str) -> Result<Self, CtxfsError> {
        // Split off optional subpath (after second ':')
        let (repo_ref, subpath) = match rest.split_once(':') {
            Some((rr, sp)) => (rr, Some(sp.to_string())),
            None => (rest, None),
        };

        let (owner_repo, git_ref) = repo_ref
            .split_once('@')
            .ok_or_else(|| CtxfsError::InvalidSource(format!("missing @ref in 'github:{rest}'")))?;

        let (owner, repo) = owner_repo.split_once('/').ok_or_else(|| {
            CtxfsError::InvalidSource(format!("missing owner/repo in 'github:{rest}'"))
        })?;

        if owner.is_empty() || repo.is_empty() || git_ref.is_empty() {
            return Err(CtxfsError::InvalidSource(format!(
                "empty owner, repo, or ref in 'github:{rest}'"
            )));
        }

        Ok(Self {
            provider_type: ProviderType::GitHub,
            name: format!("{owner}/{repo}"),
            version: git_ref.to_string(),
            subpath,
        })
    }

    fn parse_registry(provider_type: ProviderType, rest: &str) -> Result<Self, CtxfsError> {
        // Split on the *last* '@' to handle scoped packages like @babel/core@7.24.0
        let at_pos = rest.rfind('@').ok_or_else(|| {
            CtxfsError::InvalidSource(format!("missing @version in '{provider_type}:{rest}'"))
        })?;

        let name = &rest[..at_pos];
        let version = &rest[at_pos + 1..];

        if name.is_empty() {
            return Err(CtxfsError::InvalidSource(format!(
                "empty package name in '{provider_type}:{rest}'"
            )));
        }
        if version.is_empty() {
            return Err(CtxfsError::InvalidSource(format!(
                "empty version in '{provider_type}:{rest}'"
            )));
        }

        Ok(Self {
            provider_type,
            name: name.to_string(),
            version: version.to_string(),
            subpath: None,
        })
    }

    /// A stable, filesystem-safe identifier for this source.
    pub fn id(&self) -> String {
        let sanitized_name = self.name.replace('/', "_").replace('@', "_at_");
        format!("{}_{sanitized_name}_{}", self.provider_type, self.version)
    }
}

impl fmt::Display for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@{}", self.provider_type, self.name, self.version)?;
        if let Some(ref sp) = self.subpath {
            write!(f, ":{sp}")?;
        }
        Ok(())
    }
}
```

- [ ] **Step 4: Add `NoSourceRepo` variant to `CtxfsError`**

In `crates/ctxfs-core/src/error.rs`, add:

```rust
#[error("no source repository found for {registry}:{package}. Try: ctxfs mount github:owner/repo@ref")]
NoSourceRepo { package: String, registry: String },
```

And add a test:

```rust
#[test]
fn no_source_repo_error() {
    let e = CtxfsError::NoSourceRepo {
        package: "some-pkg@1.0.0".into(),
        registry: "npm".into(),
    };
    assert!(e.to_string().contains("no source repository"));
    assert!(e.to_string().contains("some-pkg@1.0.0"));
}
```

- [ ] **Step 5: Update existing tests for new field names**

In `crates/ctxfs-core/src/source.rs` tests, update old tests: replace `s.owner` → `s.name` (now `"octocat/Hello-World"`), `s.repo` → removed, `s.git_ref` → `s.version`. The `parse_basic` test becomes:

```rust
#[test]
fn parse_basic() {
    let s = SourceSpec::parse("github:octocat/Hello-World@master").unwrap();
    assert_eq!(s.provider_type, ProviderType::GitHub);
    assert_eq!(s.name, "octocat/Hello-World");
    assert_eq!(s.version, "master");
    assert_eq!(s.subpath, None);
}
```

Update all existing tests similarly. The `parse_errors` test should keep `gitlab:...` as an error case.

- [ ] **Step 6: Run all ctxfs-core tests**

Run: `cargo test -p ctxfs-core`
Expected: all tests pass

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-core/
git commit -m "refactor: make SourceSpec provider-agnostic with Npm/PyPI/Crate support

SourceSpec fields change from {owner, repo, git_ref} to {name, version,
subpath}. ProviderType gains Npm, PyPI, Crate variants. Parsing uses
last '@' split for scoped packages (e.g., @babel/core@7.24.0).
id() sanitizes special characters for safe cache keys."
```

---

### Task 2: Adapt `GitHubProvider` to new `SourceSpec`

**Files:**
- Modify: `crates/ctxfs-provider-git/src/github.rs`
- Modify: `crates/ctxfs-provider-git/tests/build_directories.rs`

The GitHub provider currently reads `source.owner`, `source.repo`, `source.git_ref`. Now it must extract these from `source.name` (which is `"owner/repo"`) and `source.version`.

- [ ] **Step 1: Update `GitHubProvider` to parse `name` field**

In `crates/ctxfs-provider-git/src/github.rs`, add a helper:

```rust
/// Extract (owner, repo) from SourceSpec.name, which is "owner/repo" for GitHub sources.
fn owner_repo(source: &SourceSpec) -> Result<(&str, &str), CtxfsError> {
    source.name.split_once('/').ok_or_else(|| {
        CtxfsError::InvalidSource(format!(
            "expected owner/repo in name '{}', got no '/'",
            source.name
        ))
    })
}
```

- [ ] **Step 2: Replace all `source.owner`, `source.repo`, `source.git_ref` usages**

In `resolve_commit`: replace `source.owner` → `owner`, `source.repo` → `repo`, `source.git_ref` → `source.version`. Use the helper:

```rust
async fn resolve_commit(&self, source: &SourceSpec) -> Result<String, CtxfsError> {
    let (owner, repo) = owner_repo(source)?;
    let url = Self::api_url(owner, repo, &format!("commits/{}", source.version));
    // ... rest unchanged
}
```

Apply the same pattern to `fetch_tree`, `fetch_blob_content`, `build_directories`, and `fetch_snapshot`. Every `source.owner` → `owner_repo(source)?`, `source.git_ref` → `source.version`.

The `api_url` method stays the same (takes `&str` args).

- [ ] **Step 3: Update integration test**

In `crates/ctxfs-provider-git/tests/build_directories.rs`, update any `SourceSpec` construction to use new fields. If the test constructs a `SourceSpec` directly, use `SourceSpec::parse("github:...")` instead.

- [ ] **Step 4: Run tests**

Run: `cargo test -p ctxfs-provider-git`
Expected: all tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-provider-git/
git commit -m "refactor: adapt GitHubProvider to generic SourceSpec

Extract owner/repo from source.name via split_once('/').
Use source.version instead of source.git_ref."
```

---

### Task 3: Fix downstream compilation (`ctxfs-nfs`, `ctxfs-daemon`, `ctxfs-cli`, integration tests)

**Files:**
- Modify: `crates/ctxfs-nfs/src/fs.rs`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`
- Modify: `crates/ctxfs-cli/src/main.rs` (if it references SourceSpec fields)
- Modify: `crates/ctxfs-core/tests/cross_crate.rs`
- Modify: `crates/ctxfs-nfs/tests/*.rs`
- Modify: `crates/ctxfs-cli/tests/e2e.rs`

The `SourceSpec` field rename will break compilation in every crate that touches it. This task is purely mechanical — update field references.

- [ ] **Step 1: Fix `ctxfs-nfs/src/fs.rs`**

`CtxfsNfs::new` stores a `SourceSpec`. Check if it accesses any old fields (it likely stores it opaquely). If `source` is just stored and passed to provider calls, no changes needed beyond what compiles.

- [ ] **Step 2: Fix `ctxfs-daemon/src/daemon.rs`**

`do_mount` accesses `source.id()` (unchanged) and constructs `GitHubProvider`. The `GitHubProvider::new` signature takes `(token, cache)` — it doesn't take owner/repo at construction. The source is passed to `fetch_snapshot`. So `do_mount` should still work after Task 1+2.

Verify `do_mount` compiles with the new `SourceSpec`.

- [ ] **Step 3: Fix `ctxfs-core/tests/cross_crate.rs`**

Update any `SourceSpec` field access to new names. Use `SourceSpec::parse(...)` instead of direct construction where possible.

- [ ] **Step 4: Fix `ctxfs-nfs/tests/*.rs` and `ctxfs-cli/tests/e2e.rs`**

These tests use `SourceSpec::parse(...)` and don't access fields directly — they should compile unchanged. Verify.

- [ ] **Step 5: Run full test suite**

Run: `cargo test -p ctxfs-core -p ctxfs-manifest -p ctxfs-cache -p ctxfs-ipc -p ctxfs-provider-git -p ctxfs-nfs -p ctxfs-daemon -p ctxfs`
Expected: all tests pass

- [ ] **Step 6: Commit**

```bash
git add -A
git commit -m "fix: update all crates for new SourceSpec field names"
```

---

### Task 4: Create `ctxfs-provider-common` crate

**Files:**
- Create: `crates/ctxfs-provider-common/Cargo.toml`
- Create: `crates/ctxfs-provider-common/src/lib.rs`
- Create: `crates/ctxfs-provider-common/src/resolver.rs`
- Create: `crates/ctxfs-provider-common/src/repo_url.rs`
- Create: `crates/ctxfs-provider-common/src/http.rs`
- Modify: `Cargo.toml` (workspace)

- [ ] **Step 1: Write failing tests for `repo_url::parse_github_url`**

Create `crates/ctxfs-provider-common/src/repo_url.rs` with tests:

```rust
//! Parse repository URLs from package registry metadata into (owner, repo) pairs.

/// Extract (owner, repo) from a GitHub URL. Returns None if not a GitHub URL.
pub fn parse_github_url(_url: &str) -> Option<(String, String)> {
    todo!()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_url() {
        let result = parse_github_url("https://github.com/lodash/lodash");
        assert_eq!(result, Some(("lodash".into(), "lodash".into())));
    }

    #[test]
    fn https_with_git_suffix() {
        let result = parse_github_url("https://github.com/facebook/react.git");
        assert_eq!(result, Some(("facebook".into(), "react".into())));
    }

    #[test]
    fn git_plus_https() {
        let result = parse_github_url("git+https://github.com/babel/babel.git");
        assert_eq!(result, Some(("babel".into(), "babel".into())));
    }

    #[test]
    fn git_ssh() {
        let result = parse_github_url("git+ssh://git@github.com/owner/repo.git");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn git_protocol() {
        let result = parse_github_url("git://github.com/owner/repo.git");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn github_shorthand() {
        let result = parse_github_url("github:facebook/react");
        assert_eq!(result, Some(("facebook".into(), "react".into())));
    }

    #[test]
    fn url_with_tree_path() {
        let result = parse_github_url("https://github.com/owner/repo/tree/main/src");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn scp_syntax() {
        let result = parse_github_url("git+ssh://git@github.com:owner/repo.git");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn gitlab_returns_none() {
        assert_eq!(parse_github_url("https://gitlab.com/owner/repo"), None);
    }

    #[test]
    fn empty_string_returns_none() {
        assert_eq!(parse_github_url(""), None);
    }

    #[test]
    fn not_a_url_returns_none() {
        assert_eq!(parse_github_url("just some text"), None);
    }
}
```

- [ ] **Step 2: Create Cargo.toml and lib.rs**

`crates/ctxfs-provider-common/Cargo.toml`:
```toml
[package]
name = "ctxfs-provider-common"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
ctxfs-core = { workspace = true }
async-trait = { workspace = true }
reqwest = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }

[lints]
workspace = true
```

`crates/ctxfs-provider-common/src/lib.rs`:
```rust
pub mod http;
pub mod repo_url;
pub mod resolver;
```

Add to workspace `Cargo.toml`:
- In `members`: add `"crates/ctxfs-provider-common"`
- In `[workspace.dependencies]`: add `ctxfs-provider-common = { path = "crates/ctxfs-provider-common", version = "0.0.0" }`

- [ ] **Step 3: Run test to verify it fails**

Run: `cargo test -p ctxfs-provider-common -- repo_url`
Expected: FAIL with `todo!()` panics

- [ ] **Step 4: Implement `parse_github_url`**

```rust
pub fn parse_github_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // Handle "github:owner/repo" shorthand (npm convention)
    if let Some(rest) = url.strip_prefix("github:") {
        return parse_owner_repo_from_path(rest);
    }

    // Normalize: strip git+ prefix, handle git:// and git+ssh://
    let normalized = url
        .strip_prefix("git+")
        .unwrap_or(url);

    // Handle SCP-style: git+ssh://git@github.com:owner/repo.git
    if let Some(after_colon) = normalized.strip_prefix("ssh://git@github.com:") {
        let path = after_colon.strip_suffix(".git").unwrap_or(after_colon);
        return parse_owner_repo_from_path(path);
    }

    // Parse as URL-like: extract host and path
    // Handles: https://github.com/..., git://github.com/..., ssh://git@github.com/...
    let path_part = if let Some(rest) = normalized.strip_prefix("ssh://git@github.com/") {
        Some(rest)
    } else if let Some(rest) = normalized.strip_prefix("https://github.com/") {
        Some(rest)
    } else if let Some(rest) = normalized.strip_prefix("http://github.com/") {
        Some(rest)
    } else if let Some(rest) = normalized.strip_prefix("git://github.com/") {
        Some(rest)
    } else {
        None
    };

    let path = path_part?;
    let path = path.strip_suffix(".git").unwrap_or(path);
    parse_owner_repo_from_path(path)
}

fn parse_owner_repo_from_path(path: &str) -> Option<(String, String)> {
    let mut parts = path.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    // Ignore anything after owner/repo (e.g., /tree/main/src)
    Some((owner.to_string(), repo.to_string()))
}
```

- [ ] **Step 5: Create `resolver.rs` with trait and struct**

```rust
//! Registry resolver types shared across npm, PyPI, and crates.io providers.

use async_trait::async_trait;
use ctxfs_core::error::CtxfsError;

/// The result of resolving a package to its GitHub source repository.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedSource {
    pub owner: String,
    pub repo: String,
    pub git_ref: String,
    pub subpath: Option<String>,
}

/// Trait implemented by each registry resolver (npm, PyPI, crates.io).
#[async_trait]
pub trait RegistryResolver: Send + Sync {
    /// Resolve a package name + version to a GitHub source repo.
    /// Returns `Err(CtxfsError::NoSourceRepo { .. })` if no GitHub repo is found.
    async fn resolve(&self, name: &str, version: &str) -> Result<ResolvedSource, CtxfsError>;

    /// Resolve "latest" to an exact version string.
    async fn resolve_latest(&self, name: &str) -> Result<String, CtxfsError>;
}
```

- [ ] **Step 6: Create `http.rs` with shared client builder**

```rust
//! Shared HTTP client for registry API calls.

use reqwest::header::HeaderMap;

const USER_AGENT: &str = "ctxfs/0.1";

/// Build a reqwest client with standard headers for registry API calls.
pub fn registry_client() -> reqwest::Client {
    let headers = HeaderMap::new();
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .build()
        .expect("failed to build HTTP client")
}
```

- [ ] **Step 7: Run all tests**

Run: `cargo test -p ctxfs-provider-common`
Expected: all tests pass

- [ ] **Step 8: Commit**

```bash
git add crates/ctxfs-provider-common/ Cargo.toml
git commit -m "feat: add ctxfs-provider-common crate

ResolvedSource struct, RegistryResolver trait, repo_url parser
(handles git+https, git://, github: shorthand, SCP syntax),
and shared HTTP client builder."
```

---

### Task 5: Create `ctxfs-provider-npm` crate

**Files:**
- Create: `crates/ctxfs-provider-npm/Cargo.toml`
- Create: `crates/ctxfs-provider-npm/src/lib.rs`
- Modify: `Cargo.toml` (workspace)

- [ ] **Step 1: Write unit tests for npm resolution**

In `crates/ctxfs-provider-npm/src/lib.rs`:

```rust
//! npm registry resolver — maps npm package specs to GitHub source repos.

use async_trait::async_trait;
use ctxfs_core::error::CtxfsError;
use ctxfs_provider_common::repo_url;
use ctxfs_provider_common::resolver::{RegistryResolver, ResolvedSource};
use serde::Deserialize;
use tracing::info;

pub struct NpmResolver {
    client: reqwest::Client,
}

// ... implementation below

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_repository_url_string() {
        let json: serde_json::Value = serde_json::json!({
            "repository": "https://github.com/lodash/lodash.git"
        });
        let result = extract_repo_info(&json);
        assert_eq!(result, Some(("lodash".into(), "lodash".into(), None)));
    }

    #[test]
    fn parse_repository_object_with_directory() {
        let json: serde_json::Value = serde_json::json!({
            "repository": {
                "type": "git",
                "url": "https://github.com/facebook/react.git",
                "directory": "packages/react-dom"
            }
        });
        let result = extract_repo_info(&json);
        assert_eq!(
            result,
            Some(("facebook".into(), "react".into(), Some("packages/react-dom".into())))
        );
    }

    #[test]
    fn parse_repository_github_shorthand() {
        let json: serde_json::Value = serde_json::json!({
            "repository": {
                "type": "git",
                "url": "github:facebook/react"
            }
        });
        let result = extract_repo_info(&json);
        assert_eq!(result, Some(("facebook".into(), "react".into(), None)));
    }

    #[test]
    fn parse_no_repository_field() {
        let json: serde_json::Value = serde_json::json!({
            "name": "some-package",
            "version": "1.0.0"
        });
        assert_eq!(extract_repo_info(&json), None);
    }

    #[test]
    fn parse_githead_present() {
        let json: serde_json::Value = serde_json::json!({
            "dist": { "gitHead": "abc123def456" }
        });
        assert_eq!(extract_git_head(&json), Some("abc123def456".to_string()));
    }

    #[test]
    fn parse_githead_absent() {
        let json: serde_json::Value = serde_json::json!({
            "dist": { "tarball": "https://..." }
        });
        assert_eq!(extract_git_head(&json), None);
    }
}
```

- [ ] **Step 2: Create Cargo.toml**

```toml
[package]
name = "ctxfs-provider-npm"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
ctxfs-core = { workspace = true }
ctxfs-provider-common = { workspace = true }
async-trait = { workspace = true }
reqwest = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }

[dev-dependencies]
tokio = { workspace = true }

[lints]
workspace = true
```

Add to workspace `Cargo.toml` members + deps.

- [ ] **Step 3: Implement helper functions and NpmResolver**

```rust
/// Extract (owner, repo, directory) from npm package metadata JSON.
fn extract_repo_info(json: &serde_json::Value) -> Option<(String, String, Option<String>)> {
    let repo = json.get("repository")?;

    let (url_str, directory) = if repo.is_string() {
        (repo.as_str()?.to_string(), None)
    } else {
        let url = repo.get("url")?.as_str()?.to_string();
        let dir = repo.get("directory").and_then(|d| d.as_str()).map(|s| s.to_string());
        (url, dir)
    };

    let (owner, repo_name) = repo_url::parse_github_url(&url_str)?;
    Some((owner, repo_name, directory))
}

/// Extract dist.gitHead commit SHA if present.
fn extract_git_head(json: &serde_json::Value) -> Option<String> {
    json.get("dist")?
        .get("gitHead")?
        .as_str()
        .map(|s| s.to_string())
}

/// Encode a package name for the npm registry URL.
fn encode_package_name(name: &str) -> String {
    if name.starts_with('@') {
        name.replace('/', "%2F")
    } else {
        name.to_string()
    }
}

impl NpmResolver {
    pub fn new(client: reqwest::Client) -> Self {
        Self { client }
    }

    async fn fetch_metadata(&self, name: &str, version: &str) -> Result<serde_json::Value, CtxfsError> {
        let encoded = encode_package_name(name);
        let url = format!("https://registry.npmjs.org/{encoded}/{version}");

        let resp = self.client.get(&url).send().await
            .map_err(|e| CtxfsError::Provider(format!("npm registry request failed: {e}")))?;

        if resp.status() == reqwest::StatusCode::NOT_FOUND {
            return Err(CtxfsError::NotFound(format!("npm:{name}@{version}")));
        }
        if resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(CtxfsError::RateLimited { retry_after_secs: 60 });
        }
        if !resp.status().is_success() {
            return Err(CtxfsError::Provider(format!(
                "npm registry returned {}", resp.status()
            )));
        }

        resp.json().await
            .map_err(|e| CtxfsError::Provider(format!("failed to parse npm response: {e}")))
    }
}

#[async_trait]
impl RegistryResolver for NpmResolver {
    async fn resolve(&self, name: &str, version: &str) -> Result<ResolvedSource, CtxfsError> {
        let metadata = self.fetch_metadata(name, version).await?;

        let (owner, repo, subpath) = extract_repo_info(&metadata).ok_or_else(|| {
            CtxfsError::NoSourceRepo {
                package: format!("{name}@{version}"),
                registry: "npm".into(),
            }
        })?;

        // Prefer exact commit from gitHead, fall back to version-based ref
        let git_ref = extract_git_head(&metadata)
            .unwrap_or_else(|| format!("v{version}"));

        info!("npm:{name}@{version} -> github:{owner}/{repo}@{git_ref}");

        Ok(ResolvedSource { owner, repo, git_ref, subpath })
    }

    async fn resolve_latest(&self, name: &str) -> Result<String, CtxfsError> {
        let metadata = self.fetch_metadata(name, "latest").await?;
        metadata.get("version")
            .and_then(|v| v.as_str())
            .map(|s| s.to_string())
            .ok_or_else(|| CtxfsError::Provider(format!(
                "npm metadata for '{name}' missing 'version' field"
            )))
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ctxfs-provider-npm`
Expected: all unit tests pass

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-provider-npm/ Cargo.toml
git commit -m "feat: add ctxfs-provider-npm registry resolver

Resolves npm package specs to GitHub source repos using registry
metadata. Uses dist.gitHead for exact commits, extracts
repository.directory for monorepo subpath support."
```

---

### Task 6: Create `ctxfs-provider-pypi` crate

**Files:**
- Create: `crates/ctxfs-provider-pypi/Cargo.toml`
- Create: `crates/ctxfs-provider-pypi/src/lib.rs`
- Modify: `Cargo.toml` (workspace)

Same pattern as npm. Key differences: reads `info.project_urls` (case-insensitive key search), falls back to `info.home_page`, no `gitHead` equivalent so always uses tag-based ref.

- [ ] **Step 1: Write unit tests**

Test `extract_repo_url` with `project_urls` containing various keys (`Source Code`, `Source`, `GitHub`, `Repository`, `Code`, `Homepage`), case variations, and `home_page` fallback.

- [ ] **Step 2: Create Cargo.toml** (same deps as npm)

- [ ] **Step 3: Implement `PyPIResolver`**

```rust
impl PyPIResolver {
    async fn fetch_metadata(&self, name: &str, version: &str) -> Result<serde_json::Value, CtxfsError> {
        let url = format!("https://pypi.org/pypi/{name}/{version}/json");
        // ... same pattern as npm
    }
}

#[async_trait]
impl RegistryResolver for PyPIResolver {
    async fn resolve(&self, name: &str, version: &str) -> Result<ResolvedSource, CtxfsError> {
        let metadata = self.fetch_metadata(name, version).await?;
        let (owner, repo) = extract_repo_url(&metadata).ok_or_else(|| {
            CtxfsError::NoSourceRepo {
                package: format!("{name}@{version}"),
                registry: "pypi".into(),
            }
        })?;
        let git_ref = format!("v{version}");
        Ok(ResolvedSource { owner, repo, git_ref, subpath: None })
    }

    async fn resolve_latest(&self, name: &str) -> Result<String, CtxfsError> {
        let url = format!("https://pypi.org/pypi/{name}/json");
        // ... fetch, read info.version
    }
}
```

Key: `extract_repo_url` checks `info.project_urls` keys case-insensitively in priority order, then `info.home_page`.

- [ ] **Step 4: Run tests, commit**

```bash
git add crates/ctxfs-provider-pypi/ Cargo.toml
git commit -m "feat: add ctxfs-provider-pypi registry resolver"
```

---

### Task 7: Create `ctxfs-provider-crate` crate

**Files:**
- Create: `crates/ctxfs-provider-crate/Cargo.toml`
- Create: `crates/ctxfs-provider-crate/src/lib.rs`
- Modify: `Cargo.toml` (workspace)

Same pattern. Key differences: reads `crate.repository`, requires `User-Agent` header, `latest` reads `crate.max_stable_version` with `max_version` fallback.

- [ ] **Step 1: Write unit tests** (parse `crate.repository`, `max_stable_version` fallback)

- [ ] **Step 2: Create Cargo.toml**

- [ ] **Step 3: Implement `CrateResolver`**

- [ ] **Step 4: Run tests, commit**

```bash
git add crates/ctxfs-provider-crate/ Cargo.toml
git commit -m "feat: add ctxfs-provider-crate registry resolver"
```

---

### Task 8: Add subpath support to `ctxfs-nfs`

**Files:**
- Modify: `crates/ctxfs-nfs/src/fs.rs`

When `subpath` is set, the NFS mount root should be the subpath directory, not the snapshot root.

- [ ] **Step 1: Write failing test**

In `crates/ctxfs-nfs/tests/nfs_read_path.rs` or a new test file, add a test that mounts with a subpath and verifies the root shows the subdirectory contents.

This is hard to unit-test in isolation since it requires a real Snapshot. Instead, add a unit test to `fs.rs` that tests the subpath resolution logic:

```rust
#[test]
fn resolve_subpath_finds_correct_directory() {
    // Test that given a snapshot with root -> dir "src" -> file "lib.rs",
    // resolving subpath "src" returns the digest of the "src" directory.
    // ... construct test data
}
```

- [ ] **Step 2: Implement subpath support**

Modify `CtxfsNfs::new()` to accept an optional `subpath: Option<&str>`. If provided, walk the directory tree from the snapshot root to find the named subdirectory, and use its digest as the root. Add a parameter to `CtxfsNfs::spawn()` as well.

- [ ] **Step 3: Run tests, commit**

```bash
git add crates/ctxfs-nfs/
git commit -m "feat: subpath support in NFS mounts for monorepo packages"
```

---

### Task 9: Wire resolver dispatch into daemon

**Files:**
- Modify: `crates/ctxfs-daemon/Cargo.toml`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

- [ ] **Step 1: Add new dependencies to daemon Cargo.toml**

```toml
ctxfs-provider-common = { workspace = true }
ctxfs-provider-npm = { workspace = true }
ctxfs-provider-pypi = { workspace = true }
ctxfs-provider-crate = { workspace = true }
```

- [ ] **Step 2: Implement resolver dispatch in `do_mount`**

Replace the current `do_mount` body with the dispatch logic from the spec pseudocode. Key change: for `Npm`/`PyPI`/`Crate`, construct a resolver, call `resolve()`, get `(owner, repo, git_ref, subpath)`, then construct `GitHubProvider` with those. Pass `subpath` to `CtxfsNfs::spawn()`.

```rust
fn do_mount(&self, source_str: &str, mount_point: &str) -> Result<MountInfo, String> {
    let mut source = SourceSpec::parse(source_str).map_err(|e| format!("invalid source: {e}"))?;

    // Resolve "latest" to exact version
    if source.version == "latest" {
        let resolver = self.make_resolver(&source)
            .map_err(|e| format!("{e}"))?;
        source.version = self.rt_handle
            .block_on(resolver.resolve_latest(&source.name))
            .map_err(|e| format!("failed to resolve latest: {e}"))?;
    }

    // Resolve to GitHub coordinates
    let (owner, repo, git_ref, subpath) = match source.provider_type {
        ProviderType::GitHub => {
            let (o, r) = source.name.split_once('/')
                .ok_or_else(|| format!("invalid github source: {}", source.name))?;
            (o.to_string(), r.to_string(), source.version.clone(), source.subpath.clone())
        }
        ProviderType::Npm | ProviderType::PyPI | ProviderType::Crate => {
            let resolver = self.make_resolver(&source)
                .map_err(|e| format!("{e}"))?;
            let resolved = self.rt_handle
                .block_on(resolver.resolve(&source.name, &source.version))
                .map_err(|e| format!("{e}"))?;
            let sp = source.subpath.clone().or(resolved.subpath);
            (resolved.owner, resolved.repo, resolved.git_ref, sp)
        }
    };

    // Build a GitHub-shaped SourceSpec for the provider
    let github_source = SourceSpec {
        provider_type: ProviderType::GitHub,
        name: format!("{owner}/{repo}"),
        version: git_ref,
        subpath: subpath.clone(),
    };

    let provider = Arc::new(GitHubProvider::new(
        self.config.github_token.as_deref(),
        self.cache.clone(),
    ));

    let snapshot_data = self.rt_handle
        .block_on(provider.fetch_snapshot(&github_source))
        .map_err(|e| format!("failed to fetch snapshot: {e}"))?;

    let snapshot: Snapshot = serde_json::from_slice(&snapshot_data)
        .map_err(|e| format!("failed to parse snapshot: {e}"))?;

    std::fs::create_dir_all(mount_point)
        .map_err(|e| format!("failed to create mount point: {e}"))?;

    let id = source.id();
    let commit_sha = snapshot.commit_sha.clone();

    let port = pick_free_port()?;
    let addr = format!("127.0.0.1:{port}");

    let fs = CtxfsNfs::new(provider, github_source, self.cache.clone(), snapshot);
    let nfs_handle = self.rt_handle
        .block_on(fs.spawn(&addr))
        .map_err(|e| format!("failed to start NFS server on {addr}: {e}"))?;

    // ... rest unchanged (build MountInfo, store handle)
}
```

- [ ] **Step 3: Add `make_resolver` helper**

```rust
impl DaemonServer {
    fn make_resolver(&self, source: &SourceSpec) -> Result<Box<dyn RegistryResolver>, CtxfsError> {
        let client = ctxfs_provider_common::http::registry_client();
        match source.provider_type {
            ProviderType::Npm => Ok(Box::new(ctxfs_provider_npm::NpmResolver::new(client))),
            ProviderType::PyPI => Ok(Box::new(ctxfs_provider_pypi::PyPIResolver::new(client))),
            ProviderType::Crate => Ok(Box::new(ctxfs_provider_crate::CrateResolver::new(client))),
            ProviderType::GitHub => Err(CtxfsError::InvalidSource(
                "GitHub sources don't use a registry resolver".into()
            )),
        }
    }
}
```

- [ ] **Step 4: Run full test suite**

Run: `cargo test`
Expected: all tests pass (existing GitHub tests unchanged, new resolver code not yet exercised by integration tests)

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-daemon/
git commit -m "feat: wire registry resolver dispatch into daemon

do_mount now dispatches to NpmResolver/PyPIResolver/CrateResolver
for non-GitHub sources, resolves to GitHub coordinates, then
constructs GitHubProvider as before. latest resolution happens
before mounting."
```

---

### Task 10: Integration tests with real registry APIs

**Files:**
- Create: `crates/ctxfs-provider-npm/tests/resolve.rs`
- Create: `crates/ctxfs-provider-pypi/tests/resolve.rs`
- Create: `crates/ctxfs-provider-crate/tests/resolve.rs`

These tests hit real APIs and are gated behind `CTXFS_E2E_NETWORK=1`.

- [ ] **Step 1: npm integration test**

```rust
//! Integration test: resolve real npm packages to GitHub repos.

#[test]
fn resolve_lodash() {
    if std::env::var("CTXFS_E2E_NETWORK").is_err() {
        eprintln!("skipping network test");
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let client = ctxfs_provider_common::http::registry_client();
    let resolver = ctxfs_provider_npm::NpmResolver::new(client);

    let result = rt.block_on(resolver.resolve("lodash", "4.17.21")).unwrap();
    assert_eq!(result.owner, "lodash");
    assert_eq!(result.repo, "lodash");
    assert!(!result.git_ref.is_empty());
}
```

- [ ] **Step 2: PyPI integration test** (resolve `six@1.16.0` → `benjaminp/six`)

- [ ] **Step 3: crates.io integration test** (resolve `itoa@1.0.11` → `dtolnay/itoa`)

- [ ] **Step 4: E2E test** — `ctxfs mount --server-only npm:lodash@4.17.21` → NFS starts

- [ ] **Step 5: Run with network flag, commit**

Run: `CTXFS_E2E_NETWORK=1 cargo test -p ctxfs-provider-npm -p ctxfs-provider-pypi -p ctxfs-provider-crate`

```bash
git add crates/ctxfs-provider-npm/tests/ crates/ctxfs-provider-pypi/tests/ crates/ctxfs-provider-crate/tests/
git commit -m "test: integration tests for registry resolvers against real APIs"
```

---

### Task 11: Update docs and CI

**Files:**
- Modify: `CLAUDE.md`
- Modify: `README.md`
- Modify: `.github/workflows/ci.yml`

- [ ] **Step 1: Update CLAUDE.md** — change "7-crate workspace" to "11-crate workspace", add new crates to architecture list

- [ ] **Step 2: Update README.md** — add npm/PyPI/crate examples to usage section, update source spec format table

- [ ] **Step 3: Update CI** — add new crates to clippy/test commands (they should already be picked up by `cargo test` since they're workspace members, but verify)

- [ ] **Step 4: Run CI locally, commit**

Run: `cargo fmt --all && RUSTFLAGS="-D warnings" cargo clippy --all-targets --tests && cargo test`

```bash
git add CLAUDE.md README.md .github/workflows/ci.yml
git commit -m "docs: update README and CLAUDE.md for multi-provider support"
```
