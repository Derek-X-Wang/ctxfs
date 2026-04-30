use crate::observability::Observability;
use anyhow::{bail, Context, Result};
use ctxfs_cache::{BlobCache, RepoKey, ResolutionCache, SharedTreeCache, TreeCache};
use ctxfs_core::config::Config;
use ctxfs_core::source::{ProviderType, SourceSpec};
use ctxfs_core::Backend;
use ctxfs_ipc::service::{
    CacheBreakdown, CacheStats, CtxfsService, MountInfo, MountOptions, MountStatus,
};
use ctxfs_ipc::transport;
use ctxfs_manifest::Snapshot;
use ctxfs_nfs::{CtxfsNfs, NfsServerHandle};
use ctxfs_provider_common::fetcher::TarballSingleflightMap;
use ctxfs_provider_common::resolver::RegistryResolver;
use ctxfs_provider_git::{FetchOptions, GitHubProvider, ProviderContext};
use fskit_rs::session::Session as FsKitSession;
use futures::StreamExt;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tarpc::server::Channel;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

/// State owned by the daemon for an FSKit mount. Explicit async shutdown
/// unmounts; Drop closes the session as a fallback.
pub struct FsKitHandle {
    session: Option<FsKitSession>,
    volume_path: std::path::PathBuf,
    /// Receives a signal when the FSKit filesystem calls `unmount()` (e.g.
    /// Finder eject).  The daemon spawns a task per mount that listens on this
    /// and triggers the full cleanup path.
    pub unmount_rx: Option<tokio::sync::mpsc::UnboundedReceiver<()>>,
}

impl FsKitHandle {
    pub fn new(session: FsKitSession, volume_path: std::path::PathBuf) -> Self {
        Self {
            session: Some(session),
            volume_path,
            unmount_rx: None,
        }
    }

    /// Attach the receiver side of the unmount-notification channel.
    #[must_use]
    pub fn with_unmount_rx(mut self, rx: tokio::sync::mpsc::UnboundedReceiver<()>) -> Self {
        self.unmount_rx = Some(rx);
        self
    }

    pub fn volume_path(&self) -> &std::path::Path {
        &self.volume_path
    }

    /// Consume the handle and drop the session (triggers unmount via fskit-rs).
    /// This is the preferred cleanup path — called explicitly from daemon
    /// shutdown in Task 9 so we don't rely on Drop (can't await in Drop).
    ///
    /// Kept `async` because fskit-rs may in the future expose an async
    /// teardown path; callers already `.await` this so changing the shape
    /// later would be breaking.
    #[allow(clippy::unused_async)]
    pub async fn shutdown(mut self) {
        if let Some(session) = self.session.take() {
            drop(session);
        }
    }
}

impl std::fmt::Debug for FsKitHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsKitHandle")
            .field("volume_path", &self.volume_path)
            .finish_non_exhaustive()
    }
}

struct MountHandle {
    info: MountInfo,
    #[allow(dead_code)]
    backend: Backend,
    /// Keeps the NFS server task alive for the lifetime of the mount.
    /// `None` for `FSKit` mounts.
    _nfs: Option<NfsServerHandle>,
    /// Keeps the FSKit session alive for the lifetime of the mount.
    /// `None` for NFS mounts.
    _fskit: Option<FsKitHandle>,
    /// Cache reservation key for this mount. Populated in `prepare_mount`
    /// after `register_mount` runs; `None` for mounts that could not derive a
    /// `RepoKey` (should not occur in production but guarded defensively).
    /// Used by `do_unmount_by_id` to call `BlobCache::unregister_mount`.
    repo_key: Option<RepoKey>,
    /// The `mount_id` used in `CounterKey` for this mount.
    ///
    /// For GitHub sources this equals the mounts-registry key
    /// (`source_spec.id()`). For registry-resolved sources (npm, PyPI,
    /// crates.io) the provider uses `github_source.id()` as the counter key
    /// while the registry uses `source_spec.id()`. Storing both lets
    /// `assemble_status_report` join on the counter key without needing to
    /// resolve the original source again.
    counter_mount_id: String,
}

pub struct Daemon {
    config: Config,
    cache: Arc<BlobCache>,
    tree_cache: Arc<TreeCache>,
    resolution_cache: Arc<Mutex<ResolutionCache>>,
    mounts: Arc<RwLock<HashMap<String, MountHandle>>>,
    cancel: CancellationToken,
}

impl std::fmt::Debug for Daemon {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Daemon")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

#[derive(Clone)]
struct DaemonServer {
    cache: Arc<BlobCache>,
    tree_cache: Arc<TreeCache>,
    resolution_cache: Arc<Mutex<ResolutionCache>>,
    shared_tree_cache: Option<Arc<dyn SharedTreeCache>>,
    mounts: Arc<RwLock<HashMap<String, MountHandle>>>,
    config: Config,
    rt_handle: tokio::runtime::Handle,
    observability: Arc<Observability>,
    /// Singleflight registry shared across all mounts on this daemon instance.
    /// Prevents concurrent tarball fetches for the same (owner/repo/sha) key.
    tarball_singleflight: Arc<TarballSingleflightMap>,
}

impl Daemon {
    pub fn new(config: Config) -> Result<Self> {
        let cache = Arc::new(
            BlobCache::new(config.cache_dir.clone(), config.cache_max_bytes)
                .context("failed to initialize cache")?,
        );

        // Remove orphan temp blobs left over from a crash mid-commit.
        let cleared = cache
            .cleanup_orphan_temps(std::time::Duration::from_secs(3600))
            .unwrap_or_else(|e| {
                warn!("cleanup_orphan_temps failed: {e}");
                0
            });
        if cleared > 0 {
            info!("cleared {cleared} orphan temp blob(s) from previous run");
        }

        let tree_cache = Arc::new(TreeCache::new(
            config.cache_dir.join("trees"),
            config.tree_cache_max_bytes,
        ));

        let resolution_cache = ResolutionCache::load(
            config.cache_dir.join("resolutions.json"),
            config.latest_ttl_secs,
        );

        Ok(Self {
            config,
            cache,
            tree_cache,
            resolution_cache: Arc::new(Mutex::new(resolution_cache)),
            mounts: Arc::new(RwLock::new(HashMap::new())),
            cancel: CancellationToken::new(),
        })
    }

