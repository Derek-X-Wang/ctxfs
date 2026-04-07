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
    pub nfs_port: u16,
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

/// tarpc service definition.
#[tarpc::service]
pub trait CtxfsService {
    async fn mount(source: String, mount_point: String) -> Result<MountInfo, String>;
    async fn unmount(target: String) -> Result<(), String>;
    async fn list() -> Vec<MountInfo>;
    async fn status(mount_id: String) -> Result<MountInfo, String>;
    async fn cache_stats() -> Result<CacheStats, String>;
    async fn cache_prune(max_bytes: Option<u64>) -> Result<CacheStats, String>;
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
            nfs_port: 11111,
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
            nfs_port: 0,
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
            nfs_port: 12345,
        };
        let debug = format!("{info:?}");
        assert!(debug.contains("MountInfo"));
        assert!(debug.contains("test"));
    }
}
