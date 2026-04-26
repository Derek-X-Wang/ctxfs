use async_trait::async_trait;
use base64::Engine;
use ctxfs_cache::{BlobCache, SharedTreeCache, TreeCache};
use ctxfs_core::error::CtxfsError;
use ctxfs_core::provider::Provider;
use ctxfs_core::source::SourceSpec;
use ctxfs_core::Digest;
use ctxfs_manifest::{DirEntry, Directory, DirectoryEntry, FileEntry, Snapshot, SymlinkEntry};
use ctxfs_provider_common::counters::CounterKey;
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_common::rate_limit::AuthIdentity;
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION};
use serde::de::DeserializeOwned;
use serde::Deserialize;
// sha2 used indirectly via ctxfs_core::Digest
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

const USER_AGENT_STR: &str = "ctxfs/0.1";

/// Hardcoded for now; M3+ may make this configurable via `CTXFS_GITHUB_HOST`
/// for GHE support and tarball-redirect host validation.
pub const GITHUB_HOST: &str = "api.github.com";

// Git file mode constants from the GitHub Trees API
const MODE_SYMLINK: &str = "120000";
const MODE_EXECUTABLE: &str = "100755";

pub struct GitHubProvider {
    client: reqwest::Client,
    cache: Arc<BlobCache>,
    tree_cache: Option<Arc<TreeCache>>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    /// Daemon-side registry shared across all mounts. Used by
    /// [`Self::check_rate_limit`] to record `rest_calls_total`,
    /// `throttle_events`, gauge updates, and secondary-throttle state.
    observability: Arc<Observability>,
    /// Computed once from the token at construction time so every gauge update
    /// keys to the same bucket.
    auth_identity: AuthIdentity,
    /// Set in `fetch_snapshot` AFTER `resolve_ref`, using the resolved commit SHA
    /// (not `source.version`). Read by `check_rate_limit` (and later, M2.T6
    /// onwards, by `fetch_blob`) to attribute counters to the right
    /// `(source, repo, commit, mount_id)` bucket. `None` until the first
    /// `fetch_snapshot` completes — early calls (none expected today) are
    /// silently un-attributed rather than blocking on registration.
    counter_key: std::sync::Mutex<Option<CounterKey>>,
    /// The most-recently-fetched source. `fetch_snapshot` records it so that
    /// subsequent `fetch_blob` calls (which only receive a `Digest`) can locate
    /// the right repo for the GitHub blob API. A provider instance is scoped
    /// to a single mount, so this is always consistent at read time.
    active_source: std::sync::Mutex<Option<SourceSpec>>,
}

impl std::fmt::Debug for GitHubProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHubProvider").finish_non_exhaustive()
    }
}

#[derive(Debug, Deserialize)]
struct CommitResponse {
    sha: String,
}

#[derive(Debug, Deserialize)]
struct TreeResponse {
    #[allow(dead_code)]
    sha: String,
    tree: Vec<TreeEntry>,
    truncated: bool,
}

/// A single entry from the GitHub Git Trees API response.
/// Public for integration testing.
#[derive(Debug, Clone, Deserialize)]
pub struct TreeEntry {
    pub path: String,
    pub mode: String,
    #[serde(rename = "type")]
    pub entry_type: String,
    pub sha: String,
    pub size: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct BlobResponse {
    content: String,
    encoding: String,
    #[allow(dead_code)]
    sha: String,
}

/// Extract (owner, repo) from `SourceSpec.name`, which is `"owner/repo"` for GitHub sources.
fn owner_repo(source: &SourceSpec) -> Result<(&str, &str), CtxfsError> {
    source.name.split_once('/').ok_or_else(|| {
        CtxfsError::InvalidSource(format!(
            "expected owner/repo in name '{}', got no '/'",
            source.name
        ))
    })
}

impl GitHubProvider {
    pub fn new(
        token: Option<&str>,
        cache: Arc<BlobCache>,
        tree_cache: Option<Arc<TreeCache>>,
        shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
        observability: Arc<Observability>,
    ) -> Self {
        let auth_identity = match token {
            Some(t) => AuthIdentity::pat(GITHUB_HOST, t),
            None => AuthIdentity::anonymous(GITHUB_HOST),
        };

        let mut default_headers = HeaderMap::new();
        let _ = default_headers.insert(ACCEPT, "application/vnd.github.v3+json".parse().unwrap());
        if let Some(token) = token {
            let _ =
                default_headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        }

        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT_STR)
            .default_headers(default_headers)
            .build()
            .expect("failed to build HTTP client");