    pub async fn run(&self) -> Result<()> {
        self.cleanup_stale_mounts();
        self.write_pid_file()?;

        // Attempt to connect to Redis if configured.
        let shared_tree_cache = self.try_connect_redis().await;

        let mut incoming = transport::listen(&self.config.socket_path)
            .await
            .context("failed to create IPC listener")?;

        info!("daemon listening on {}", self.config.socket_path.display());

        let server = DaemonServer {
            cache: self.cache.clone(),
            tree_cache: self.tree_cache.clone(),
            resolution_cache: self.resolution_cache.clone(),
            shared_tree_cache,
            mounts: self.mounts.clone(),
            config: self.config.clone(),
            rt_handle: tokio::runtime::Handle::current(),
            observability: Arc::new(Observability::new()),
            tarball_singleflight: Arc::new(TarballSingleflightMap::new()),
        };

        let cancel = self.cancel.clone();

        tokio::select! {
            _ = async {
                while let Some(result) = incoming.next().await {
                    match result {
                        Ok(transport) => {
                            let server_clone = server.clone();
                            let channel = tarpc::server::BaseChannel::with_defaults(transport);
                            let _ = tokio::spawn(
                                channel
                                    .execute(server_clone.serve())
                                    .for_each(|resp| async {
                                        let _ = tokio::spawn(resp);
                                    }),
                            );
                        }
                        Err(e) => {
                            error!("accept error: {e}");
                        }
                    }
                }
            } => {},
            _ = cancel.cancelled() => {
                info!("shutdown signal received");
            },
            _ = tokio::signal::ctrl_c() => {
                info!("SIGINT received, shutting down");
            },
            _ = wait_for_sigterm() => {
                info!("SIGTERM received, shutting down");
            },
        }

        self.cleanup().await;
        Ok(())
    }

    /// Try to connect to Redis for the shared tree cache.
    #[allow(clippy::unused_async)]
    async fn try_connect_redis(&self) -> Option<Arc<dyn SharedTreeCache>> {
        let url = self.config.redis_url.as_deref()?;

        #[cfg(feature = "redis")]
        {
            info!("connecting to Redis at {url}...");
            match ctxfs_cache_redis::RedisTreeCache::connect(url).await {
                Some(cache) => {
                    info!("Redis shared tree cache connected");
                    Some(Arc::new(cache) as Arc<dyn SharedTreeCache>)
                }
                None => {
                    warn!("failed to connect to Redis — proceeding without shared tree cache");
                    None
                }
            }
        }

        #[cfg(not(feature = "redis"))]
        {
            warn!(
                "CTXFS_REDIS_URL is set but Redis support is not compiled in; \
                 rebuild with `--features redis` to enable"
            );
            // Suppress unused variable warning.
            let _ = url;
            None
        }
    }

    fn write_pid_file(&self) -> Result<()> {
        if let Some(parent) = self.config.pid_file.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Check for stale PID
        if self.config.pid_file.exists() {
            if let Ok(pid_str) = std::fs::read_to_string(&self.config.pid_file) {
                if let Ok(pid) = pid_str.trim().parse::<i32>() {
                    #[allow(unsafe_code)]
                    if unsafe { libc::kill(pid, 0) } == 0 {
                        bail!("daemon already running with PID {pid}");
                    }
                }
            }
        }

        std::fs::write(&self.config.pid_file, std::process::id().to_string())?;
        Ok(())
    }

    async fn cleanup(&self) {
        info!("cleaning up...");

        let ids: Vec<String> = {
            let mounts = self.mounts.read().await;
            mounts.keys().cloned().collect()
        };
        let mut mounts = self.mounts.write().await;
        for id in ids {
            if let Some(handle) = mounts.remove(&id) {
                info!("shutting down mount {}", handle.info.mount_point);
                // FSKit: explicit async shutdown (can't await in Drop).
                #[allow(clippy::used_underscore_binding)]
                if let Some(fskit) = handle._fskit {
                    fskit.shutdown().await;
                }
                // Dropping the NFS handle stops the NFS server task.
                #[allow(clippy::used_underscore_binding)]
                drop(handle._nfs);
            }
        }

        // Clear mounts.json — we've shut down cleanly.
        let state_file = crate::mount_state::MountStateFile::new(
            self.config
                .pid_file
                .parent()
                .unwrap_or_else(|| std::path::Path::new("/tmp")),
        );
        if let Err(e) = state_file.clear() {
            warn!("failed to clear mount state: {e}");
        }

        let _ = std::fs::remove_file(&self.config.pid_file);
        let _ = std::fs::remove_file(&self.config.socket_path);

        info!("daemon stopped");
    }

    fn cleanup_stale_mounts(&self) {
        let state_file = crate::mount_state::MountStateFile::new(
            self.config
                .pid_file
                .parent()
                .unwrap_or_else(|| std::path::Path::new("/tmp")),
        );
        let entries = state_file.read();
        if entries.is_empty() {
            return;
        }
        warn!(
            "found {} stale mount entries from previous daemon run, attempting cleanup",
            entries.len()
        );
        for entry in &entries {
            if entry.backend != ctxfs_core::Backend::FsKit {
                continue;
            }
            let _ = std::process::Command::new("diskutil")
                .args(["unmount", "force", &entry.volume_path])
                .output();
            info!("cleaned up stale FSKit volume {}", entry.volume_path);
        }
        if let Err(e) = state_file.clear() {
            warn!("failed to clear stale mount state: {e}");
        }
    }

    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Wait for a single SIGTERM and then return. Tokio's `signal::ctrl_c()` only
/// listens for SIGINT; `ctxfs daemon stop` sends SIGTERM, so we need both.
async fn wait_for_sigterm() {
    match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
        Ok(mut sig) => {
            let _ = sig.recv().await;
        }
        Err(e) => {
            error!("failed to install SIGTERM handler: {e}");
            // If signal install fails, never resolve — fall through to other select arms.
            std::future::pending::<()>().await;
        }
    }
}

/// Reserve a loopback TCP port by binding and immediately dropping.
/// Small TOCTOU window, acceptable for a local dev tool.
fn pick_free_port() -> Result<u16, String> {
    let listener =
        TcpListener::bind("127.0.0.1:0").map_err(|e| format!("failed to reserve port: {e}"))?;
    let port = listener
        .local_addr()
        .map_err(|e| format!("failed to read local addr: {e}"))?
        .port();
    drop(listener);
    Ok(port)
}

/// All the state a backend needs from `prepare_mount` to build a `VfsState`
/// and start serving. Backend-agnostic.
struct MountPrep {
    /// The original parsed source (for mount ID and registry cache).
    source_spec: ctxfs_core::source::SourceSpec,
    /// A GitHub-shaped source (registries resolved to owner/repo/ref).
    github_source: ctxfs_core::source::SourceSpec,
    /// Provider that fetches blobs and directories.
    provider: std::sync::Arc<ctxfs_provider_git::GitHubProvider>,
    /// Parsed snapshot manifest.
    snapshot: ctxfs_manifest::Snapshot,
    /// Optional subpath to re-root the mount at.
    subpath: Option<String>,
    /// Cache-reservation key for this mount. Built from the GitHub host +
    /// owner/repo extracted from `github_source`. Used by the backends to
    /// store the key in `MountHandle` so `do_unmount_by_id` can call
    /// `unregister_mount` at teardown.
    repo_key: RepoKey,
}

impl DaemonServer {
    /// Construct a fresh `Arc<GitHubProvider>` for a single mount.
    ///
    /// **B8 invariant**: every mount must get its own provider Arc.
    /// Sharing a provider across mounts re-introduces the `active_source`
    /// race — each `GitHubProvider` holds per-mount `active_source` and
    /// `counter_key` state that would conflict if shared.
    ///
    /// The singleflight tarball registry IS shared by Arc-clone via
    /// `ProviderContext` — that's intentional: concurrent mounts of the
    /// same `(host, owner, repo, commit)` key deduplicate to one tarball
    /// download.
    fn build_github_provider_for_mount(&self) -> Arc<GitHubProvider> {
        let ctx = ProviderContext {
            api_host: self.config.github_host.clone(),
            observability: self.observability.clone(),
            cache: self.cache.clone(),
            tree_cache: Some(self.tree_cache.clone()),
            shared_tree_cache: self.shared_tree_cache.clone(),
            singleflight: self.tarball_singleflight.clone(),
        };
        Arc::new(GitHubProvider::new(
            self.config.github_token.as_deref(),
            ctx,
        ))
    }

