//! Source-agnostic content fetching contract.
//!
//! Skeletal in M3: the types and trait shape ship so that
//! `GitHubProvider`'s tarball-vs-lazy decision can be expressed as a
//! `FetchPolicy` value (rather than inline `if`/`else`), and so that
//! provider-common-level tests can describe expected behavior without
//! pulling in `provider-git`. M4 promotes `GitHubProvider` to the first
//! concrete `ContentFetcher` impl without restructuring the call shape.
//!
//! The trait is intentionally minimal: future native-CDN providers (npm
//! tarballs, PyPI sdists, crates.io `.crate` files) plug in by
//! implementing it.

use crate::counters::CounterKey;
use ctxfs_core::Digest;
use std::path::PathBuf;

/// What kind of mount-relative entry a request points at.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ContentKind {
    /// Regular file blob.
    File,
    /// Symlink — `digest` (when set) names the blob storing the link target.
    Symlink,
    /// Git LFS pointer file (M5 detect; M3 stores pointer bytes verbatim).
    LfsPointer,
}

/// Mount-relative content request. Provider-agnostic: GitHub blobs, npm
/// tarball entries, PyPI sdist entries can all be expressed.
#[derive(Debug, Clone)]
pub struct ContentRequest {
    /// Mount-relative path (semantic key — what the user would `cat`).
    pub path: PathBuf,
    /// Content hash if the source provides one. GitHub: blob SHA-1 (post-M5
    /// `Digest::Sha1`); npm: integrity SHA. None => no upstream digest available.
    pub digest: Option<Digest>,
    /// Estimated bytes from the manifest. None => unknown until fetched.
    pub size: Option<u64>,
    /// Entry shape.
    pub kind: ContentKind,
}

/// How a batch of requests should be fulfilled.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum FetchMode {
    /// Best-effort lazy: request when the user reads the file.
    Lazy,
    /// Bulk prefetch (e.g., GitHub tarball, npm tarball-of-tarballs).
    BulkPrefetch,
    /// User explicitly forced (e.g., `--prefetch`).
    Forced,
}

/// User-facing prefetch policy from `MountOptions`.
#[derive(Debug, Clone, Copy, Eq, PartialEq, serde::Serialize, serde::Deserialize)]
pub enum PrefetchPolicy {
    /// Auto-gate: tarball if (count >= threshold) AND (bytes <= cap).
    Auto,
    /// Bypass the byte cap; warn if estimated_bytes > cache budget.
    Force,
    /// Never prefetch; always lazy.
    Disabled,
}

impl Default for PrefetchPolicy {
    fn default() -> Self {
        Self::Auto
    }
}

/// Result of the auto-gate computation: which fetch shape to use this mount.
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum FetchPolicy {
    /// Bulk-fetch via the source's archive endpoint (Stage 1 GitHub tarball).
    Tarball {
        estimated_bytes: u64,
        blob_count: u64,
    },
    /// Per-blob lazy fetch on read.
    Lazy,
    /// Auto-gate would have fired but the byte cap was exceeded.
    /// Telemetry-bearing variant so callers increment
    /// `prefetch_skipped_oversized` and surface the reason in `ctxfs status`.
    LazyOversized {
        estimated_bytes: u64,
        blob_count: u64,
        cap: u64,
    },
}

/// Cost-estimate signal a `ContentFetcher` can offer up front. M3 uses only
/// `total_bytes` and `request_count` — the rest are reserved for M4.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct CostEstimate {
    pub total_bytes: Option<u64>,
    pub request_count: usize,
    pub fetch_mode: Option<FetchMode>,
}

/// Key for daemon-side singleflight tarball dedupe. Lives in provider-common
/// (not daemon) so provider-git can construct it without inducing a dep cycle
/// (provider-git → provider-common is the existing direction).
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct TarballKey {
    /// API host: `api.github.com`, or the configured `CTXFS_GITHUB_HOST` for
    /// GHE. Differentiates so a single daemon serving multiple hosts is fine.
    pub host: String,
    /// GitHub organization or user that owns the repository.
    pub owner: String,
    /// Repository name (without the owner prefix).
    pub repo: String,
    /// Full 40-character commit SHA that the tarball was fetched at.
    pub commit_sha: String,
}

/// Skeletal trait. M3 does not require providers to implement it; M4 will.
/// The signature is kept narrow on purpose — extending the surface is a
/// breaking change once a second provider implements it.
#[async_trait::async_trait]
pub trait ContentFetcher: Send + Sync {
    /// Cost-of-fulfillment estimate for a batch. Pure if possible; safe to
    /// call multiple times. M4 callers use this to drive scheduling decisions.
    fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate;

    /// Fetch the given requests. The provider chooses tarball vs lazy via
    /// its own `decide_policy(...)` (which is the M3 free function).
    /// The returned map is keyed by `ContentRequest::path`.
    async fn fetch_batch(
        &self,
        requests: &[ContentRequest],
        mode: FetchMode,
        counter_key: Option<CounterKey>,
    ) -> Result<std::collections::HashMap<PathBuf, Vec<u8>>, ctxfs_core::error::CtxfsError>;
}

/// Pure auto-gate: given a count + estimated bytes + the user's policy + the
/// configured thresholds, return the `FetchPolicy` to apply. Lives here so
/// providers (M3 GitHub, M4 future) all use the same algorithm.
#[must_use]
pub fn decide_policy(
    blob_count: u64,
    estimated_bytes: u64,
    policy: PrefetchPolicy,
    threshold_count: u64,
    max_bytes: u64,
) -> FetchPolicy {
    match policy {
        PrefetchPolicy::Disabled => FetchPolicy::Lazy,
        PrefetchPolicy::Force => FetchPolicy::Tarball {
            estimated_bytes,
            blob_count,
        },
        PrefetchPolicy::Auto => {
            if blob_count < threshold_count {
                FetchPolicy::Lazy
            } else if estimated_bytes > max_bytes {
                FetchPolicy::LazyOversized {
                    estimated_bytes,
                    blob_count,
                    cap: max_bytes,
                }
            } else {
                FetchPolicy::Tarball {
                    estimated_bytes,
                    blob_count,
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_gate_below_count_is_lazy() {
        assert_eq!(
            decide_policy(10, 1_000, PrefetchPolicy::Auto, 30, 256_000_000),
            FetchPolicy::Lazy,
        );
    }

    #[test]
    fn auto_gate_at_count_within_bytes_is_tarball() {
        let p = decide_policy(30, 1_000, PrefetchPolicy::Auto, 30, 256_000_000);
        assert!(matches!(p, FetchPolicy::Tarball { .. }));
    }

    #[test]
    fn auto_gate_above_bytes_is_lazy_oversized() {
        let p = decide_policy(1_000, 500_000_000, PrefetchPolicy::Auto, 30, 256_000_000);
        match p {
            FetchPolicy::LazyOversized {
                estimated_bytes,
                blob_count,
                cap,
            } => {
                assert_eq!(estimated_bytes, 500_000_000);
                assert_eq!(blob_count, 1_000);
                assert_eq!(cap, 256_000_000);
            }
            other => panic!("expected LazyOversized, got {other:?}"),
        }
    }

    #[test]
    fn force_bypasses_byte_cap() {
        let p = decide_policy(2, 999_999_999, PrefetchPolicy::Force, 30, 1_000);
        assert!(matches!(p, FetchPolicy::Tarball { .. }));
    }

    #[test]
    fn disabled_is_always_lazy() {
        assert_eq!(
            decide_policy(1_000, 100, PrefetchPolicy::Disabled, 30, 256_000_000),
            FetchPolicy::Lazy,
        );
    }
}