        Self {
            client,
            cache,
            tree_cache,
            shared_tree_cache,
            observability,
            auth_identity,
            counter_key: std::sync::Mutex::new(None),
            active_source: std::sync::Mutex::new(None),
        }
    }

    fn api_url(owner: &str, repo: &str, path: &str) -> String {
        format!("https://{GITHUB_HOST}/repos/{owner}/{repo}/{path}")
    }

    /// Send a GET request, check rate limits and status, and parse the JSON response.
    async fn get_json<T: DeserializeOwned>(
        &self,
        url: &str,
        context: &str,
    ) -> Result<T, CtxfsError> {
        let resp = self
            .client
            .get(url)
            .send()
            .await
            .map_err(|e| CtxfsError::Provider(format!("HTTP error {context}: {e}")))?;

        self.check_rate_limit(&resp)?;

        if !resp.status().is_success() {
            return Err(CtxfsError::Provider(format!(
                "failed to {context}: HTTP {}",
                resp.status()
            )));
        }

        resp.json()
            .await
            .map_err(|e| CtxfsError::Provider(format!("JSON parse error: {e}")))
    }

    async fn resolve_ref(&self, source: &SourceSpec) -> Result<String, CtxfsError> {
        let (owner, repo) = owner_repo(source)?;
        let url = Self::api_url(owner, repo, &format!("commits/{}", source.version));

        let commit: CommitResponse = self
            .get_json(&url, &format!("resolve ref '{}'", source.version))
            .await?;

        Ok(commit.sha)
    }

    async fn fetch_tree(
        &self,
        source: &SourceSpec,
        tree_sha: &str,
    ) -> Result<TreeResponse, CtxfsError> {
        let (owner, repo) = owner_repo(source)?;
        let url = Self::api_url(owner, repo, &format!("git/trees/{tree_sha}?recursive=1"));

        let tree: TreeResponse = self.get_json(&url, "fetch tree").await?;

        if tree.truncated {
            warn!(
                "tree response was truncated for {}; large repos may be incomplete",
                source.name
            );
        }

        Ok(tree)
    }

    async fn fetch_blob_content(
        &self,
        source: &SourceSpec,
        sha: &str,
    ) -> Result<Vec<u8>, CtxfsError> {
        let (owner, repo) = owner_repo(source)?;
        let url = Self::api_url(owner, repo, &format!("git/blobs/{sha}"));

        let blob: BlobResponse = self.get_json(&url, &format!("fetch blob {sha}")).await?;

        if blob.encoding != "base64" {
            return Err(CtxfsError::Provider(format!(
                "unexpected blob encoding: {}",
                blob.encoding
            )));
        }

        // GitHub base64 content has newlines; strip them
        let cleaned: String = blob
            .content
            .chars()
            .filter(|c| !c.is_whitespace())
            .collect();
        let data = base64::engine::general_purpose::STANDARD
            .decode(&cleaned)
            .map_err(|e| CtxfsError::Provider(format!("base64 decode error: {e}")))?;

        Ok(data)
    }

    /// Pure-logic classifier on (status, headers map). Unit-testable. Used by
    /// the [`Self::check_rate_limit`] adapter that operates on a real
    /// `reqwest::Response`.
    fn classify_response(
        status: u16,
        headers: &std::collections::HashMap<String, String>,
    ) -> Result<(), CtxfsError> {
        use ctxfs_provider_common::rate_limit::{RateLimitVerdict, ThrottleClassifier};
        match ThrottleClassifier::classify(status, headers) {
            RateLimitVerdict::Ok { .. } => Ok(()),
            RateLimitVerdict::SecondaryThrottle { retry_after, .. } => {
                Err(CtxfsError::RateLimited {
                    retry_after_secs: retry_after.as_secs(),
                })
            }
            RateLimitVerdict::PrimaryExhausted { reset_at, .. } => {
                let now = std::time::SystemTime::now();
                let secs = reset_at
                    .duration_since(now)
                    .map(|d| d.as_secs())
                    .unwrap_or(60);
                Err(CtxfsError::RateLimited {
                    retry_after_secs: secs,
                })
            }
            RateLimitVerdict::Other { .. } => Ok(()),
        }
    }