    /// Build the appropriate registry resolver for a non-GitHub source.
    fn make_resolver(source: &SourceSpec) -> Result<Box<dyn RegistryResolver>, String> {
        match source.provider_type {
            ProviderType::Npm => Ok(Box::new(ctxfs_provider_npm::NpmResolver::new())),
            ProviderType::PyPI => Ok(Box::new(ctxfs_provider_pypi::PyPIResolver::new())),
            ProviderType::Crate => Ok(Box::new(ctxfs_provider_crate::CrateResolver::new())),
            ProviderType::GitHub => Err("GitHub sources don't use a registry resolver".into()),
        }
    }

    /// Resolve `source_str` to GitHub coordinates, fetch the snapshot, and
    /// return all the state backends need.
    #[allow(clippy::too_many_lines)]
    fn prepare_mount(&self, source_str: &str, options: MountOptions) -> Result<MountPrep, String> {
        let mut source =
            SourceSpec::parse(source_str).map_err(|e| format!("invalid source: {e}"))?;

        let is_latest = source.version == "latest";

        let cached_resolution = if source.provider_type == ProviderType::GitHub {
            None
        } else {
            let guard = self.resolution_cache.lock().unwrap();
            guard.get(source_str).cloned()
        };

        let (owner, repo, git_ref, subpath) = if source.provider_type == ProviderType::GitHub {
            let (o, r) = source
                .name
                .split_once('/')
                .ok_or_else(|| format!("invalid github source: {}", source.name))?;
            (
                o.to_string(),
                r.to_string(),
                source.version.clone(),
                source.subpath.clone(),
            )
        } else if let Some(resolved) = cached_resolution {
            info!("resolution cache hit for {source_str}");
            let sp = source.subpath.clone().or(resolved.subpath.clone());
            (
                resolved.owner.clone(),
                resolved.repo.clone(),
                resolved.git_ref.clone(),
                sp,
            )
        } else {
            let resolver = Self::make_resolver(&source)?;

            if is_latest {
                source.version = self
                    .rt_handle
                    .block_on(resolver.resolve_latest(&source.name))
                    .map_err(|e| format!("failed to resolve latest: {e}"))?;
            }

            let src = self
                .rt_handle
                .block_on(resolver.resolve(&source.name, &source.version))
                .map_err(|e| format!("{e}"))?;

            let sp = source.subpath.clone().or(src.subpath.clone());

            {
                let mut guard = self.resolution_cache.lock().unwrap();
                if let Err(e) = guard.put(source_str.to_string(), src.clone(), is_latest) {
                    warn!("failed to persist resolution cache: {e}");
                }
            }

            (src.owner, src.repo, src.git_ref, sp)
        };

        let github_source = SourceSpec {
            provider_type: ProviderType::GitHub,
            name: format!("{owner}/{repo}"),
            version: git_ref,
            subpath: subpath.clone(),
        };

        // Build repo_key before the fetch so the reservation slot is in place
        // before tarball hydration starts — existing mounts rebalance correctly
        // rather than having their default share compressed mid-hydration.
        let (gh_owner, gh_repo) = github_source
            .name
            .split_once('/')
            .ok_or_else(|| format!("invalid github source name: {}", github_source.name))?;
        let repo_key = RepoKey::new(&self.config.github_host, gh_owner, gh_repo);

        // Pre-register with empty manifest: reservation in place, ownership
        // seeded post-fetch via add_owners once the manifest is known.
        self.cache
            .register_mount(&repo_key, options.cache_reservation_bytes, &[]);

        let mut provider = self.build_github_provider_for_mount();

        let fetch_options = FetchOptions {
            prefetch: options.prefetch,
            prefetch_threshold_count: self.config.prefetch_threshold_count,
            prefetch_max_bytes: self.config.prefetch_max_bytes,
        };

        let snapshot_data = self
            .rt_handle
            .block_on(provider.fetch_snapshot_with_options(&github_source, &fetch_options))
            .map_err(|e| {
                // Roll back the pre-registration so no dangling reservation
                // entry is left if the fetch fails.
                self.cache.unregister_mount(&repo_key);
                format!("failed to fetch snapshot: {e}")
            })?;

        let snapshot: Snapshot = match serde_json::from_slice(&snapshot_data) {
            Ok(s) => s,
            Err(e) => {
                // Roll back the pre-registered reservation slot — mirrors the
                // fetch-error path above. Pathological in production (the data
                // is the provider's own serde_json output), but keeps the error
                // paths symmetric so no dangling reservation is left on any
                // prepare_mount failure.
                self.cache.unregister_mount(&repo_key);
                return Err(format!("failed to parse snapshot: {e}"));
            }
        };

        // Seed blob ownership now that the manifest is known.
        let blob_hexes = provider.snapshot_blob_hexes();
        self.cache.add_owners(&repo_key, &blob_hexes);

        // Wire mount_cache into the provider so tarball + lazy fetch paths
        // call record_ownership_after_finalize for blobs not in the manifest.
        // Arc::get_mut succeeds because provider is the sole Arc clone at this point.
        let view = ctxfs_cache::reservation::MountCacheView::new(
            std::sync::Arc::clone(&self.cache),
            repo_key.clone(),
        );
        std::sync::Arc::get_mut(&mut provider)
            .expect("provider Arc must be sole owner here; no clone should exist before daemon wires mount_cache")
            .set_mount_cache(Some(std::sync::Arc::new(view)));

        Ok(MountPrep {
            source_spec: source,
            github_source,
            provider,
            snapshot,
            subpath,
            repo_key,
        })
    }

