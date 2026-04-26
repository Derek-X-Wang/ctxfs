//! M1 exit-criterion benchmark: `ctxfs status` p95 latency over 100
//! sequential calls must be ≤ 100ms with one mounted 1k-file repo and
//! zero concurrent read load.
//!
//! This test drives `Observability::status_report()` directly (not through
//! IPC) — the expensive registry walk is what the 100ms budget tests.
//!
//! Skipped in CI (`#[ignore]`) because timing is host-dependent.
//! Run locally with:
//!   cargo test --release -p ctxfs --test status_bench -- --ignored --nocapture

use std::time::{Duration, Instant};

use ctxfs_daemon::observability::Observability;
use ctxfs_provider_common::counters::CounterKey;

#[ignore = "host-timing-dependent; run locally with `cargo test --release -p ctxfs --test status_bench -- --ignored`"]
#[test]
fn status_p95_within_100ms() {
    let obs = Observability::new();

    // Populate with a representative load: 1 mount, simulated counter activity.
    let key = CounterKey {
        source: "github".to_string(),
        repo: "foo/bar".to_string(),
        commit: "abc".to_string(),
        mount_id: "mnt-1".to_string(),
    };
    let counters = obs.counters_for(key);
    for _ in 0..1000 {
        counters.record_cache_hit();
    }

    let n = 100;
    let mut durations = Vec::with_capacity(n);
    for _ in 0..n {
        let start = Instant::now();
        let _ = obs.status_report();
        durations.push(start.elapsed());
    }

    durations.sort();
    let p95 = durations[(n * 95) / 100];
    assert!(
        p95 <= Duration::from_millis(100),
        "p95 latency {p95:?} exceeded 100ms target"
    );
    eprintln!(
        "p95: {p95:?}, p50: {:?}, max: {:?}",
        durations[n / 2],
        durations[n - 1]
    );
}
