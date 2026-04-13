use anyhow::{bail, Context, Result};
use ctxfs_cache::{BlobCache, ResolutionCache, SharedTreeCache, TreeCache};
use ctxfs_core::config::Config;
use ctxfs_core::provider::Provider;
use ctxfs_core::source::{ProviderType, SourceSpec};
use ctxfs_core::Backend;
use ctxfs_ipc::service::{CacheStats, CtxfsService, MountInfo, MountStatus};
use ctxfs_ipc::transport;
use ctxfs_manifest::Snapshot;
use ctxfs_nfs::{CtxfsNfs, NfsServerHandle};
use ctxfs_provider_common::resolver::RegistryResolver;
use ctxfs_provider_git::GitHubProvider;
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
}

impl FsKitHandle {
    pub fn new(session: FsKitSession, volume_path: std::path::PathBuf) -> Self {
        Self {
            session: Some(session),
            volume_path,
        }
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
}

impl Daemon {
    pub fn new(config: Config) -> Result<Self> {
        let cache = Arc::new(
            BlobCache::new(config.cache_dir.clone(), config.cache_max_bytes)
                .context("failed to initialize cache")?,
        );

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

        let mut mounts = self.mounts.write().await;
        let ids: Vec<String> = mounts.keys().cloned().collect();
        for id in ids {
            if let Some(handle) = mounts.remove(&id) {
                info!("stopping NFS server for {}", handle.info.mount_point);
                // Dropping the handle stops the NFS server task; the kernel
                // mount is the CLI's responsibility to tear down.
                drop(handle);
            }
        }

        let _ = std::fs::remove_file(&self.config.pid_file);
        let _ = std::fs::remove_file(&self.config.socket_path);

        info!("daemon stopped");
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
}

impl DaemonServer {
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
    fn prepare_mount(&self, source_str: &str) -> Result<MountPrep, String> {
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

        let provider = Arc::new(GitHubProvider::new(
            self.config.github_token.as_deref(),
            self.cache.clone(),
            Some(self.tree_cache.clone()),
            self.shared_tree_cache.clone(),
        ));

        let snapshot_data = self
            .rt_handle
            .block_on(provider.fetch_snapshot(&github_source))
            .map_err(|e| format!("failed to fetch snapshot: {e}"))?;

        let snapshot: Snapshot = serde_json::from_slice(&snapshot_data)
            .map_err(|e| format!("failed to parse snapshot: {e}"))?;

        Ok(MountPrep {
            source_spec: source,
            github_source,
            provider,
            snapshot,
            subpath,
        })
    }

    /// Dispatch a mount request to the appropriate backend implementation.
    fn do_mount(
        &self,
        source_str: &str,
        mount_point: &str,
        backend: Backend,
    ) -> Result<MountInfo, String> {
        match backend {
            Backend::Nfs => self.do_mount_nfs(source_str, mount_point),
            Backend::FsKit => self.do_mount_fskit(source_str, mount_point),
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
    ) -> Result<MountInfo, String> {
        let bundle_id = self
            .config
            .fskit_bundle_id
            .clone()
            .ok_or_else(|| "CTXFS_FSKIT_BUNDLE_ID not set — cannot start FSKit mount".to_string())?;

        let prep = self.prepare_mount(source_str)?;

        let fskit_handle = self
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
        };

        self.rt_handle.block_on(async {
            let _ = self.mounts.write().await.insert(id, handle);
        });

        Ok(info)
    }

    /// Fetch the snapshot and start an NFS server for it. The CLI is responsible
    /// for the actual kernel `mount_nfs` call so sudo prompts land in the user's
    /// terminal instead of the daemon's log.
    fn do_mount_nfs(&self, source_str: &str, mount_point: &str) -> Result<MountInfo, String> {
        let prep = self.prepare_mount(source_str)?;

        std::fs::create_dir_all(mount_point)
            .map_err(|e| format!("failed to create mount point: {e}"))?;

        let id = prep.source_spec.id();
        let commit_sha = prep.snapshot.commit_sha.clone();

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
        };

        self.rt_handle.block_on(async {
            let _ = self.mounts.write().await.insert(id, handle);
        });

        Ok(info)
    }
}

impl CtxfsService for DaemonServer {
    async fn mount(
        self,
        _: tarpc::context::Context,
        source: String,
        mount_point: String,
        backend: Backend,
    ) -> Result<MountInfo, String> {
        info!("mount request: {source} -> {mount_point} (backend={backend})");
        let server = self.clone();
        tokio::task::spawn_blocking(move || server.do_mount(&source, &mount_point, backend))
            .await
            .map_err(|e| format!("mount task panicked: {e}"))?
    }

    async fn unmount(self, _: tarpc::context::Context, target: String) -> Result<(), String> {
        info!("unmount request: {target}");
        let mut mounts = self.mounts.write().await;

        let key = mounts
            .iter()
            .find(|(_, h)| {
                h.info.mount_point == target
                    || h.info.id == target
                    || h.info.volume_path.as_deref() == Some(&target)
            })
            .map(|(k, _)| k.clone());

        match key {
            Some(k) => {
                if let Some(handle) = mounts.remove(&k) {
                    let volume_path = handle.info.volume_path.clone();

                    // FSKit: explicit async shutdown (can't await in Drop).
                    // The `_fskit`/`_nfs` prefix marks these as "lifetime-only"
                    // at construction; here we deliberately consume them.
                    #[allow(clippy::used_underscore_binding)]
                    let fskit_opt = handle._fskit;
                    #[allow(clippy::used_underscore_binding)]
                    let nfs_opt = handle._nfs;
                    if let Some(fskit) = fskit_opt {
                        fskit.shutdown().await;
                    }
                    drop(nfs_opt);

                    // Clean up mounts.json entry for FSKit mounts.
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

                    info!("stopped mount for {target}");
                    Ok(())
                } else {
                    Err(format!("mount not found: {target}"))
                }
            }
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

    async fn ping(self, _: tarpc::context::Context) -> String {
        "pong".to_string()
    }
}