    /// Adapter: extracts headers from a `reqwest::Response`, classifies, updates
    /// the daemon-side gauge, increments `rest_calls_total`, and records throttle
    /// events. Returns `CtxfsError::RateLimited` for both primary-exhausted and
    /// secondary-throttle verdicts; `Ok(())` otherwise (HTTP-status checks for
    /// non-throttle errors live in the caller).
    fn check_rate_limit(&self, resp: &reqwest::Response) -> Result<(), CtxfsError> {
        use ctxfs_provider_common::rate_limit::{
            RateLimitVerdict, ResourceClass, ThrottleClassifier,
        };

        let status = resp.status().as_u16();
        let headers: std::collections::HashMap<String, String> = resp
            .headers()
            .iter()
            .filter_map(|(k, v)| {
                v.to_str()
                    .ok()
                    .map(|s| (k.as_str().to_lowercase(), s.to_string()))
            })
            .collect();

        // Always increment rest_calls_total for quota-bearing GitHub API calls.
        // (Codeload tarball downloads aren't quota-bearing and don't go through here.)
        if let Some(key) = self.counter_key.lock().unwrap().clone() {
            self.observability.counters_for(key).record_rest_call();
        }

        // Update the gauge from response headers (best-effort; missing headers
        // leave the gauge unchanged per RateLimitGauge::update_from_headers).
        let resource = headers
            .get("x-ratelimit-resource")
            .map(|s| ResourceClass::parse(s))
            .unwrap_or_else(|| ResourceClass::Other("unknown".to_string()));
        self.observability
            .update_gauge(self.auth_identity.clone(), resource.clone(), &headers);

        // Classify and act on secondary throttle.
        let verdict = ThrottleClassifier::classify(status, &headers);
        if let RateLimitVerdict::SecondaryThrottle { retry_after, .. } = verdict {
            self.observability.mark_secondary_throttle(
                self.auth_identity.clone(),
                resource,
                retry_after,
            );
            if let Some(key) = self.counter_key.lock().unwrap().clone() {
                self.observability.counters_for(key).record_throttle_event();
            }
            tracing::warn!(
                target: "ctxfs.provider.throttle",
                retry_after_secs = retry_after.as_secs(),
                "secondary throttle in provider-git"
            );
        }

        Self::classify_response(status, &headers)
    }

