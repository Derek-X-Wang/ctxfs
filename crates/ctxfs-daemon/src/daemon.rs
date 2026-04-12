use anyhow::{bail, Context, Result};
use ctxfs_cache::{BlobCache, ResolutionCache, SharedTreeCache, TreeCache};
use ctxfs_core::config::Config;
use ctxfs_core::provider::Provider;
use ctxfs_core::source::{ProviderType, SourceSpec};
use ctxfs_ipc::service::{CacheStats, CtxfsService, MountInfo, MountStatus};
use ctxfs_ipc::transport;
use ctxfs_manifest::Snapshot;
use ctxfs_nfs::{CtxfsNfs, NfsServerHandle};
use ctxfs_provider_common::resolver::RegistryResolver;
use ctxfs_provider_git::GitHubProvider;
use futures::StreamExt;
use std::collections::HashMap;
use std::net::TcpListener;
use std::sync::{Arc, Mutex};
use tarpc::server::Channel;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info, warn};

struct MountHandle {
    info: MountInfo,
    /// Keeps the NFS server task alive for the lifetime of the mount.
    _nfs: NfsServerHandle,
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

    /// Fetch the snapshot and start an NFS server for it. The CLI is responsible
    /// for the actual kernel `mount_nfs` call so sudo prompts land in the user's
    /// terminal instead of the daemon's log.
    #[allow(clippy::too_many_lines)]
    fn do_mount(&self, source_str: &str, mount_point: &str) -> Result<MountInfo, String> {
        let mut source =
            SourceSpec::parse(source_str).map_err(|e| format!("invalid source: {e}"))?;

        let is_latest = source.version == "latest";

        // ── Resolution cache check ──────────────────────────────────────────
        // For registry sources, see if we already have cached GitHub coordinates.
        let cached_resolution = if source.provider_type == ProviderType::GitHub {
            None
        } else {
            let guard = self.resolution_cache.lock().unwrap();
            guard.get(source_str).cloned()
        };

        // ── Resolve to GitHub coordinates ───────────────────────────────────
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
            // Cache hit — skip registry resolution entirely.
            info!("resolution cache hit for {source_str}");
            let sp = source.subpath.clone().or(resolved.subpath.clone());
            (
                resolved.owner.clone(),
                resolved.repo.clone(),
                resolved.git_ref.clone(),
                sp,
            )
        } else {
            // Cache miss — resolve from the registry.
            let resolver = Self::make_resolver(&source)?;

            // Step 1: Resolve "latest" to an exact version.
            if is_latest {
                source.version = self
                    .rt_handle
                    .block_on(resolver.resolve_latest(&source.name))
                    .map_err(|e| format!("failed to resolve latest: {e}"))?;
            }

            // Step 2: Resolve to GitHub coordinates.
            let src = self
                .rt_handle
                .block_on(resolver.resolve(&source.name, &source.version))
                .map_err(|e| format!("{e}"))?;

            let sp = source.subpath.clone().or(src.subpath.clone());

            // Cache the resolution for future mounts.
            {
                let mut guard = self.resolution_cache.lock().unwrap();
                if let Err(e) = guard.put(source_str.to_string(), src.clone(), is_latest) {
                    warn!("failed to persist resolution cache: {e}");
                }
            }

            (src.owner, src.repo, src.git_ref, sp)
        };

        // Build a GitHub-shaped SourceSpec for the provider.
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

        std::fs::create_dir_all(mount_point)
            .map_err(|e| format!("failed to create mount point: {e}"))?;

        let id = source.id();
        let commit_sha = snapshot.commit_sha.clone();

        // Pick a loopback port and spawn the NFS server in the daemon's runtime.
        let port = pick_free_port()?;
        let addr = format!("127.0.0.1:{port}");

        // Build the protocol-agnostic VFS, then wrap it in the NFS adapter.
        let vfs = self
            .rt_handle
            .block_on(ctxfs_vfs::VfsState::new(
                provider,
                self.cache.clone(),
                snapshot,
                subpath,
            ))
            .map_err(|e| format!("failed to build VFS: {e}"))?;
        let fs = CtxfsNfs::new(Arc::new(vfs), github_source);
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
            backend: ctxfs_core::backend::Backend::Nfs,
            volume_path: None,
            symlink_paths: vec![],
        };

        let handle = MountHandle {
            info: info.clone(),
            _nfs: nfs_handle,
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
    ) -> Result<MountInfo, String> {
        info!("mount request: {source} -> {mount_point}");
        let server = self.clone();
        tokio::task::spawn_blocking(move || server.do_mount(&source, &mount_point))
            .await
            .map_err(|e| format!("mount task panicked: {e}"))?
    }

    async fn unmount(self, _: tarpc::context::Context, target: String) -> Result<(), String> {
        info!("unmount request: {target}");
        let mut mounts = self.mounts.write().await;

        let key = mounts
            .iter()
            .find(|(_, h)| h.info.mount_point == target || h.info.id == target)
            .map(|(k, _)| k.clone());

        match key {
            Some(k) => {
                if let Some(handle) = mounts.remove(&k) {
                    // Dropping the handle stops the NFS server task; the CLI
                    // has already run the kernel `umount` before calling us.
                    drop(handle);
                    info!("stopped NFS server for {target}");
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
