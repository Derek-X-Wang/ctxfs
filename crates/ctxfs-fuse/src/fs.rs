use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::source::SourceSpec;
use ctxfs_core::Digest;
use ctxfs_manifest::{DirEntry, Directory, InodeEntry, InodeKind, InodeTable, Snapshot};
use fuser::{
    FileAttr, FileType, Filesystem, MountOption, ReplyAttr, ReplyData, ReplyDirectory, ReplyEntry,
    ReplyOpen, ReplyStatfs, Request,
};
use std::ffi::OsStr;
use std::sync::Arc;
use std::time::{Duration, UNIX_EPOCH};
use tokio::runtime::Handle;
use tracing::{debug, error};

const TTL: Duration = Duration::from_secs(3600); // 1 hour — immutable snapshot
const BLOCK_SIZE: u32 = 4096;

pub struct CtxfsFilesystem {
    rt_handle: Handle,
    provider: SharedProvider,
    source: SourceSpec,
    cache: Arc<BlobCache>,
    inodes: InodeTable,
    #[allow(dead_code)] // retained for future snapshot refresh
    snapshot: Snapshot,
}

impl std::fmt::Debug for CtxfsFilesystem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtxfsFilesystem")
            .field("source", &self.source.to_string())
            .finish_non_exhaustive()
    }
}

impl CtxfsFilesystem {
    pub fn new(
        rt_handle: Handle,
        provider: SharedProvider,
        source: SourceSpec,
        cache: Arc<BlobCache>,
        snapshot: Snapshot,
    ) -> Self {
        let inodes = InodeTable::new();
        inodes.insert_root(snapshot.root_directory.clone());

        Self {
            rt_handle,
            provider,
            source,
            cache,
            inodes,
            snapshot,
        }
    }

    /// Mount this filesystem at the given path. Blocks until unmounted.
    pub fn mount(self, mountpoint: &str) -> Result<fuser::BackgroundSession, std::io::Error> {
        let options = vec![
            MountOption::RO,
            MountOption::FSName("ctxfs".to_string()),
            MountOption::CUSTOM("nobrowse".to_string()),
            MountOption::CUSTOM("volname=ContextFS".to_string()),
        ];

        fuser::spawn_mount2(self, mountpoint, &options)
    }

    fn ensure_populated(&self, ino: u64) {
        if self.inodes.is_populated(ino) {
            return;
        }

        let entry = match self.inodes.get(ino) {
            Some(e) => e,
            None => return,
        };

        let digest = match entry.digest() {
            Some(d) => d.clone(),
            None => return,
        };

        // Fetch directory data via provider
        let data = match self
            .rt_handle
            .block_on(self.provider.fetch_directory(&digest))
        {
            Ok(d) => d,
            Err(e) => {
                error!("failed to fetch directory {}: {}", digest, e);
                return;
            }
        };

        let dir: Directory = match serde_json::from_slice(&data) {
            Ok(d) => d,
            Err(e) => {
                error!("failed to parse directory {}: {}", digest, e);
                return;
            }
        };

        // Populate children
        for dir_entry in &dir.entries {
            match dir_entry {
                DirEntry::File(f) => {
                    let _ = self.inodes.allocate_inode(
                        ino,
                        f.name.clone(),
                        InodeKind::File {
                            digest: f.digest.clone(),
                            size: f.size,
                            executable: f.executable,
                            inline_content: f.inline_content.clone(),
                        },
                    );
                }
                DirEntry::Directory(d) => {
                    let _ = self.inodes.allocate_inode(
                        ino,
                        d.name.clone(),
                        InodeKind::Directory {
                            digest: d.digest.clone(),
                            children: Vec::new(),
                            populated: false,
                        },
                    );
                }
                DirEntry::Symlink(s) => {
                    let _ = self.inodes.allocate_inode(
                        ino,
                        s.name.clone(),
                        InodeKind::Symlink {
                            target: s.target.clone(),
                        },
                    );
                }
            }
        }

        self.inodes.mark_populated(ino);
    }

    fn make_attr(&self, entry: &InodeEntry) -> FileAttr {
        let (kind, perm, size, nlink) = match &entry.kind {
            InodeKind::Directory { .. } => (FileType::Directory, 0o555, 0, 2),
            InodeKind::File {
                size, executable, ..
            } => {
                let perm = if *executable { 0o555 } else { 0o444 };
                (FileType::RegularFile, perm, *size, 1)
            }
            InodeKind::Symlink { target } => (FileType::Symlink, 0o777, target.len() as u64, 1),
        };

        FileAttr {
            ino: entry.ino,
            size,
            blocks: (size + BLOCK_SIZE as u64 - 1) / BLOCK_SIZE as u64,
            atime: UNIX_EPOCH,
            mtime: UNIX_EPOCH,
            ctime: UNIX_EPOCH,
            crtime: UNIX_EPOCH,
            kind,
            perm,
            nlink,
            uid: unsafe { libc::getuid() },
            gid: unsafe { libc::getgid() },
            rdev: 0,
            blksize: BLOCK_SIZE,
            flags: 0,
        }
    }

