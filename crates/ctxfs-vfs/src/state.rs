use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::Digest;
use ctxfs_manifest::{DirEntry, Directory, Snapshot};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, error};

use crate::types::{NodeAttr, NodeType, StatFsResult, VfsError};

const ROOT_ID: u64 = 1;
const BLOCK_SIZE: u64 = 4096;

// ---------------------------------------------------------------------------
// Internal types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone)]
enum NodeKind {
    Directory {
        digest: Digest,
        /// Whether children have been loaded from the provider into the node table.
        populated: bool,
    },
    File {
        digest: Digest,
        size: u64,
        executable: bool,
        /// Small files (<=4 KB) are inlined in the manifest and don't require a separate fetch.
        inline_content: Option<Vec<u8>>,
    },
    Symlink {
        target: String,
    },
}

#[derive(Debug, Clone)]
struct Node {
    id: u64,
    parent: u64,
    name: String,
    kind: NodeKind,
}

// ---------------------------------------------------------------------------
// VfsState
// ---------------------------------------------------------------------------

/// Protocol-agnostic virtual filesystem state.
///
/// Manages an inode table backed by [`DashMap`] with lazy directory population
/// from a [`Provider`](ctxfs_core::provider::Provider) and blob fetching through
/// a [`BlobCache`].
pub struct VfsState {
    provider: SharedProvider,
    cache: Arc<BlobCache>,
    #[allow(dead_code)]
    snapshot: Snapshot,
    next_id: AtomicU64,
    nodes: DashMap<u64, Node>,
    dir_cache: DashMap<(u64, String), u64>,
    dir_children: DashMap<u64, Vec<u64>>,
}

