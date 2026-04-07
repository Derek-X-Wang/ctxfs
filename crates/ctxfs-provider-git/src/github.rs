use async_trait::async_trait;
use base64::Engine;
use ctxfs_cache::BlobCache;
use ctxfs_core::error::CtxfsError;
use ctxfs_core::provider::Provider;
use ctxfs_core::source::SourceSpec;
use ctxfs_core::Digest;
use ctxfs_manifest::{DirEntry, Directory, DirectoryEntry, FileEntry, Snapshot, SymlinkEntry};
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION};
use serde::de::DeserializeOwned;
use serde::Deserialize;
// sha2 used indirectly via ctxfs_core::Digest
use std::collections::HashMap;
use std::sync::Arc;
use tracing::{debug, warn};

const USER_AGENT_STR: &str = "ctxfs/0.1";

// Git file mode constants from the GitHub Trees API
const MODE_SYMLINK: &str = "120000";
const MODE_EXECUTABLE: &str = "100755";

pub struct GitHubProvider {
    client: reqwest::Client,
    cache: Arc<BlobCache>,
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

impl GitHubProvider {
    pub fn new(token: Option<&str>, cache: Arc<BlobCache>) -> Self {
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
            active_source: std::sync::Mutex::new(None),
        }
    }

    fn api_url(owner: &str, repo: &str, path: &str) -> String {
        format!("https://api.github.com/repos/{owner}/{repo}/{path}")
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

        Self::check_rate_limit(&resp)?;

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
        let url = Self::api_url(
            &source.owner,
            &source.repo,
            &format!("commits/{}", source.git_ref),
        );

        let commit: CommitResponse = self
            .get_json(&url, &format!("resolve ref '{}'", source.git_ref))
            .await?;

        Ok(commit.sha)
    }

    async fn fetch_tree(
        &self,
        source: &SourceSpec,
        tree_sha: &str,
    ) -> Result<TreeResponse, CtxfsError> {
        let url = Self::api_url(
            &source.owner,
            &source.repo,
            &format!("git/trees/{tree_sha}?recursive=1"),
        );

        let tree: TreeResponse = self.get_json(&url, "fetch tree").await?;

        if tree.truncated {
            warn!(
                "tree response was truncated for {}/{}; large repos may be incomplete",
                source.owner, source.repo
            );
        }

        Ok(tree)
    }

    async fn fetch_blob_content(
        &self,
        source: &SourceSpec,
        sha: &str,
    ) -> Result<Vec<u8>, CtxfsError> {
        let url = Self::api_url(&source.owner, &source.repo, &format!("git/blobs/{sha}"));

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

    fn check_rate_limit(resp: &reqwest::Response) -> Result<(), CtxfsError> {
        if resp.status() == reqwest::StatusCode::FORBIDDEN
            || resp.status() == reqwest::StatusCode::TOO_MANY_REQUESTS
        {
            let remaining = resp
                .headers()
                .get("x-ratelimit-remaining")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.parse::<u64>().ok())
                .unwrap_or(0);

            if remaining == 0 {
                let reset = resp
                    .headers()
                    .get("x-ratelimit-reset")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.parse::<u64>().ok())
                    .unwrap_or(0);

                let now = std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_secs();

                let retry_after = if reset > now { reset - now } else { 60 };

                return Err(CtxfsError::RateLimited {
                    retry_after_secs: retry_after,
                });
            }
        }
        Ok(())
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
        debug!("resolved ref {} -> {}", source.git_ref, commit_sha);

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
}
