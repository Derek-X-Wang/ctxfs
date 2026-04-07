use async_trait::async_trait;
use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::source::SourceSpec;
use ctxfs_core::Digest;
use ctxfs_manifest::{DirEntry, Directory, Snapshot};
use dashmap::DashMap;
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3,
};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use nfsserve::vfs::{DirEntry as NfsDirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, error};

const ROOT_ID: fileid3 = 1;
const BLOCK_SIZE: u64 = 4096;

/// Kind-specific metadata for a node in the virtual filesystem.
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
        /// Small files (<=4KB) are inlined in the manifest and don't require a separate fetch.
        inline_content: Option<Vec<u8>>,
    },
    Symlink {
        target: String,
    },
}

/// A single inode in the NFS filesystem.
#[derive(Debug, Clone)]
struct Node {
    id: fileid3,
    parent: fileid3,
    name: Vec<u8>,
    kind: NodeKind,
}

/// `CtxfsNfs` implements `NFSFileSystem` on top of a `Provider` + `BlobCache`.
pub struct CtxfsNfs {
    provider: SharedProvider,
    #[allow(dead_code)] // retained for future refresh/reload
    source: SourceSpec,
    cache: Arc<BlobCache>,
    #[allow(dead_code)]
    snapshot: Snapshot,

    /// Monotonic inode id allocator; root is id 1.
    next_id: AtomicU64,
    /// All known inodes keyed by fileid3.
    nodes: DashMap<fileid3, Node>,
    /// `(parent_id, name)` → `child_id` lookup cache.
    dir_cache: DashMap<(fileid3, Vec<u8>), fileid3>,
    /// Children list per directory, populated on demand.
    dir_children: DashMap<fileid3, Vec<fileid3>>,
}

