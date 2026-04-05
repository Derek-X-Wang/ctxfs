//! `NFSv3` loopback backend for `ContextFS`.
//!
//! Implements `nfsserve::vfs::NFSFileSystem` on top of our existing
//! `Provider` + `BlobCache` + `Snapshot` stack. Zero kernel extension —
//! macOS and Linux mount via their built-in `mount_nfs` / `mount.nfs`.

mod fs;

pub use fs::{CtxfsNfs, NfsServerHandle};
