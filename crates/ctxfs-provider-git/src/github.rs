use async_trait::async_trait;
use base64::Engine;
use ctxfs_cache::{BlobCache, SharedTreeCache, TreeCache};
use ctxfs_core::error::CtxfsError;
use ctxfs_core::provider::Provider;
use ctxfs_core::source::SourceSpec;
use ctxfs_core::Digest;
use ctxfs_manifest::{DirEntry, Directory, DirectoryEntry, FileEntry, Snapshot, SymlinkEntry};
use ctxfs_provider_common::counters::CounterKey;
use ctxfs_provider_common::fetcher::{SlotClaim, TarballKey, TarballSingleflightMap};
use ctxfs_provider_common::observability::Observability;
use ctxfs_provider_common::rate_limit::AuthIdentity;
use reqwest::header::{HeaderMap, ACCEPT, AUTHORIZATION};
use serde::de::DeserializeOwned;
use serde::Deserialize;
use std::collections::HashMap;
use std::sync::Arc;
use tar::EntryType;
use tracing::{debug, warn};

const USER_AGENT_STR: &str = "ctxfs/0.1";

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
    /// GitHub API host (e.g. `api.github.com` for public GitHub; the configured
    /// `CTXFS_GITHUB_HOST` value for GHE deployments). Used in `api_url`,
    /// `AuthIdentity`, and redirect-target validation.
    api_host: String,
    /// Codeload host derived from `api_host` (or explicitly overridden via
    /// [`Self::new_with_codeload_host`]). Tarball 302 redirects must land on
    /// this host. Override is used by Task 8 replay tests that point both the
    /// API and codeload at a local mock server.
    /// Task 7 wires `fetch_tarball_into_cache` from `dispatch_fetch_policy`.
    codeload_host: String,
    /// HTTP client used for the codeload-host hop in the tarball flow.
    /// Built once at construction time with NO default headers (so the
    /// Authorization header used for api.github.com calls cannot leak to
    /// codeload). Has redirect::Policy::none() too — we control the chain.
    codeload_client: reqwest::Client,
    /// Daemon-side singleflight registry for in-flight tarball prefetches.
    /// Shared across all mounts via `Arc`; per-mount providers are still
    /// constructed fresh in `prepare_mount` (B8 constraint). Two concurrent
    /// mounts of the same `(host, owner, repo, commit)` await the same
    /// `OnceCell` so only one tarball download happens.
    tarball_singleflight: Arc<TarballSingleflightMap>,
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

/// Outcome of one tarball extraction. Returned to the caller for telemetry
/// and auto-gate-fallback decisions.
#[derive(Debug, Default)]
pub(crate) struct TarballOutcome {
    pub blobs_committed: u64,
    pub blobs_skipped_invalid: u64,
    pub blobs_skipped_digest: u64,
    pub total_bytes: u64,
}

/// Incremental Git-blob SHA-1 hasher. Computes `sha1("blob <size>\0" || content)`
/// in streaming fashion so we never buffer a whole entry in memory.
///
/// Feed bytes via [`Self::update`]; call [`Self::finalize_hex`] once to get the
/// 40-char hex digest. Size header is emitted lazily on the first `update` call.
pub(crate) struct GitBlobSha1 {
    h: sha1::Sha1,
    size_written: u64,
    size_header_emitted: bool,
    expected_size: u64,
}

impl std::fmt::Debug for GitBlobSha1 {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitBlobSha1")
            .field("size_written", &self.size_written)
            .field("expected_size", &self.expected_size)
            .finish()
    }
}

impl GitBlobSha1 {
    pub fn new(expected_size: u64) -> Self {
        use sha1::Digest as _;
        let h = sha1::Sha1::new();
        Self {
            h,
            size_written: 0,
            size_header_emitted: false,
            expected_size,
        }
    }

    pub fn update(&mut self, bytes: &[u8]) {
        use sha1::Digest as _;
        if !self.size_header_emitted {
            self.h
                .update(format!("blob {}", self.expected_size).as_bytes());
            self.h.update(b"\0");
            self.size_header_emitted = true;
        }
        self.h.update(bytes);
        self.size_written += bytes.len() as u64;
    }

    pub fn finalize_hex(self) -> String {
        use sha1::Digest as _;
        let mut h = self.h;
        if !self.size_header_emitted {
            h.update(format!("blob {}", self.expected_size).as_bytes());
            h.update(b"\0");
        }
        hex::encode(h.finalize())
    }
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

/// Per-mount fetch options passed to [`GitHubProvider::fetch_snapshot_with_options`].
///
/// The existing [`ctxfs_core::provider::Provider::fetch_snapshot`] trait method
/// delegates to `fetch_snapshot_inner(source, &FetchOptions::default())` so all
/// callers that don't explicitly opt in to M3 behaviour (`Daemon::prepare_mount`,
/// NFS tests, FSKit paths) remain unchanged.
#[derive(Debug, Clone)]
pub struct FetchOptions {
    /// How aggressively to prefetch blobs via the tarball endpoint.
    pub prefetch: ctxfs_provider_common::fetcher::PrefetchPolicy,
    /// Minimum blob count for the auto-gate to fire (ignored when
    /// `prefetch == Force`).
    pub prefetch_threshold_count: u64,
    /// Maximum estimated bytes for the auto-gate to approve tarball (ignored
    /// when `prefetch == Force`).
    pub prefetch_max_bytes: u64,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            // Disabled so non-daemon callers (NFS tests, FSKit, etc.) keep
            // pre-M3 lazy-fetch behaviour unchanged.
            prefetch: ctxfs_provider_common::fetcher::PrefetchPolicy::Disabled,
            prefetch_threshold_count: 30,
            prefetch_max_bytes: 256 * 1024 * 1024,
        }
    }
}

impl GitHubProvider {
    /// Production constructor. Derives the codeload host automatically from
    /// `api_host` (e.g. `api.github.com` → `codeload.github.com`).
    ///
    /// `tarball_singleflight` is the daemon-level registry shared across
    /// concurrent mounts so only one tarball download happens per
    /// `(host, owner, repo, commit)` at a time. Pass
    /// `Arc::new(dashmap::DashMap::new())` in tests that don't exercise the
    /// singleflight path.
    pub fn new(
        token: Option<&str>,
        api_host: String,
        cache: Arc<BlobCache>,
        tree_cache: Option<Arc<TreeCache>>,
        shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
        observability: Arc<Observability>,
        tarball_singleflight: Arc<TarballSingleflightMap>,
    ) -> Self {
        Self::new_with_codeload_host(
            token,
            api_host,
            None,
            cache,
            tree_cache,
            shared_tree_cache,
            observability,
            tarball_singleflight,
        )
    }

