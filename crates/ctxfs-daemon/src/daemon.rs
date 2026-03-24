use anyhow::{bail, Context, Result};
use ctxfs_cache::BlobCache;
use ctxfs_core::config::Config;
use ctxfs_core::source::SourceSpec;
use ctxfs_fuse::CtxfsFilesystem;
use ctxfs_ipc::service::{CacheStats, CtxfsService, MountInfo, MountStatus};
use ctxfs_ipc::transport;
use ctxfs_manifest::Snapshot;
use ctxfs_provider_git::GitHubProvider;
use futures::StreamExt;
use std::collections::HashMap;
use std::sync::Arc;
use tarpc::server::Channel;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;
use tracing::{error, info};

struct MountHandle {
    info: MountInfo,
    session: fuser::BackgroundSession,
}

pub struct Daemon {
    config: Config,
    cache: Arc<BlobCache>,
    mounts: Arc<RwLock<HashMap<String, MountHandle>>>,
    cancel: CancellationToken,
}

#[derive(Clone)]
struct DaemonServer {
    cache: Arc<BlobCache>,
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

        Ok(Self {
            config,
            cache,
            mounts: Arc::new(RwLock::new(HashMap::new())),
            cancel: CancellationToken::new(),
        })
    }

    pub async fn run(&self) -> Result<()> {
        self.write_pid_file()?;

        let mut incoming = transport::listen(&self.config.socket_path)
            .await
            .context("failed to create IPC listener")?;

        info!(
            "daemon listening on {}",
            self.config.socket_path.display()
        );

        let server = DaemonServer {
            cache: self.cache.clone(),
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
                            tokio::spawn(channel.execute(server_clone.serve()));
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
        }

        self.cleanup().await;
        Ok(())
    }

    fn write_pid_file(&self) -> Result<()> {
        if let Some(parent) = self.config.pid_file.parent() {
            std::fs::create_dir_all(parent)?;
        }

        // Check for stale PID
        if self.config.pid_file.exists() {
            if let Ok(pid_str) = std::fs::read_to_string(&self.config.pid_file) {
                if let Ok(pid) = pid_str.trim().parse::<i32>() {
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
                info!("unmounting {}", handle.info.mount_point);
                drop(handle.session);
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

impl DaemonServer {
    fn do_mount(&self, source_str: &str, mount_point: &str) -> Result<MountInfo, String> {
        let source =
            SourceSpec::parse(source_str).map_err(|e| format!("invalid source: {e}"))?;

        let provider = Arc::new(GitHubProvider::new(
            self.config.github_token.as_deref(),
            self.cache.clone(),
        ));

        let snapshot_data = self
            .rt_handle
            .block_on(provider.fetch_snapshot(&source))
            .map_err(|e| format!("failed to fetch snapshot: {e}"))?;

        let snapshot: Snapshot = serde_json::from_slice(&snapshot_data)
            .map_err(|e| format!("failed to parse snapshot: {e}"))?;

        std::fs::create_dir_all(mount_point)
            .map_err(|e| format!("failed to create mount point: {e}"))?;

        let id = source.id();
        let commit_sha = snapshot.commit_sha.clone();

        let fs = CtxfsFilesystem::new(
            self.rt_handle.clone(),
            provider,
            source,
            self.cache.clone(),
            snapshot,
        );

        let session = fs
            .mount(mount_point)
            .map_err(|e| format!("FUSE mount failed: {e}"))?;

        let info = MountInfo {
            id: id.clone(),
            source: source_str.to_string(),
            mount_point: mount_point.to_string(),
            commit_sha,
            status: MountStatus::Ready,
            mounted_at: chrono::Utc::now().to_rfc3339(),
        };

        let handle = MountHandle {
            info: info.clone(),
            session,
        };

        self.rt_handle.block_on(async {
            self.mounts.write().await.insert(id, handle);
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
                    drop(handle.session);
                    info!("unmounted {target}");
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
        Ok(CacheStats {
            total_bytes,
            entry_count,
            freed_bytes: 0,
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

        let (total_bytes, entry_count) = self.cache.stats();

        Ok(CacheStats {
            total_bytes,
            entry_count,
            freed_bytes: freed,
        })
    }

    async fn ping(self, _: tarpc::context::Context) -> String {
        "pong".to_string()
    }
}
