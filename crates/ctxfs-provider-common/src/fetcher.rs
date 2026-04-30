//! Source-agnostic content fetching contract.
//!
//! The types and trait shape let `GitHubProvider`'s tarball-vs-lazy decision
//! be expressed as a `FetchPolicy` value (rather than inline `if`/`else`),
//! and let provider-common-level tests describe expected behavior without
//! pulling in `provider-git`.
//!
//! The trait is intentionally minimal: future native-CDN providers (npm
//! tarballs, PyPI sdists, crates.io `.crate` files) plug in by
//! implementing it.

use crate::counters::CounterKey;
use ctxfs_core::source::SourceSpec;
use ctxfs_core::Digest;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

/// What kind of mount-relative entry a request points at.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum ContentKind {
    /// Regular file blob.
    File,
    /// Symlink — `digest` (when set) names the blob storing the link target.
    Symlink,
    /// Git LFS pointer file (pointer bytes stored verbatim for now).
    LfsPointer,
}

/// Mount-relative content request. Provider-agnostic: GitHub blobs, npm
/// tarball entries, PyPI sdist entries can all be expressed.
#[derive(Debug, Clone)]
pub struct ContentRequest {
    /// Mount-relative path (semantic key — what the user would `cat`).
    pub path: PathBuf,
    /// Content hash if the source provides one. GitHub: blob SHA-1;
    /// npm: integrity SHA. None => no upstream digest available.
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

/// Cost-estimate signal a `ContentFetcher` can offer up front. Currently
/// `total_bytes` and `request_count` are used; remaining fields are reserved.
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

/// A single in-flight tarball fetch slot. The leader populates `cell`; waiters
/// call `cell.get_or_init(...)` and block until the cell is filled.
///
/// Stored as `Arc<TarballSlot>` in [`TarballSingleflightMap`] so the leader can
/// use [`Arc::ptr_eq`] in [`SlotClaim::release`] to ensure it removes *its own*
/// slot and not a newer one inserted for the same key after the leader finished.
#[derive(Debug, Default)]
pub struct TarballSlot {
    /// Populated by the leader. Stores `Ok(())` on success, `Err(msg)` on
    /// failure so waiters can observe the error without retrying in-flight.
    pub cell: tokio::sync::OnceCell<Result<(), String>>,
}

/// Type alias for the daemon-side singleflight registry. Lives in
/// `provider-common` (not daemon) so `provider-git` can import it without
/// inducing a circular dependency (`daemon` → `provider-git`).
pub type TarballSingleflightMap = dashmap::DashMap<TarballKey, Arc<TarballSlot>>;

/// Returned by `claim_singleflight_slot`. Carries the slot Arc, whether this
/// caller is the leader, and a reference to the registry so the leader can
/// remove its slot when done.
///
/// ## Leader-cancellation semantics
///
/// If the leader task is cancelled before [`SlotClaim::release`] is called, the
/// slot stays in the registry with an uninitialized cell. The next waiter on
/// `get_or_init` becomes the de-facto initializer with `is_leader = false`, so
/// its `release()` is a no-op and the slot persists for the daemon session.
/// This is bounded — one slot per `(host, owner, repo, commit)` tuple — and
/// causes at most one spurious no-op release per surviving waiter.
#[derive(Debug)]
pub struct SlotClaim {
    /// The key this claim is for — needed by [`SlotClaim::release`].
    pub key: TarballKey,
    /// The shared slot. Leader writes to `cell`; waiters read from `cell`.
    pub slot: Arc<TarballSlot>,
    /// `true` iff this caller inserted the slot (i.e., was first for this key).
    pub is_leader: bool,
    /// Reference back to the registry so the leader can call `remove_if`.
    pub registry: Arc<TarballSingleflightMap>,
}

impl SlotClaim {
    /// Leader-only: remove the slot from the registry when work is complete.
    ///
    /// Uses [`Arc::ptr_eq`] so a stale leader claim (whose work finished before
    /// a newer slot was inserted for the same key) cannot accidentally remove
    /// the new slot. Waiters' `release()` is a no-op.
    pub fn release(&self) {
        if !self.is_leader {
            return;
        }
        let target = Arc::clone(&self.slot);
        let _ = self
            .registry
            .remove_if(&self.key, |_, slot| Arc::ptr_eq(slot, &target));
    }
}

/// Source-bound context passed to [`ContentFetcher::fetch_batch`]. Carries the
/// resolved source spec and revision so providers don't have to derive these
/// from `CounterKey` (which is a telemetry concern, not a data dependency).
#[derive(Debug, Clone)]
pub struct FetchBatchContext {
    /// The source being fetched. Provider-specific fields like `name`
    /// (`owner/repo` for GitHub) are interpreted by the provider.
    pub source: SourceSpec,
    /// Resolved upstream revision (e.g., 40-char Git commit SHA for GitHub;
    /// version string for npm/PyPI/crates.io). Always concrete, never a ref.
    pub resolved_revision: String,
}

/// Content fetching trait. The signature is kept narrow on purpose —
/// extending the surface is a breaking change once a second provider
/// implements it.
#[async_trait::async_trait]
pub trait ContentFetcher: Send + Sync {
    /// Cost-of-fulfillment estimate for a batch. Pure if possible; safe to
    /// call multiple times. Used to drive scheduling decisions.
    fn estimate_cost(&self, requests: &[ContentRequest]) -> CostEstimate;

