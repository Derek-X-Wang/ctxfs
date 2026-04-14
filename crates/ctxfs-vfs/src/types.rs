use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeType {
    File,
    Directory,
    Symlink,
}

#[derive(Debug, Clone)]
pub struct NodeAttr {
    pub inode: u64,
    /// Inode of the parent directory. The root's parent is itself.
    pub parent_inode: u64,
    pub size: u64,
    pub kind: NodeType,
    pub executable: bool,
}

#[derive(Debug, Clone)]
pub struct StatFsResult {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub block_size: u64,
    pub total_files: u64,
}

#[derive(Debug, Error)]
pub enum VfsError {
    #[error("not found")]
    NotFound,
    #[error("not a directory")]
    NotDir,
    #[error("is a directory")]
    IsDir,
    #[error("invalid argument")]
    Invalid,
    #[error("read-only filesystem")]
    ReadOnly,
    #[error("I/O error: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_type_debug() {
        assert_eq!(format!("{:?}", NodeType::File), "File");
        assert_eq!(format!("{:?}", NodeType::Directory), "Directory");
        assert_eq!(format!("{:?}", NodeType::Symlink), "Symlink");
    }

    #[test]
    fn node_type_equality() {
        assert_eq!(NodeType::File, NodeType::File);
        assert_ne!(NodeType::File, NodeType::Directory);
        assert_ne!(NodeType::File, NodeType::Symlink);
    }

    #[test]
    fn node_type_copy() {
        let a = NodeType::Directory;
        let b = a;
        assert_eq!(a, b);
    }

    #[test]
    fn vfs_error_display_not_found() {
        assert_eq!(VfsError::NotFound.to_string(), "not found");
    }

    #[test]
    fn vfs_error_display_not_dir() {
        assert_eq!(VfsError::NotDir.to_string(), "not a directory");
    }

    #[test]
    fn vfs_error_display_is_dir() {
        assert_eq!(VfsError::IsDir.to_string(), "is a directory");
    }

    #[test]
    fn vfs_error_display_invalid() {
        assert_eq!(VfsError::Invalid.to_string(), "invalid argument");
    }

    #[test]
    fn vfs_error_display_read_only() {
        assert_eq!(VfsError::ReadOnly.to_string(), "read-only filesystem");
    }

    #[test]
    fn vfs_error_display_io() {
        assert_eq!(
            VfsError::Io("disk failure".to_string()).to_string(),
            "I/O error: disk failure"
        );
    }

    #[test]
    fn node_attr_properties() {
        let attr = NodeAttr {
            inode: 42,
            parent_inode: 1,
            size: 1024,
            kind: NodeType::File,
            executable: true,
        };
        assert_eq!(attr.inode, 42);
        assert_eq!(attr.size, 1024);
        assert_eq!(attr.kind, NodeType::File);
        assert!(attr.executable);
    }

    #[test]
    fn node_attr_directory() {
        let attr = NodeAttr {
            inode: 1,
            parent_inode: 1,
            size: 0,
            kind: NodeType::Directory,
            executable: false,
        };
        assert_eq!(attr.kind, NodeType::Directory);
        assert!(!attr.executable);
    }

    #[test]
    fn node_attr_clone() {
        let attr = NodeAttr {
            inode: 10,
            parent_inode: 1,
            size: 512,
            kind: NodeType::Symlink,
            executable: false,
        };
        let cloned = attr.clone();
        assert_eq!(cloned.inode, attr.inode);
        assert_eq!(cloned.size, attr.size);
        assert_eq!(cloned.kind, attr.kind);
        assert_eq!(cloned.executable, attr.executable);
    }

    #[test]
    fn stat_fs_result_properties() {
        let stat = StatFsResult {
            total_bytes: 1_000_000,
            free_bytes: 500_000,
            block_size: 4096,
            total_files: 100,
        };
        assert_eq!(stat.total_bytes, 1_000_000);
        assert_eq!(stat.free_bytes, 500_000);
        assert_eq!(stat.block_size, 4096);
        assert_eq!(stat.total_files, 100);
    }
}
