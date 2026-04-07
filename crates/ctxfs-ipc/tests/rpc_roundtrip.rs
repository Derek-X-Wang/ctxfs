#![allow(unused_results)]
//! Integration test: tarpc IPC round-trip over Unix domain sockets.
//!
//! Spins up a real tarpc server implementing `CtxfsService`, connects a client,
//! and verifies all RPC methods work end-to-end through the actual transport layer.

use ctxfs_ipc::service::{CacheStats, CtxfsService, MountInfo, MountStatus};
use ctxfs_ipc::transport;
use futures::StreamExt;
use std::sync::Arc;
use tarpc::server::Channel;
use tokio::sync::RwLock;

/// Minimal in-memory implementation of the service for testing transport.
#[derive(Clone)]
struct MockServer {
    mounts: Arc<RwLock<Vec<MountInfo>>>,
}

impl CtxfsService for MockServer {
    async fn mount(
        self,
        _: tarpc::context::Context,
        source: String,
        mount_point: String,
    ) -> Result<MountInfo, String> {
        let info = MountInfo {
            id: format!("mount_{}", self.mounts.read().await.len()),
            source,
            mount_point,
            commit_sha: "abc123".into(),
            status: MountStatus::Ready,
            mounted_at: "2025-01-01T00:00:00Z".into(),
            nfs_port: 11111,
        };
        self.mounts.write().await.push(info.clone());
        Ok(info)
    }

    async fn unmount(self, _: tarpc::context::Context, target: String) -> Result<(), String> {
        let mut mounts = self.mounts.write().await;
        let before = mounts.len();
        mounts.retain(|m| m.mount_point != target && m.id != target);
        if mounts.len() < before {
            Ok(())
        } else {
            Err(format!("not found: {target}"))
        }
    }

    async fn list(self, _: tarpc::context::Context) -> Vec<MountInfo> {
        self.mounts.read().await.clone()
    }

    async fn status(
        self,
        _: tarpc::context::Context,
        mount_id: String,
    ) -> Result<MountInfo, String> {
        self.mounts
            .read()
            .await
            .iter()
            .find(|m| m.id == mount_id)
            .cloned()
            .ok_or_else(|| format!("not found: {mount_id}"))
    }

    async fn cache_stats(self, _: tarpc::context::Context) -> Result<CacheStats, String> {
        Ok(CacheStats {
            total_bytes: 1024,
            entry_count: 5,
            freed_bytes: 0,
        })
    }

    async fn cache_prune(
        self,
        _: tarpc::context::Context,
        _max_bytes: Option<u64>,
    ) -> Result<CacheStats, String> {
        Ok(CacheStats {
            total_bytes: 512,
            entry_count: 3,
            freed_bytes: 512,
        })
    }

    async fn ping(self, _: tarpc::context::Context) -> String {
        "pong".into()
    }
}

/// Helper: start a server on a temp socket and return the socket path.
async fn start_server(socket_path: &std::path::Path) -> tokio::task::JoinHandle<()> {
    let server = MockServer {
        mounts: Arc::new(RwLock::new(Vec::new())),
    };

    let mut incoming = transport::listen(socket_path).await.unwrap();

    tokio::spawn(async move {
        while let Some(result) = incoming.next().await {
            if let Ok(transport) = result {
                let server = server.clone();
                let channel = tarpc::server::BaseChannel::with_defaults(transport);
                tokio::spawn(channel.execute(server.serve()).for_each(|resp| async {
                    tokio::spawn(resp);
                }));
            }
        }
    })
}

#[tokio::test]
async fn ping_pong() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("test.sock");

    let _server = start_server(&socket).await;
    // Small delay to let server bind
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = transport::connect_client(&socket).await.unwrap();
    let resp = client.ping(tarpc::context::current()).await.unwrap();
    assert_eq!(resp, "pong");
}

