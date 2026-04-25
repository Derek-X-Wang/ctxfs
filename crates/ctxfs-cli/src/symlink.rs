use std::path::{Path, PathBuf};

/// The prefix used by ctxfs FSKit mounts under `/Volumes/`.
pub const CTXFS_VOLUMES_PREFIX: &str = "/Volumes/ctxfs/";

/// Create a symlink at `link_path` pointing to `target_path`.
///
/// The parent directory of `link_path` is resolved to an absolute path and
/// created if it does not exist. Returns the canonicalized absolute path of
/// the created symlink.
pub fn create_symlink(link_path: &Path, target_path: &Path) -> std::io::Result<PathBuf> {
    // Resolve parent to an absolute path, creating it if needed.
    let parent = link_path.parent().unwrap_or(Path::new("."));
    std::fs::create_dir_all(parent)?;
    let abs_parent = std::fs::canonicalize(parent)?;
    let abs_link = abs_parent.join(link_path.file_name().unwrap_or_default());

    #[cfg(unix)]
    std::os::unix::fs::symlink(target_path, &abs_link)?;

    #[cfg(not(unix))]
    return Err(std::io::Error::new(
        std::io::ErrorKind::Unsupported,
        "symlinks are not supported on this platform",
    ));

    Ok(abs_link)
}

/// Remove a symlink at `link_path` only if it resolves into `/Volumes/ctxfs/`.
///
/// Returns `Ok(true)` if the symlink was removed, `Ok(false)` if the path
/// does not exist, is not a symlink, or does not point into `/Volumes/ctxfs/`.
pub fn safe_remove_symlink(link_path: &Path) -> std::io::Result<bool> {
    // Path must exist (as a symlink, not following).
    let meta = match link_path.symlink_metadata() {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };

    if !meta.file_type().is_symlink() {
        return Ok(false);
    }

    if !is_ctxfs_symlink(link_path) {
        return Ok(false);
    }

    std::fs::remove_file(link_path)?;
    Ok(true)
}

/// Return `true` if `path` is a symlink whose target starts with
/// `/Volumes/ctxfs/`.
pub fn is_ctxfs_symlink(path: &Path) -> bool {
    match std::fs::read_link(path) {
        Ok(target) => target
            .to_str()
            .is_some_and(|s| s.starts_with(CTXFS_VOLUMES_PREFIX)),
        Err(_) => false,
    }
}

/// Resolve `path` through a ctxfs symlink.
///
/// If `path` is a ctxfs symlink, returns the symlink target. Otherwise returns
/// `path` unchanged.
pub fn resolve_ctxfs_path(path: &Path) -> PathBuf {
    if is_ctxfs_symlink(path) {
        if let Ok(target) = std::fs::read_link(path) {
            return target;
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    #[cfg(unix)]
    fn create_and_remove_symlink() {
        let dir = TempDir::new().unwrap();
        let target = PathBuf::from(CTXFS_VOLUMES_PREFIX).join("myrepo");
        let link = dir.path().join("link");

        // Create the symlink.
        let abs_link = create_symlink(&link, &target).unwrap();

        // The returned path must be absolute.
        assert!(abs_link.is_absolute(), "link path must be absolute");

        // The symlink must exist and point to our target.
        let resolved = std::fs::read_link(&abs_link).unwrap();
        assert_eq!(resolved, target);

        // is_ctxfs_symlink should return true.
        assert!(is_ctxfs_symlink(&abs_link));

        // safe_remove_symlink should succeed and return true.
        let removed = safe_remove_symlink(&abs_link).unwrap();
        assert!(removed);

        // The symlink should no longer exist.
        assert!(!abs_link.exists());
    }

    #[test]
    fn safe_remove_nonexistent() {
        let dir = TempDir::new().unwrap();
        let missing = dir.path().join("does_not_exist");
        let result = safe_remove_symlink(&missing).unwrap();
        assert!(!result, "should return false for non-existent path");
    }

    #[test]
    fn safe_remove_not_symlink() {
        let dir = TempDir::new().unwrap();
        let regular = dir.path().join("regular.txt");
        std::fs::write(&regular, b"hello").unwrap();
        let result = safe_remove_symlink(&regular).unwrap();
        assert!(!result, "should return false for a regular file");
        // File must still exist (not deleted).
        assert!(regular.exists());
    }

    #[test]
    fn resolve_ctxfs_path_regular_file() {
        let dir = TempDir::new().unwrap();
        let regular = dir.path().join("regular.txt");
        std::fs::write(&regular, b"data").unwrap();
        let resolved = resolve_ctxfs_path(&regular);
        assert_eq!(
            resolved, regular,
            "non-symlink path must be returned unchanged"
        );
    }

    #[test]
    #[cfg(unix)]
    fn is_ctxfs_symlink_non_ctxfs_target() {
        let dir = TempDir::new().unwrap();
        let target = dir.path().join("other_target");
        let link = dir.path().join("link_other");
        std::os::unix::fs::symlink(&target, &link).unwrap();
        assert!(!is_ctxfs_symlink(&link));
    }

    #[test]
    fn resolve_ctxfs_path_not_symlink() {
        // A path that doesn't exist should be returned unchanged.
        let non_existent = PathBuf::from("/tmp/ctxfs_test_nonexistent_path_xyz");
        let result = resolve_ctxfs_path(&non_existent);
        assert_eq!(result, non_existent);
    }
}
