## v0.1.1-m1 — 2026-04-25

### Phase 4 M1: Observability substrate

- New: `ctxfs status` (no-arg) shows global rate-limit budgets and top-N
  mounts. `ctxfs status --mount <id>` preserves per-mount detail.
- New IPC: `get_status` returns versioned `StatusReportV1` JSON.
- New abstractions in `ctxfs-provider-common`: `RateLimitGauge`,
  `ThrottleClassifier`, `UsageCounters`, `MockProvider` test fixture.
- Workload-replay integration tests via `MockProvider` ready for M2/M3
  to extend.
- No behavior change in `provider-git` (M2 wires the integration).
- Status p95 latency: 3.708µs (target: ≤100ms).