    /// Build directory tree from flat GitHub tree entries.
    pub fn build_directories(
        entries: &[TreeEntry],
        _source: &SourceSpec,
    ) -> (Digest, HashMap<String, Directory>) {
        // Group entries by parent path
        let mut dir_children: HashMap<String, Vec<DirEntry>> = HashMap::new();
        let _ = dir_children.insert(String::new(), Vec::new()); // root

        // Single pass: ensure directories exist, ensure parents exist, and place entries
        for entry in entries {
            let parent = parent_path(&entry.path);
            let name = file_name(&entry.path);

            // Ensure tree entries have their own key in dir_children
            if entry.entry_type == "tree" {
                let _ = dir_children.entry(entry.path.clone()).or_default();
            }

            // Ensure parent directory exists
            if let Some(ref p) = parent {
                let _ = dir_children.entry(p.clone()).or_default();
            }

            let parent_key = parent.unwrap_or_default();

            // Check mode first: symlinks are entry_type "blob" with mode "120000"
            let dir_entry = if entry.mode == MODE_SYMLINK {
                DirEntry::Symlink(SymlinkEntry {
                    name,
                    target: String::new(), // target resolved lazily via blob fetch
                })
            } else {
                match entry.entry_type.as_str() {
                    "blob" => {
                        let executable = entry.mode == MODE_EXECUTABLE;
                        let size = entry.size.unwrap_or(0);
                        DirEntry::File(FileEntry {
                            name,
                            digest: Digest::from_sha256_hex(&entry.sha),
                            size,
                            executable,
                            inline_content: None, // filled lazily
                        })
                    }
                    "tree" => DirEntry::Directory(DirectoryEntry {
                        name,
                        digest: Digest::from_sha256_hex(&entry.sha), // placeholder, recomputed
                    }),
                    // "commit" (submodule) and other unknown types: skip
                    _ => continue,
                }
            };

            dir_children.entry(parent_key).or_default().push(dir_entry);
        }

        // Build Directory objects with computed digests
        // Process in reverse depth order (deepest first) so parent digests incorporate child digests
        // Precompute depth to avoid O(n log n * path_length) scanning during sort
        let mut paths_with_depth: Vec<(usize, String)> = dir_children
            .keys()
            .map(|p| (p.matches('/').count(), p.clone()))
            .collect();
        // Sort deepest first; at same depth, longer paths first (ensures children before parents)
        paths_with_depth.sort_by(|(da, a), (db, b)| db.cmp(da).then_with(|| b.len().cmp(&a.len())));

        let mut directories: HashMap<String, Directory> = HashMap::new();
        let mut path_to_digest: HashMap<String, Digest> = HashMap::new();

        for (_, path) in &paths_with_depth {
            let mut entries = dir_children.remove(path).unwrap_or_default();

            // Sort entries by name for deterministic digests
            entries.sort_by(|a, b| a.name().cmp(b.name()));

            // Update directory entries with computed child digests
            for entry in &mut entries {
                if let DirEntry::Directory(ref mut de) = entry {
                    let child_path = if path.is_empty() {
                        de.name.clone()
                    } else {
                        format!("{}/{}", path, de.name)
                    };
                    if let Some(child_digest) = path_to_digest.get(&child_path) {
                        de.digest = child_digest.clone();
                    }
                }
            }

            let digest = Directory::compute_digest(&entries);
            let _ = path_to_digest.insert(path.clone(), digest.clone());

            let _ = directories.insert(
                path.clone(),
                Directory {
                    digest: digest.clone(),
                    entries,
                },
            );
        }

        let root_digest = path_to_digest
            .get("")
            .cloned()
            .unwrap_or_else(|| Digest::sha256(b"empty"));

        (root_digest, directories)
    }
}

#[async_trait]
impl Provider for GitHubProvider {
    async fn fetch_snapshot(&self, source: &SourceSpec) -> Result<Vec<u8>, CtxfsError> {
        debug!("fetching snapshot for {source}");

        // Record the source so later `fetch_blob` calls know which repo to hit.
        *self.active_source.lock().unwrap() = Some(source.clone());

        let commit_sha = self.resolve_ref(source).await?;
        debug!("resolved ref {} -> {}", source.version, commit_sha);

        let (owner, repo) = owner_repo(source)?;

        // Tier 2a: local tree cache
        if let Some(ref tc) = self.tree_cache {
            if let Some(data) = tc.get(owner, repo, &commit_sha) {
                debug!("tree cache HIT for {owner}/{repo}@{commit_sha}");
                return Ok(data);
            }
        }

        // Tier 2b: shared (Redis) tree cache
        if let Some(ref stc) = self.shared_tree_cache {
            if let Some(data) = stc.get_tree(owner, repo, &commit_sha).await {
                debug!("shared tree cache HIT for {owner}/{repo}@{commit_sha}");
                // Populate local cache
                if let Some(ref tc) = self.tree_cache {
                    let _ = tc.put(owner, repo, &commit_sha, &data);
                }
                return Ok(data);
            }
        }

        let tree = self.fetch_tree(source, &commit_sha).await?;
        debug!("fetched tree with {} entries", tree.tree.len());

        let (root_digest, directories) = Self::build_directories(&tree.tree, source);

        // Cache all directory objects
        for (path, dir) in &directories {
            let json = serde_json::to_vec(dir)
                .map_err(|e| CtxfsError::Manifest(format!("serialize directory: {e}")))?;
            self.cache.put(&dir.digest, &json)?;
            debug!("cached directory '{}' as {}", path, dir.digest);
        }

        let snapshot = Snapshot {
            source: source.to_string(),
            commit_sha,
            root_directory: root_digest,
            created_at: chrono::Utc::now().to_rfc3339(),
        };

        let json = serde_json::to_vec(&snapshot)
            .map_err(|e| CtxfsError::Manifest(format!("serialize snapshot: {e}")))?;

        // Store in tree caches for future lookups.
        if let Some(ref tc) = self.tree_cache {
            let _ = tc.put(owner, repo, &snapshot.commit_sha, &json);
        }
        if let Some(ref stc) = self.shared_tree_cache {
            stc.put_tree(owner, repo, &snapshot.commit_sha, &json).await;
        }

        Ok(json)
    }