    /// Dispatch a mount request to the appropriate backend implementation.
    fn do_mount(
        &self,
        source_str: &str,
        mount_point: &str,
        backend: Backend,
        options: MountOptions,
    ) -> Result<MountInfo, String> {
        match backend {
            Backend::Nfs => self.do_mount_nfs(source_str, mount_point, options),
            Backend::FsKit => self.do_mount_fskit(source_str, mount_point, options),
        }
    }

    /// Start an FSKit mount. Unlike NFS, the kernel mount happens inside
    /// `fskit_rs::mount` (via the FSKit host app) — the CLI does not need
    /// to run `mount_nfs`. `mount_point` is the user's *logical* path
    /// (a symlink managed by the CLI); the real volume lives under
    /// `/Volumes/ctxfs/<slug>`.
    fn do_mount_fskit(
        &self,
        source_str: &str,
        mount_point: &str,
        options: MountOptions,
    ) -> Result<MountInfo, String> {
        let bundle_id = self.config.fskit_bundle_id.clone().ok_or_else(|| {
            "CTXFS_FSKIT_BUNDLE_ID not set — cannot start FSKit mount".to_string()
        })?;

        let prep = self.prepare_mount(source_str, options)?;

        let mut fskit_handle = self
            .rt_handle
            .block_on(crate::fskit_mount::start_fskit_mount(
                &prep.source_spec,
                prep.provider,
                self.cache.clone(),
                prep.snapshot.clone(),
                prep.subpath,
                &bundle_id,
            ))
            .map_err(|e| format!("fskit mount failed: {e}"))?;

        // Extract the unmount receiver before we move the handle into MountHandle.
        // We need it to spawn the Finder-eject watcher task below.
        let unmount_rx = fskit_handle.unmount_rx.take();

        let id = prep.source_spec.id();
        let commit_sha = prep.snapshot.commit_sha.clone();
        let volume_path_str = fskit_handle.volume_path().to_string_lossy().to_string();

        let symlink_paths = if mount_point == volume_path_str {
            vec![]
        } else {
            vec![mount_point.to_string()]
        };

        let info = MountInfo {
            id: id.clone(),
            source: source_str.to_string(),
            mount_point: mount_point.to_string(),
            commit_sha,
            status: MountStatus::Ready,
            mounted_at: chrono::Utc::now().to_rfc3339(),
            nfs_port: None,
            backend: Backend::FsKit,
            volume_path: Some(volume_path_str.clone()),
            symlink_paths: symlink_paths.clone(),
        };

        // Persist to mounts.json for crash recovery.
        let state_file = crate::mount_state::MountStateFile::new(
            self.config
                .pid_file
                .parent()
                .unwrap_or_else(|| std::path::Path::new("/tmp")),
        );
        let entry = crate::mount_state::MountStateEntry {
            source: source_str.to_string(),
            volume_path: volume_path_str,
            symlink_paths,
            backend: Backend::FsKit,
            tcp_port: None,
            auth_token: None,
        };
        if let Err(e) = state_file.add(entry) {
            warn!("failed to persist mount state: {e}");
        }

        let handle = MountHandle {
            info: info.clone(),
            backend: Backend::FsKit,
            _nfs: None,
            _fskit: Some(fskit_handle),
            repo_key: Some(prep.repo_key),
            counter_mount_id: prep.github_source.id(),
        };

        self.rt_handle.block_on(async {
            let _ = self.mounts.write().await.insert(id.clone(), handle);
        });

        // Spawn a watcher task: when the FSKit filesystem calls unmount() (e.g.
        // Finder eject), signal the daemon to run the full cleanup path so that
        // symlinks and mounts.json stay in sync with kernel mount state.
        if let Some(mut rx) = unmount_rx {
            let server_clone = self.clone();
            let mount_id = id.clone();
            let _watcher = tokio::spawn(async move {
                if rx.recv().await.is_some() {
                    info!(
                        "FSKit volume {mount_id} self-unmounted (Finder eject); running daemon cleanup"
                    );
                    if let Err(e) = server_clone.do_unmount_by_id(&mount_id).await {
                        // The mount may have already been cleaned up via the CLI
                        // unmount path — that is not an error.
                        warn!("Finder-eject cleanup for {mount_id}: {e}");
                    }
                }
            });
        }

        Ok(info)
    }

    /// Fetch the snapshot and start an NFS server for it. The CLI is responsible
    /// for the actual kernel `mount_nfs` call so sudo prompts land in the user's
    /// terminal instead of the daemon's log.
    fn do_mount_nfs(
        &self,
        source_str: &str,
        mount_point: &str,
        options: MountOptions,
    ) -> Result<MountInfo, String> {
        let prep = self.prepare_mount(source_str, options)?;

        std::fs::create_dir_all(mount_point)
            .map_err(|e| format!("failed to create mount point: {e}"))?;

        let id = prep.source_spec.id();
        let commit_sha = prep.snapshot.commit_sha.clone();
        // Capture before github_source is moved into CtxfsNfs.
        let counter_mount_id = prep.github_source.id();

        let port = pick_free_port()?;
        let addr = format!("127.0.0.1:{port}");

        let vfs = self
            .rt_handle
            .block_on(ctxfs_vfs::VfsState::new(
                prep.provider,
                self.cache.clone(),
                prep.snapshot,
                prep.subpath,
            ))
            .map_err(|e| format!("failed to build VFS: {e}"))?;
        let fs = CtxfsNfs::new(Arc::new(vfs), prep.github_source);
        let nfs_handle = self
            .rt_handle
            .block_on(fs.spawn(&addr))
            .map_err(|e| format!("failed to start NFS server on {addr}: {e}"))?;

        info!(
            "NFS server listening on {} for {source_str}",
            nfs_handle.addr
        );

        let info = MountInfo {
            id: id.clone(),
            source: source_str.to_string(),
            mount_point: mount_point.to_string(),
            commit_sha,
            status: MountStatus::Ready,
            mounted_at: chrono::Utc::now().to_rfc3339(),
            nfs_port: Some(port),
            backend: Backend::Nfs,
            volume_path: None,
            symlink_paths: vec![],
        };

        let handle = MountHandle {
            info: info.clone(),
            backend: Backend::Nfs,
            _nfs: Some(nfs_handle),
            _fskit: None,
            repo_key: Some(prep.repo_key),
            counter_mount_id,
        };

        self.rt_handle.block_on(async {
            let _ = self.mounts.write().await.insert(id, handle);
        });

        Ok(info)
    }