#[tokio::test]
async fn mount_list_unmount_lifecycle() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("lifecycle.sock");

    let _server = start_server(&socket).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = transport::connect_client(&socket).await.unwrap();

    // Mount
    let info = client
        .mount(
            tarpc::context::current(),
            "github:owner/repo@main".into(),
            "/tmp/mnt".into(),
        )
        .await
        .unwrap()
        .unwrap();

    assert_eq!(info.source, "github:owner/repo@main");
    assert_eq!(info.mount_point, "/tmp/mnt");
    assert_eq!(info.commit_sha, "abc123");

    // List
    let mounts = client.list(tarpc::context::current()).await.unwrap();
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].id, info.id);

    // Status
    let status = client
        .status(tarpc::context::current(), info.id.clone())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(status.source, "github:owner/repo@main");

    // Unmount
    client
        .unmount(tarpc::context::current(), "/tmp/mnt".into())
        .await
        .unwrap()
        .unwrap();

    // List should be empty
    let mounts = client.list(tarpc::context::current()).await.unwrap();
    assert!(mounts.is_empty());
}

#[tokio::test]
async fn unmount_nonexistent_returns_error() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("err.sock");

    let _server = start_server(&socket).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = transport::connect_client(&socket).await.unwrap();

    let result = client
        .unmount(tarpc::context::current(), "nonexistent".into())
        .await
        .unwrap();
    assert!(result.is_err());
    assert!(result.unwrap_err().contains("not found"));
}

#[tokio::test]
async fn cache_stats_rpc() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("cache.sock");

    let _server = start_server(&socket).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = transport::connect_client(&socket).await.unwrap();

    let stats = client
        .cache_stats(tarpc::context::current())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stats.total_bytes, 1024);
    assert_eq!(stats.entry_count, 5);
    assert_eq!(stats.freed_bytes, 0);
}

#[tokio::test]
async fn cache_prune_rpc() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("prune.sock");

    let _server = start_server(&socket).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = transport::connect_client(&socket).await.unwrap();

    let stats = client
        .cache_prune(tarpc::context::current(), Some(512))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(stats.freed_bytes, 512);
}

#[tokio::test]
async fn multiple_mounts() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("multi.sock");

    let _server = start_server(&socket).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    let client = transport::connect_client(&socket).await.unwrap();

    // Mount two repos
    let _ = client
        .mount(
            tarpc::context::current(),
            "github:a/b@main".into(),
            "/mnt/1".into(),
        )
        .await
        .unwrap()
        .unwrap();

    let _ = client
        .mount(
            tarpc::context::current(),
            "github:c/d@main".into(),
            "/mnt/2".into(),
        )
        .await
        .unwrap()
        .unwrap();

    let mounts = client.list(tarpc::context::current()).await.unwrap();
    assert_eq!(mounts.len(), 2);

    // Unmount first
    client
        .unmount(tarpc::context::current(), "/mnt/1".into())
        .await
        .unwrap()
        .unwrap();

    let mounts = client.list(tarpc::context::current()).await.unwrap();
    assert_eq!(mounts.len(), 1);
    assert_eq!(mounts[0].mount_point, "/mnt/2");
}

#[tokio::test]
async fn concurrent_clients() {
    let dir = tempfile::tempdir().unwrap();
    let socket = dir.path().join("concurrent.sock");

    let _server = start_server(&socket).await;
    tokio::time::sleep(std::time::Duration::from_millis(50)).await;

    // Spawn multiple clients concurrently
    let mut handles = Vec::new();
    for i in 0..5 {
        let socket = socket.clone();
        handles.push(tokio::spawn(async move {
            let client = transport::connect_client(&socket).await.unwrap();
            let resp = client.ping(tarpc::context::current()).await.unwrap();
            assert_eq!(resp, "pong");

            let _ = client
                .mount(
                    tarpc::context::current(),
                    format!("github:owner/repo{i}@main"),
                    format!("/mnt/{i}"),
                )
                .await
                .unwrap()
                .unwrap();
        }));
    }

    for handle in handles {
        handle.await.unwrap();
    }
}
