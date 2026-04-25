//! Daemon-owned registry of rate-limit gauges and per-mount counters.
//! Assembles the StatusReportV1 payload for IPC `get_status`.

use ctxfs_provider_common::counters::{CounterKey, MountCounters};
use ctxfs_provider_common::rate_limit::{
    AuthIdentity, AuthKind, RateLimitGauge, ResourceClass, ThrottleState,
};
use ctxfs_provider_common::status::{BudgetEntry, CounterEntry, MountSummary, StatusReportV1};
use dashmap::DashMap;
use std::sync::Arc;
use std::time::UNIX_EPOCH;

/// Registry of all rate-limit gauges and per-mount counters owned by the daemon.
#[derive(Debug, Default)]
pub struct Observability {
    /// Keyed by (AuthIdentity, ResourceClass).
    gauges: DashMap<(AuthIdentity, ResourceClass), RateLimitGauge>,
    /// Keyed by (source, repo, commit, mount_id).
    counters: DashMap<CounterKey, Arc<MountCounters>>,
}

impl Observability {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create the counters for a mount.
    pub fn counters_for(&self, key: CounterKey) -> Arc<MountCounters> {
        self.counters
            .entry(key)
            .or_insert_with(|| Arc::new(MountCounters::new()))
            .clone()
    }

    /// Get a clone of the gauge for an (auth, resource) pair, or `unknown` if absent.
    #[must_use]
    pub fn gauge_for(&self, auth: &AuthIdentity, resource: &ResourceClass) -> RateLimitGauge {
        self.gauges
            .get(&(auth.clone(), resource.clone()))
            .map(|g| g.clone())
            .unwrap_or_else(RateLimitGauge::unknown)
    }

    /// Update a gauge from response headers. Creates the entry if absent.
    pub fn update_gauge(
        &self,
        auth: AuthIdentity,
        resource: ResourceClass,
        headers: &std::collections::HashMap<String, String>,
    ) {
        let mut g = self
            .gauges
            .entry((auth, resource))
            .or_insert_with(RateLimitGauge::unknown);
        g.update_from_headers(headers);
    }

    /// Assemble a StatusReportV1 payload for IPC.
    #[must_use]
    pub fn status_report(&self) -> StatusReportV1 {
        let budgets = self
            .gauges
            .iter()
            .map(|entry| {
                let (auth, resource) = entry.key();
                let g = entry.value();
                BudgetEntry {
                    host: auth.host.clone(),
                    auth_kind: auth_kind_string(&auth.kind),
                    resource_class: resource_class_string(resource),
                    limit: g.limit,
                    remaining: g.remaining,
                    reset_at_unix: g
                        .reset_at
                        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                        .map(|d| d.as_secs()),
                    throttle_active_until_unix: match &g.secondary_throttle_state {
                        ThrottleState::None => None,
                        ThrottleState::Active { until } => {
                            until.duration_since(UNIX_EPOCH).ok().map(|d| d.as_secs())
                        }
                    },
                }
            })
            .collect();

        let counters: Vec<CounterEntry> = self
            .counters
            .iter()
            .map(|entry| CounterEntry {
                key: entry.key().clone(),
                counters: entry.value().snapshot(),
            })
            .collect();

        #[allow(clippy::cast_precision_loss)]
        let mut mounts: Vec<MountSummary> = counters
            .iter()
            .map(|c| {
                let total_cache_ops = c.counters.cache_hits + c.counters.cache_misses;
                let cache_hit_ratio = if total_cache_ops > 0 {
                    Some(c.counters.cache_hits as f64 / total_cache_ops as f64)
                } else {
                    None
                };
                MountSummary {
                    mount_id: c.key.mount_id.clone(),
                    source: c.key.source.clone(),
                    repo: c.key.repo.clone(),
                    commit: c.key.commit.clone(),
                    rest_calls_total: c.counters.rest_calls_total,
                    bytes_total: c.counters.bytes_total,
                    prefetch_hits: c.counters.prefetch_hits,
                    cache_hit_ratio,
                }
            })
            .collect();

        // Sort by rest_calls_total descending so the top-N consumers are first.
        // When two mounts have equal cost, iteration order is undefined (acceptable
        // for top-N reporting where only the top 10 are shown by the CLI).
        mounts.sort_by(|a, b| b.rest_calls_total.cmp(&a.rest_calls_total));

        StatusReportV1 {
            schema_version: 1,
            budgets,
            counters,
            mounts,
        }
    }
}

