use ctxfs_core::Digest;
use std::collections::HashMap;
use std::sync::RwLock;

/// What kind of filesystem entry this inode represents.
#[derive(Debug, Clone)]
pub enum InodeKind {
    Directory {
        digest: Digest,
        children: Vec<u64>, // child inode numbers
        populated: bool,    // whether children have been loaded from provider
    },
    File {
        digest: Digest,
        size: u64,
        executable: bool,
        inline_content: Option<Vec<u8>>,
    },
    Symlink {
        target: String,
    },
}

#[derive(Debug, Clone)]
pub struct InodeEntry {
    pub ino: u64,
    pub parent: u64,
    pub name: String,
    pub kind: InodeKind,
}

impl InodeEntry {
    pub fn is_dir(&self) -> bool {
        matches!(self.kind, InodeKind::Directory { .. })
    }

    pub fn is_file(&self) -> bool {
        matches!(self.kind, InodeKind::File { .. })
    }

    pub fn is_symlink(&self) -> bool {
        matches!(self.kind, InodeKind::Symlink { .. })
    }

    pub fn size(&self) -> u64 {
        match &self.kind {
            InodeKind::File { size, .. } => *size,
            InodeKind::Directory { .. } => 0,
            InodeKind::Symlink { target } => target.len() as u64,
        }
    }

    pub fn digest(&self) -> Option<&Digest> {
        match &self.kind {
            InodeKind::File { digest, .. } | InodeKind::Directory { digest, .. } => Some(digest),
            InodeKind::Symlink { .. } => None,
        }
    }
}

/// Thread-safe inode table for the FUSE filesystem.
pub struct InodeTable {
    inodes: RwLock<HashMap<u64, InodeEntry>>,
    next_ino: RwLock<u64>,
    /// Map of (`parent_ino`, name) -> `child_ino` for fast lookup.
    lookup_table: RwLock<HashMap<(u64, String), u64>>,
}

impl InodeTable {
    pub fn new() -> Self {
        Self {
            inodes: RwLock::new(HashMap::new()),
            next_ino: RwLock::new(2), // 1 is reserved for root
            lookup_table: RwLock::new(HashMap::new()),
        }
    }

    /// Insert the root inode (ino=1).
    pub fn insert_root(&self, digest: Digest) {
        let entry = InodeEntry {
            ino: 1,
            parent: 1,
            name: String::new(),
            kind: InodeKind::Directory {
                digest,
                children: Vec::new(),
                populated: false,
            },
        };
        let _ = self.inodes.write().unwrap().insert(1, entry);
    }

    /// Allocate and insert a new inode, returning its number.
    pub fn allocate_inode(&self, parent: u64, name: String, kind: InodeKind) -> u64 {
        let mut next = self.next_ino.write().unwrap();
        let ino = *next;
        *next += 1;

        let entry = InodeEntry {
            ino,
            parent,
            name: name.clone(),
            kind,
        };

        // Acquire the inodes write lock once for both the insert and
        // the parent-children update so concurrent readers never see a
        // torn state (child present but not yet in parent's list).
        {
            let mut inodes = self.inodes.write().unwrap();
            let _ = inodes.insert(ino, entry);

            // Add child to parent's children list
            if let Some(parent_entry) = inodes.get_mut(&parent) {
                if let InodeKind::Directory { children, .. } = &mut parent_entry.kind {
                    children.push(ino);
                }
            }
        }

        let _ = self.lookup_table
            .write()
            .unwrap()
            .insert((parent, name), ino);

        ino
    }

    pub fn get(&self, ino: u64) -> Option<InodeEntry> {
        self.inodes.read().unwrap().get(&ino).cloned()
    }

    pub fn lookup(&self, parent: u64, name: &str) -> Option<u64> {
        self.lookup_table
            .read()
            .unwrap()
            .get(&(parent, name.to_string()))
            .copied()
    }

    pub fn children(&self, parent: u64) -> Vec<InodeEntry> {
        let inodes = self.inodes.read().unwrap();
        if let Some(entry) = inodes.get(&parent) {
            if let InodeKind::Directory { children, .. } = &entry.kind {
                return children
                    .iter()
                    .filter_map(|ino| inodes.get(ino).cloned())
                    .collect();
            }
        }
        Vec::new()
    }

    pub fn is_populated(&self, ino: u64) -> bool {
        let inodes = self.inodes.read().unwrap();
        if let Some(entry) = inodes.get(&ino) {
            if let InodeKind::Directory { populated, .. } = &entry.kind {
                return *populated;
            }
        }
        false
    }

    pub fn mark_populated(&self, ino: u64) {
        let mut inodes = self.inodes.write().unwrap();
        if let Some(entry) = inodes.get_mut(&ino) {
            if let InodeKind::Directory { populated, .. } = &mut entry.kind {
                *populated = true;
            }
        }
    }
}

impl std::fmt::Debug for InodeTable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("InodeTable")
            .field("inodes", &*self.inodes.read().unwrap())
            .field("next_ino", &*self.next_ino.read().unwrap())
            .field("lookup_table", &*self.lookup_table.read().unwrap())
            .finish()
    }
}

impl Default for InodeTable {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxfs_core::Digest;