    async fn fetch_directory(&self, digest: &Digest) -> Result<Vec<u8>, CtxfsError> {
        // Try cache first
        if let Some(data) = self.cache.get(digest) {
            return Ok(data);
        }

        Err(CtxfsError::NotFound(format!(
            "directory {digest} not in cache; re-fetch snapshot to populate"
        )))
    }

    async fn fetch_blob(&self, digest: &Digest) -> Result<Vec<u8>, CtxfsError> {
        if let Some(data) = self.cache.get(digest) {
            return Ok(data);
        }

        let source = self.active_source.lock().unwrap().clone().ok_or_else(|| {
            CtxfsError::Provider("fetch_blob called before fetch_snapshot; no active source".into())
        })?;

        let data = self.fetch_blob_content(&source, &digest.hex).await?;
        self.cache.put(digest, &data)?;
        Ok(data)
    }
}

fn parent_path(path: &str) -> Option<String> {
    let idx = path.rfind('/')?;
    Some(path[..idx].to_string())
}

fn file_name(path: &str) -> String {
    match path.rfind('/') {
        Some(idx) => path[idx + 1..].to_string(),
        None => path.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parent_path() {
        assert_eq!(parent_path("a/b/c"), Some("a/b".to_string()));
        assert_eq!(parent_path("a/b"), Some("a".to_string()));
        assert_eq!(parent_path("a"), None);
    }

    #[test]
    fn test_file_name() {
        assert_eq!(file_name("a/b/c.txt"), "c.txt");
        assert_eq!(file_name("readme.md"), "readme.md");
    }

    #[test]
    fn test_parent_path_root_level() {
        assert_eq!(parent_path("file.txt"), None);
    }

    #[test]
    fn test_parent_path_deep() {
        assert_eq!(parent_path("a/b/c/d/e.txt"), Some("a/b/c/d".to_string()));
    }

    #[test]
    fn test_file_name_deep_path() {
        assert_eq!(file_name("a/b/c/d/e.txt"), "e.txt");
    }

    #[test]
    fn build_directories_flat_repo() {
        let source = SourceSpec::parse("github:test/repo@main").unwrap();

        let entries = vec![
            TreeEntry {
                path: "README.md".to_string(),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: "abc123".to_string(),
                size: Some(100),
            },
            TreeEntry {
                path: "LICENSE".to_string(),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: "def456".to_string(),
                size: Some(200),
            },
        ];

        let (root_digest, directories) = GitHubProvider::build_directories(&entries, &source);
        assert!(!root_digest.hex.is_empty());

        // Should have one directory (root "")
        assert!(directories.contains_key(""));
        let root = &directories[""];
        assert_eq!(root.entries.len(), 2);
    }

    #[test]
    fn build_directories_nested_structure() {
        let source = SourceSpec::parse("github:test/repo@main").unwrap();

        let entries = vec![
            TreeEntry {
                path: "src".to_string(),
                mode: "040000".to_string(),
                entry_type: "tree".to_string(),
                sha: "tree1".to_string(),
                size: None,
            },
            TreeEntry {
                path: "src/main.rs".to_string(),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: "blob1".to_string(),
                size: Some(500),
            },
            TreeEntry {
                path: "src/lib.rs".to_string(),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: "blob2".to_string(),
                size: Some(300),
            },
            TreeEntry {
                path: "Cargo.toml".to_string(),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: "blob3".to_string(),
                size: Some(150),
            },
        ];

        let (root_digest, directories) = GitHubProvider::build_directories(&entries, &source);
        assert!(!root_digest.hex.is_empty());

        // Root should have: Cargo.toml (file) + src (dir)
        let root = &directories[""];
        assert_eq!(root.entries.len(), 2);

        // src/ should have: lib.rs + main.rs (sorted)
        let src = &directories["src"];
        assert_eq!(src.entries.len(), 2);
        assert_eq!(src.entries[0].name(), "lib.rs");
        assert_eq!(src.entries[1].name(), "main.rs");
    }

    #[test]
    fn build_directories_executable_files() {
        let source = SourceSpec::parse("github:test/repo@main").unwrap();

        let entries = vec![TreeEntry {
            path: "script.sh".to_string(),
            mode: "100755".to_string(),
            entry_type: "blob".to_string(),
            sha: "exec1".to_string(),
            size: Some(50),
        }];

        let (_root_digest, directories) = GitHubProvider::build_directories(&entries, &source);
        let root = &directories[""];
        if let DirEntry::File(f) = &root.entries[0] {
            assert!(f.executable);
        } else {
            panic!("expected file entry");
        }
    }

    #[test]
    fn build_directories_submodule_skipped() {
        let source = SourceSpec::parse("github:test/repo@main").unwrap();

        let entries = vec![
            TreeEntry {
                path: "vendor/lib".to_string(),
                mode: "160000".to_string(),
                entry_type: "commit".to_string(),
                sha: "submod1".to_string(),
                size: None,
            },
            TreeEntry {
                path: "README.md".to_string(),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: "blob1".to_string(),
                size: Some(10),
            },
        ];

        let (_root_digest, directories) = GitHubProvider::build_directories(&entries, &source);
        let root = &directories[""];
        // Only README.md should be present (submodule skipped)
        // Note: vendor dir may exist but is empty
        let file_count = root
            .entries
            .iter()
            .filter(|e| matches!(e, DirEntry::File(_)))
            .count();
        assert_eq!(file_count, 1);
    }

    #[test]
    fn build_directories_deterministic_digests() {
        let source = SourceSpec::parse("github:test/repo@main").unwrap();

        let entries = vec![
            TreeEntry {
                path: "a.txt".to_string(),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: "sha_a".to_string(),
                size: Some(10),
            },
            TreeEntry {
                path: "b.txt".to_string(),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: "sha_b".to_string(),
                size: Some(20),
            },
        ];

        let (d1, _) = GitHubProvider::build_directories(&entries, &source);
        let (d2, _) = GitHubProvider::build_directories(&entries, &source);
        assert_eq!(d1, d2);
    }

    #[test]
    fn build_directories_empty_repo() {
        let source = SourceSpec::parse("github:test/repo@main").unwrap();

        let entries: Vec<TreeEntry> = vec![];
        let (root_digest, directories) = GitHubProvider::build_directories(&entries, &source);

        assert!(!root_digest.hex.is_empty());
        let root = &directories[""];
        assert!(root.entries.is_empty());
    }

    #[test]
    fn classify_response_secondary_throttle_with_remaining_nonzero_returns_rate_limited() {
        use std::collections::HashMap;
        let mut headers = HashMap::new();
        let _ = headers.insert("retry-after".to_string(), "60".to_string());
        let _ = headers.insert("x-ratelimit-remaining".to_string(), "4500".to_string());
        let _ = headers.insert("x-ratelimit-resource".to_string(), "core".to_string());

        let err = GitHubProvider::classify_response(429, &headers).unwrap_err();
        match err {
            CtxfsError::RateLimited { retry_after_secs } => assert_eq!(retry_after_secs, 60),
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classify_response_primary_exhausted_returns_rate_limited() {
        use std::collections::HashMap;
        let mut headers = HashMap::new();
        let _ = headers.insert("x-ratelimit-remaining".to_string(), "0".to_string());
        let now = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let _ = headers.insert("x-ratelimit-reset".to_string(), (now + 120).to_string());

        let err = GitHubProvider::classify_response(403, &headers).unwrap_err();
        match err {
            CtxfsError::RateLimited { retry_after_secs } => {
                assert!(retry_after_secs > 100 && retry_after_secs <= 120);
            }
            other => panic!("expected RateLimited, got {other:?}"),
        }
    }

    #[test]
    fn classify_response_ok_returns_ok() {
        use std::collections::HashMap;
        let mut headers = HashMap::new();
        let _ = headers.insert("x-ratelimit-remaining".to_string(), "100".to_string());
        let _ = headers.insert("x-ratelimit-resource".to_string(), "core".to_string());
        assert!(GitHubProvider::classify_response(200, &headers).is_ok());
    }
}
