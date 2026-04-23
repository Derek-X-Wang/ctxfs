use ctxfs_core::backend::Backend;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum MountStatus {
    Mounting,
    Ready,
    Error(String),
    Unmounting,
}

impl std::fmt::Display for MountStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MountStatus::Mounting => write!(f, "mounting"),
            MountStatus::Ready => write!(f, "ready"),
            MountStatus::Error(e) => write!(f, "error: {e}"),
            MountStatus::Unmounting => write!(f, "unmounting"),
        }
    }
}

fn default_backend() -> Backend {
    Backend::Nfs
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountInfo {
    pub id: String,
    pub source: String,
    pub mount_point: String,
    pub commit_sha: String,
    pub status: MountStatus,
    pub mounted_at: String,
    /// Loopback port where the daemon's NFS server for this mount is listening.
    /// The CLI uses this to invoke `mount_nfs` on the user's behalf.
    /// `None` for `FSKit` mounts which do not use NFS.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nfs_port: Option<u16>,
    /// Which backend is serving this mount.
    #[serde(default = "default_backend")]
    pub backend: Backend,
    /// Filesystem path to the volume (`FSKit` mounts only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_path: Option<String>,
    /// Symlink paths tracked for this mount (e.g. project-level convenience links).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symlink_paths: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheStats {
    pub total_bytes: u64,
    pub entry_count: usize,
    pub freed_bytes: u64,
    pub tree_count: usize,
    pub tree_bytes: u64,
    pub resolution_count: usize,
}

/// Structured breakdown of blob and tree cache state.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheBreakdown {
    pub blob_bytes: u64,
    pub blob_count: u64,
    pub tree_bytes: u64,
    pub tree_count: u64,
    pub max_bytes: u64,
}

/// tarpc service definition.
#[tarpc::service]
pub trait CtxfsService {
    async fn mount(
        source: String,
        mount_point: String,
        backend: ctxfs_core::Backend,
    ) -> Result<MountInfo, String>;
    async fn unmount(target: String) -> Result<(), String>;
    async fn list() -> Vec<MountInfo>;
    async fn status(mount_id: String) -> Result<MountInfo, String>;
    async fn cache_stats() -> Result<CacheStats, String>;
    async fn cache_prune(max_bytes: Option<u64>) -> Result<CacheStats, String>;
    /// Returns a structured breakdown of blob/tree bytes, counts, and current max.
    async fn cache_breakdown() -> Result<CacheBreakdown, String>;
    /// Updates `BlobCache.max_bytes` at runtime (triggers eviction if needed).
    /// Returns a fresh `CacheBreakdown` after the limit change.
    async fn set_cache_limits(max_bytes: u64) -> Result<CacheBreakdown, String>;
    /// Prune blob cache only until usage fits within `target_bytes`.
    /// Returns bytes freed.
    async fn prune_blobs(target_bytes: u64) -> Result<u64, String>;
    async fn ping() -> String;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mount_status_display() {
        assert_eq!(MountStatus::Mounting.to_string(), "mounting");
        assert_eq!(MountStatus::Ready.to_string(), "ready");
        assert_eq!(MountStatus::Unmounting.to_string(), "unmounting");
        assert_eq!(
            MountStatus::Error("disk full".into()).to_string(),
            "error: disk full"
        );
    }

    #[test]
    fn mount_info_serde_roundtrip() {
        let info = MountInfo {
            id: "test_repo_main".into(),
            source: "github:owner/repo@main".into(),
            mount_point: "/tmp/mnt".into(),
            commit_sha: "abc123def456".into(),
            status: MountStatus::Ready,
            mounted_at: "2025-01-01T00:00:00Z".into(),
            nfs_port: Some(11111),
            backend: Backend::Nfs,
            volume_path: None,
            symlink_paths: vec![],
        };

        let json = serde_json::to_string(&info).unwrap();
        let info2: MountInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(info.id, info2.id);
        assert_eq!(info.source, info2.source);
        assert_eq!(info.mount_point, info2.mount_point);
        assert_eq!(info.commit_sha, info2.commit_sha);
        assert_eq!(info.mounted_at, info2.mounted_at);
    }

    #[test]
    fn mount_info_with_error_status() {
        let info = MountInfo {
            id: "err_mount".into(),
            source: "github:owner/repo@main".into(),
            mount_point: "/tmp/err".into(),
            commit_sha: "000000".into(),
            status: MountStatus::Error("FUSE unavailable".into()),
            mounted_at: "2025-01-01T00:00:00Z".into(),
            nfs_port: None,
            backend: Backend::Nfs,
            volume_path: None,
            symlink_paths: vec![],
        };

        let json = serde_json::to_string(&info).unwrap();
        let info2: MountInfo = serde_json::from_str(&json).unwrap();
        match info2.status {
            MountStatus::Error(msg) => assert_eq!(msg, "FUSE unavailable"),
            other => panic!("expected Error, got {other}"),
        }
    }

