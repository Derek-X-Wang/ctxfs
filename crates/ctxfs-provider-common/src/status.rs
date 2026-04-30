//! Versioned IPC schema for `ctxfs status`.
//!
//! `schema_version` is an explicit field so future StatusReportV2 can be
//! added with parallel support without breaking older CLI clients.

use crate::counters::{CounterKey, CounterSnapshot};
use serde::{Deserialize, Serialize};

/// Top-level versioned status payload returned by IPC `get_status`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StatusReportV1 {
    pub schema_version: u32, // always 1 for this struct; v2 will use a different struct
    pub budgets: Vec<BudgetEntry>,
    pub counters: Vec<CounterEntry>,
    pub mounts: Vec<MountSummary>,
    /// Cache-global counter: number of LRU-eviction candidates skipped
    /// because evicting them would have violated a per-repo reservation
    /// (B5). Populated by daemon's status assembly in T3c.
    #[serde(default)]
    pub cache_eviction_attempts_blocked_by_reservation: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BudgetEntry {
    pub host: String,
    pub auth_kind: String, // "anonymous" | "pat:ghp_xxxxxxxx" | "github_app:12345"
    pub resource_class: String, // "core" | "search" | "code_search" | "graphql" | "other:audit_log"
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_at_unix: Option<u64>,
    pub throttle_active_until_unix: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CounterEntry {
    pub key: CounterKey,
    pub counters: CounterSnapshot,
}

/// Per-mount summary derived from counters; sorted by `rest_calls_total`
/// descending in the daemon-side assembly.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountSummary {
    pub mount_id: String,
    pub source: String,
    pub repo: String,
    pub commit: String,
    pub rest_calls_total: u64,
    pub bytes_total: u64,
    pub prefetch_hits: u64,
    pub cache_hit_ratio: Option<f64>, // None when (cache_hits + cache_misses) == 0
    /// Total bytes currently consumed by this mount's working set in the
    /// blob cache. Populated in T3c (B5).
    #[serde(default)]
    pub working_set_bytes: u64,
    /// Reservation registered for this mount's RepoKey. Populated in T3c (B5).
    #[serde(default)]
    pub cache_reservation_bytes: u64,
    /// Number of LFS pointer files detected during this mount's fetches.
    #[serde(default)]
    pub lfs_pointer_files: u64,
    /// Up to 3 sample paths (mount-relative) of detected LFS pointers.
    #[serde(default)]
    pub lfs_pointer_sample_paths: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip_empty_report() {
        let r = StatusReportV1 {
            schema_version: 1,
            budgets: vec![],
            counters: vec![],
            mounts: vec![],
            cache_eviction_attempts_blocked_by_reservation: 0,
        };
        let json = serde_json::to_string(&r).unwrap();
        let r2: StatusReportV1 = serde_json::from_str(&json).unwrap();
        assert_eq!(r2.schema_version, 1);
        assert!(r2.budgets.is_empty());
    }

    #[test]
    fn schema_version_field_is_explicit_and_stable() {
        let r = StatusReportV1 {
            schema_version: 1,
            budgets: vec![],
            counters: vec![],
            mounts: vec![],
            cache_eviction_attempts_blocked_by_reservation: 0,
        };
        let json = serde_json::to_string(&r).unwrap();
        assert!(json.contains("\"schema_version\":1"));
    }

    /// Older v1 JSON without the new additive fields must deserialize cleanly
    /// with defaults (Codex M5-plan-v1 #10).
    #[test]
    fn additive_fields_default_when_absent() {
        let old_json = r#"{"schema_version":1,"budgets":[],"counters":[],"mounts":[]}"#;
        let r: StatusReportV1 = serde_json::from_str(old_json).unwrap();
        assert_eq!(r.cache_eviction_attempts_blocked_by_reservation, 0);
        assert_eq!(r.schema_version, 1);
    }

    /// Legacy MountSummary payloads (without B5/B6 fields) must deserialize
    /// with defaults.
    #[test]
    fn mount_summary_defaults_for_legacy_payload() {
        let old_json = r#"{
            "mount_id":"m1","source":"github","repo":"a/b","commit":"abc",
            "rest_calls_total":0,"bytes_total":0,"prefetch_hits":0,
            "cache_hit_ratio":null
        }"#;
        let m: MountSummary = serde_json::from_str(old_json).unwrap();
        assert_eq!(m.working_set_bytes, 0);
        assert_eq!(m.cache_reservation_bytes, 0);
        assert_eq!(m.lfs_pointer_files, 0);
        assert!(m.lfs_pointer_sample_paths.is_empty());
    }
}