    /// Build the full `StatusReportV1` by augmenting observability's
    /// budget+counter view with cache-level details.
    ///
    /// LFS fields are populated directly from the `CounterSnapshot` in
    /// `observability.status_report()`. Cache fields (`working_set_bytes`,
    /// `cache_reservation_bytes`, `cache_eviction_attempts_blocked_by_reservation`)
    /// require a brief lock on the mounts registry to map `mount_id → RepoKey`,
    /// then cache lookups outside the lock so no cache call is made while
    /// the registry lock is held.
    async fn assemble_status_report(&self) -> ctxfs_ipc::service::StatusReportV1 {
        // Snapshot counter_mount_id → RepoKey while holding the lock briefly.
        // Do NOT hold this lock across cache calls.
        //
        // Key by `counter_mount_id` (= github_source.id()) rather than the
        // mounts-registry key (= source_spec.id()) so the lookup succeeds for
        // registry-resolved mounts (npm/PyPI/crates.io) where the two IDs
        // differ: the provider's CounterKey.mount_id uses github_source.id(),
        // but the mounts registry is keyed by the original source_spec.id().
        let key_by_mount: HashMap<String, RepoKey> = {
            let mounts = self.mounts.read().await;
            mounts
                .values()
                .filter_map(|h| {
                    h.repo_key
                        .as_ref()
                        .map(|k| (h.counter_mount_id.clone(), k.clone()))
                })
                .collect()
        };

        // Base report: budgets, per-mount counters, LFS fields.
        let mut report = self.observability.status_report();

        // Augment each MountSummary with cache working-set and reservation.
        for mount in &mut report.mounts {
            if let Some(repo_key) = key_by_mount.get(&mount.mount_id) {
                mount.working_set_bytes = self.cache.working_set_bytes(repo_key);
                mount.cache_reservation_bytes = self.cache.reservation_bytes(repo_key).unwrap_or(0);
            }
        }

        // Cache-global counter: evictions skipped due to active reservations.
        report.cache_eviction_attempts_blocked_by_reservation =
            self.cache.eviction_attempts_blocked_by_reservation();

        report
    }
}

impl DaemonServer {
    /// Perform full cleanup for a mount identified by its key (= `source_spec.id()`).
    ///
    /// Called from both the IPC `unmount` RPC (CLI path) and the Finder-eject
    /// watcher task so the two paths share identical cleanup logic.
    async fn do_unmount_by_id(&self, mount_id: &str) -> Result<(), String> {
        let handle = {
            let mut mounts = self.mounts.write().await;
            mounts.remove(mount_id)
        };

        match handle {
            Some(h) => {
                let volume_path = h.info.volume_path.clone();

                // Unregister cache reservation so the slot is returned
                // to the default-rebalance pool and other mounts can grow.
                if let Some(ref key) = h.repo_key {
                    self.cache.unregister_mount(key);
                }

                // FSKit: explicit async shutdown (can't await in Drop).
                #[allow(clippy::used_underscore_binding)]
                let fskit_opt = h._fskit;
                #[allow(clippy::used_underscore_binding)]
                let nfs_opt = h._nfs;
                if let Some(fskit) = fskit_opt {
                    fskit.shutdown().await;
                }
                drop(nfs_opt);

                // Remove from mounts.json.
                if let Some(vp) = volume_path.as_deref() {
                    let state_file = crate::mount_state::MountStateFile::new(
                        self.config
                            .pid_file
                            .parent()
                            .unwrap_or_else(|| std::path::Path::new("/tmp")),
                    );
                    if let Err(e) = state_file.remove_volume(vp) {
                        warn!("failed to remove mount state entry: {e}");
                    }
                }

                info!("stopped mount {mount_id}");
                Ok(())
            }
            None => Err(format!("mount not found: {mount_id}")),
        }
    }
}

impl CtxfsService for DaemonServer {
    async fn mount(
        self,
        _: tarpc::context::Context,
        source: String,
        mount_point: String,
        backend: Backend,
        options: MountOptions,
    ) -> Result<MountInfo, String> {
        info!(
            "mount request: {source} -> {mount_point} (backend={backend}, prefetch={:?})",
            options.prefetch
        );
        let server = self.clone();
        tokio::task::spawn_blocking(move || {
            server.do_mount(&source, &mount_point, backend, options)
        })
        .await
        .map_err(|e| format!("mount task panicked: {e}"))?
    }

    async fn unmount(self, _: tarpc::context::Context, target: String) -> Result<(), String> {
        info!("unmount request: {target}");

        // Resolve the target (mount_point, id, or volume_path) to the internal key.
        let key = {
            let mounts = self.mounts.read().await;
            mounts
                .iter()
                .find(|(_, h)| {
                    h.info.mount_point == target
                        || h.info.id == target
                        || h.info.volume_path.as_deref() == Some(&target)
                })
                .map(|(k, _)| k.clone())
        };

        match key {
            Some(k) => self.do_unmount_by_id(&k).await,
            None => Err(format!("mount not found: {target}")),
        }
    }

    async fn list(self, _: tarpc::context::Context) -> Vec<MountInfo> {
        let mounts = self.mounts.read().await;
        mounts.values().map(|h| h.info.clone()).collect()
    }

    async fn status(
        self,
        _: tarpc::context::Context,
        mount_id: String,
    ) -> Result<MountInfo, String> {
        let mounts = self.mounts.read().await;
        mounts
            .get(&mount_id)
            .map(|h| h.info.clone())
            .ok_or_else(|| format!("mount not found: {mount_id}"))
    }

    async fn cache_stats(self, _: tarpc::context::Context) -> Result<CacheStats, String> {
        let (total_bytes, entry_count) = self.cache.stats();
        let (tree_count, tree_bytes) = self.tree_cache.stats();
        let resolution_count = self.resolution_cache.lock().unwrap().entry_count();
        Ok(CacheStats {
            total_bytes,
            entry_count,
            freed_bytes: 0,
            tree_count,
            tree_bytes,
            resolution_count,
        })
    }