    #[test]
    fn cache_stats_serde_roundtrip() {
        let stats = CacheStats {
            total_bytes: 1024,
            entry_count: 10,
            freed_bytes: 512,
            tree_count: 5,
            tree_bytes: 2048,
            resolution_count: 3,
        };
        let json = serde_json::to_string(&stats).unwrap();
        let stats2: CacheStats = serde_json::from_str(&json).unwrap();
        assert_eq!(stats.total_bytes, stats2.total_bytes);
        assert_eq!(stats.entry_count, stats2.entry_count);
        assert_eq!(stats.freed_bytes, stats2.freed_bytes);
    }

    #[test]
    fn mount_status_all_variants_serialize() {
        let variants = vec![
            MountStatus::Mounting,
            MountStatus::Ready,
            MountStatus::Error("test".into()),
            MountStatus::Unmounting,
        ];
        for v in variants {
            let json = serde_json::to_string(&v).unwrap();
            let v2: MountStatus = serde_json::from_str(&json).unwrap();
            assert_eq!(v.to_string(), v2.to_string());
        }
    }

    #[test]
    fn mount_info_debug() {
        let info = MountInfo {
            id: "test".into(),
            source: "github:a/b@c".into(),
            mount_point: "/mnt".into(),
            commit_sha: "sha".into(),
            status: MountStatus::Ready,
            mounted_at: "now".into(),
            nfs_port: Some(12345),
            backend: Backend::Nfs,
            volume_path: None,
            symlink_paths: vec![],
        };
        let debug = format!("{info:?}");
        assert!(debug.contains("MountInfo"));
        assert!(debug.contains("test"));
    }

    #[test]
    fn mount_info_with_backend_and_volume_path() {
        let info = MountInfo {
            id: "fskit_mount".into(),
            source: "github:owner/repo@main".into(),
            mount_point: "/tmp/fskit_mnt".into(),
            commit_sha: "def456abc789".into(),
            status: MountStatus::Ready,
            mounted_at: "2025-01-01T00:00:00Z".into(),
            nfs_port: None,
            backend: Backend::FsKit,
            volume_path: Some("/Volumes/ctxfs-owner-repo-main".into()),
            symlink_paths: vec!["/tmp/links/repo".into(), "/tmp/links/repo-alias".into()],
        };

        let json = serde_json::to_string(&info).unwrap();
        let info2: MountInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(info2.backend, Backend::FsKit);
        assert_eq!(info2.nfs_port, None);
        assert_eq!(
            info2.volume_path.as_deref(),
            Some("/Volumes/ctxfs-owner-repo-main")
        );
        assert_eq!(info2.symlink_paths.len(), 2);
        assert_eq!(info2.symlink_paths[0], "/tmp/links/repo");

        // FSKit fields should appear in JSON
        assert!(json.contains("fskit"));
        assert!(json.contains("volume_path"));
        assert!(json.contains("symlink_paths"));
        // nfs_port should be absent (skip_serializing_if)
        assert!(!json.contains("nfs_port"));
    }

    #[test]
    fn mount_info_nfs_backward_compat() {
        let info = MountInfo {
            id: "nfs_mount".into(),
            source: "github:owner/repo@main".into(),
            mount_point: "/tmp/nfs_mnt".into(),
            commit_sha: "abc123".into(),
            status: MountStatus::Ready,
            mounted_at: "2025-01-01T00:00:00Z".into(),
            nfs_port: Some(12345),
            backend: Backend::Nfs,
            volume_path: None,
            symlink_paths: vec![],
        };

        let json = serde_json::to_string(&info).unwrap();
        let info2: MountInfo = serde_json::from_str(&json).unwrap();

        assert_eq!(info2.backend, Backend::Nfs);
        assert_eq!(info2.nfs_port, Some(12345));
        assert_eq!(info2.volume_path, None);
        assert!(info2.symlink_paths.is_empty());

        // volume_path and symlink_paths should be absent (skip_serializing_if)
        assert!(!json.contains("volume_path"));
        assert!(!json.contains("symlink_paths"));

        // Old JSON without backend field should deserialize to Nfs by default
        let old_json = r#"{"id":"old","source":"github:a/b@c","mount_point":"/mnt","commit_sha":"abc","status":"Ready","mounted_at":"2025-01-01T00:00:00Z","nfs_port":9999}"#;
        let old: MountInfo = serde_json::from_str(old_json).unwrap();
        assert_eq!(old.backend, Backend::Nfs);
        assert_eq!(old.nfs_port, Some(9999));
        assert!(old.symlink_paths.is_empty());
    }
}