    /// Construct with an explicit codeload host override. Production code
    /// calls [`Self::new`] which derives the codeload host from `api_host`.
    /// Primarily for tests (e.g. Task 8 replay tests) that need both API
    /// calls and tarball redirects to point at a local mock server.
    #[allow(clippy::too_many_arguments)] // M4 will collapse via ProviderContext.
    pub fn new_with_codeload_host(
        token: Option<&str>,
        api_host: String,
        codeload_host_override: Option<String>,
        cache: Arc<BlobCache>,
        tree_cache: Option<Arc<TreeCache>>,
        shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
        observability: Arc<Observability>,
        tarball_singleflight: Arc<TarballSingleflightMap>,
    ) -> Self {
        let auth_identity = match token {
            Some(t) => AuthIdentity::pat(&api_host, t),
            None => AuthIdentity::anonymous(&api_host),
        };

        let mut default_headers = HeaderMap::new();
        let _ = default_headers.insert(ACCEPT, "application/vnd.github.v3+json".parse().unwrap());
        if let Some(token) = token {
            let _ =
                default_headers.insert(AUTHORIZATION, format!("Bearer {token}").parse().unwrap());
        }

        // Build the client with redirect::Policy::none() so reqwest does NOT
        // auto-follow the tarball 302 with the Authorization header attached.
        // Manual redirect handling (host whitelist, Authorization strip, depth
        // ≤ 3) lives in fetch_tarball_into_cache. Non-tarball REST endpoints
        // (commits, trees, blobs) don't redirect in practice; a stray 3xx on
        // those paths returns an HTTP-status error from get_json — which is the
        // right behavior for unhandled redirects.
        let client = reqwest::Client::builder()
            .user_agent(USER_AGENT_STR)
            .default_headers(default_headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build HTTP client");

        let codeload_client = reqwest::Client::builder()
            .user_agent(USER_AGENT_STR)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .expect("failed to build codeload HTTP client");

        let codeload_host =
            codeload_host_override.unwrap_or_else(|| Self::codeload_host_for(&api_host));

        Self {
            client,
            cache,
            tree_cache,
            shared_tree_cache,
            observability,
            auth_identity,
            api_host,
            codeload_host,
            codeload_client,
            tarball_singleflight,
            counter_key: std::sync::Mutex::new(None),
            active_source: std::sync::Mutex::new(None),
        }
    }

    fn api_url(&self, owner: &str, repo: &str, path: &str) -> String {
        // If `api_host` already embeds a scheme (test-only: `http://127.0.0.1:PORT`)
        // use it as-is so replay tests can point the provider at a local HTTP server.
        // Production always passes a bare hostname (e.g. `api.github.com`), which
        // gets the `https://` prefix applied here.
        if self.api_host.starts_with("http://") || self.api_host.starts_with("https://") {
            format!("{}/repos/{owner}/{repo}/{path}", self.api_host)
        } else {
            format!("https://{}/repos/{owner}/{repo}/{path}", self.api_host)
        }
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
        let url = self.api_url(owner, repo, &format!("commits/{}", source.version));

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
        let url = self.api_url(owner, repo, &format!("git/trees/{tree_sha}?recursive=1"));
        self.get_json(&url, "fetch tree").await
    }

    /// Walk a tree by calling `get_subtree` per subtree SHA (closure-injected
    /// so the pure path-prefixing logic can be unit-tested without HTTP).
    /// Iterative DFS — bounded stack on adversarial repos (e.g. a maliciously
    /// deep symlink chain from B7).
    ///
    /// Returns all entries flattened with path-prefixes, matching the shape of
    /// a `recursive=1` response.
    ///
    /// Test-only: production uses `fetch_tree_walked` which drives the DFS
    /// asynchronously via `fetch_subtree`.
    ///
    /// **If you change the DFS loop body, mirror the change in `fetch_tree_walked`.**
    #[cfg(test)]
    fn assemble_walked_tree<F>(root_sha: &str, mut get_subtree: F) -> Vec<TreeEntry>
    where
        F: FnMut(&str) -> Vec<TreeEntry>,
    {
        let mut out = Vec::new();
        let mut stack: Vec<(String, String)> = vec![(root_sha.to_string(), String::new())];
        while let Some((sha, prefix)) = stack.pop() {
            for entry in get_subtree(&sha) {
                let prefixed = if prefix.is_empty() {
                    entry.path.clone()
                } else {
                    format!("{prefix}/{}", entry.path)
                };
                let mut owned = entry.clone();
                owned.path = prefixed.clone();
                if entry.entry_type == "tree" {
                    stack.push((entry.sha.clone(), prefixed));
                }
                out.push(owned);
            }
        }
        out
    }

    /// Fetch a single tree without recursion. Used by the B2 fallback path.
    async fn fetch_subtree(
        &self,
        source: &SourceSpec,
        tree_sha: &str,
    ) -> Result<Vec<TreeEntry>, CtxfsError> {
        let (owner, repo) = owner_repo(source)?;
        let url = self.api_url(owner, repo, &format!("git/trees/{tree_sha}"));
        let tree: TreeResponse = self.get_json(&url, "fetch subtree").await?;
        Ok(tree.tree)
    }

    /// B2 fallback: when `fetch_tree` returns `truncated=true`, walk
    /// per-directory to assemble a complete manifest. Increments the
    /// `truncated_tree_fallbacks` counter once per fallback fire.
    ///
    /// Note: per-subtree responses include `size` for blob entries (per
    /// GitHub Trees API docs). If an entry returns `size=None`, the auto-gate
    /// in `dispatch_fetch_policy` (Task 7) treats the manifest as having
    /// unknown total bytes and falls back to Lazy (fail-closed). Don't try to
    /// estimate by file extension or cache mtimes — be honest about the
    /// missing signal.
    async fn fetch_tree_walked(
        &self,
        source: &SourceSpec,
        root_tree_sha: &str,
    ) -> Result<Vec<TreeEntry>, CtxfsError> {
        if let Some(key) = self.counter_key.lock().unwrap().clone() {
            self.observability
                .counters_for(key)
                .record_truncated_tree_fallback();
        }
        warn!(
            target: "ctxfs.provider.tree",
            root_sha = root_tree_sha,
            "tree response was truncated; falling back to per-directory walk"
        );

        // Iterative DFS; one HTTP call per subtree. Mirrors assemble_walked_tree
        // but drives each subtree fetch asynchronously.
        let mut out = Vec::new();
        let mut stack: Vec<(String, String)> = vec![(root_tree_sha.to_string(), String::new())];
        while let Some((sha, prefix)) = stack.pop() {
            let subtree = self.fetch_subtree(source, &sha).await?;
            for entry in subtree {
                let prefixed = if prefix.is_empty() {
                    entry.path.clone()
                } else {
                    format!("{prefix}/{}", entry.path)
                };
                let mut owned = entry.clone();
                owned.path = prefixed.clone();
                if entry.entry_type == "tree" {
                    stack.push((entry.sha.clone(), prefixed));
                }
                out.push(owned);
            }
        }
        Ok(out)
    }

    async fn fetch_blob_content(
        &self,
        source: &SourceSpec,
        sha: &str,
    ) -> Result<Vec<u8>, CtxfsError> {
        let (owner, repo) = owner_repo(source)?;
        let url = self.api_url(owner, repo, &format!("git/blobs/{sha}"));

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

    // ---- Tarball auto-gate + singleflight dispatch ----

    /// Run the auto-gate and, if it elects tarball, fetch it (with singleflight
    /// dedupe + cache pre-check). Tarball failure is non-fatal — the snapshot
    /// still completes; lazy reads pick up uncached blobs.
    ///
    /// Counters:
    /// - `prefetch_hits` — incremented per committed blob *inside*
    ///   `fetch_tarball_into_cache`'s per-entry loop (Codex M3-plan-v1 #7).
    /// - `prefetch_failures` — one tick per failed tarball attempt.
    /// - `prefetch_skipped_oversized` — auto-gate said no (byte cap exceeded).
    async fn dispatch_fetch_policy(
        &self,
        source: &SourceSpec,
        commit_sha: &str,
        tree_entries: &[TreeEntry],
        policy: ctxfs_provider_common::fetcher::PrefetchPolicy,
        threshold_count: u64,
        max_bytes: u64,
    ) -> Result<(), CtxfsError> {
        use ctxfs_provider_common::fetcher::{decide_policy, FetchPolicy, PrefetchPolicy};

        // Count blobs and sum estimated bytes. Treat any entry with size==None
        // as "unknown size" — forces Lazy when policy==Auto (fail-closed so the
        // byte cap cannot be underestimated). (Codex M3-plan-v1 #8.)
        let blob_iter = tree_entries.iter().filter(|e| e.entry_type == "blob");
        let blob_count = blob_iter.clone().count() as u64;
        let any_unknown_size = blob_iter.clone().any(|e| e.size.is_none());
        let estimated_bytes: u64 = blob_iter.clone().map(|e| e.size.unwrap_or(0)).sum();

        let effective_policy = if any_unknown_size && policy == PrefetchPolicy::Auto {
            tracing::info!(
                target: "ctxfs.provider.tarball",
                "manifest has entries with unknown size; degrading auto-gate to Lazy"
            );
            PrefetchPolicy::Disabled
        } else {
            policy
        };

        let decision = decide_policy(
            blob_count,
            estimated_bytes,
            effective_policy,
            threshold_count,
            max_bytes,
        );

        match decision {
            FetchPolicy::Lazy => Ok(()),

            FetchPolicy::LazyOversized {
                estimated_bytes,
                blob_count,
                cap,
            } => {
                if let Some(key) = self.counter_key.lock().unwrap().clone() {
                    self.observability
                        .counters_for(key)
                        .record_prefetch_skipped_oversized();
                }
                tracing::warn!(
                    target: "ctxfs.provider.tarball",
                    estimated_bytes,
                    blob_count,
                    cap,
                    "tarball auto-gate skipped: estimated_bytes > prefetch_max_bytes"
                );
                Ok(())
            }

            FetchPolicy::Tarball {
                estimated_bytes,
                blob_count,
            } => {
                // Pre-claim cache check: if every manifest blob is already
                // cached, skip the tarball entirely (one lock acquire, cheap).
                let blob_digests: Vec<Digest> = tree_entries
                    .iter()
                    .filter(|e| e.entry_type == "blob")
                    .map(|e| Digest::from_sha256_hex(&e.sha))
                    .collect();
                if self.cache.contains_all(blob_digests.iter()) {
                    tracing::info!(
                        target: "ctxfs.provider.tarball",
                        blob_count,
                        "all manifest blobs already cached; skipping tarball"
                    );
                    return Ok(());
                }

                // Singleflight: claim slot for this (host, owner, repo, commit).
                let (owner, repo) = owner_repo(source)?;
                let key = TarballKey {
                    host: self.api_host.clone(),
                    owner: owner.to_string(),
                    repo: repo.to_string(),
                    commit_sha: commit_sha.to_string(),
                };
                let claim = self.claim_singleflight_slot(key);

                // Leader populates the cell; waiters await the same result.
                // get_or_init guarantees the closure runs exactly once.
                let outcome_res: Result<(), String> = claim
                    .slot
                    .cell
                    .get_or_init(|| async {
                        match self
                            .fetch_tarball_into_cache(source, commit_sha, tree_entries)
                            .await
                        {
                            Ok(out) => {
                                // prefetch_hits already recorded per-entry in
                                // fetch_tarball_into_cache (Codex M3-plan-v1 #7).
                                if let Some(k) = self.counter_key.lock().unwrap().clone() {
                                    let counters = self.observability.counters_for(k);
                                    counters.record_bytes_transferred(out.total_bytes);
                                    counters.record_http_transfer();
                                }
                                tracing::info!(
                                    target: "ctxfs.provider.tarball",
                                    blob_count,
                                    estimated_bytes,
                                    blobs_committed = out.blobs_committed,
                                    blobs_skipped_invalid = out.blobs_skipped_invalid,
                                    blobs_skipped_digest = out.blobs_skipped_digest,
                                    total_bytes = out.total_bytes,
                                    "tarball prefetch ok"
                                );
                                Ok(())
                            }
                            Err(e) => {
                                // Tarball failed mid-stream. Blobs committed so far
                                // are kept (they're already in BlobCache under their
                                // own SHA-1 keys — correct by content-addressing).
                                // Record one failure tick; prefetch_hits already
                                // recorded incrementally for committed blobs.
                                if let Some(k) = self.counter_key.lock().unwrap().clone() {
                                    self.observability.counters_for(k).record_prefetch_failure();
                                }
                                tracing::warn!(
                                    target: "ctxfs.provider.tarball",
                                    error = format!("{e:?}").as_str(),
                                    "tarball prefetch failed; falling back to lazy"
                                );
                                Err(format!("{e}"))
                            }
                        }
                    })
                    .await
                    .clone();

                // Leader removes its slot; waiters' release() is no-op.
                claim.release();

                // Tarball failure is non-fatal: lazy reads fill any gaps.
                let _ = outcome_res;
                Ok(())
            }
        }
    }

    // ---- Singleflight helpers ----

    /// Claim a singleflight slot for the given `TarballKey`.
    ///
    /// The first caller for a given key inserts a fresh `TarballSlot` and
    /// returns a claim with `is_leader = true`. Subsequent callers for the
    /// same key get the *same* `Arc<TarballSlot>` and `is_leader = false`.
    ///
    /// The leader populates `claim.slot.cell` via `OnceCell::get_or_init`;
    /// waiters call the same method and block until the cell is filled. The
    /// leader's `claim.release()` removes the slot via `Arc::ptr_eq` so a
    /// stale leader cannot remove a newer slot for the same key.
    fn claim_singleflight_slot(&self, key: TarballKey) -> SlotClaim {
        use ctxfs_provider_common::fetcher::TarballSlot;
        let mut is_leader = false;
        let slot = self
            .tarball_singleflight
            .entry(key.clone())
            .or_insert_with(|| {
                is_leader = true;
                Arc::new(TarballSlot {
                    cell: tokio::sync::OnceCell::new(),
                })
            })
            .clone();
        SlotClaim {
            key,
            slot,
            is_leader,
            registry: Arc::clone(&self.tarball_singleflight),
        }
    }

    // ---- Tarball helpers ----

    /// Maximum redirect-chain depth when following the tarball 302.
    const TARBALL_REDIRECT_MAX_DEPTH: u8 = 3;

    /// Derive the codeload host from the API host. For `api.github.com` →
    /// `codeload.github.com`. For GHE deployments the convention is
    /// `codeload.<host>`.
    pub(crate) fn codeload_host_for(api_host: &str) -> String {
        if api_host == "api.github.com" {
            "codeload.github.com".to_string()
        } else {
            format!("codeload.{api_host}")
        }
    }

    /// Validate a 302 Location target against the explicitly supplied codeload
    /// host. Requires scheme=https and that the URL host equals
    /// `expected_codeload_host`.
    ///
    /// Production calls this with `self.codeload_host` (derived in [`Self::new`]
    /// or overridden via [`Self::new_with_codeload_host`]). Unit tests pass an
    /// explicit value — no env lookup, no derivation.
    pub(crate) fn validate_redirect_target(
        location: &str,
        expected_codeload_host: &str,
    ) -> Result<reqwest::Url, CtxfsError> {
        let url = reqwest::Url::parse(location)
            .map_err(|e| CtxfsError::Provider(format!("invalid redirect URL: {e}")))?;
        if url.scheme() != "https" {
            return Err(CtxfsError::Provider(format!(
                "tarball redirect rejected: scheme={} is not https",
                url.scheme()
            )));
        }
        let actual_host = url.host_str().unwrap_or("");
        if actual_host != expected_codeload_host {
            return Err(CtxfsError::Provider(format!(
                "tarball redirect rejected: host {actual_host} is not codeload host \
                 {expected_codeload_host}"
            )));
        }
        Ok(url)
    }

    /// Validate a tarball entry's path. Takes raw bytes (not `&str`) so
    /// invalid UTF-8 is caught explicitly rather than silently rewritten by
    /// `to_string_lossy`. Takes the `tar::EntryType` so we can distinguish
    /// "wrapper directory at root" from "stray regular file at root" (both
    /// have no `/`; only the directory case is benign).
    ///
    /// Rules applied in order:
    /// - reject invalid UTF-8
    /// - reject leading `/` (absolute path)
    /// - reject NUL or any control char < 0x20
    /// - reject `..` segments anywhere
    /// - require the codeload top-level wrapper dir (e.g. `owner-repo-sha/`);
    ///   strip it and return the remainder
    /// - no-slash + `Directory` → wrapper itself; return empty `PathBuf` (skip)
    /// - no-slash + anything else → reject (codeload always wraps)
    pub(crate) fn validate_tar_entry_path(
        raw: &[u8],
        entry_type: EntryType,
    ) -> Result<std::path::PathBuf, CtxfsError> {
        let s = std::str::from_utf8(raw)
            .map_err(|_| CtxfsError::Provider("tar entry path is not UTF-8".into()))?;
        if s.contains('\0') {
            return Err(CtxfsError::Provider("tar entry path contains NUL".into()));
        }
        if s.starts_with('/') {
            return Err(CtxfsError::Provider(format!("tar entry is absolute: {s}")));
        }
        if s.chars().any(|c| (c as u32) < 0x20) {
            return Err(CtxfsError::Provider(format!(
                "tar entry has control chars: {s}"
            )));
        }

        // Reject `..` anywhere in the path, including the wrapper segment.
        // (The wrapper is normally "owner-repo-sha"; a malicious tarball could
        // place `..` there to bypass the post-strip check.)
        for seg in s.split('/') {
            if seg == ".." {
                return Err(CtxfsError::Provider(format!(
                    "tar entry contains '..': {s}"
                )));
            }
        }

        let cleaned = match s.split_once('/') {
            Some((_wrapper, rest)) => rest,
            None => {
                // No '/': only a directory entry (the wrapper itself) is acceptable.
                return match entry_type {
                    EntryType::Directory => Ok(std::path::PathBuf::new()),
                    _ => Err(CtxfsError::Provider(format!(
                        "tar entry not under wrapper dir: {s}"
                    ))),
                };
            }
        };

        // After stripping the wrapper prefix, an empty remainder means this IS
        // the wrapper directory itself (trailing-slash form: "owner-repo-sha/").
        // Only a Directory entry is acceptable at that position; a Regular file
        // claiming that path is malformed.
        if cleaned.is_empty() {
            return match entry_type {
                EntryType::Directory => Ok(std::path::PathBuf::new()),
                _ => Err(CtxfsError::Provider(format!(
                    "tar entry not under wrapper dir: {s}"
                ))),
            };
        }

        for seg in cleaned.split('/') {
            if seg == ".." {
                return Err(CtxfsError::Provider(format!(
                    "tar entry contains '..': {s}"
                )));
            }
        }

        Ok(std::path::PathBuf::from(cleaned))
    }

    /// Build a `path → (expected_sha, expected_size)` index from blob entries.
    /// Used by `fetch_tarball_into_cache` to look up the expected digest for
    /// each tar entry and to compute the Git blob SHA-1 (which prefixes the
    /// size in the hash header).
    fn build_path_to_sha_size(entries: &[TreeEntry]) -> HashMap<std::path::PathBuf, (String, u64)> {
        entries
            .iter()
            .filter(|e| e.entry_type == "blob")
            .map(|e| {
                (
                    std::path::PathBuf::from(&e.path),
                    (e.sha.clone(), e.size.unwrap_or(0)),
                )
            })
            .collect()
    }

    /// Download `/repos/{o}/{r}/tarball/{ref}`, follow the codeload 302
    /// (with security checks), stream-extract via `tar::Archive`, and commit
    /// each blob atomically into `BlobCache`.
    ///
    /// Streaming end-to-end:
    /// - reqwest body → `bytes_stream` → `StreamReader` (async Read)
    /// - `SyncIoBridge` → sync Read for `tar::Archive` (runs in `spawn_blocking`)
    /// - Each entry pipes through `GitBlobSha1` (incremental hasher) AND
    ///   `BlobCache::commit_atomic_with_writer` (streaming writer) in one
    ///   `std::io::copy` via a `Tee` adapter.
    ///
    /// Memory ceiling: per-entry only. Force-mode tarballs that exceed the
    /// cache budget commit blobs successfully but trigger LRU evictions of
    /// earlier entries — that's the documented "warm-cache guarantee will not
    /// hold" warning.
    async fn fetch_tarball_into_cache(
        &self,
        source: &SourceSpec,
        commit_sha: &str,
        tree_entries: &[TreeEntry],
    ) -> Result<TarballOutcome, CtxfsError> {
        // Tee adapter: copies bytes to both hasher and writer in one io::copy.
        struct Tee<'a, W: std::io::Write> {
            hasher: &'a mut GitBlobSha1,
            writer: &'a mut W,
        }
        impl<W: std::io::Write> std::io::Write for Tee<'_, W> {
            fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
                let n = self.writer.write(buf)?;
                self.hasher.update(&buf[..n]);
                Ok(n)
            }
            fn flush(&mut self) -> std::io::Result<()> {
                self.writer.flush()
            }
        }

