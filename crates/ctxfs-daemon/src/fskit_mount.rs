//! FSKit mount orchestration.
//!
//! Builds the FilesystemAdapter, validates the mount directory, starts the
//! fskit-rs session, and returns an `FsKitHandle` the daemon tracks.

use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::source::SourceSpec;
use ctxfs_fskit::{volume_slug, FilesystemAdapter};
use ctxfs_manifest::Snapshot;
use ctxfs_vfs::VfsState;
use fskit_rs::MountOptions;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

use crate::daemon::FsKitHandle;

#[derive(Debug, thiserror::Error)]
pub enum FsKitMountError {
    #[error("fskit_bundle_id is not configured (set CTXFS_FSKIT_BUNDLE_ID)")]
    MissingBundleId,
    #[error("/Volumes/ctxfs/ does not exist — run `ctxfs setup install-fskit`")]
    ParentMissing,
    #[error("/Volumes/ctxfs/{slug} appears to already be mounted — unmount it first")]
    AlreadyMounted { slug: String },
    #[error("failed to create /Volumes/ctxfs/{slug}: {source}")]
    MountDir {
        slug: String,
        source: std::io::Error,
    },
    #[error("failed to build VfsState: {0}")]
    Vfs(String),
    #[error("failed to start fskit-rs session: {0}")]
    Session(String),
}

/// Start an FSKit mount. Returns a handle whose shutdown() unmounts on scope exit.
///
/// `token_hex` is the hex-encoded form of `token`; it is forwarded to fskitd as
/// `FSTaskOptions` so the appex can send it back as the first frame of every bridge
/// TCP connection for authentication.
pub async fn start_fskit_mount(
    source: &SourceSpec,
    provider: SharedProvider,
    cache: Arc<BlobCache>,
    snapshot: Snapshot,
    subpath: Option<String>,
    bundle_id: &str,
    token: &ctxfs_fskit::AuthToken,
    token_hex: &str,
) -> Result<FsKitHandle, FsKitMountError> {
    let parent = PathBuf::from("/Volumes/ctxfs");
    if !parent.exists() {
        return Err(FsKitMountError::ParentMissing);
    }

    let slug = volume_slug(source);
    let volume_path = parent.join(&slug);

    validate_volume_path(&volume_path, &slug)?;

    let vfs = Arc::new(
        VfsState::new(provider, cache, snapshot, subpath)
            .await
            .map_err(|e| FsKitMountError::Vfs(e.to_string()))?,
    );
    let adapter = FilesystemAdapter::new(vfs, slug.clone());

    let opts = MountOptions {
        fskit_id: bundle_id.to_string(),
        mount_point: volume_path.clone(),
        force: true,
        auth_token: Some(token.bytes_vec()),
        task_options: vec![format!("token={token_hex}")],
    };
    info!(
        "starting FSKit mount at {} (bundle_id={}, auth=yes)",
        volume_path.display(),
        bundle_id
    );
    let session = fskit_rs::mount(adapter, opts)
        .await
        .map_err(|e| FsKitMountError::Session(e.to_string()))?;

    Ok(FsKitHandle::new(session, volume_path))
}

/// Ensure /Volumes/ctxfs/<slug> exists and nothing is already mounted on it.
fn validate_volume_path(volume_path: &std::path::Path, slug: &str) -> Result<(), FsKitMountError> {
    match std::fs::symlink_metadata(volume_path) {
        Ok(meta) if meta.is_dir() => {
            if is_already_mounted(volume_path) {
                return Err(FsKitMountError::AlreadyMounted {
                    slug: slug.to_string(),
                });
            }
            Ok(())
        }
        Ok(_) => Err(FsKitMountError::MountDir {
            slug: slug.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path exists but is not a directory",
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(volume_path)
            .map_err(|source| FsKitMountError::MountDir {
                slug: slug.to_string(),
                source,
            }),
        Err(e) => Err(FsKitMountError::MountDir {
            slug: slug.to_string(),
            source: e,
        }),
    }
}

fn is_already_mounted(path: &std::path::Path) -> bool {
    match std::process::Command::new("mount").output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let path_str = path.to_string_lossy();
            stdout
                .lines()
                .any(|line| line.contains(&format!(" on {path_str} ")))
        }
        _ => {
            warn!("could not query mount table; assuming not mounted");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_nonexistent_creates_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("newslug");
        assert!(!path.exists());
        validate_volume_path(&path, "newslug").unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn validate_existing_empty_dir_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("exists");
        std::fs::create_dir(&path).unwrap();
        validate_volume_path(&path, "exists").unwrap();
    }

    #[test]
    fn validate_file_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("regularfile");
        std::fs::write(&path, "data").unwrap();
        assert!(matches!(
            validate_volume_path(&path, "regularfile"),
            Err(FsKitMountError::MountDir { .. })
        ));
    }
}
