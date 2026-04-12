use async_trait::async_trait;
use ctxfs_core::source::SourceSpec;
use ctxfs_vfs::{NodeAttr, NodeType, VfsError, VfsState};
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3,
};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use nfsserve::vfs::{DirEntry as NfsDirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use std::sync::Arc;
use tracing::error;

const BLOCK_SIZE: u64 = 4096;

/// `CtxfsNfs` is a thin adapter that translates between [`VfsState`] and the
/// NFS3 protocol types required by [`NFSFileSystem`].
pub struct CtxfsNfs {
    vfs: Arc<VfsState>,
    #[allow(dead_code)] // retained for debug / future refresh
    source: SourceSpec,
}

impl std::fmt::Debug for CtxfsNfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtxfsNfs")
            .field("source", &self.source.to_string())
            .field("vfs", &self.vfs)
            .finish_non_exhaustive()
    }
}

/// Handle returned by [`CtxfsNfs::spawn`] that keeps the NFS server running
/// until dropped. Currently a marker -- the tokio task is detached.
#[derive(Debug)]
pub struct NfsServerHandle {
    /// The address the NFS server is listening on, e.g. `127.0.0.1:11111`.
    pub addr: String,
}

impl CtxfsNfs {
    /// Create a new `CtxfsNfs` that delegates all filesystem operations to the
    /// given [`VfsState`].
    #[must_use]
    pub fn new(vfs: Arc<VfsState>, source: SourceSpec) -> Self {
        Self { vfs, source }
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
}

// ---------------------------------------------------------------------------
// Translation helpers
// ---------------------------------------------------------------------------

/// Convert a [`NodeAttr`] to the NFS3 `fattr3` struct.
fn attr_to_fattr3(attr: &NodeAttr) -> fattr3 {
    let epoch = nfstime3 {
        seconds: 0,
        nseconds: 0,
    };
    match attr.kind {
        NodeType::Directory => fattr3 {
            ftype: ftype3::NF3DIR,
            mode: 0o555,
            nlink: 2,
            uid: 0,
            gid: 0,
            size: BLOCK_SIZE,
            used: BLOCK_SIZE,
            rdev: specdata3::default(),
            fsid: 0x6374_7866_7300_0001,
            fileid: attr.inode,
            atime: epoch,
            mtime: epoch,
            ctime: epoch,
        },
        NodeType::File => fattr3 {
            ftype: ftype3::NF3REG,
            mode: if attr.executable { 0o555 } else { 0o444 },
            nlink: 1,
            uid: 0,
            gid: 0,
            size: attr.size,
            used: attr.size,
            rdev: specdata3::default(),
            fsid: 0x6374_7866_7300_0001,
            fileid: attr.inode,
            atime: epoch,
            mtime: epoch,
            ctime: epoch,
        },
        NodeType::Symlink => fattr3 {
            ftype: ftype3::NF3LNK,
            mode: 0o777,
            nlink: 1,
            uid: 0,
            gid: 0,
            size: attr.size,
            used: attr.size,
            rdev: specdata3::default(),
            fsid: 0x6374_7866_7300_0001,
            fileid: attr.inode,
            atime: epoch,
            mtime: epoch,
            ctime: epoch,
        },
    }
}

/// Convert a [`VfsError`] to the corresponding NFS3 status code.
fn vfs_err_to_nfs(e: &VfsError) -> nfsstat3 {
    match e {
        VfsError::NotFound => nfsstat3::NFS3ERR_NOENT,
        VfsError::NotDir => nfsstat3::NFS3ERR_NOTDIR,
        VfsError::IsDir => nfsstat3::NFS3ERR_ISDIR,
        VfsError::Invalid => nfsstat3::NFS3ERR_INVAL,
        VfsError::ReadOnly => nfsstat3::NFS3ERR_ROFS,
        VfsError::Io(_) => nfsstat3::NFS3ERR_IO,
    }
}

// ---------------------------------------------------------------------------
// NFSFileSystem implementation
// ---------------------------------------------------------------------------

#[async_trait]
impl NFSFileSystem for CtxfsNfs {
    fn root_dir(&self) -> fileid3 {
        self.vfs.root_id()
    }

    fn capabilities(&self) -> VFSCapabilities {
        VFSCapabilities::ReadOnly
    }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let name = std::str::from_utf8(filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        let (id, _attr) = self.vfs.lookup(dirid, name).await.map_err(|ref e| vfs_err_to_nfs(e))?;
        Ok(id)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let attr = self.vfs.getattr(id).await.map_err(|ref e| vfs_err_to_nfs(e))?;
        Ok(attr_to_fattr3(&attr))
    }

    async fn read(
        &self,
        id: fileid3,
        offset: u64,
        count: u32,
    ) -> Result<(Vec<u8>, bool), nfsstat3> {
        let attr = self.vfs.getattr(id).await.map_err(|ref e| vfs_err_to_nfs(e))?;
        let data = self.vfs.read(id, offset, count).await.map_err(|ref e| vfs_err_to_nfs(e))?;
        let eof = (offset + data.len() as u64) >= attr.size;
        Ok((data, eof))
    }

    async fn readdir(
        &self,
        dirid: fileid3,
        start_after: fileid3,
        max_entries: usize,
    ) -> Result<ReadDirResult, nfsstat3> {
        let children = self.vfs.readdir(dirid).await.map_err(|ref e| vfs_err_to_nfs(e))?;

        let mut entries: Vec<NfsDirEntry> = Vec::new();
        let mut started = start_after == 0;

        for (child_id, name, _kind) in &children {
            if !started {
                if *child_id == start_after {
                    started = true;
                }
                continue;
            }
            if entries.len() >= max_entries {
                break;
            }
            let attr = self.vfs.getattr(*child_id).await.map_err(|ref e| vfs_err_to_nfs(e))?;
            entries.push(NfsDirEntry {
                fileid: *child_id,
                name: filename3::from(name.as_bytes().to_vec()),
                attr: attr_to_fattr3(&attr),
            });
        }

        let last_returned = entries.last().map(|e| e.fileid);
        let end = match (last_returned, children.last()) {
            (Some(last), Some((total_last, _, _))) => last == *total_last,
            _ => true,
        };

        Ok(ReadDirResult { entries, end })
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let target = self.vfs.readlink(id).await.map_err(|ref e| vfs_err_to_nfs(e))?;
        Ok(nfspath3::from(target.into_bytes()))
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