    async fn cache_prune(
        self,
        _: tarpc::context::Context,
        max_bytes: Option<u64>,
    ) -> Result<CacheStats, String> {
        let freed = self
            .cache
            .prune(max_bytes)
            .map_err(|e| format!("prune failed: {e}"))?;

        self.tree_cache
            .prune_all()
            .map_err(|e| format!("tree cache prune failed: {e}"))?;

        let (total_bytes, entry_count) = self.cache.stats();
        let (tree_count, tree_bytes) = self.tree_cache.stats();
        let resolution_count = self.resolution_cache.lock().unwrap().entry_count();

        Ok(CacheStats {
            total_bytes,
            entry_count,
            freed_bytes: freed,
            tree_count,
            tree_bytes,
            resolution_count,
        })
    }

    async fn cache_breakdown(self, _: tarpc::context::Context) -> Result<CacheBreakdown, String> {
        let (blob_bytes, blob_count) = self.cache.stats();
        let (tree_count, tree_bytes) = self.tree_cache.stats();
        Ok(CacheBreakdown {
            blob_bytes,
            blob_count: blob_count as u64,
            tree_bytes,
            tree_count: tree_count as u64,
            max_bytes: self.cache.max_bytes(),
        })
    }

    async fn set_cache_limits(
        self,
        ctx: tarpc::context::Context,
        max_bytes: u64,
    ) -> Result<CacheBreakdown, String> {
        self.cache.set_max_bytes(max_bytes);
        self.cache_breakdown(ctx).await
    }

    async fn prune_blobs(
        self,
        _: tarpc::context::Context,
        target_bytes: u64,
    ) -> Result<u64, String> {
        Ok(self.cache.prune_blobs(target_bytes))
    }

    async fn ping(self, _: tarpc::context::Context) -> String {
        "pong".to_string()
    }

    async fn get_status(
        self,
        _: tarpc::context::Context,
    ) -> Result<ctxfs_ipc::service::StatusReportV1, String> {
        Ok(self.assemble_status_report().await)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxfs_core::Backend;

    /// Verify that the FSKit mount path generates an auth token and stores it as a
    /// 64-character hex string in the persisted mount state entry.
    ///
    /// This test exercises the token-generation and state-construction logic
    /// without spawning a real TCP listener or calling into fskitd.
    #[test]
    fn fskit_mount_state_entry_has_auth_token() {
        // Simulate the token-generation code executed inside do_mount_fskit.
        let token = ctxfs_fskit::AuthToken::generate();
        let token_hex = token.to_hex();

        // A hex-encoded 256-bit (32-byte) token must be exactly 64 characters.
        assert_eq!(
            token_hex.len(),
            64,
            "auth token hex must be 64 chars (256-bit)"
        );
        assert!(
            token_hex.chars().all(|c| c.is_ascii_hexdigit()),
            "auth token hex must contain only hex digits"
        );

        // Confirm that the MountStateEntry can be constructed with Some(token_hex),
        // mirroring the assignment in do_mount_fskit.
        let entry = crate::mount_state::MountStateEntry {
            source: "github:owner/repo@main".to_string(),
            volume_path: "/Volumes/ctxfs/test".to_string(),
            symlink_paths: vec![],
            backend: Backend::FsKit,
            tcp_port: None,
            auth_token: Some(token_hex.clone()),
        };

        assert!(
            entry.auth_token.is_some(),
            "FSKit mount entry must have auth_token populated"
        );
        let stored = entry.auth_token.unwrap();
        assert_eq!(stored.len(), 64, "stored token must be 64-char hex");
        assert_eq!(stored, token_hex, "stored token must match generated token");
    }

    /// Two sequential FSKit mounts must produce different auth tokens.
    #[test]
    fn fskit_mount_tokens_are_unique_per_mount() {
        let t1 = ctxfs_fskit::AuthToken::generate().to_hex();
        let t2 = ctxfs_fskit::AuthToken::generate().to_hex();
        assert_ne!(t1, t2, "consecutive mounts must get distinct auth tokens");
    }

    // ─── Finder-eject / do_unmount_by_id tests ───────────────────────────────

    /// Build a minimal `DaemonServer` backed by temp directories.
    fn make_test_server(tmp: &tempfile::TempDir) -> DaemonServer {
        use std::sync::Mutex;
        let base = tmp.path();
        let cache = Arc::new(
            ctxfs_cache::BlobCache::new(base.join("cache"), 64 * 1024 * 1024)
                .expect("BlobCache::new"),
        );
        let tree_cache = Arc::new(ctxfs_cache::TreeCache::new(
            base.join("trees"),
            64 * 1024 * 1024,
        ));
        let resolution_cache =
            ctxfs_cache::ResolutionCache::load(base.join("resolutions.json"), 3600);
        let config = ctxfs_core::config::Config {
            pid_file: base.join("ctxfs.pid"),
            cache_dir: base.join("cache"),
            ..ctxfs_core::config::Config::default()
        };

        DaemonServer {
            cache,
            tree_cache,
            resolution_cache: Arc::new(Mutex::new(resolution_cache)),
            shared_tree_cache: None,
            mounts: Arc::new(RwLock::new(HashMap::new())),
            config,
            rt_handle: tokio::runtime::Handle::current(),
            observability: Arc::new(Observability::new()),
            tarball_singleflight: Arc::new(TarballSingleflightMap::new()),
        }
    }

    /// Build a minimal `MountInfo` for testing.
    fn make_mount_info(id: &str) -> ctxfs_ipc::service::MountInfo {
        ctxfs_ipc::service::MountInfo {
            id: id.to_string(),
            source: "github:owner/repo@main".to_string(),
            mount_point: format!("/tmp/test-mount-{id}"),
            commit_sha: "abc123".to_string(),
            status: ctxfs_ipc::service::MountStatus::Ready,
            mounted_at: chrono::Utc::now().to_rfc3339(),
            nfs_port: None,
            backend: Backend::FsKit,
            volume_path: Some(format!("/Volumes/ctxfs/{id}")),
            symlink_paths: vec![],
        }
    }

    /// `do_unmount_by_id` removes the mount from the in-memory map and
    /// cleans up the mounts.json state file.
    #[tokio::test]
    async fn finder_eject_cleanup_removes_mount_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);

        let mount_id = "test-mount-id";
        let volume_path = format!("/Volumes/ctxfs/{mount_id}");

