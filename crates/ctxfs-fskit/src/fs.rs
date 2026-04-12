use ctxfs_vfs::VfsState;
use std::sync::Arc;

/// `FSKit` backend stub wrapping a [`VfsState`].
///
/// The actual `fskit_rs::Filesystem` trait implementation comes after the Phase 0 `PoC`.
#[derive(Debug)]
pub struct CtxfsFsKit {
    vfs: Arc<VfsState>,
}

impl CtxfsFsKit {
    /// Create a new [`CtxfsFsKit`] wrapping the given [`VfsState`].
    pub fn new(vfs: Arc<VfsState>) -> Self {
        Self { vfs }
    }

    /// Return a reference to the inner [`VfsState`].
    pub fn vfs(&self) -> &VfsState {
        &self.vfs
    }
}
