use async_trait::async_trait;
use std::sync::Arc;

use crate::digest::Digest;
use crate::error::CtxfsError;
use crate::source::SourceSpec;

// Re-export manifest types used in the provider trait — they are defined in ctxfs-manifest
// but we use opaque types here to avoid circular dependency. The actual manifest types
// are used by concrete implementations.

/// Opaque snapshot data returned by a provider.
/// Concrete providers return their own Snapshot type; the daemon bridges the gap.
#[async_trait]
pub trait Provider: Send + Sync {
    /// Fetch a full snapshot (tree manifest) for the given source.
    /// Returns serialized snapshot JSON bytes for flexibility.
    async fn fetch_snapshot(&self, source: &SourceSpec) -> Result<Vec<u8>, CtxfsError>;

    /// Fetch a directory listing by its content digest.
    /// Returns serialized Directory JSON bytes.
    async fn fetch_directory(&self, digest: &Digest) -> Result<Vec<u8>, CtxfsError>;

    /// Fetch a blob's content by its digest.
    async fn fetch_blob(&self, digest: &Digest) -> Result<Vec<u8>, CtxfsError>;
}

/// Type alias for shared provider reference.
pub type SharedProvider = Arc<dyn Provider>;