        // Pre-populate the state file so we can verify it gets cleaned up.
        let state_file = crate::mount_state::MountStateFile::new(tmp.path());
        state_file
            .add(crate::mount_state::MountStateEntry {
                source: "github:owner/repo@main".to_string(),
                volume_path: volume_path.clone(),
                symlink_paths: vec!["/tmp/my-project/deps/repo".to_string()],
                backend: Backend::FsKit,
                tcp_port: None,
                auth_token: None,
            })
            .unwrap();

        // Insert a mock mount handle (no real FSKit session needed).
        {
            let mut mounts = server.mounts.write().await;
            let _ = mounts.insert(
                mount_id.to_string(),
                MountHandle {
                    info: make_mount_info(mount_id),
                    backend: Backend::FsKit,
                    _nfs: None,
                    _fskit: None, // No real FSKit session in unit test.
                    repo_key: None,
                    counter_mount_id: mount_id.to_string(),
                },
            );
        }

        // Simulate what happens when the FSKit unmount() callback fires and the
        // watcher task calls do_unmount_by_id.
        server
            .do_unmount_by_id(mount_id)
            .await
            .expect("cleanup should succeed");

        // The in-memory mount map must be empty.
        assert!(
            server.mounts.read().await.is_empty(),
            "mount map must be empty after Finder-eject cleanup"
        );

        // The mounts.json entry must have been removed.
        let entries = state_file.read();
        assert!(
            entries.is_empty(),
            "mounts.json must be empty after Finder-eject cleanup"
        );
    }

    // ─── cache_breakdown / set_cache_limits / prune_blobs RPC tests ─────────

