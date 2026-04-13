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
    debug_assert!(fskit_id >= FSKIT_ROOT_ID, "FSKit inode {fskit_id} is reserved");
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
pub(crate) fn vfs_err_to_fskit(err: &VfsError) -> FsKitError {
    let errno = match err {
        VfsError::NotFound => libc::ENOENT,
        VfsError::NotDir => libc::ENOTDIR,
        VfsError::IsDir => libc::EISDIR,
        VfsError::Invalid => libc::EINVAL,
        VfsError::ReadOnly => libc::EROFS,
        VfsError::Io(_) => libc::EIO,
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
        assert!(matches!(node_type_to_item_type(NodeType::File), ItemType::File));
        assert!(matches!(node_type_to_item_type(NodeType::Directory), ItemType::Directory));
        assert!(matches!(node_type_to_item_type(NodeType::Symlink), ItemType::Symlink));
    }

    #[test]
    fn file_attr_translates_correctly() {
        let attr = file_attr(5, 1, 1024, false);
        let item_attr = node_attr_to_item_attributes(&attr);
        assert_eq!(item_attr.file_id, Some(6));   // 5 + 1
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
        assert!(matches!(vfs_err_to_fskit(&VfsError::NotFound), FsKitError::Posix(e) if e == libc::ENOENT));
        assert!(matches!(vfs_err_to_fskit(&VfsError::NotDir), FsKitError::Posix(e) if e == libc::ENOTDIR));
        assert!(matches!(vfs_err_to_fskit(&VfsError::IsDir), FsKitError::Posix(e) if e == libc::EISDIR));
        assert!(matches!(vfs_err_to_fskit(&VfsError::Invalid), FsKitError::Posix(e) if e == libc::EINVAL));
        assert!(matches!(vfs_err_to_fskit(&VfsError::ReadOnly), FsKitError::Posix(e) if e == libc::EROFS));
        assert!(matches!(vfs_err_to_fskit(&VfsError::Io("x".into())), FsKitError::Posix(e) if e == libc::EIO));
    }
}