    /// Fetch the given requests as a single batch. Called by orchestration
    /// code (e.g., `fetch_snapshot_inner` in `GitHubProvider`) when the
    /// provider's auto-gate elects bulk prefetch.
    ///
    /// ## Return contract
    ///
    /// The returned `HashMap<PathBuf, Vec<u8>>` is best-effort: it contains
    /// bytes for whatever paths the provider fulfilled in this call.
    /// Missing paths are NOT errors — callers should fall back to their
    /// per-request fetch path (e.g., `Provider::fetch_blob` for GitHub)
    /// for paths absent from the map.
    ///
    /// GitHub's tarball flow warms `BlobCache` by digest and may return an
    /// empty map even on success. Future native-CDN providers (npm tarballs,
    /// PyPI sdists, crates.io `.crate` files) may populate the map directly
    /// if their tarball returns bytes.
    async fn fetch_batch(
        &self,
        ctx: &FetchBatchContext,
        requests: &[ContentRequest],
        mode: FetchMode,
        counter_key: Option<CounterKey>,
    ) -> Result<HashMap<PathBuf, Vec<u8>>, ctxfs_core::error::CtxfsError>;
}

/// Pure auto-gate: given a count + estimated bytes + the user's policy + the
/// configured thresholds, return the `FetchPolicy` to apply. Lives here so
/// all providers share the same algorithm.
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
    use std::sync::Arc;

    // ---- SlotClaim release semantics ----

    fn make_registry() -> Arc<TarballSingleflightMap> {
        Arc::new(dashmap::DashMap::new())
    }

    fn make_key(suffix: &str) -> TarballKey {
        TarballKey {
            host: "api.github.com".to_string(),
            owner: "owner".to_string(),
            repo: "repo".to_string(),
            commit_sha: suffix.to_string(),
        }
    }

    /// Leader's `release()` removes its slot from the registry via Arc::ptr_eq.
    #[test]
    fn slot_claim_release_leader_removes_slot() {
        let registry = make_registry();
        let key = make_key("abc123");
        let slot = Arc::new(TarballSlot::default());
        let _ = registry.insert(key.clone(), Arc::clone(&slot));

        let claim = SlotClaim {
            key: key.clone(),
            slot,
            is_leader: true,
            registry: Arc::clone(&registry),
        };
        claim.release();

        assert!(
            registry.get(&key).is_none(),
            "leader release must remove the slot from the registry"
        );
    }

    /// Waiter's `release()` is a no-op — the slot stays in the registry.
    #[test]
    fn slot_claim_release_waiter_is_noop() {
        let registry = make_registry();
        let key = make_key("abc123");
        let slot = Arc::new(TarballSlot::default());
        let _ = registry.insert(key.clone(), Arc::clone(&slot));

        let claim = SlotClaim {
            key: key.clone(),
            slot: Arc::clone(&slot),
            is_leader: false,
            registry: Arc::clone(&registry),
        };
        claim.release();

        assert!(
            registry.get(&key).is_some(),
            "waiter release must not remove the slot"
        );
    }

    /// A stale leader cannot remove a *newer* slot inserted for the same key
    /// after the old slot was replaced. Arc::ptr_eq distinguishes them.
    #[test]
    fn stale_leader_cannot_remove_newer_slot() {
        let registry = make_registry();
        let key = make_key("abc123");

        // Insert the old slot and capture it in a stale claim.
        let old_slot = Arc::new(TarballSlot {
            cell: tokio::sync::OnceCell::new(),
        });
        let _ = registry.insert(key.clone(), Arc::clone(&old_slot));
        let stale_claim = SlotClaim {
            key: key.clone(),
            slot: old_slot,
            is_leader: true,
            registry: Arc::clone(&registry),
        };

        // Replace the registry entry with a new slot (simulating a later
        // concurrent mount that already inserted a fresh slot for the same key).
        let new_slot = Arc::new(TarballSlot {
            cell: tokio::sync::OnceCell::new(),
        });
        let _ = registry.insert(key.clone(), Arc::clone(&new_slot));

        // Stale leader's release must NOT remove the new slot.
        stale_claim.release();

        assert!(
            registry.get(&key).is_some(),
            "stale leader must not remove a newer slot via Arc::ptr_eq mismatch"
        );
    }

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