        let (owner, repo) = owner_repo(source)?;

        // 1. Initial API call. check_rate_limit ticks rest_calls_total.
        let api_url = self.api_url(owner, repo, &format!("tarball/{commit_sha}"));
        let initial_resp = self
            .client
            .get(&api_url)
            .send()
            .await
            .map_err(|e| CtxfsError::Provider(format!("HTTP error tarball: {e}")))?;
        self.check_rate_limit(&initial_resp)?;

        // 2. Manual redirect chain. The provider's client has
        //    redirect::Policy::none(), so we control the chain. Authorization
        //    is dropped on hop 1+ by using self.codeload_client which has no
        //    default headers. Host is validated against self.codeload_host.
        let codeload_client = &self.codeload_client;
        let mut current = initial_resp;
        let mut depth = 0u8;
        let final_resp = loop {
            if !current.status().is_redirection() {
                break current;
            }
            if depth >= Self::TARBALL_REDIRECT_MAX_DEPTH {
                return Err(CtxfsError::Provider(format!(
                    "tarball redirect chain exceeded depth {}",
                    Self::TARBALL_REDIRECT_MAX_DEPTH
                )));
            }
            let location = current
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|v| v.to_str().ok())
                .ok_or_else(|| CtxfsError::Provider("redirect without Location header".into()))?
                .to_string();
            let next_url = Self::validate_redirect_target(&location, &self.codeload_host)?;
            current = codeload_client
                .get(next_url)
                .send()
                .await
                .map_err(|e| CtxfsError::Provider(format!("codeload GET: {e}")))?;
            depth += 1;
        };

        if !final_resp.status().is_success() {
            return Err(CtxfsError::Provider(format!(
                "tarball download failed: HTTP {}",
                final_resp.status()
            )));
        }

        // 3. Build the streaming pipeline.
        //    bytes_stream() is a Stream<Item = Result<Bytes, reqwest::Error>>.
        //    map_err converts the error to io::Error so StreamReader accepts it.
        //    StreamReader makes it AsyncRead; SyncIoBridge gives blocking Read
        //    inside the spawn_blocking thread (Handle::current() is valid there).
        use futures::TryStreamExt as _;
        use tokio_util::io::{StreamReader, SyncIoBridge};

        let body_stream = final_resp.bytes_stream().map_err(std::io::Error::other);
        let async_reader = StreamReader::new(body_stream);

        let path_to_sha_size = Self::build_path_to_sha_size(tree_entries);
        let cache = Arc::clone(&self.cache);
        let counter_key = self.counter_key.lock().unwrap().clone();
        let observability = Arc::clone(&self.observability);

        // 4. Run sync tar+gz extraction in spawn_blocking so the tokio runtime
        //    is not blocked. Per-entry work is fully streaming: tar::Entry
        //    implements Read, and we copy through GitBlobSha1 + BlobTempWriter.
        let outcome = tokio::task::spawn_blocking(move || -> Result<TarballOutcome, CtxfsError> {
            let sync_reader = SyncIoBridge::new(async_reader);
            let gz = flate2::read::GzDecoder::new(sync_reader);
            let mut archive = tar::Archive::new(gz);
            let mut outcome = TarballOutcome::default();

            for entry_result in archive
                .entries()
                .map_err(|e| CtxfsError::Provider(format!("tar entries iter: {e}")))?
            {
                let mut entry =
                    entry_result.map_err(|e| CtxfsError::Provider(format!("tar entry: {e}")))?;

                let entry_type = entry.header().entry_type();
                let raw_bytes = entry.path_bytes().into_owned();

                // Path validation. Failure → counter + skip.
                let mount_path = match Self::validate_tar_entry_path(&raw_bytes, entry_type) {
                    Ok(p) => p,
                    Err(e) => {
                        outcome.blobs_skipped_invalid += 1;
                        if let Some(ref key) = counter_key {
                            observability
                                .counters_for(key.clone())
                                .record_tarball_invalid_entries();
                        }
                        tracing::warn!(
                            target: "ctxfs.provider.tarball",
                            path = String::from_utf8_lossy(&raw_bytes).as_ref(),
                            error = format!("{e:?}").as_str(),
                            "tarball entry rejected"
                        );
                        continue;
                    }
                };

                // Skip non-regular entries (directories, symlinks, etc. are
                // represented via the manifest; only regular files go to cache).
                if entry_type != EntryType::Regular {
                    continue;
                }
                // Empty PathBuf signals "this was the codeload wrapper dir" —
                // already caught above, but belt-and-suspenders.
                if mount_path.as_os_str().is_empty() {
                    continue;
                }

                // Look up expected (sha, size). No manifest entry → orphaned
                // tar entry we cannot verify; skip.
                let (expected_sha, expected_size) = match path_to_sha_size.get(&mount_path) {
                    Some(t) => t.clone(),
                    None => continue,
                };

                // Pipe entry → hasher + writer in one std::io::copy.
                let mut hasher = GitBlobSha1::new(expected_size);
                let mut writer = cache.commit_atomic_with_writer()?;
                {
                    let mut tee = Tee {
                        hasher: &mut hasher,
                        writer: &mut writer,
                    };
                    let _ = std::io::copy(&mut entry, &mut tee)
                        .map_err(|e| CtxfsError::Provider(format!("tar entry stream: {e}")))?;
                }

                // Verify SHA-1 against manifest before committing.
                let actual_sha = hasher.finalize_hex();
                if actual_sha != expected_sha {
                    outcome.blobs_skipped_digest += 1;
                    if let Some(ref key) = counter_key {
                        observability
                            .counters_for(key.clone())
                            .record_tarball_digest_mismatch();
                    }
                    // Drop without finalizing — NamedTempFile RAII unlinks temp.
                    drop(writer);
                    tracing::warn!(
                        target: "ctxfs.provider.tarball",
                        path = mount_path.display().to_string().as_str(),
                        expected = expected_sha.as_str(),
                        actual = actual_sha.as_str(),
                        "tarball blob SHA-1 mismatch"
                    );
                    continue;
                }

                // Manifest stores Git blob SHA-1 in the digest hex. The cache
                // key uses Digest::from_sha256_hex (B3-label rename is M5;
                // the stored hex is what matters, not the field name).
                let digest = Digest::from_sha256_hex(&expected_sha);
                writer.finalize(&digest)?;

                outcome.blobs_committed += 1;
                outcome.total_bytes += expected_size;
                // Increment prefetch_hits per committed blob incrementally so
                // partial commits (mid-stream failure) are visible in telemetry.
                // (Codex M3-plan-v1 #7 — folded from T6 handoff note.)
                if let Some(ref key) = counter_key {
                    observability
                        .counters_for(key.clone())
                        .record_prefetch_hit();
                }
            }
            Ok(outcome)
        })
        .await
        .map_err(|e| CtxfsError::Provider(format!("spawn_blocking join: {e}")))??;

        Ok(outcome)
    }

    // ---- Public options-aware API + shared inner ----

    /// Fetch a snapshot using explicit prefetch options. The daemon calls
    /// this path, routing `MountOptions.prefetch` + `Config.prefetch_*` into
    /// a `FetchOptions` so the tarball auto-gate is engaged per-mount.
    ///
    /// Callers that don't need M3 prefetch behaviour use
    /// [`Provider::fetch_snapshot`], which delegates with
    /// [`FetchOptions::default()`] (`PrefetchPolicy::Disabled`).
    pub async fn fetch_snapshot_with_options(
        &self,
        source: &SourceSpec,
        options: &FetchOptions,
    ) -> Result<Vec<u8>, CtxfsError> {
        self.fetch_snapshot_inner(source, options).await
    }

    /// Shared implementation: `fetch_snapshot_with_options` and the trait
    /// `fetch_snapshot` both call this so the body is never duplicated.
    async fn fetch_snapshot_inner(
        &self,
        source: &SourceSpec,
        options: &FetchOptions,
    ) -> Result<Vec<u8>, CtxfsError> {
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

        // 4. Fold the placeholder bucket into the real bucket so that
        //    rest_calls_total (from the resolve_ref API call) appears under
        //    the real commit key rather than the `<resolving:…>` key.
        let placeholder_key = CounterKey {
            source: source.provider_type.to_string(),
            repo: source.name.clone(),
            commit: format!("<resolving:{}>", source.version),
            mount_id: source.id(),
        };
        let real_key = CounterKey {
            source: source.provider_type.to_string(),
            repo: source.name.clone(),
            commit: commit_sha.clone(),
            mount_id: source.id(),
        };
        self.observability
            .merge_and_drop_placeholder(&placeholder_key, &real_key);

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
        let mut tree = self.fetch_tree(source, &commit_sha).await?;
        if tree.truncated {
            // B2 fallback: per-directory walk produces a complete manifest.
            // Walk from the actual root tree SHA returned by the API, NOT
            // from commit_sha — those are different git objects (a commit
            // and the tree it points to). The recursive=1 call returns `sha`
            // set to the root tree SHA we should walk.
            // (Codex M3-plan-v1 review #8.)
            let walked = self.fetch_tree_walked(source, &tree.sha).await?;
            tree = TreeResponse {
                sha: tree.sha.clone(),
                tree: walked,
                truncated: false,
            };
        }
        debug!("fetched tree with {} entries", tree.tree.len());

        // 4a. Tarball auto-gate / dispatch. May commit blobs into BlobCache;
        //     does not affect the manifest. Failures are non-fatal — the
        //     snapshot still completes; lazy reads pick up uncached blobs.
        self.dispatch_fetch_policy(
            source,
            &commit_sha,
            &tree.tree,
            options.prefetch,
            options.prefetch_threshold_count,
            options.prefetch_max_bytes,
        )
        .await?;

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
}