impl std::fmt::Debug for CtxfsNfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtxfsNfs")
            .field("source", &self.source.to_string())
            .field("node_count", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

/// Handle returned by [`CtxfsNfs::spawn`] that keeps the NFS server running
/// until dropped. Currently a marker — the tokio task is detached.
#[derive(Debug)]
pub struct NfsServerHandle {
    /// The address the NFS server is listening on, e.g. `127.0.0.1:11111`.
    pub addr: String,
}

impl CtxfsNfs {
    pub fn new(
        provider: SharedProvider,
        source: SourceSpec,
        cache: Arc<BlobCache>,
        snapshot: Snapshot,
    ) -> Self {
        let fs = Self {
            provider,
            source,
            cache,
            snapshot: snapshot.clone(),
            next_id: AtomicU64::new(2),
            nodes: DashMap::new(),
            dir_cache: DashMap::new(),
            dir_children: DashMap::new(),
        };

        // Seed the root inode from the snapshot.
        let _ = fs.nodes.insert(
            ROOT_ID,
            Node {
                id: ROOT_ID,
                parent: ROOT_ID,
                name: b"/".to_vec(),
                kind: NodeKind::Directory {
                    digest: snapshot.root_directory,
                    populated: false,
                },
            },
        );

        fs
    }

    /// Bind an `NFSv3` listener on the given `addr` and spawn the server loop.
    /// Returns once the listener is bound and ready to accept clients.
    pub async fn spawn(self, addr: &str) -> std::io::Result<NfsServerHandle> {
        let listener = NFSTcpListener::bind(addr, self)
            .await
            .map_err(|e| std::io::Error::other(format!("bind failed: {e}")))?;

        let addr = addr.to_string();
        let _task = tokio::spawn(async move {
            if let Err(e) = listener.handle_forever().await {
                error!("NFS server exited: {e}");
            }
        });

        Ok(NfsServerHandle { addr })
    }

    fn alloc_id(&self) -> fileid3 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    /// Build the `fattr3` struct NFS clients expect for a node.
    fn node_to_fattr(node: &Node) -> fattr3 {
        let epoch = nfstime3 {
            seconds: 0,
            nseconds: 0,
        };
        match &node.kind {
            NodeKind::Directory { .. } => fattr3 {
                ftype: ftype3::NF3DIR,
                mode: 0o555,
                nlink: 2,
                uid: 0,
                gid: 0,
                size: BLOCK_SIZE,
                used: BLOCK_SIZE,
                rdev: specdata3::default(),
                fsid: 0x6374_7866_7300_0001,
                fileid: node.id,
                atime: epoch,
                mtime: epoch,
                ctime: epoch,
            },
            NodeKind::File {
                size, executable, ..
            } => fattr3 {
                ftype: ftype3::NF3REG,
                mode: if *executable { 0o555 } else { 0o444 },
                nlink: 1,
                uid: 0,
                gid: 0,
                size: *size,
                used: *size,
                rdev: specdata3::default(),
                fsid: 0x6374_7866_7300_0001,
                fileid: node.id,
                atime: epoch,
                mtime: epoch,
                ctime: epoch,
            },
            NodeKind::Symlink { target } => fattr3 {
                ftype: ftype3::NF3LNK,
                mode: 0o777,
                nlink: 1,
                uid: 0,
                gid: 0,
                size: target.len() as u64,
                used: target.len() as u64,
                rdev: specdata3::default(),
                fsid: 0x6374_7866_7300_0001,
                fileid: node.id,
                atime: epoch,
                mtime: epoch,
                ctime: epoch,
            },
        }
    }

    /// Fetch a directory's children from the provider (or reuse the cached list
    /// if already populated). Returns the list of child ids in order.
    async fn ensure_populated(&self, dirid: fileid3) -> Result<Vec<fileid3>, nfsstat3> {
        // Fast path: already populated.
        {
            let node = self.nodes.get(&dirid).ok_or(nfsstat3::NFS3ERR_NOENT)?;
            if let NodeKind::Directory {
                populated: true, ..
            } = &node.kind
            {
                drop(node);
                if let Some(children) = self.dir_children.get(&dirid) {
                    return Ok(children.clone());
                }
            }
        }

        // Slow path: fetch the directory manifest from the provider.
        let digest = {
            let node = self.nodes.get(&dirid).ok_or(nfsstat3::NFS3ERR_NOENT)?;
            match &node.kind {
                NodeKind::Directory { digest, .. } => digest.clone(),
                _ => return Err(nfsstat3::NFS3ERR_NOTDIR),
            }
        };

        let data = self.provider.fetch_directory(&digest).await.map_err(|e| {
            error!("fetch_directory({}) failed: {}", digest, e);
            nfsstat3::NFS3ERR_IO
        })?;

        let directory: Directory = serde_json::from_slice(&data).map_err(|e| {
            error!("parse directory {}: {}", digest, e);
            nfsstat3::NFS3ERR_IO
        })?;

        // Allocate ids for each entry and insert them into the node table.
        let mut child_ids = Vec::with_capacity(directory.entries.len());
        for entry in &directory.entries {
            let name = entry.name().as_bytes().to_vec();
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

        // Mark the directory as populated and store the child list.
        if let Some(mut node_ref) = self.nodes.get_mut(&dirid) {
            if let NodeKind::Directory {
                ref mut populated, ..
            } = node_ref.kind
            {
                *populated = true;
            }
        }
        let _ = self.dir_children.insert(dirid, child_ids.clone());

        Ok(child_ids)
    }

    /// Fetch the full content of a file, using the provider+cache.
    async fn fetch_file_bytes(&self, node: &Node) -> Result<Vec<u8>, nfsstat3> {
        if let NodeKind::File {
            digest,
            inline_content: Some(content),
            ..
        } = &node.kind
        {
            debug!(
                "inline content for file id={} ({} bytes)",
                node.id,
                content.len()
            );
            let _ = digest;
            return Ok(content.clone());
        }

        let digest = match &node.kind {
            NodeKind::File { digest, .. } => digest.clone(),
            _ => return Err(nfsstat3::NFS3ERR_ISDIR),
        };

        if let Some(data) = self.cache.get(&digest) {
            return Ok(data);
        }

        let data = self.provider.fetch_blob(&digest).await.map_err(|e| {
            error!("fetch_blob({}) failed: {}", digest, e);
            nfsstat3::NFS3ERR_IO
        })?;

        if let Err(e) = self.cache.put(&digest, &data) {
            error!("cache put failed for {}: {}", digest, e);
        }
        Ok(data)
    }
}

#[async_trait]
impl NFSFileSystem for CtxfsNfs {
    fn root_dir(&self) -> fileid3 {
        ROOT_ID
    }

    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let name: &[u8] = filename.as_ref();

        if name == b"." {
            return Ok(dirid);
        }
        if name == b".." {
            return Ok(self.nodes.get(&dirid).map_or(ROOT_ID, |n| n.parent));
        }

        let cache_key = (dirid, name.to_vec());
        if let Some(existing) = self.dir_cache.get(&cache_key) {
            return Ok(*existing);
        }

        // Not in cache — populate directory and try again.
        let _ = self.ensure_populated(dirid).await?;

        self.dir_cache
            .get(&cache_key)
            .map(|id| *id)
            .ok_or(nfsstat3::NFS3ERR_NOENT)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let node = self.nodes.get(&id).ok_or(nfsstat3::NFS3ERR_NOENT)?;
        Ok(Self::node_to_fattr(&node))
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let node = self.nodes.get(&id).ok_or(nfsstat3::NFS3ERR_NOENT)?.clone();

        let data = self.fetch_file_bytes(&node).await?;

        let total = data.len() as u64;
        let start = offset.min(total) as usize;
        let end = (offset + u64::from(count)).min(total) as usize;
        let eof = (end as u64) >= total;
        Ok((data[start..end].to_vec(), eof))
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let child_ids = self.ensure_populated(dirid).await?;

        let mut entries: Vec<NfsDirEntry> = Vec::new();
        let mut started = start_after == 0;

        for child_id in &child_ids {
            if !started {
                if *child_id == start_after {
                    started = true;
                }
                continue;
            }
            if entries.len() >= max_entries {
                break;
            }
            let Some(node) = self.nodes.get(child_id) else {
                continue;
            };
            entries.push(NfsDirEntry {
                fileid: node.id,
                name: filename3::from(node.name.clone()),
                attr: Self::node_to_fattr(&node),
            });
        }

        let last_returned = entries.last().map(|e| e.fileid);
        let end = match (last_returned, child_ids.last()) {
            (Some(last), Some(total_last)) => last == *total_last,
            _ => true,
        };

        Ok(ReadDirResult { entries, end })
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let node = self.nodes.get(&id).ok_or(nfsstat3::NFS3ERR_NOENT)?;
        match &node.kind {
            NodeKind::Symlink { target } => Ok(nfspath3::from(target.as_bytes().to_vec())),
            _ => Err(nfsstat3::NFS3ERR_INVAL),
        }
    }

    // --- Read-only stubs ---

    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn write(&self, _id: fileid3, _offset: u64, _data: &[u8]) -> Result<fattr3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
        _attr: sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn create_exclusive(
        &self,
        _dirid: fileid3,
        _filename: &filename3,
    ) -> Result<fileid3, nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn mkdir(
        &self,
        _dirid: fileid3,
        _dirname: &filename3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn remove(&self, _dirid: fileid3, _filename: &filename3) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn rename(
        &self,
        _from_dirid: fileid3,
        _from_filename: &filename3,
        _to_dirid: fileid3,
        _to_filename: &filename3,
    ) -> Result<(), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }

    async fn symlink(
        &self,
        _dirid: fileid3,
        _linkname: &filename3,
        _symlink: &nfspath3,
        _attr: &sattr3,
    ) -> Result<(fileid3, fattr3), nfsstat3> {
        Err(nfsstat3::NFS3ERR_ROFS)
    }
}
