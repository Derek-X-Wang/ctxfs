use std::collections::HashMap;
use std::path::Path;
use std::path::PathBuf;

use ctxfs_core::Backend;
use ctxfs_ipc::service::CtxfsServiceClient;
use futures::StreamExt;

/// Max concurrent daemon mount RPCs to avoid overwhelming the daemon.
const MAX_CONCURRENT_MOUNTS: usize = 16;

/// Result of a single mount operation within a batch.
#[derive(Debug)]
pub struct MountResult {
    pub source: String,
    pub mount_point: PathBuf,
    pub success: bool,
    pub error: Option<String>,
    pub note: Option<String>,
}

/// Result of a single unmount operation within a batch.
#[derive(Debug)]
pub struct UnmountResult {
    pub mount_point: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Issue daemon mount RPCs concurrently, then run kernel mounts sequentially.
///
/// For each (`source_spec` -> `mount_path`) pair the daemon is asked to start an
/// NFS server. Successful daemon mounts are then kernel-mounted one at a time.
/// If the kernel mount fails for a given entry, the daemon mount is cleaned up.
pub async fn batch_mount(
    client: &CtxfsServiceClient,
    mounts: &HashMap<String, PathBuf>,
    server_only: bool,
) -> Vec<MountResult> {
    // Collect into a stable order for deterministic output.
    let mut entries: Vec<(&String, &PathBuf)> = mounts.iter().collect();
    entries.sort_by_key(|(src, _)| src.to_owned());

    // Query active mounts so we can skip already-mounted targets.
    let active_mount_points: std::collections::HashSet<String> =
        match client.list(tarpc::context::current()).await {
            Ok(mounts) => mounts.into_iter().map(|m| m.mount_point).collect(),
            Err(e) => {
                tracing::warn!("failed to query active mounts: {e}");
                std::collections::HashSet::new()
            }
        };

    // Separate already-mounted entries from new ones.
    let mut results = Vec::with_capacity(entries.len());
    let mut to_mount = Vec::new();
    for (source, mount_path) in &entries {
        let mp_str = mount_path.to_string_lossy().to_string();
        if active_mount_points.contains(&mp_str) {
            results.push(MountResult {
                source: (*source).clone(),
                mount_point: (*mount_path).clone(),
                success: true,
                error: None,
                note: Some("already mounted".to_string()),
            });
        } else {
            to_mount.push((*source, *mount_path));
        }
    }

    // Phase 1: issue all daemon mount RPCs concurrently.
    let daemon_futs: Vec<_> = to_mount
        .iter()
        .map(|(source, mount_path)| {
            let mp_str = mount_path.to_string_lossy().to_string();
            let src = (*source).clone();
            let client = client.clone();
            async move {
                // Ensure the mount directory exists.
                if let Err(e) = std::fs::create_dir_all(mount_path) {
                    return (src, mp_str, Err(format!("create dir: {e}")));
                }
                let ctx = crate::long_context();
                match client
                    .mount(ctx, src.clone(), mp_str.clone(), Backend::Nfs)
                    .await
                {
                    Ok(Ok(info)) => (src, mp_str, Ok(info)),
                    Ok(Err(e)) => (src, mp_str, Err(e)),
                    Err(e) => (src, mp_str, Err(format!("rpc: {e}"))),
                }
            }
        })
        .collect();

    let daemon_results: Vec<_> = futures::stream::iter(daemon_futs)
        .buffer_unordered(MAX_CONCURRENT_MOUNTS)
        .collect()
        .await;

    // Phase 2: kernel mount (sequential — requires sudo prompts).
    for (source, mp_str, daemon_res) in daemon_results {
        match daemon_res {
            Ok(info) => {
                if server_only {
                    results.push(MountResult {
                        source,
                        mount_point: PathBuf::from(&mp_str),
                        success: true,
                        error: None,
                        note: None,
                    });
                    continue;
                }

                let port = match info.nfs_port {
                    Some(p) => p,
                    None => {
                        results.push(MountResult {
                            source,
                            mount_point: PathBuf::from(&mp_str),
                            success: false,
                            error: Some("mount did not return an NFS port".into()),
                            note: None,
                        });
                        continue;
                    }
                };
                if let Err(e) = crate::run_mount_nfs(port, &mp_str) {
                    // Clean up daemon-side mount.
                    let _ = client
                        .unmount(tarpc::context::current(), mp_str.clone())
                        .await;
                    results.push(MountResult {
                        source,
                        mount_point: PathBuf::from(&mp_str),
                        success: false,
                        error: Some(format!("kernel mount: {e}")),
                        note: None,
                    });
                } else {
                    results.push(MountResult {
                        source,
                        mount_point: PathBuf::from(&mp_str),
                        success: true,
                        error: None,
                        note: None,
                    });
                }
            }
            Err(e) => {
                results.push(MountResult {
                    source,
                    mount_point: PathBuf::from(&mp_str),
                    success: false,
                    error: Some(e),
                    note: None,
                });
            }
        }
    }

    results
}

/// Unmount all active mounts whose mount point is a child of `mount_dir`.
pub async fn batch_unmount_dir(
    client: &CtxfsServiceClient,
    mount_dir: &Path,
) -> Vec<UnmountResult> {
    let mounts = match client.list(tarpc::context::current()).await {
        Ok(m) => m,
        Err(e) => {
            return vec![UnmountResult {
                mount_point: mount_dir.to_string_lossy().to_string(),
                success: false,
                error: Some(format!("failed to list mounts: {e}")),
            }];
        }
    };

    let exact = mount_dir.to_string_lossy().to_string();
    let prefix = {
        let mut p = exact.clone();
        if !p.ends_with('/') {
            p.push('/');
        }
        p
    };
    let targets: Vec<String> = mounts
        .into_iter()
        .filter(|m| m.mount_point == exact || m.mount_point.starts_with(&prefix))
        .map(|m| m.mount_point)
        .collect();

    unmount_targets(client, &targets).await
}

/// Unmount all active mounts.
pub async fn batch_unmount_all(client: &CtxfsServiceClient) -> Vec<UnmountResult> {
    let mounts = match client.list(tarpc::context::current()).await {
        Ok(m) => m,
        Err(e) => {
            return vec![UnmountResult {
                mount_point: String::from("(all)"),
                success: false,
                error: Some(format!("failed to list mounts: {e}")),
            }];
        }
    };

    let targets: Vec<String> = mounts.into_iter().map(|m| m.mount_point).collect();
    unmount_targets(client, &targets).await
}

async fn unmount_targets(client: &CtxfsServiceClient, targets: &[String]) -> Vec<UnmountResult> {
    let mut results = Vec::with_capacity(targets.len());

    for target in targets {
        // Step 1: kernel umount.
        if let Err(e) = crate::run_umount(target) {
            eprintln!("warning: kernel umount for {target} failed: {e}");
        }

        // Step 2: daemon cleanup.
        match client
            .unmount(tarpc::context::current(), target.clone())
            .await
        {
            Ok(Ok(())) => results.push(UnmountResult {
                mount_point: target.clone(),
                success: true,
                error: None,
            }),
            Ok(Err(e)) => results.push(UnmountResult {
                mount_point: target.clone(),
                success: false,
                error: Some(e),
            }),
            Err(e) => results.push(UnmountResult {
                mount_point: target.clone(),
                success: false,
                error: Some(format!("rpc: {e}")),
            }),
        }
    }

    results
}

/// Print a summary table for batch mount results.
pub fn print_mount_summary(results: &[MountResult]) {
    let ok = results.iter().filter(|r| r.success).count();
    let fail = results.len() - ok;

    for r in results {
        let status = if r.success { " ok " } else { "ERR " };
        print!("  [{status}] {} -> {}", r.source, r.mount_point.display());
        if let Some(ref e) = r.error {
            print!("  ({e})");
        }
        if let Some(ref n) = r.note {
            print!("  [{n}]");
        }
        println!();
    }
    println!();
    println!("{ok} mounted, {fail} failed (of {} total)", results.len());
}

/// Print a summary table for batch unmount results.
pub fn print_unmount_summary(results: &[UnmountResult]) {
    let ok = results.iter().filter(|r| r.success).count();
    let fail = results.len() - ok;

    for r in results {
        let status = if r.success { " ok " } else { "ERR " };
        print!("  [{status}] {}", r.mount_point);
        if let Some(ref e) = r.error {
            print!("  ({e})");
        }
        println!();
    }
    println!();
    println!("{ok} unmounted, {fail} failed (of {} total)", results.len());
}