#[async_trait]
impl Provider for GitHubProvider {
    /// Delegates to `fetch_snapshot_inner` with [`FetchOptions::default()`]
    /// (`PrefetchPolicy::Disabled`) so non-daemon callers (NFS tests, FSKit
    /// paths, etc.) keep their pre-M3 lazy-fetch behaviour unchanged. The
    /// daemon calls `fetch_snapshot_with_options` directly.
    async fn fetch_snapshot(&self, source: &SourceSpec) -> Result<Vec<u8>, CtxfsError> {
        self.fetch_snapshot_inner(source, &FetchOptions::default())
            .await
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

    /// Construct an empty singleflight registry for tests that don't exercise
    /// the dedupe path.
    fn make_singleflight() -> std::sync::Arc<ctxfs_provider_common::fetcher::TarballSingleflightMap>
    {
        std::sync::Arc::new(dashmap::DashMap::new())
    }

    /// Minimal `GitHubProvider` for unit tests that don't make real HTTP calls.
    fn make_test_provider() -> GitHubProvider {
        let cache_dir = tempfile::tempdir().unwrap();
        let cache = Arc::new(
            ctxfs_cache::BlobCache::new(cache_dir.path().to_path_buf(), 1024 * 1024).unwrap(),
        );
        GitHubProvider::new(
            None,
            "api.github.com".to_string(),
            cache,
            None,
            None,
            Arc::new(Observability::new()),
            make_singleflight(),
        )
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

    #[test]
    fn claim_singleflight_slot_first_caller_is_leader() {
        let provider = make_test_provider();
        use ctxfs_provider_common::fetcher::TarballKey;
        let key = TarballKey {
            host: "api.github.com".to_string(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            commit_sha: "abc123".to_string(),
        };
        let claim = provider.claim_singleflight_slot(key);
        assert!(
            claim.is_leader,
            "first caller for a fresh key must be the leader"
        );
    }

    #[test]
    fn claim_singleflight_slot_second_caller_is_waiter_with_same_slot() {
        let provider = make_test_provider();
        use ctxfs_provider_common::fetcher::TarballKey;
        let key = TarballKey {
            host: "api.github.com".to_string(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            commit_sha: "abc123".to_string(),
        };
        let claim1 = provider.claim_singleflight_slot(key.clone());
        let claim2 = provider.claim_singleflight_slot(key);
        assert!(claim1.is_leader, "first caller must be leader");
        assert!(!claim2.is_leader, "second caller must be waiter");
        assert!(
            std::sync::Arc::ptr_eq(&claim1.slot, &claim2.slot),
            "leader and waiter must share the same slot Arc"
        );
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
        let provider = GitHubProvider::new(
            None,
            "api.github.com".to_string(),
            cache,
            None,
            None,
            Arc::new(Observability::new()),
            make_singleflight(),
        );
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

    // ---- path-validation and redirect-validation tests ----

    fn pv(raw: &str, et: tar::EntryType) -> Result<std::path::PathBuf, CtxfsError> {
        GitHubProvider::validate_tar_entry_path(raw.as_bytes(), et)
    }

    #[test]
    fn validate_tar_entry_path_strips_top_level_prefix_for_files() {
        let p = pv("owner-repo-abc123/src/lib.rs", tar::EntryType::Regular).unwrap();
        assert_eq!(p, std::path::PathBuf::from("src/lib.rs"));
    }

    #[test]
    fn validate_tar_entry_path_accepts_wrapper_dir_only_for_directories() {
        // The codeload wrapper dir itself appears as a directory entry.
        // Returning empty PathBuf signals "skip — this is the wrapper".
        let dir = pv("owner-repo-abc/", tar::EntryType::Directory).unwrap();
        assert_eq!(dir, std::path::PathBuf::new());

        // The same string with a regular-file entry is malformed.
        assert!(pv("owner-repo-abc/", tar::EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_no_slash_regular_file() {
        // codeload always wraps; a regular file at the archive root is malformed.
        assert!(pv("README.md", tar::EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_dotdot() {
        assert!(pv("owner-repo-abc/../escape", tar::EntryType::Regular).is_err());
        assert!(pv("owner-repo-abc/sub/../../escape", tar::EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_dotdot_in_wrapper_segment() {
        // Pre-fix, "../escape" had `..` discarded as the wrapper and "escape"
        // returned Ok. The whole-path scan before split_once catches this.
        assert!(pv("../escape", tar::EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_absolute() {
        assert!(pv("/etc/passwd", tar::EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_nul_and_control() {
        assert!(pv("owner-repo-abc/foo\0bar", tar::EntryType::Regular).is_err());
        assert!(pv("owner-repo-abc/foo\x01bar", tar::EntryType::Regular).is_err());
    }

    #[test]
    fn validate_tar_entry_path_rejects_invalid_utf8() {
        // Raw bytes (not str) are passed in so we can prove the rejection.
        let mut bytes = Vec::from(b"owner-repo-abc/".as_slice());
        bytes.push(0xFFu8);
        bytes.extend_from_slice(b".rs");
        assert!(GitHubProvider::validate_tar_entry_path(&bytes, tar::EntryType::Regular).is_err());
    }

    #[test]
    fn redirect_url_validates_codeload_only() {
        // validate_redirect_target takes the explicit codeload host (already derived),
        // not api_host.
        assert!(GitHubProvider::validate_redirect_target(
            "https://codeload.github.com/owner/repo/tar.gz/abc",
            "codeload.github.com"
        )
        .is_ok());
        assert!(GitHubProvider::validate_redirect_target(
            "https://attacker.example.com/foo",
            "codeload.github.com"
        )
        .is_err());
        assert!(
            GitHubProvider::validate_redirect_target(
                "http://codeload.github.com/foo",
                "codeload.github.com"
            )
            .is_err(),
            "http rejected even on codeload"
        );
    }

    #[test]
    fn git_blob_sha1_zero_byte_matches_empty_blob_hash() {
        // Git's canonical empty-blob SHA-1: sha1("blob 0\0") = e69de29b...
        // std::io::copy on a zero-byte tar entry never calls Tee::write, so
        // update() is never invoked. finalize_hex must emit the header itself.
        let hasher = GitBlobSha1::new(0);
        assert_eq!(
            hasher.finalize_hex(),
            "e69de29bb2d1d6434b8b29ae775ad8c2e48c5391"
        );
    }

    #[test]
    fn codeload_host_for_default_is_codeload_github_com() {
        assert_eq!(
            GitHubProvider::codeload_host_for("api.github.com"),
            "codeload.github.com"
        );
    }

    #[test]
    fn codeload_host_for_ghe_uses_codeload_prefix() {
        assert_eq!(
            GitHubProvider::codeload_host_for("ghe.example.com"),
            "codeload.ghe.example.com"
        );
    }

    // ---- Task 5: assemble_walked_tree unit tests ----

    #[test]
    fn assemble_walked_tree_recurses_directories() {
        let mut subtrees: std::collections::HashMap<&str, Vec<TreeEntry>> =
            std::collections::HashMap::new();
        // Root: one file + one subdir.
        let _ = subtrees.insert(
            "root_sha",
            vec![
                TreeEntry {
                    path: "README.md".into(),
                    mode: "100644".into(),
                    entry_type: "blob".into(),
                    sha: "blob_a".into(),
                    size: Some(100),
                },
                TreeEntry {
                    path: "src".into(),
                    mode: "040000".into(),
                    entry_type: "tree".into(),
                    sha: "src_sha".into(),
                    size: None,
                },
            ],
        );
        // src: one file.
        let _ = subtrees.insert(
            "src_sha",
            vec![TreeEntry {
                path: "lib.rs".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "blob_b".into(),
                size: Some(200),
            }],
        );

        let assembled = GitHubProvider::assemble_walked_tree("root_sha", |sha| {
            subtrees.get(sha).cloned().unwrap_or_default()
        });

        // Expect path-prefixed entries: README.md, src, src/lib.rs
        assert_eq!(assembled.len(), 3);
        assert!(assembled
            .iter()
            .any(|e| e.path == "README.md" && e.sha == "blob_a"));
        assert!(assembled
            .iter()
            .any(|e| e.path == "src" && e.entry_type == "tree"));
        assert!(assembled
            .iter()
            .any(|e| e.path == "src/lib.rs" && e.sha == "blob_b"));
    }

    #[test]
    fn assemble_walked_tree_handles_deep_nesting() {
        let mut subtrees: std::collections::HashMap<&str, Vec<TreeEntry>> =
            std::collections::HashMap::new();
        let _ = subtrees.insert(
            "a",
            vec![TreeEntry {
                path: "b".into(),
                mode: "040000".into(),
                entry_type: "tree".into(),
                sha: "b".into(),
                size: None,
            }],
        );
        let _ = subtrees.insert(
            "b",
            vec![TreeEntry {
                path: "c".into(),
                mode: "040000".into(),
                entry_type: "tree".into(),
                sha: "c".into(),
                size: None,
            }],
        );
        let _ = subtrees.insert(
            "c",
            vec![TreeEntry {
                path: "deep.txt".into(),
                mode: "100644".into(),
                entry_type: "blob".into(),
                sha: "deep_blob".into(),
                size: Some(7),
            }],
        );
        let assembled = GitHubProvider::assemble_walked_tree("a", |sha| {
            subtrees.get(sha).cloned().unwrap_or_default()
        });
        assert!(assembled.iter().any(|e| e.path == "b/c/deep.txt"));
    }

    #[test]
    fn assemble_walked_tree_empty_root() {
        let assembled = GitHubProvider::assemble_walked_tree("empty_sha", |_sha| vec![]);
        assert!(assembled.is_empty());
    }

    // ---- dispatch_fetch_policy unit tests ----

    /// PrefetchPolicy::Disabled → dispatch returns Ok(()) without touching the
    /// singleflight registry or making any HTTP calls.
    #[tokio::test]
    async fn dispatch_fetch_policy_disabled_returns_ok() {
        use ctxfs_provider_common::fetcher::PrefetchPolicy;
        let provider = make_test_provider();
        let source = SourceSpec::parse("github:owner/repo@main").unwrap();
        let result = provider
            .dispatch_fetch_policy(
                &source,
                "abc123",
                &[],
                PrefetchPolicy::Disabled,
                30,
                256 * 1024 * 1024,
            )
            .await;
        assert!(result.is_ok(), "Disabled policy must return Ok immediately");
    }

    /// Auto-gate: blob count below threshold → Lazy → Ok(()) immediately.
    #[tokio::test]
    async fn dispatch_fetch_policy_auto_below_threshold_returns_ok() {
        use ctxfs_provider_common::fetcher::PrefetchPolicy;
        let provider = make_test_provider();
        let source = SourceSpec::parse("github:owner/repo@main").unwrap();
        // 5 blob entries, threshold = 30 → below threshold → Lazy
        let entries: Vec<TreeEntry> = (0..5)
            .map(|i| TreeEntry {
                path: format!("file{i}.txt"),
                mode: "100644".to_string(),
                entry_type: "blob".to_string(),
                sha: format!("{i:040x}"),
                size: Some(100),
            })
            .collect();
        let result = provider
            .dispatch_fetch_policy(
                &source,
                "abc123",
                &entries,
                PrefetchPolicy::Auto,
                30,
                256 * 1024 * 1024,
            )
            .await;
        assert!(
            result.is_ok(),
            "Auto/below-threshold must return Ok immediately"
        );
    }

    // ---- FetchOptions tests ----

    #[test]
    fn fetch_options_default_is_disabled_prefetch() {
        use ctxfs_provider_common::fetcher::PrefetchPolicy;
        let opts = FetchOptions::default();
        assert_eq!(
            opts.prefetch,
            PrefetchPolicy::Disabled,
            "FetchOptions::default() must use PrefetchPolicy::Disabled so trait callers \
             (NFS tests, FSKit, etc.) keep pre-M3 behavior"
        );
    }

    #[test]
    fn fetch_options_default_thresholds_are_nonzero() {
        let opts = FetchOptions::default();
        assert!(
            opts.prefetch_threshold_count > 0,
            "default threshold_count must be > 0"
        );
        assert!(opts.prefetch_max_bytes > 0, "default max_bytes must be > 0");
    }
}
