//! Adapter translating between `ctxfs-vfs` types and `fskit-rs` (`FSKit` protobuf) types.

use ctxfs_vfs::{NodeAttr, NodeType, VfsError};
use fskit_rs::{Error as FsKitError, Item, ItemAttributes, ItemType};

/// `FSKit` root inode ID. `FSKit` conventionally expects the root at 2.
pub(crate) const FSKIT_ROOT_ID: u64 = 2;

/// Map a VFS inode ID to an `FSKit` inode ID.
///
/// `VfsState` uses root=1, children=2,3,... `FSKit` requires root=2. We use a
/// simple `+1` offset: vfs(1)→fskit(2), vfs(2)→fskit(3), vfs(3)→fskit(4), ...
/// This is a bijection — `FSKit` inode 1 is never used.
pub(crate) fn vfs_to_fskit_inode(vfs_id: u64) -> u64 {
    vfs_id.saturating_add(1)
}

/// Inverse of `vfs_to_fskit_inode`. Debug-asserts on `FSKit` id 0 or 1
/// (never emitted by us, so they indicate a bug if `FSKit` ever sends them).
pub(crate) fn fskit_to_vfs_inode(fskit_id: u64) -> u64 {
    debug_assert!(
        fskit_id >= FSKIT_ROOT_ID,
        "FSKit inode {fskit_id} is reserved"
    );
    fskit_id.saturating_sub(1).max(1)
}

/// Translate a VFS `NodeType` to an `FSKit` `ItemType`.
pub(crate) fn node_type_to_item_type(kind: NodeType) -> ItemType {
    match kind {
        NodeType::File => ItemType::File,
        NodeType::Directory => ItemType::Directory,
        NodeType::Symlink => ItemType::Symlink,
    }
}

/// Translate a VFS `NodeAttr` to an `FSKit` `ItemAttributes`.
///
/// Both `inode` and `parent_inode` are remapped through `vfs_to_fskit_inode`.
#[allow(unsafe_code)]
pub(crate) fn node_attr_to_item_attributes(attr: &NodeAttr) -> ItemAttributes {
    let mode = match attr.kind {
        NodeType::Directory => 0o555,
        NodeType::File => {
            if attr.executable {
                0o555
            } else {
                0o444
            }
        }
        NodeType::Symlink => 0o777,
    };
    let link_count = match attr.kind {
        NodeType::Directory => 2,
        _ => 1,
    };
    // SAFETY: getuid/getgid are safe POSIX calls with no UB.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    ItemAttributes {
        file_id: Some(vfs_to_fskit_inode(attr.inode)),
        parent_id: Some(vfs_to_fskit_inode(attr.parent_inode)),
        r#type: Some(node_type_to_item_type(attr.kind) as i32),
        mode: Some(mode),
        uid: Some(uid),
        gid: Some(gid),
        link_count: Some(link_count),
        size: Some(attr.size),
        alloc_size: Some(attr.size),
        ..Default::default()
    }
}

/// Build an `FSKit` `Item` for a given name and attributes.
pub(crate) fn make_item(name: &str, attr: &NodeAttr) -> Item {
    Item {
        name: name.as_bytes().to_vec(),
        attributes: Some(node_attr_to_item_attributes(attr)),
    }
}