    #[test]
    fn inode_table_basics() {
        let table = InodeTable::new();
        let root_digest = Digest::sha256(b"root");
        table.insert_root(root_digest.clone());

        let root = table.get(1).unwrap();
        assert!(root.is_dir());
        assert_eq!(root.parent, 1);

        let child_digest = Digest::sha256(b"file");
        let ino = table.allocate_inode(
            1,
            "hello.txt".to_string(),
            InodeKind::File {
                digest: child_digest,
                size: 42,
                executable: false,
                inline_content: None,
            },
        );

        assert_eq!(ino, 2);
        let child = table.get(ino).unwrap();
        assert!(child.is_file());
        assert_eq!(child.size(), 42);

        assert_eq!(table.lookup(1, "hello.txt"), Some(ino));
        assert_eq!(table.children(1).len(), 1);
    }

    #[test]
    fn mark_populated() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));
        assert!(!table.is_populated(1));
        table.mark_populated(1);
        assert!(table.is_populated(1));
    }

    #[test]
    fn lookup_nonexistent_returns_none() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));
        assert_eq!(table.lookup(1, "nope"), None);
    }

    #[test]
    fn get_nonexistent_returns_none() {
        let table = InodeTable::new();
        assert!(table.get(999).is_none());
    }

    #[test]
    fn children_of_nonexistent_returns_empty() {
        let table = InodeTable::new();
        assert!(table.children(999).is_empty());
    }

    #[test]
    fn children_of_file_returns_empty() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));
        let file_ino = table.allocate_inode(
            1,
            "file.txt".to_string(),
            InodeKind::File {
                digest: Digest::sha256(b"f"),
                size: 5,
                executable: false,
                inline_content: None,
            },
        );
        assert!(table.children(file_ino).is_empty());
    }

    #[test]
    fn symlink_inode() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));
        let ino = table.allocate_inode(
            1,
            "link".to_string(),
            InodeKind::Symlink {
                target: "/some/target".to_string(),
            },
        );

        let entry = table.get(ino).unwrap();
        assert!(entry.is_symlink());
        assert!(!entry.is_file());
        assert!(!entry.is_dir());
        assert_eq!(entry.size(), 12); // "/some/target".len()
        assert!(entry.digest().is_none());
    }

    #[test]
    fn nested_directories() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));

        let dir_ino = table.allocate_inode(
            1,
            "subdir".to_string(),
            InodeKind::Directory {
                digest: Digest::sha256(b"subdir"),
                children: Vec::new(),
                populated: false,
            },
        );

        let file_ino = table.allocate_inode(
            dir_ino,
            "nested.txt".to_string(),
            InodeKind::File {
                digest: Digest::sha256(b"nested"),
                size: 100,
                executable: true,
                inline_content: Some(b"inline".to_vec()),
            },
        );

        assert_eq!(table.lookup(dir_ino, "nested.txt"), Some(file_ino));
        assert_eq!(table.children(dir_ino).len(), 1);

        // Root should have one child (the subdir)
        assert_eq!(table.children(1).len(), 1);

        let nested = table.get(file_ino).unwrap();
        assert_eq!(nested.parent, dir_ino);
    }

    #[test]
    fn inode_numbers_monotonic() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));

        let ino1 = table.allocate_inode(
            1,
            "a".to_string(),
            InodeKind::File {
                digest: Digest::sha256(b"a"),
                size: 1,
                executable: false,
                inline_content: None,
            },
        );
        let ino2 = table.allocate_inode(
            1,
            "b".to_string(),
            InodeKind::File {
                digest: Digest::sha256(b"b"),
                size: 1,
                executable: false,
                inline_content: None,
            },
        );

        assert!(ino2 > ino1);
        assert!(ino1 >= 2); // 1 is root
    }

    #[test]
    fn executable_file_flag() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));

        let ino = table.allocate_inode(
            1,
            "script.sh".to_string(),
            InodeKind::File {
                digest: Digest::sha256(b"script"),
                size: 50,
                executable: true,
                inline_content: None,
            },
        );

        let entry = table.get(ino).unwrap();
        if let InodeKind::File { executable, .. } = &entry.kind {
            assert!(executable);
        } else {
            panic!("expected file");
        }
    }

    #[test]
    fn debug_impl() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));
        let debug = format!("{table:?}");
        assert!(debug.contains("InodeTable"));
    }

    #[test]
    fn is_populated_on_file_returns_false() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));
        let ino = table.allocate_inode(
            1,
            "file".to_string(),
            InodeKind::File {
                digest: Digest::sha256(b"f"),
                size: 1,
                executable: false,
                inline_content: None,
            },
        );
        assert!(!table.is_populated(ino));
    }

    #[test]
    fn mark_populated_on_file_is_noop() {
        let table = InodeTable::new();
        table.insert_root(Digest::sha256(b"root"));
        let ino = table.allocate_inode(
            1,
            "file".to_string(),
            InodeKind::File {
                digest: Digest::sha256(b"f"),
                size: 1,
                executable: false,
                inline_content: None,
            },
        );
        table.mark_populated(ino); // should not panic
        assert!(!table.is_populated(ino));
    }
}
