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

/// Hardcoded for now; M3+ may promote this to `pub` and make it configurable
/// via `CTXFS_GITHUB_HOST` for GHE support and tarball-redirect host
/// validation. No external consumer needs it today, so keep the surface tight.
pub(crate) const GITHUB_HOST: &str = "api.github.com";

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
    /// Set in `fetch_snapshot`. Pre-seeded with a `<resolving:ref>` placeholder
    /// commit BEFORE `resolve_ref` runs so that API call is attributed to this
    /// mount in `rest_calls_total`; replaced with the resolved commit SHA
    /// AFTER `resolve_ref` returns so all subsequent fetch_tree / prefetch /
    /// fetch_blob calls attribute to the real
    /// `(source, repo, commit, mount_id)` bucket. `None` only on a fresh
    /// provider instance before its first `fetch_snapshot` call.
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
        use ctxfs_provider_common::http::response_headers_map;
        use ctxfs_provider_common::rate_limit::{
            RateLimitVerdict, ResourceClass, ThrottleClassifier,
        };

        let status = resp.status().as_u16();
        let headers = response_headers_map(resp);

        // Hoist the counter_key clone so `record_rest_call` and the optional
        // throttle-event branch share one mutex acquisition per response.
        let key_for_counters = self.counter_key.lock().unwrap().clone();

        // Always increment rest_calls_total for quota-bearing GitHub API calls.
        // (Codeload tarball downloads aren't quota-bearing and don't go through here.)
        if let Some(ref key) = key_for_counters {
            self.observability
                .counters_for(key.clone())
                .record_rest_call();
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
            if let Some(key) = key_for_counters {
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

    /// Threshold: files ≤ this byte size are eligible for inline prefetch.
    /// Larger blobs go through the lazy per-read path.
    pub const SMALL_BLOB_THRESHOLD_BYTES: u64 = 4096;

    /// Maximum concurrent in-flight blob requests during prefetch.
    /// 8 is the GitHub-best-practices recommendation for batched fetches;
    /// higher concurrencies risk tripping secondary rate limits.
    /// See https://docs.github.com/en/rest/using-the-rest-api/best-practices-for-using-the-rest-api
    const PREFETCH_CONCURRENCY: usize = 8;

    /// Returns deduplicated SHAs for blob entries (regular or symlink)
    /// ≤ [`Self::SMALL_BLOB_THRESHOLD_BYTES`]. Trees and submodules are
    /// excluded. Symlinks are size-checked because hostile remotes could
    /// otherwise send oversized "symlink" blobs (legitimate git symlinks
    /// are always < PATH_MAX, well under 4 KB) to bypass the cap.
    /// Result is sorted for deterministic ordering.
    fn small_blob_shas(entries: &[TreeEntry]) -> Vec<String> {
        use std::collections::BTreeSet;
        let mut seen = BTreeSet::new();
        for e in entries {
            if e.entry_type != "blob" {
                continue;
            }
            // Apply the size threshold uniformly to regular blobs and
            // symlinks. Without the size check on symlinks, a hostile
            // remote could send a 5 MB "symlink" blob and force us to
            // fetch + inline the entire payload as the link target.
            let is_small = e
                .size
                .is_some_and(|s| s <= Self::SMALL_BLOB_THRESHOLD_BYTES);
            if is_small {
                let _ = seen.insert(e.sha.clone());
            }
        }
        seen.into_iter().collect()
    }

    /// Identifies blob SHAs that come from symlink (mode-120000) entries.
    /// Used by `prefetch_small_blobs` to apply the strict-failure policy to
    /// symlinks (which have no lazy fallback in the read path: `readlink`
    /// just returns the stored target string).
    fn symlink_shas(entries: &[TreeEntry]) -> std::collections::HashSet<String> {
        entries
            .iter()
            .filter(|e| e.entry_type == "blob" && e.mode == MODE_SYMLINK)
            .map(|e| e.sha.clone())
            .collect()
    }

    /// Fetches blob SHAs in `shas` in parallel (capped at
    /// [`Self::PREFETCH_CONCURRENCY`]) and returns a map sha → bytes.
    ///
    /// Failure policy:
    /// - Files (non-symlink): per-blob errors are logged and the
    ///   `prefetch_failures` counter is incremented; the SHA is omitted from
    ///   the map; the caller falls back to lazy fetch on read.
    /// - Symlinks (SHA in `symlink_shas` set): per-blob errors **fail the
    ///   entire prefetch** and propagate as the returned error. Symlinks have
    ///   no lazy provider path (readlink returns the stored target string),
    ///   so an empty target would be a silent data-correctness regression.
    async fn prefetch_small_blobs(
        &self,
        source: &SourceSpec,
        shas: Vec<String>,
        symlink_shas: &std::collections::HashSet<String>,
    ) -> Result<std::collections::HashMap<String, Vec<u8>>, CtxfsError> {
        use futures::stream::{FuturesUnordered, StreamExt};

        let mut results: std::collections::HashMap<String, Vec<u8>> =
            std::collections::HashMap::new();
        let mut in_flight = FuturesUnordered::new();
        let mut iter = shas.into_iter();

        // Prime the queue with up to PREFETCH_CONCURRENCY in-flight requests.
        for _ in 0..Self::PREFETCH_CONCURRENCY {
            if let Some(sha) = iter.next() {
                in_flight.push(self.fetch_blob_with_sha(source, sha));
            }
        }

        while let Some((sha, result)) = in_flight.next().await {
            match result {
                Ok(bytes) => {
                    let _ = results.insert(sha, bytes);
                }
                Err(e) => {
                    // Symlink: fail the prefetch (no lazy fallback for readlink).
                    if symlink_shas.contains(&sha) {
                        // Note: in-flight `FuturesUnordered` futures are dropped
                        // here. Their connections may have hit GitHub already
                        // (consuming quota) but won't tick `rest_calls_total`
                        // because they never reach `check_rate_limit`. Acceptable
                        // for M2 (symlink-fail path is rare); revisit in M3+ if
                        // telemetry shows meaningful undercount.
                        return Err(CtxfsError::Provider(format!(
                            "symlink prefetch failed for sha {sha}: {e}"
                        )));
                    }
                    // File: log + counter + skip; lazy fetch will retry on read.
                    if let Some(key) = self.counter_key.lock().unwrap().clone() {
                        self.observability
                            .counters_for(key)
                            .record_prefetch_failure();
                    }
                    tracing::warn!(
                        target: "ctxfs.provider.fetch",
                        sha = sha.as_str(),
                        error = format!("{e:?}").as_str(),
                        "prefetch_small_blobs: per-file fetch failed; falling back to lazy"
                    );
                }
            }
            if let Some(next_sha) = iter.next() {
                in_flight.push(self.fetch_blob_with_sha(source, next_sha));
            }
        }
        Ok(results)
    }

    /// Wrapper that pairs the SHA with the fetch result, so the caller can map
    /// back after `FuturesUnordered` completes them out of order.
    async fn fetch_blob_with_sha(
        &self,
        source: &SourceSpec,
        sha: String,
    ) -> (String, Result<Vec<u8>, CtxfsError>) {
        let result = self.fetch_blob_content(source, &sha).await;
        (sha, result)
    }

    /// Build directory tree from flat GitHub tree entries (no inline content).
    ///
    /// Backward-compat wrapper for callers that don't have a prefetched-blob
    /// map. `FileEntry::inline_content` is left `None` and `SymlinkEntry::target`
    /// is left empty; the read path will fetch lazily.
    pub fn build_directories(
        entries: &[TreeEntry],
        source: &SourceSpec,
    ) -> (Digest, HashMap<String, Directory>) {
        Self::build_directories_inner(entries, source, None)
    }

    /// Like [`Self::build_directories`], but populates `FileEntry::inline_content`
    /// for blobs ≤ [`Self::SMALL_BLOB_THRESHOLD_BYTES`] whose SHA appears in
    /// `inline`, and decodes `SymlinkEntry::target` from the same map for
    /// mode-120000 entries.
    ///
    /// Files (B1): the size guard inside `build_directories_inner` prevents a
    /// misbuilt map from accidentally inlining a >4 KB blob even if the caller
    /// places larger bytes in the map.
    ///
    /// Symlinks (B7): no size guard — symlinks are always small in practice,
    /// and the prefetch path's strict-on-symlink failure policy ensures the
    /// map already contains the target before this function runs in production.
    pub fn build_directories_with_inline(
        entries: &[TreeEntry],
        source: &SourceSpec,
        inline: &std::collections::HashMap<String, Vec<u8>>,
    ) -> (Digest, HashMap<String, Directory>) {
        Self::build_directories_inner(entries, source, Some(inline))
    }

    /// Shared implementation behind [`Self::build_directories`] and
    /// [`Self::build_directories_with_inline`]. When `inline` is `Some`, file
    /// entries ≤ [`Self::SMALL_BLOB_THRESHOLD_BYTES`] whose SHA appears in the
    /// map get `inline_content` populated, and symlink entries decode their
    /// target from the same map. When `inline` is `None`, behavior matches the
    /// pre-M2 path: empty target, no inline content.
    fn build_directories_inner(
        entries: &[TreeEntry],
        _source: &SourceSpec,
        inline: Option<&std::collections::HashMap<String, Vec<u8>>>,
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
                let target = match inline.and_then(|m| m.get(&entry.sha)) {
                    Some(bytes) => match std::str::from_utf8(bytes) {
                        Ok(s) => s.to_string(),
                        Err(e) => {
                            // Defensive: real-world git symlinks are always
                            // valid UTF-8. If we ever see otherwise, log and
                            // fall through to empty target rather than crashing
                            // the snapshot build.
                            tracing::warn!(
                                target: "ctxfs.provider.fetch",
                                path = entry.path.as_str(),
                                sha = entry.sha.as_str(),
                                error = format!("{e:?}").as_str(),
                                "symlink target bytes are not valid UTF-8; using empty target"
                            );
                            String::new()
                        }
                    },
                    None => String::new(),
                };
                DirEntry::Symlink(SymlinkEntry { name, target })
            } else {
                match entry.entry_type.as_str() {
                    "blob" => {
                        let executable = entry.mode == MODE_EXECUTABLE;
                        let size = entry.size.unwrap_or(0);
                        // Size guard: only inline if the entry's recorded size
                        // is ≤ the threshold. Prevents a misbuilt map from
                        // sneaking a large blob into the manifest.
                        let inline_content = inline
                            .filter(|_| size <= Self::SMALL_BLOB_THRESHOLD_BYTES)
                            .and_then(|m| m.get(&entry.sha))
                            .cloned();
                        DirEntry::File(FileEntry {
                            name,
                            digest: Digest::from_sha256_hex(&entry.sha),
                            size,
                            executable,
                            inline_content,
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

        // 1. Pre-seed counter_key with a "<resolving:ref>" placeholder commit
        //    so the upcoming `resolve_ref` API call is attributed to this
        //    mount in `rest_calls_total`. Without this, resolve_ref runs with
        //    counter_key=None and the API call is silently un-counted.
        //
        //    The placeholder bucket is filtered out of `ctxfs status` mount
        //    summaries via `Observability::status_report`; the per-key
        //    telemetry counter still accumulates for full fidelity.
        *self.counter_key.lock().unwrap() = Some(CounterKey {
            source: source.provider_type.to_string(),
            repo: source.name.clone(),
            commit: format!("<resolving:{}>", source.version),
            mount_id: source.id(),
        });

        // 2. Resolve the ref to a concrete commit sha.
        let commit_sha = self.resolve_ref(source).await?;
        debug!("resolved ref {} -> {}", source.version, commit_sha);

        // 3. Replace counter_key with the resolved commit sha now that we
        //    know it. All subsequent fetch_tree / prefetch / fetch_blob calls
        //    attribute to the real (source, repo, commit, mount_id) bucket.
        *self.counter_key.lock().unwrap() = Some(CounterKey {
            source: source.provider_type.to_string(),
            repo: source.name.clone(),
            commit: commit_sha.clone(),
            mount_id: source.id(),
        });

        let (owner, repo) = owner_repo(source)?;

        // Tier 2a: local tree cache. counter_key is already set (step 3) so
        // any subsequent `fetch_blob` calls on the cached snapshot attribute
        // correctly to the real commit bucket.
        if let Some(ref tc) = self.tree_cache {
            if let Some(data) = tc.get(owner, repo, &commit_sha) {
                debug!("tree cache HIT for {owner}/{repo}@{commit_sha}");
                return Ok(data);
            }
        }

        // Tier 2b: shared (Redis) tree cache.
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

        // 4. Fetch tree.
        let tree = self.fetch_tree(source, &commit_sha).await?;
        debug!("fetched tree with {} entries", tree.tree.len());

        // 5. Prefetch small blobs + symlink targets for B1/B7 inlining.
        //    Skip the call entirely if no entries are eligible (avoids a
        //    no-op futures-stream construction).
        let symlink_set = Self::symlink_shas(&tree.tree);
        let small_shas = Self::small_blob_shas(&tree.tree);
        let inline = if small_shas.is_empty() {
            std::collections::HashMap::new()
        } else {
            self.prefetch_small_blobs(source, small_shas, &symlink_set)
                .await?
        };

        // 6. Record prefetch_hits per successfully prefetched blob.
        if let Some(key) = self.counter_key.lock().unwrap().clone() {
            self.observability
                .counters_for(key)
                .record_prefetch_hits(inline.len() as u64);
        }

        // 7. Build directories with inline content (B1) and resolved
        //    symlink targets (B7).
        let (root_digest, directories) =
            Self::build_directories_with_inline(&tree.tree, source, &inline);

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

    /// Test helper: produces a stable `SourceSpec` that build_directories tests
    /// can pass without re-parsing the same string at each call site.
    fn make_test_source() -> SourceSpec {
        SourceSpec::parse("github:test/repo@main").unwrap()
    }

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

    #[test]
    fn small_blobs_filter_picks_under_4kb_files_and_symlinks() {
        let entries = vec![
            TreeEntry {
                path: "a.rs".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "aaa".into(),
                size: Some(100),
            },
            TreeEntry {
                path: "big.bin".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "bbb".into(),
                size: Some(10_000),
            },
            TreeEntry {
                path: "link".into(),
                mode: "120000".into(),
                entry_type: "blob".into(),
                sha: "ccc".into(),
                size: Some(20),
            },
            // Adversarial: a hostile remote could declare a symlink with a
            // 5 MB body. small_blob_shas must exclude it because the prefetch
            // path would otherwise inline 5 MB as the link target.
            TreeEntry {
                path: "huge_link".into(),
                mode: "120000".into(),
                entry_type: "blob".into(),
                sha: "huge".into(),
                size: Some(5_000_000),
            },
            TreeEntry {
                path: "subtree".into(),
                mode: "040000".into(),
                entry_type: "tree".into(),
                sha: "ddd".into(),
                size: None,
            },
            TreeEntry {
                path: "dup.rs".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "aaa".into(),
                size: Some(100),
            },
        ];
        let shas = GitHubProvider::small_blob_shas(&entries);
        // "aaa" (small file, dup eliminated), "ccc" (small symlink). NOT
        // included: "bbb" (size > 4 KB), "huge" (adversarial 5 MB symlink),
        // "ddd" (tree, not a blob).
        assert_eq!(shas, vec!["aaa".to_string(), "ccc".to_string()]);
    }

    /// With no shas, no HTTP is performed and the returned map is empty.
    /// Behavioral test: `fetch_snapshot` skips the prefetch call entirely
    /// when `small_blob_shas` is empty, so this is mostly defense-in-depth,
    /// but it still locks down the contract that the helper is a no-op for
    /// empty input regardless of provider state.
    #[tokio::test]
    async fn prefetch_small_blobs_empty_shas_returns_empty_map_without_http() {
        let cache_dir = tempfile::tempdir().unwrap();
        let cache =
            Arc::new(ctxfs_cache::BlobCache::new(cache_dir.path().to_path_buf(), 1024).unwrap());
        let provider = GitHubProvider::new(None, cache, None, None, Arc::new(Observability::new()));
        let source = SourceSpec::parse("github:test/repo@main").unwrap();
        let result = provider
            .prefetch_small_blobs(&source, vec![], &std::collections::HashSet::new())
            .await
            .unwrap();
        assert!(result.is_empty());
    }

    #[test]
    fn symlink_shas_picks_only_mode_120000_blobs() {
        let entries = vec![
            TreeEntry {
                path: "regular.rs".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "aaa".into(),
                size: Some(100),
            },
            TreeEntry {
                path: "link1".into(),
                mode: "120000".into(),
                entry_type: "blob".into(),
                sha: "lnk1".into(),
                size: Some(20),
            },
            TreeEntry {
                path: "exec.sh".into(),
                mode: "100755".into(),
                entry_type: "blob".into(),
                sha: "exe".into(),
                size: Some(50),
            },
            TreeEntry {
                path: "submod".into(),
                mode: "160000".into(),
                entry_type: "commit".into(),
                sha: "mod".into(),
                size: None,
            },
        ];
        let set = GitHubProvider::symlink_shas(&entries);
        assert_eq!(set.len(), 1);
        assert!(set.contains("lnk1"));
    }

    #[test]
    fn build_directories_inlines_small_files_when_map_provided() {
        let source = make_test_source();
        let entries = vec![
            TreeEntry {
                path: "small.txt".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "abc".into(),
                size: Some(10),
            },
            TreeEntry {
                path: "big.bin".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "def".into(),
                size: Some(99_999),
            },
        ];
        let mut inline = std::collections::HashMap::new();
        let _ = inline.insert("abc".to_string(), b"hello!".to_vec());

        let (_, dirs) = GitHubProvider::build_directories_with_inline(&entries, &source, &inline);
        let root = dirs.get("").unwrap();
        let small_entry = root
            .entries
            .iter()
            .find(|e| e.name() == "small.txt")
            .unwrap();
        let big_entry = root.entries.iter().find(|e| e.name() == "big.bin").unwrap();

        match small_entry {
            DirEntry::File(f) => assert_eq!(f.inline_content, Some(b"hello!".to_vec())),
            _ => panic!("expected file"),
        }
        match big_entry {
            DirEntry::File(f) => assert_eq!(f.inline_content, None),
            _ => panic!("expected file"),
        }
    }

    #[test]
    fn build_directories_resolves_symlink_target_from_inline_map() {
        let source = make_test_source();
        let entries = vec![TreeEntry {
            path: "link".into(),
            mode: "120000".into(),
            entry_type: "blob".into(),
            sha: "lnk".into(),
            size: Some(13),
        }];
        let mut inline = std::collections::HashMap::new();
        let _ = inline.insert("lnk".to_string(), b"path/to/target".to_vec());

        let (_, dirs) = GitHubProvider::build_directories_with_inline(&entries, &source, &inline);
        let root = dirs.get("").unwrap();
        let link_entry = root.entries.iter().find(|e| e.name() == "link").unwrap();
        match link_entry {
            DirEntry::Symlink(s) => assert_eq!(s.target, "path/to/target"),
            _ => panic!("expected symlink"),
        }
    }

    #[test]
    fn build_directories_size_guard_excludes_large_blob_even_if_in_map() {
        // Even if the inline map mistakenly contains bytes for a >4 KB SHA,
        // the size guard in build_directories_inner short-circuits BEFORE
        // the map lookup, so inline_content stays None. This locks down
        // the size-guard contract that the docs promise.
        let source = make_test_source();
        let entries = vec![TreeEntry {
            path: "big.bin".into(),
            mode: "100644".into(),
            entry_type: "blob".into(),
            sha: "def".into(),
            size: Some(99_999),
        }];
        let mut inline = std::collections::HashMap::new();
        let _ = inline.insert("def".to_string(), b"this should NOT be inlined".to_vec());

        let (_, dirs) = GitHubProvider::build_directories_with_inline(&entries, &source, &inline);
        let root = dirs.get("").unwrap();
        let big_entry = root.entries.iter().find(|e| e.name() == "big.bin").unwrap();
        match big_entry {
            DirEntry::File(f) => assert_eq!(
                f.inline_content, None,
                "size guard should exclude >4 KB blobs even if the map has bytes"
            ),
            _ => panic!("expected file"),
        }
    }

    #[test]
    fn build_directories_without_inline_keeps_target_empty_and_no_inline_content() {
        // Backward-compat: existing build_directories(...) with no inline map
        // produces empty target and no inline_content.
        let source = make_test_source();
        let entries = vec![
            TreeEntry {
                path: "small.txt".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "abc".into(),
                size: Some(10),
            },
            TreeEntry {
                path: "link".into(),
                mode: "120000".into(),
                entry_type: "blob".into(),
                sha: "lnk".into(),
                size: Some(13),
            },
        ];
        let (_, dirs) = GitHubProvider::build_directories(&entries, &source);
        let root = dirs.get("").unwrap();
        let small_entry = root
            .entries
            .iter()
            .find(|e| e.name() == "small.txt")
            .unwrap();
        let link_entry = root.entries.iter().find(|e| e.name() == "link").unwrap();

        match small_entry {
            DirEntry::File(f) => assert!(f.inline_content.is_none()),
            _ => panic!("expected file"),
        }
        match link_entry {
            DirEntry::Symlink(s) => assert_eq!(s.target, ""),
            _ => panic!("expected symlink"),
        }
    }
}