/// Produces wire-format string for AuthKind (safe to log, contains no secrets).
/// Format: "anonymous" | "pat:{8-char-prefix}" | "github_app:{installation_id}"
fn auth_kind_string(kind: &AuthKind) -> String {
    match kind {
        AuthKind::Anonymous => "anonymous".to_string(),
        AuthKind::Pat { token_id_prefix } => format!("pat:{token_id_prefix}"),
        AuthKind::GithubApp { installation_id } => format!("github_app:{installation_id}"),
    }
}

/// Produces wire-format string for ResourceClass (matches GitHub x-ratelimit-resource header values).
/// Format: "core" | "search" | "code_search" | "graphql" | "other:{value}"
fn resource_class_string(rc: &ResourceClass) -> String {
    match rc {
        ResourceClass::Core => "core".to_string(),
        ResourceClass::Search => "search".to_string(),
        ResourceClass::CodeSearch => "code_search".to_string(),
        ResourceClass::Graphql => "graphql".to_string(),
        ResourceClass::Other(s) => format!("other:{s}"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(mount_id: &str) -> CounterKey {
        CounterKey {
            source: "github".to_string(),
            repo: "foo/bar".to_string(),
            commit: "abc".to_string(),
            mount_id: mount_id.to_string(),
        }
    }

    #[test]
    fn empty_registry_produces_v1_report() {
        let o = Observability::new();
        let r = o.status_report();
        assert_eq!(r.schema_version, 1);
        assert!(r.budgets.is_empty());
        assert!(r.counters.is_empty());
        assert!(r.mounts.is_empty());
    }

    #[test]
    fn counters_for_creates_and_returns_same_arc() {
        let o = Observability::new();
        let c1 = o.counters_for(key("mnt-1"));
        let c2 = o.counters_for(key("mnt-1"));
        c1.record_rest_call();
        // c2 should observe the increment because they share the Arc.
        assert_eq!(c2.snapshot().rest_calls_total, 1);
    }

    #[test]
    fn mounts_sorted_by_rest_calls_descending() {
        let o = Observability::new();
        let a = o.counters_for(key("mnt-A"));
        let b = o.counters_for(key("mnt-B"));
        for _ in 0..3 {
            a.record_rest_call();
        }
        for _ in 0..7 {
            b.record_rest_call();
        }
        let r = o.status_report();
        assert_eq!(r.mounts.len(), 2);
        assert_eq!(r.mounts[0].mount_id, "mnt-B"); // 7 > 3
        assert_eq!(r.mounts[1].mount_id, "mnt-A");
    }

    #[test]
    fn cache_hit_ratio_is_none_when_no_cache_ops() {
        let o = Observability::new();
        let _ = o.counters_for(key("mnt-1"));
        let r = o.status_report();
        assert!(r.mounts[0].cache_hit_ratio.is_none());
    }

    #[test]
    fn cache_hit_ratio_calculated_when_ops_recorded() {
        let o = Observability::new();
        let c = o.counters_for(key("mnt-1"));
        c.record_cache_hit();
        c.record_cache_hit();
        c.record_cache_hit();
        c.record_cache_miss();
        let r = o.status_report();
        let ratio = r.mounts[0].cache_hit_ratio.unwrap();
        assert!((ratio - 0.75).abs() < 1e-9);
    }
}