impl std::fmt::Debug for VfsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VfsState")
            .field("node_count", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

impl VfsState {
    /// Create a new `VfsState`, optionally scoped to a subdirectory via `subpath`.
    ///
    /// The constructor seeds the root inode from the snapshot and, when a subpath
    /// is given, walks the directory tree to re-root the filesystem at the target
    /// subdirectory before returning.
    pub async fn new(
        provider: SharedProvider,
        cache: Arc<BlobCache>,
        snapshot: Snapshot,
        subpath: Option<String>,
    ) -> Result<Self, VfsError> {
        let state = Self {
            provider,
            cache,
            snapshot: snapshot.clone(),
            next_id: AtomicU64::new(2),
            nodes: DashMap::new(),
            dir_cache: DashMap::new(),
            dir_children: DashMap::new(),
        };

        // Seed the root inode.
        let _ = state.nodes.insert(
            ROOT_ID,
            Node {
                id: ROOT_ID,
                parent: ROOT_ID,
                name: "/".into(),
                kind: NodeKind::Directory {
                    digest: snapshot.root_directory,
                    populated: false,
                },
            },
        );

        if let Some(sp) = subpath {
            state.resolve_subpath(&sp).await?;
        }

        Ok(state)
    }

    /// The root inode id (always 1).
    #[must_use]
    pub fn root_id(&self) -> u64 {
        ROOT_ID
    }

    // -- public VFS operations ------------------------------------------------

    /// Look up a child by name under the given parent directory.
    pub async fn lookup(&self, parent: u64, name: &str) -> Result<(u64, NodeAttr), VfsError> {
        if name == "." {
            let node = self.nodes.get(&parent).ok_or(VfsError::NotFound)?;
            return Ok((parent, Self::node_to_attr(&node)));
        }
        if name == ".." {
            let parent_of = self.nodes.get(&parent).map_or(ROOT_ID, |n| n.parent);
            let node = self.nodes.get(&parent_of).ok_or(VfsError::NotFound)?;
            return Ok((parent_of, Self::node_to_attr(&node)));
        }

        let cache_key = (parent, name.to_string());
        if let Some(existing) = self.dir_cache.get(&cache_key) {
            let id = *existing;
            let node = self.nodes.get(&id).ok_or(VfsError::NotFound)?;
            return Ok((id, Self::node_to_attr(&node)));
        }

        // Populate directory and retry.
        let _ = self.ensure_populated(parent).await?;

        let id = self.dir_cache.get(&cache_key).map(|r| *r).ok_or(VfsError::NotFound)?;
        let node = self.nodes.get(&id).ok_or(VfsError::NotFound)?;
        Ok((id, Self::node_to_attr(&node)))
    }

    /// Get attributes for the given inode.
    #[allow(clippy::unused_async)] // async for API uniformity with other VFS methods
    pub async fn getattr(&self, inode: u64) -> Result<NodeAttr, VfsError> {
        let node = self.nodes.get(&inode).ok_or(VfsError::NotFound)?;
        Ok(Self::node_to_attr(&node))
    }

    /// Read file content with the given offset and count.
    pub async fn read(&self, inode: u64, offset: u64, count: u32) -> Result<Vec<u8>, VfsError> {
        let node = self.nodes.get(&inode).ok_or(VfsError::NotFound)?.clone();
        let data = self.fetch_file_bytes(&node).await?;

        let total = data.len() as u64;
        let start = offset.min(total) as usize;
        let end = (offset + u64::from(count)).min(total) as usize;
        Ok(data[start..end].to_vec())
    }

    /// List directory entries. Returns `(inode, name, kind)` triples.
    pub async fn readdir(&self, inode: u64) -> Result<Vec<(u64, String, NodeType)>, VfsError> {
        let child_ids = self.ensure_populated(inode).await?;
        let mut entries = Vec::with_capacity(child_ids.len());
        for child_id in &child_ids {
            let Some(node) = self.nodes.get(child_id) else {
                continue;
            };
            let kind = match &node.kind {
                NodeKind::Directory { .. } => NodeType::Directory,
                NodeKind::File { .. } => NodeType::File,
                NodeKind::Symlink { .. } => NodeType::Symlink,
            };
            entries.push((node.id, node.name.clone(), kind));
        }
        Ok(entries)
    }

    /// Read the target of a symlink.
    #[allow(clippy::unused_async)] // async for API uniformity with other VFS methods
    pub async fn readlink(&self, inode: u64) -> Result<String, VfsError> {
        let node = self.nodes.get(&inode).ok_or(VfsError::NotFound)?;
        match &node.kind {
            NodeKind::Symlink { target } => Ok(target.clone()),
            _ => Err(VfsError::Invalid),
        }
    }

    /// Return filesystem-level statistics.
    #[must_use]
    pub fn statfs(&self) -> StatFsResult {
        StatFsResult {
            total_bytes: 1024 * 1024 * 1024, // 1 GiB virtual
            free_bytes: 0,
            block_size: BLOCK_SIZE,
            total_files: self.nodes.len() as u64,
        }
    }

    // -- internal helpers -----------------------------------------------------

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn node_to_attr(node: &Node) -> NodeAttr {
        match &node.kind {
            NodeKind::Directory { .. } => NodeAttr {
                inode: node.id,
                size: BLOCK_SIZE,
                kind: NodeType::Directory,
                executable: false,
            },
            NodeKind::File {
                size, executable, ..
            } => NodeAttr {
                inode: node.id,
                size: *size,
                kind: NodeType::File,
                executable: *executable,
            },
            NodeKind::Symlink { target } => NodeAttr {
                inode: node.id,
                size: target.len() as u64,
                kind: NodeType::Symlink,
                executable: false,
            },
        }
    }

    /// Walk the directory tree to resolve a subpath and re-root the filesystem.
    async fn resolve_subpath(&self, subpath: &str) -> Result<(), VfsError> {
        let mut current_digest = {
            let root = self.nodes.get(&ROOT_ID).ok_or(VfsError::NotFound)?;
            match &root.kind {
                NodeKind::Directory { digest, .. } => digest.clone(),
                _ => return Err(VfsError::NotDir),
            }
        };

        for component in subpath.split('/').filter(|s| !s.is_empty()) {
            let data = self
                .provider
                .fetch_directory(&current_digest)
                .await
                .map_err(|e| VfsError::Io(format!("fetching directory for subpath: {e}")))?;

            let directory: Directory = serde_json::from_slice(&data)
                .map_err(|e| VfsError::Io(format!("parsing directory: {e}")))?;

            let child_digest = directory
                .entries
                .iter()
                .find_map(|entry| match entry {
                    DirEntry::Directory(d) if d.name == component => Some(d.digest.clone()),
                    _ => None,
                })
                .ok_or(VfsError::NotFound)?;

            current_digest = child_digest;
        }

        // Re-root: replace the root node's digest.
        if let Some(mut root_node) = self.nodes.get_mut(&ROOT_ID) {
            root_node.kind = NodeKind::Directory {
                digest: current_digest,
                populated: false,
            };
        }

        Ok(())
    }

    /// Ensure a directory's children are loaded into the node table.
    async fn ensure_populated(&self, dirid: u64) -> Result<Vec<u64>, VfsError> {
        // Fast path: already populated.
        {
            let node = self.nodes.get(&dirid).ok_or(VfsError::NotFound)?;
            if let NodeKind::Directory { populated: true, .. } = &node.kind {
                drop(node);
                if let Some(children) = self.dir_children.get(&dirid) {
                    return Ok(children.clone());
                }
            }
        }

        // Slow path: fetch directory manifest.
        let digest = {
            let node = self.nodes.get(&dirid).ok_or(VfsError::NotFound)?;
            match &node.kind {
                NodeKind::Directory { digest, .. } => digest.clone(),
                _ => return Err(VfsError::NotDir),
            }
        };

        let data = self.provider.fetch_directory(&digest).await.map_err(|e| {
            error!("fetch_directory({}) failed: {}", digest, e);
            VfsError::Io(format!("fetch_directory: {e}"))
        })?;

        let directory: Directory = serde_json::from_slice(&data).map_err(|e| {
            error!("parse directory {}: {}", digest, e);
            VfsError::Io(format!("parse directory: {e}"))
        })?;

        let mut child_ids = Vec::with_capacity(directory.entries.len());
        for entry in &directory.entries {
            let name = entry.name().to_string();
            let cache_key = (dirid, name.clone());

            let child_id = if let Some(existing) = self.dir_cache.get(&cache_key) {
                *existing
            } else {
                let new_id = self.alloc_id();
                let node = match entry {
                    DirEntry::File(f) => Node {
                        id: new_id,
                        parent: dirid,
                        name: name.clone(),
                        kind: NodeKind::File {
                            digest: f.digest.clone(),
                            size: f.size,
                            executable: f.executable,
                            inline_content: f.inline_content.clone(),
                        },
                    },
                    DirEntry::Directory(d) => Node {
                        id: new_id,
                        parent: dirid,
                        name: name.clone(),
                        kind: NodeKind::Directory {
                            digest: d.digest.clone(),
                            populated: false,
                        },
                    },
                    DirEntry::Symlink(s) => Node {
                        id: new_id,
                        parent: dirid,
                        name: name.clone(),
                        kind: NodeKind::Symlink {
                            target: s.target.clone(),
                        },
                    },
                };
                let _ = self.nodes.insert(new_id, node);
                let _ = self.dir_cache.insert(cache_key, new_id);
                new_id
            };
            child_ids.push(child_id);
        }

        // Mark populated and store child list.
        if let Some(mut node_ref) = self.nodes.get_mut(&dirid) {
            if let NodeKind::Directory { ref mut populated, .. } = node_ref.kind {
                *populated = true;
            }
        }
        let _ = self.dir_children.insert(dirid, child_ids.clone());

        debug!("populated directory {} with {} children", dirid, child_ids.len());
        Ok(child_ids)
    }

    /// Fetch file content: inline if available, otherwise via cache/provider.
    async fn fetch_file_bytes(&self, node: &Node) -> Result<Vec<u8>, VfsError> {
        if let NodeKind::File {
            digest,
            inline_content: Some(content),
            ..
        } = &node.kind
        {
            debug!("inline content for file id={} ({} bytes)", node.id, content.len());
            let _ = digest;
            return Ok(content.clone());
        }

        let digest = match &node.kind {
            NodeKind::File { digest, .. } => digest.clone(),
            _ => return Err(VfsError::IsDir),
        };

        if let Some(data) = self.cache.get(&digest) {
            return Ok(data);
        }

        let data = self.provider.fetch_blob(&digest).await.map_err(|e| {
            error!("fetch_blob({}) failed: {}", digest, e);
            VfsError::Io(format!("fetch_blob: {e}"))
        })?;

        if let Err(e) = self.cache.put(&digest, &data) {
            error!("cache put failed for {}: {}", digest, e);
        }
        Ok(data)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_kind_debug() {
        let kind = NodeKind::Directory {
            digest: Digest::sha256(b"test"),
            populated: false,
        };
        let s = format!("{kind:?}");
        assert!(s.contains("Directory"));
    }

    #[test]
    fn node_to_attr_file() {
        let node = Node {
            id: 42,
            parent: 1,
            name: "test.rs".into(),
            kind: NodeKind::File {
                digest: Digest::sha256(b"x"),
                size: 100,
                executable: true,
                inline_content: None,
            },
        };
        let attr = VfsState::node_to_attr(&node);
        assert_eq!(attr.inode, 42);
        assert_eq!(attr.size, 100);
        assert_eq!(attr.kind, NodeType::File);
        assert!(attr.executable);
    }

    #[test]
    fn node_to_attr_symlink() {
        let node = Node {
            id: 7,
            parent: 1,
            name: "link".into(),
            kind: NodeKind::Symlink {
                target: "README.md".into(),
            },
        };
        let attr = VfsState::node_to_attr(&node);
        assert_eq!(attr.inode, 7);
        assert_eq!(attr.size, 9); // "README.md".len()
        assert_eq!(attr.kind, NodeType::Symlink);
    }

    #[test]
    fn node_to_attr_directory() {
        let node = Node {
            id: 1,
            parent: 1,
            name: "/".into(),
            kind: NodeKind::Directory {
                digest: Digest::sha256(b"root"),
                populated: false,
            },
        };
        let attr = VfsState::node_to_attr(&node);
        assert_eq!(attr.inode, 1);
        assert_eq!(attr.size, BLOCK_SIZE);
        assert_eq!(attr.kind, NodeType::Directory);
        assert!(!attr.executable);
    }
}