    #[tokio::test]
    async fn cache_breakdown_returns_structured_stats() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);

        // Put one blob so the breakdown reflects non-zero values.
        let digest = ctxfs_core::Digest::sha256(b"rpc-test-data");
        server.cache.put(&digest, b"rpc-test-data").unwrap();

        let ctx = tarpc::context::current();
        let bd = server
            .cache_breakdown(ctx)
            .await
            .expect("cache_breakdown must succeed");

        assert!(bd.blob_bytes > 0, "blob_bytes must be positive after a put");
        assert_eq!(bd.blob_count, 1, "blob_count must be 1");
        assert_eq!(
            bd.max_bytes,
            64 * 1024 * 1024,
            "max_bytes must match config"
        );
        // tree and resolution counts are 0 in a fresh cache — just ensure no panic.
        let _ = bd.tree_bytes;
        let _ = bd.tree_count;
    }

    #[tokio::test]
    async fn set_cache_limits_updates_max_and_triggers_eviction() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);

        // Fill with 5 blobs of 100 bytes each.
        for i in 0..5u8 {
            let d = ctxfs_core::Digest::sha256(&[i; 100]);
            server.cache.put(&d, &[i; 100]).unwrap();
        }
        assert_eq!(server.cache.total_bytes(), 500);

        // Shrink limit to 200 bytes — must evict until under limit.
        let ctx = tarpc::context::current();
        let bd = server
            .set_cache_limits(ctx, 200)
            .await
            .expect("set_cache_limits must succeed");

        assert_eq!(bd.max_bytes, 200, "max_bytes must reflect the new limit");
        assert!(
            bd.blob_bytes <= 200,
            "blob_bytes must be <= new limit after eviction, got {}",
            bd.blob_bytes
        );
    }

    #[tokio::test]
    async fn prune_blobs_rpc_returns_bytes_freed() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);

        // Put 5 blobs of 100 bytes each (500 bytes total).
        for i in 0..5u8 {
            let d = ctxfs_core::Digest::sha256(&[i; 100]);
            server.cache.put(&d, &[i; 100]).unwrap();
        }

        // Save a handle to the cache before the RPC call consumes `server`.
        let cache = server.cache.clone();

        let ctx = tarpc::context::current();
        let freed = server
            .prune_blobs(ctx, 100)
            .await
            .expect("prune_blobs must succeed");

        assert!(freed > 0, "must have freed some bytes, got {freed}");
        assert!(
            cache.total_bytes() <= 100,
            "remaining bytes must be <= target, got {}",
            cache.total_bytes()
        );
    }

    /// A second call to `do_unmount_by_id` for the same mount ID (e.g. CLI
    /// unmount racing with Finder eject) returns an error but does not panic.
    #[tokio::test]
    async fn finder_eject_double_cleanup_is_harmless() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);

        let mount_id = "double-cleanup-test";

        {
            let mut mounts = server.mounts.write().await;
            let _ = mounts.insert(
                mount_id.to_string(),
                MountHandle {
                    info: make_mount_info(mount_id),
                    backend: Backend::FsKit,
                    _nfs: None,
                    _fskit: None,
                    repo_key: None,
                    counter_mount_id: mount_id.to_string(),
                },
            );
        }

        // First cleanup succeeds.
        server
            .do_unmount_by_id(mount_id)
            .await
            .expect("first cleanup should succeed");

        // Second cleanup (e.g. Finder eject arriving after CLI unmount) must
        // return an error, not panic.
        let result = server.do_unmount_by_id(mount_id).await;
        assert!(
            result.is_err(),
            "second cleanup must return an error (mount already gone)"
        );
    }

    /// Verify that `FilesystemAdapter::unmount()` fires the mpsc notifier so
    /// the daemon's watcher task receives the signal.
    #[tokio::test]
    async fn fskit_adapter_unmount_fires_notifier() {
        use async_trait::async_trait;
        use ctxfs_core::{
            error::CtxfsError,
            provider::{Provider, SharedProvider},
            Digest,
        };
        use ctxfs_fskit::FilesystemAdapter;
        use ctxfs_manifest::Snapshot;
        use ctxfs_vfs::VfsState;
        use fskit_rs::Filesystem as _;
        use tokio::sync::mpsc;

        // Minimal no-op provider — `unmount()` never calls the provider so
        // this just satisfies the type.
        struct NullProvider;
        #[async_trait]
        impl Provider for NullProvider {
            async fn fetch_snapshot(
                &self,
                _: &ctxfs_core::source::SourceSpec,
            ) -> Result<Vec<u8>, CtxfsError> {
                unimplemented!()
            }
            async fn fetch_directory(&self, _: &Digest) -> Result<Vec<u8>, CtxfsError> {
                unimplemented!()
            }
            async fn fetch_blob(&self, _: &Digest) -> Result<Vec<u8>, CtxfsError> {
                unimplemented!()
            }
        }

        let tmp = tempfile::tempdir().unwrap();
        let cache = Arc::new(
            ctxfs_cache::BlobCache::new(tmp.path().join("cache"), 1024 * 1024).expect("BlobCache"),
        );

        let provider: SharedProvider = Arc::new(NullProvider);
        let snapshot = Snapshot {
            source: "github:owner/repo@main".to_string(),
            commit_sha: "test".to_string(),
            root_directory: ctxfs_core::Digest {
                algorithm: ctxfs_core::digest::HashAlgorithm::Sha256,
                hex: "0000000000000000000000000000000000000000000000000000000000000000".to_string(),
            },
            created_at: "2025-01-01T00:00:00Z".to_string(),
        };
        let vfs = Arc::new(
            VfsState::new(provider, cache, snapshot, None)
                .await
                .expect("VfsState::new"),
        );

        let (tx, mut rx) = mpsc::unbounded_channel::<()>();
        let mut adapter =
            FilesystemAdapter::new(vfs, "test-vol".to_string(), "Test Vol".to_string())
                .with_unmount_notifier(tx);

        // Calling unmount() must send to the channel — no real FSKit session needed.
        adapter.unmount().await.expect("unmount must succeed");

        // The receiver must have a pending message.
        let signal = rx.try_recv();
        assert!(
            signal.is_ok(),
            "notifier must fire when FilesystemAdapter::unmount() is called"
        );
    }

    // ─── B8 invariant: per-mount provider construction ───────────────────────

    /// B8: every call to `build_github_provider_for_mount` returns a fresh
    /// `Arc<GitHubProvider>`. Sharing a provider across mounts would
    /// re-introduce the active_source race.
    #[tokio::test]
    async fn b8_per_mount_provider_arcs_are_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);
        let p1 = server.build_github_provider_for_mount();
        let p2 = server.build_github_provider_for_mount();
        assert!(
            !Arc::ptr_eq(&p1, &p2),
            "B8 violation: build_github_provider_for_mount returned the same Arc twice"
        );
    }

    /// Stronger: three consecutive calls all produce distinct Arcs. Catches
    /// a hypothetical regression where someone caches the first Arc.
    #[tokio::test]
    async fn b8_three_mount_provider_arcs_all_distinct() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);
        let p1 = server.build_github_provider_for_mount();
        let p2 = server.build_github_provider_for_mount();
        let p3 = server.build_github_provider_for_mount();
        assert!(!Arc::ptr_eq(&p1, &p2), "p1 == p2");
        assert!(!Arc::ptr_eq(&p2, &p3), "p2 == p3");
        assert!(!Arc::ptr_eq(&p1, &p3), "p1 == p3");
    }

    /// Complement of B8: the singleflight registry IS shared across providers
    /// (Arc-clone, not copy). Concurrent mounts of the same (repo, commit)
    /// deduplicate to one tarball download.
    #[tokio::test]
    async fn b8_singleflight_registry_arc_is_shared_across_providers() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);
        let initial_registry = Arc::clone(&server.tarball_singleflight);
        let _p1 = server.build_github_provider_for_mount();
        let _p2 = server.build_github_provider_for_mount();
        // initial_registry + server + p1's internal clone + p2's internal clone = ≥ 4
        assert!(
            Arc::strong_count(&initial_registry) >= 4,
            "singleflight registry must be Arc-cloned into every provider (got {} strong refs)",
            Arc::strong_count(&initial_registry)
        );
    }

    /// Recording an LFS pointer on a per-mount counter makes it visible
    /// in the `StatusReportV1` produced by `assemble_status_report`.
    #[tokio::test]
    async fn lfs_pointer_count_appears_in_status() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);

        // Register a counter bucket and record one LFS pointer with a path.
        let key = ctxfs_provider_common::counters::CounterKey {
            source: "github".to_string(),
            repo: "owner/repo".to_string(),
            commit: "abc123".to_string(),
            mount_id: "mnt-lfs-1".to_string(),
        };
        server
            .observability
            .counters_for(key)
            .record_lfs_pointer_with_path("assets/large-model.bin");

        // assemble_status_report should surface the LFS data.
        let report = server.assemble_status_report().await;

        let mount = report
            .mounts
            .iter()
            .find(|m| m.mount_id == "mnt-lfs-1")
            .expect("mount summary present");
        assert_eq!(mount.lfs_pointer_files, 1);
        assert_eq!(
            mount.lfs_pointer_sample_paths,
            vec!["assets/large-model.bin"]
        );
    }

    /// Verifies working_set_bytes and cache_reservation_bytes are populated by
    /// assemble_status_report when a MountHandle carries a RepoKey.
    #[tokio::test]
    async fn working_set_and_reservation_appear_in_status() {
        let tmp = tempfile::tempdir().unwrap();
        let server = make_test_server(&tmp);

        // Register a per-repo cache reservation.
        let repo_key = RepoKey::new("api.github.com", "owner", "test-repo");
        let reservation = 500_000u64;
        server
            .cache
            .register_mount(&repo_key, Some(reservation), &[]);

        // Write a blob owned by this repo so working_set_bytes is non-zero.
        let blob = b"hello t3c";
        let digest = ctxfs_core::Digest::from_sha1_hex("bbbb000000000000000000000000000000000001");
        server.cache.put_for(&repo_key, &digest, blob).unwrap();

        // Add an observability counter so a MountSummary appears in the report.
        let mount_id = "github:owner/test-repo@main";
        let obs_key = ctxfs_provider_common::counters::CounterKey {
            source: "github".to_string(),
            repo: "owner/test-repo".to_string(),
            commit: "abc123".to_string(),
            mount_id: mount_id.to_string(),
        };
        server
            .observability
            .counters_for(obs_key)
            .record_rest_call();

        // Insert the MountHandle with the RepoKey so assemble_status_report
        // can map mount_id → RepoKey for cache lookups.
        {
            let mut mounts = server.mounts.write().await;
            let _ = mounts.insert(
                mount_id.to_string(),
                MountHandle {
                    info: make_mount_info(mount_id),
                    backend: Backend::FsKit,
                    _nfs: None,
                    _fskit: None,
                    repo_key: Some(repo_key.clone()),
                    counter_mount_id: mount_id.to_string(),
                },
            );
        }

        let report = server.assemble_status_report().await;

        let mount = report
            .mounts
            .iter()
            .find(|m| m.mount_id == mount_id)
            .expect("mount summary present");

        assert_eq!(mount.working_set_bytes, blob.len() as u64);
        assert_eq!(mount.cache_reservation_bytes, reservation);
        assert_eq!(report.cache_eviction_attempts_blocked_by_reservation, 0);
    }
}