/// Translate `VfsError` to `fskit_rs::Error` (POSIX errno).
///
/// `RateLimited` maps to `EAGAIN` so macOS Finder / userspace clients see
/// a retryable signal rather than `EIO`. The `retry_after_secs` value is
/// not propagated to FSKit (no field for it in the POSIX error shape); it
/// is logged at the `ctxfs.provider.throttle` tracing target by
/// `provider-git::check_rate_limit` for diagnosis.
pub(crate) fn vfs_err_to_fskit(err: &VfsError) -> FsKitError {
    let errno = match err {
        VfsError::NotFound => libc::ENOENT,
        VfsError::NotDir => libc::ENOTDIR,
        VfsError::IsDir => libc::EISDIR,
        VfsError::Invalid => libc::EINVAL,
        VfsError::ReadOnly => libc::EROFS,
        VfsError::Io(_) => libc::EIO,
        VfsError::RateLimited { .. } => libc::EAGAIN,
    };
    FsKitError::Posix(errno)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_attr(inode: u64, parent: u64, size: u64, executable: bool) -> NodeAttr {
        NodeAttr {
            inode,
            parent_inode: parent,
            size,
            kind: NodeType::File,
            executable,
        }
    }

    #[test]
    fn inode_bijection_root() {
        assert_eq!(vfs_to_fskit_inode(1), FSKIT_ROOT_ID);
        assert_eq!(fskit_to_vfs_inode(FSKIT_ROOT_ID), 1);
    }

    #[test]
    fn inode_bijection_children() {
        // VfsState children start at 2
        assert_eq!(vfs_to_fskit_inode(2), 3);
        assert_eq!(vfs_to_fskit_inode(3), 4);
        assert_eq!(fskit_to_vfs_inode(3), 2);
        assert_eq!(fskit_to_vfs_inode(4), 3);
    }

    #[test]
    fn inode_bijection_roundtrip() {
        for vfs_id in [1u64, 2, 3, 100, 1_000_000] {
            assert_eq!(fskit_to_vfs_inode(vfs_to_fskit_inode(vfs_id)), vfs_id);
        }
    }

    #[test]
    fn no_collision_between_root_and_children() {
        // The critical bug v1 had: vfs_to_fskit(1) must not equal vfs_to_fskit(2).
        assert_ne!(vfs_to_fskit_inode(1), vfs_to_fskit_inode(2));
        assert_eq!(vfs_to_fskit_inode(1), 2);
        assert_eq!(vfs_to_fskit_inode(2), 3);
    }

    #[test]
    fn node_type_translation() {
        assert!(matches!(
            node_type_to_item_type(NodeType::File),
            ItemType::File
        ));
        assert!(matches!(
            node_type_to_item_type(NodeType::Directory),
            ItemType::Directory
        ));
        assert!(matches!(
            node_type_to_item_type(NodeType::Symlink),
            ItemType::Symlink
        ));
    }

    #[test]
    fn file_attr_translates_correctly() {
        let attr = file_attr(5, 1, 1024, false);
        let item_attr = node_attr_to_item_attributes(&attr);
        assert_eq!(item_attr.file_id, Some(6)); // 5 + 1
        assert_eq!(item_attr.parent_id, Some(2)); // 1 + 1 (root)
        assert_eq!(item_attr.r#type, Some(ItemType::File as i32));
        assert_eq!(item_attr.mode, Some(0o444));
        assert_eq!(item_attr.size, Some(1024));
    }

    #[test]
    fn directory_mode_and_link_count() {
        let attr = NodeAttr {
            inode: 1,
            parent_inode: 1,
            size: 0,
            kind: NodeType::Directory,
            executable: false,
        };
        let item_attr = node_attr_to_item_attributes(&attr);
        assert_eq!(item_attr.mode, Some(0o555));
        assert_eq!(item_attr.link_count, Some(2));
    }

    #[test]
    fn executable_file_mode() {
        let attr = file_attr(5, 1, 10, true);
        assert_eq!(node_attr_to_item_attributes(&attr).mode, Some(0o555));
    }

    #[test]
    fn error_translation_all_variants() {
        assert!(
            matches!(vfs_err_to_fskit(&VfsError::NotFound), FsKitError::Posix(e) if e == libc::ENOENT)
        );
        assert!(
            matches!(vfs_err_to_fskit(&VfsError::NotDir), FsKitError::Posix(e) if e == libc::ENOTDIR)
        );
        assert!(
            matches!(vfs_err_to_fskit(&VfsError::IsDir), FsKitError::Posix(e) if e == libc::EISDIR)
        );
        assert!(
            matches!(vfs_err_to_fskit(&VfsError::Invalid), FsKitError::Posix(e) if e == libc::EINVAL)
        );
        assert!(
            matches!(vfs_err_to_fskit(&VfsError::ReadOnly), FsKitError::Posix(e) if e == libc::EROFS)
        );
        assert!(
            matches!(vfs_err_to_fskit(&VfsError::Io("x".into())), FsKitError::Posix(e) if e == libc::EIO)
        );
        // RateLimited must map to EAGAIN, NOT EIO — EIO would defeat the
        // spec's "zero EIO under 429" criterion.
        assert!(matches!(
            vfs_err_to_fskit(&VfsError::RateLimited { retry_after_secs: 30 }),
            FsKitError::Posix(e) if e == libc::EAGAIN
        ));
    }
}

// ─── FilesystemAdapter ───────────────────────────────────────────────────────

use async_trait::async_trait;
use ctxfs_vfs::VfsState;
use fskit_rs::{
    directory_entries, AccessMask, DirectoryEntries, Filesystem, OpenMode, PathConfOperations,
    PreallocateFlag, ResourceIdentifier, Result as FsKitResult, SetXattrPolicy, StatFsResult,
    SupportedCapabilities, SyncFlags, TaskOptions, VolumeBehavior, VolumeIdentifier, Xattrs,
};
use std::ffi::OsStr;
use std::sync::Arc;
use tokio::sync::mpsc;
use tracing::{debug, info};

/// Adapter implementing `fskit_rs::Filesystem` on top of a shared `VfsState`.
///
/// Cloneable because `fskit_rs::mount` requires `Clone + Send + Sync + 'static`.
/// All state lives in `Arc<VfsState>`; clones share the inode table.
#[derive(Clone, Debug)]
pub struct FilesystemAdapter {
    vfs: Arc<VfsState>,
    /// Slug-based identifier, safe as a file-system path component.
    volume_name: String,
    /// Stable opaque ID derived from the slug (used in `VolumeIdentifier.id`).
    volume_id: String,
    /// Human-readable name shown in Finder sidebar (used in `VolumeIdentifier.name`).
    display_name: String,
    /// Signaled when the filesystem unmounts (e.g., Finder eject).
    /// The daemon listens on the paired receiver and performs full cleanup
    /// (symlinks, mounts.json, `MountHandle` drop).
    unmount_notifier: Option<mpsc::UnboundedSender<()>>,
}

impl FilesystemAdapter {
    /// Create an adapter for a VFS whose root inode must be 1.
    ///
    /// `volume_name` is the slug (safe for path use); `display_name` is the
    /// human-readable string shown in the Finder sidebar.
    ///
    /// # Panics (debug)
    /// Panics in debug builds if `vfs.root_id() != 1`. The inode bijection
    /// in `vfs_to_fskit_inode` assumes this.
    #[must_use]
    pub fn new(vfs: Arc<VfsState>, volume_name: String, display_name: String) -> Self {
        debug_assert_eq!(
            vfs.root_id(),
            1,
            "FilesystemAdapter requires VfsState with root_id=1"
        );
        let volume_id = format!("ctxfs-{volume_name}");
        Self {
            vfs,
            volume_name,
            volume_id,
            display_name,
            unmount_notifier: None,
        }
    }

    /// Attach a sender that fires when `unmount()` is called (e.g. Finder eject).
    ///
    /// The daemon passes a channel receiver here so it can react to filesystem-
    /// initiated unmounts and run the full cleanup path (symlinks, mounts.json).
    #[must_use]
    pub fn with_unmount_notifier(mut self, notifier: mpsc::UnboundedSender<()>) -> Self {
        self.unmount_notifier = Some(notifier);
        self
    }
}

#[async_trait]
impl Filesystem for FilesystemAdapter {
    // ─── Volume lifecycle ────────────────────────────────────────────────

    async fn get_resource_identifier(&mut self) -> FsKitResult<ResourceIdentifier> {
        Ok(ResourceIdentifier {
            name: Some(self.volume_name.clone()),
            container_id: Some("com.ctxfs.volume".into()),
        })
    }

    async fn get_volume_identifier(&mut self) -> FsKitResult<VolumeIdentifier> {
        Ok(VolumeIdentifier {
            id: Some(self.volume_id.clone()), // "ctxfs-npm-react-19.1.0" (slug-based)
            name: Some(self.display_name.clone()), // "react 19.1.0" (human-readable)
        })
    }

    async fn get_volume_behavior(&mut self) -> FsKitResult<VolumeBehavior> {
        Ok(VolumeBehavior {
            is_open_close_inhibited: Some(true),
            is_access_check_inhibited: Some(true),
            is_volume_rename_inhibited: Some(true),
            is_preallocate_inhibited: Some(true),
            ..Default::default()
        })
    }

    async fn get_volume_capabilities(&mut self) -> FsKitResult<SupportedCapabilities> {
        Ok(SupportedCapabilities {
            // Signal read-only: no setting file permissions, no immutable-file
            // flag support, which together tell FSKit this volume cannot be
            // written to.
            does_not_support_setting_file_permissions: Some(true),
            does_not_support_immutable_files: Some(true),
            ..Default::default()
        })
    }

    async fn get_volume_statistics(&mut self) -> FsKitResult<StatFsResult> {
        let stats = self.vfs.statfs();
        let total_blocks = stats.total_bytes / stats.block_size;
        // FSKit's proto defines block_size/io_size as int64; VFS uses u64.
        // Saturate to i64::MAX to avoid wrap on absurd values.
        let block_size_i64 = i64::try_from(stats.block_size).unwrap_or(i64::MAX);
        Ok(StatFsResult {
            block_size: block_size_i64,
            io_size: block_size_i64,
            total_blocks,
            available_blocks: 0,
            free_blocks: 0,
            used_blocks: total_blocks,
            total_bytes: stats.total_bytes,
            available_bytes: 0,
            free_bytes: 0,
            used_bytes: stats.total_bytes,
            total_files: stats.total_files,
            free_files: 0,
        })
    }

    async fn get_path_conf_operations(&mut self) -> FsKitResult<PathConfOperations> {
        Ok(PathConfOperations::default())
    }

    async fn mount(&mut self, _options: TaskOptions) -> FsKitResult<()> {
        debug!("fskit mount called for volume {}", self.volume_name);
        Ok(())
    }

    async fn unmount(&mut self) -> FsKitResult<()> {
        info!(
            "FSKit unmount requested for volume {} (Finder eject or system)",
            self.volume_name
        );
        if let Some(notifier) = &self.unmount_notifier {
            // Ignore send errors — the daemon may have already been torn down.
            let _ = notifier.send(());
        }
        Ok(())
    }

    async fn synchronize(&mut self, _flags: SyncFlags) -> FsKitResult<()> {
        Ok(())
    }

    async fn activate(&mut self, _options: TaskOptions) -> FsKitResult<Item> {
        let root_id = self.vfs.root_id();
        let attr = self
            .vfs
            .getattr(root_id)
            .await
            .map_err(|e| vfs_err_to_fskit(&e))?;
        Ok(make_item("/", &attr))
    }

    async fn deactivate(&mut self) -> FsKitResult<()> {
        Ok(())
    }

    async fn set_volume_name(&mut self, _name: Vec<u8>) -> FsKitResult<Vec<u8>> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    // ─── Item attributes ─────────────────────────────────────────────────

    async fn get_attributes(&mut self, item_id: u64) -> FsKitResult<ItemAttributes> {
        let vfs_id = fskit_to_vfs_inode(item_id);
        let attr = self
            .vfs
            .getattr(vfs_id)
            .await
            .map_err(|e| vfs_err_to_fskit(&e))?;
        Ok(node_attr_to_item_attributes(&attr))
    }

    async fn set_attributes(
        &mut self,
        _item_id: u64,
        _attributes: ItemAttributes,
    ) -> FsKitResult<ItemAttributes> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    // ─── Directory operations ────────────────────────────────────────────

    async fn lookup_item(&mut self, name: &OsStr, directory_id: u64) -> FsKitResult<Item> {
        let name_str = name.to_str().ok_or(FsKitError::Posix(libc::EINVAL))?;
        let parent_vfs = fskit_to_vfs_inode(directory_id);
        let (_, attr) = self
            .vfs
            .lookup(parent_vfs, name_str)
            .await
            .map_err(|e| vfs_err_to_fskit(&e))?;
        Ok(make_item(name_str, &attr))
    }

    async fn enumerate_directory(
        &mut self,
        directory_id: u64,
        cookie: u64,
        _verifier: u64,
    ) -> FsKitResult<DirectoryEntries> {
        let parent_vfs = fskit_to_vfs_inode(directory_id);
        let children = self
            .vfs
            .readdir(parent_vfs)
            .await
            .map_err(|e| vfs_err_to_fskit(&e))?;

        let start = cookie as usize;
        let mut entries = Vec::with_capacity(children.len().saturating_sub(start));

        for (index, (child_inode, name, _kind)) in children.into_iter().enumerate().skip(start) {
            // Fetch real attrs per child (Codex finding #3: zeroed attrs
            // cause Finder to cache empty values).
            let attr = self
                .vfs
                .getattr(child_inode)
                .await
                .map_err(|e| vfs_err_to_fskit(&e))?;
            entries.push(directory_entries::Entry {
                item: Some(make_item(&name, &attr)),
                next_cookie: (index + 1) as u64,
            });
        }

        Ok(DirectoryEntries {
            entries,
            verifier: 0,
        })
    }

    async fn reclaim_item(&mut self, _item_id: u64) -> FsKitResult<()> {
        Ok(())
    }

    async fn deactivate_item(&mut self, _item_id: u64) -> FsKitResult<()> {
        Ok(())
    }

    // ─── File operations (read-only) ─────────────────────────────────────

    async fn create_item(
        &mut self,
        _name: &OsStr,
        _type: ItemType,
        _dir_id: u64,
        _attrs: ItemAttributes,
    ) -> FsKitResult<Item> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn remove_item(&mut self, _item_id: u64, _name: &OsStr, _dir_id: u64) -> FsKitResult<()> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn rename_item(
        &mut self,
        _item_id: u64,
        _to_dir_id: u64,
        _name: &OsStr,
        _to_name: &OsStr,
        _to_item_id: u64,
        _to_item_existing_id: Option<u64>,
    ) -> FsKitResult<Vec<u8>> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn open_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> FsKitResult<()> {
        Ok(())
    }

    async fn close_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> FsKitResult<()> {
        Ok(())
    }

    async fn read(&mut self, item_id: u64, offset: i64, length: i64) -> FsKitResult<Vec<u8>> {
        if offset < 0 || length < 0 {
            return Err(FsKitError::Posix(libc::EINVAL));
        }
        let vfs_id = fskit_to_vfs_inode(item_id);
        let data = self
            .vfs
            .read(vfs_id, offset as u64, length as u32)
            .await
            .map_err(|e| vfs_err_to_fskit(&e))?;
        Ok(data)
    }

    async fn write(&mut self, _contents: Vec<u8>, _item_id: u64, _offset: i64) -> FsKitResult<i64> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn preallocate_space(
        &mut self,
        _item_id: u64,
        _offset: i64,
        _length: i64,
        _flags: Vec<PreallocateFlag>,
    ) -> FsKitResult<i64> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    // ─── Link operations ─────────────────────────────────────────────────

    async fn create_symbolic_link(
        &mut self,
        _name: &OsStr,
        _directory_id: u64,
        _attributes: ItemAttributes,
        _contents: Vec<u8>,
    ) -> FsKitResult<Item> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn create_link(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> FsKitResult<Vec<u8>> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn read_symbolic_link(&mut self, item_id: u64) -> FsKitResult<Vec<u8>> {
        let vfs_id = fskit_to_vfs_inode(item_id);
        let target = self
            .vfs
            .readlink(vfs_id)
            .await
            .map_err(|e| vfs_err_to_fskit(&e))?;
        Ok(target.into_bytes())
    }

    // ─── Access control ──────────────────────────────────────────────────

    async fn check_access(&mut self, _item_id: u64, _access: Vec<AccessMask>) -> FsKitResult<bool> {
        Ok(true)
    }

    // ─── Extended attributes (unsupported) ───────────────────────────────

    async fn get_supported_xattr_names(&mut self, _item_id: u64) -> FsKitResult<Xattrs> {
        Ok(Xattrs::default())
    }

    async fn get_xattr(&mut self, _name: &OsStr, _item_id: u64) -> FsKitResult<Vec<u8>> {
        // ENOATTR is the macOS spelling; Linux uses ENODATA for the same value.
        // This crate only runs on macOS, but it cross-compiles on Linux CI as a
        // workspace member, so use the platform-correct constant.
        #[cfg(target_os = "macos")]
        let err = libc::ENOATTR;
        #[cfg(not(target_os = "macos"))]
        let err = libc::ENODATA;
        Err(FsKitError::Posix(err))
    }

    async fn set_xattr(
        &mut self,
        _name: &OsStr,
        _value: Option<Vec<u8>>,
        _item_id: u64,
        _policy: SetXattrPolicy,
    ) -> FsKitResult<()> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn get_xattrs(&mut self, _item_id: u64) -> FsKitResult<Xattrs> {
        Ok(Xattrs::default())
    }
}