    fn read_blob(&self, digest: &Digest) -> Option<Vec<u8>> {
        // Try cache
        if let Some(data) = self.cache.get(digest) {
            return Some(data);
        }

        // Try provider
        match self.rt_handle.block_on(self.provider.fetch_blob(digest)) {
            Ok(data) => {
                let _ = self.cache.put(digest, &data);
                Some(data)
            }
            Err(e) => {
                error!("failed to fetch blob {}: {}", digest, e);
                None
            }
        }
    }
}

impl Filesystem for CtxfsFilesystem {
    fn init(
        &mut self,
        _req: &Request<'_>,
        _config: &mut fuser::KernelConfig,
    ) -> Result<(), libc::c_int> {
        debug!("FUSE init for {}", self.source);
        // Eagerly populate root directory
        self.ensure_populated(1);
        Ok(())
    }

    fn lookup(&mut self, _req: &Request<'_>, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let name_str = name.to_string_lossy();
        self.ensure_populated(parent);

        match self.inodes.lookup(parent, &name_str) {
            Some(ino) => {
                if let Some(entry) = self.inodes.get(ino) {
                    reply.entry(&TTL, &self.make_attr(&entry), 0);
                } else {
                    reply.error(libc::ENOENT);
                }
            }
            None => {
                reply.error(libc::ENOENT);
            }
        }
    }

    fn getattr(&mut self, _req: &Request<'_>, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        match self.inodes.get(ino) {
            Some(entry) => {
                reply.attr(&TTL, &self.make_attr(&entry));
            }
            None => {
                reply.error(libc::ENOENT);
            }
        }
    }

    fn readdir(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        self.ensure_populated(ino);

        let entry = match self.inodes.get(ino) {
            Some(e) => e,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let mut entries: Vec<(u64, FileType, String)> = vec![
            (ino, FileType::Directory, ".".to_string()),
            (entry.parent, FileType::Directory, "..".to_string()),
        ];

        for child in self.inodes.children(ino) {
            let ft = match &child.kind {
                InodeKind::Directory { .. } => FileType::Directory,
                InodeKind::File { .. } => FileType::RegularFile,
                InodeKind::Symlink { .. } => FileType::Symlink,
            };
            entries.push((child.ino, ft, child.name.clone()));
        }

        for (i, (ino, ft, name)) in entries.iter().enumerate().skip(offset as usize) {
            if reply.add(*ino, (i + 1) as i64, *ft, name) {
                break;
            }
        }

        reply.ok();
    }

    fn open(&mut self, _req: &Request<'_>, ino: u64, flags: i32, reply: ReplyOpen) {
        // Reject writes
        let access_mode = flags & libc::O_ACCMODE;
        if access_mode == libc::O_WRONLY || access_mode == libc::O_RDWR {
            reply.error(libc::EACCES);
            return;
        }

        match self.inodes.get(ino) {
            Some(_) => reply.opened(0, fuser::consts::FOPEN_KEEP_CACHE),
            None => reply.error(libc::ENOENT),
        }
    }

    fn read(
        &mut self,
        _req: &Request<'_>,
        ino: u64,
        _fh: u64,
        offset: i64,
        size: u32,
        _flags: i32,
        _lock_owner: Option<u64>,
        reply: ReplyData,
    ) {
        let entry = match self.inodes.get(ino) {
            Some(e) => e,
            None => {
                reply.error(libc::ENOENT);
                return;
            }
        };

        let data = match &entry.kind {
            InodeKind::File {
                inline_content: Some(content),
                ..
            } => content.clone(),
            InodeKind::File { digest, .. } => match self.read_blob(digest) {
                Some(d) => d,
                None => {
                    reply.error(libc::EIO);
                    return;
                }
            },
            _ => {
                reply.error(libc::EISDIR);
                return;
            }
        };

        let offset = offset as usize;
        if offset >= data.len() {
            reply.data(&[]);
        } else {
            let end = (offset + size as usize).min(data.len());
            reply.data(&data[offset..end]);
        }
    }

    fn readlink(&mut self, _req: &Request<'_>, ino: u64, reply: ReplyData) {
        match self.inodes.get(ino) {
            Some(InodeEntry {
                kind: InodeKind::Symlink { ref target },
                ..
            }) => {
                reply.data(target.as_bytes());
            }
            _ => {
                reply.error(libc::ENOENT);
            }
        }
    }

    fn statfs(&mut self, _req: &Request<'_>, _ino: u64, reply: ReplyStatfs) {
        reply.statfs(
            0,    // blocks
            0,    // bfree
            0,    // bavail
            0,    // files
            0,    // ffree
            4096, // bsize
            256,  // namelen
            0,    // frsize
        );
    }
}
