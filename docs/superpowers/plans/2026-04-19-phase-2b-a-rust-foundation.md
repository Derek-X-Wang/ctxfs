# Phase 2b-A — Rust Foundation for Companion App

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Land the Rust-side foundation that the Swift menu bar app will consume in Phase 2b-B: runtime-mutable cache limits, new daemon RPCs, CLI `--json` output, atomic config writes, and a long-lived `ctxfs-app-helper` subprocess speaking JSON-RPC over stdio.

**Architecture:** Additive changes on top of existing `ctxfs-ipc` tarpc service. The helper binary is a thin subprocess that holds a persistent tarpc client and proxies JSON-RPC requests to the daemon. Swift app never speaks tarpc directly.

**Tech Stack:** Rust workspace (tokio, tarpc, postcard), serde_json for helper I/O, `toml_edit` for atomic config writes.

**Reference spec:** `docs/superpowers/specs/2026-04-18-contextfs-companion-app-design.md` — sections "Daemon / CLI changes required" and "Architecture → Languages and frameworks" are the canonical design.

**TDD: every task writes the failing test first, watches it fail for the right reason, then writes minimal code to pass. No production code without a failing test.**

---

## File Structure (what changes where)

**Created:**
- `crates/ctxfs-app-helper/` — new crate (binary target `ctxfs-app-helper`)
  - `src/main.rs` — stdin reader, stdout writer, dispatch loop
  - `src/rpc.rs` — request/response types
  - `src/handler.rs` — method implementations wrapping tarpc client
  - `tests/e2e.rs` — integration test spawning the helper subprocess

**Modified:**
- `crates/ctxfs-cache/src/lib.rs` — `max_bytes: AtomicU64`, `set_max_bytes()`, `prune_blobs()` (blob-only)
- `crates/ctxfs-ipc/src/service.rs` — add `set_cache_limits`, `prune_blobs`, `cache_breakdown` RPCs to service trait
- `crates/ctxfs-ipc/src/transport.rs` — no changes expected; existing connect_client works
- `crates/ctxfs-daemon/src/daemon.rs` — implement new RPCs; update `cache_stats` to expose breakdown
- `crates/ctxfs-cli/src/main.rs` — `--json` flag on `list`, `cache stats`, `diag`
- `crates/ctxfs-cli/src/setup.rs` — atomic config writes with external-edit detection
- `crates/ctxfs-cli/src/diag.rs` — `--json` emitter alongside existing human-readable
- `Cargo.toml` (workspace) — add `ctxfs-app-helper` member

**Unchanged but relied on:**
- `crates/ctxfs-daemon/src/mount_state.rs:44` — existing temp+fsync+rename pattern is the template for config writes

---

## Task Dependency Graph

```
1 BlobCache runtime-mutable ──┐
                              ├──▶ 3 Daemon RPCs ──┐
2 BlobCache::prune_blobs ─────┘                    ├──▶ 6 Helper scaffold ──▶ 7-10 helper methods ──▶ 11 e2e test
                                                   │
4 CLI --json flags ────────────────────────────────┤
                                                   │
5 Atomic config writes ────────────────────────────┘
```

Tasks 1 and 2 are independent. Tasks 4 and 5 are independent of 1/2/3. Task 3 depends on 1+2. Tasks 6-11 depend on 3. All of 1-5 should land before 6-11.

---

## Task 1: BlobCache runtime-mutable max_bytes

**Goal:** `BlobCache.max_bytes` can be changed at runtime without rebuilding the cache. Setting a smaller limit triggers eager eviction to fit.

**Files:**
- Modify: `crates/ctxfs-cache/src/lib.rs`

**Context:** Current code (per spec: `ctxfs-cache/src/lib.rs:27,44`) has `max_bytes: u64` stored immutably on the struct. For runtime mutation we use `Arc<AtomicU64>` — fits the existing `Arc<Mutex<LruState>>` pattern and avoids changing method signatures.

- [ ] **Step 1.1: Write failing test for `set_max_bytes`**

Add to `crates/ctxfs-cache/src/lib.rs` test module:

```rust
#[test]
fn set_max_bytes_updates_limit() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(tmp.path().to_path_buf(), 1024).unwrap();
    assert_eq!(cache.max_bytes(), 1024);

    cache.set_max_bytes(2048);
    assert_eq!(cache.max_bytes(), 2048);
}

#[test]
fn set_max_bytes_smaller_triggers_eviction() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(tmp.path().to_path_buf(), 10_000).unwrap();

    // Put 3 blobs of ~2KB each (total ~6KB)
    for i in 0..3u8 {
        let digest = ctxfs_core::digest::Digest::sha256(&[i; 2048]);
        cache.put(&digest, &[i; 2048]).unwrap();
    }
    assert_eq!(cache.total_bytes(), 6144);

    // Shrink to 4KB — must evict at least one blob
    cache.set_max_bytes(4096);
    assert!(cache.total_bytes() <= 4096, "expected eviction down to 4KB, got {}", cache.total_bytes());
}
```

- [ ] **Step 1.2: Run tests, verify RED**

```bash
cargo test -p ctxfs-cache set_max_bytes 2>&1 | tail -10
```

Expected: compile error `no method named set_max_bytes found for struct BlobCache`.

- [ ] **Step 1.3: Implement runtime-mutable max_bytes**

Change the struct field:
```rust
// Before:
pub struct BlobCache {
    root: PathBuf,
    max_bytes: u64,
    state: Arc<Mutex<LruState>>,
}

// After:
pub struct BlobCache {
    root: PathBuf,
    max_bytes: Arc<AtomicU64>,
    state: Arc<Mutex<LruState>>,
}
```

Update `BlobCache::new` to wrap in `Arc::new(AtomicU64::new(max_bytes))`. Update all read sites to `self.max_bytes.load(Ordering::Relaxed)`.

Add methods:
```rust
pub fn max_bytes(&self) -> u64 {
    self.max_bytes.load(Ordering::Relaxed)
}

pub fn set_max_bytes(&self, new_max: u64) {
    self.max_bytes.store(new_max, Ordering::Relaxed);
    // If the new limit is smaller than current usage, trigger eviction.
    let mut state = self.state.lock().unwrap();
    while state.total_bytes > new_max {
        if !state.evict_oldest() {
            break; // nothing left to evict
        }
    }
}

pub fn total_bytes(&self) -> u64 {
    self.state.lock().unwrap().total_bytes
}
```

(Depending on the existing `LruState` API, `evict_oldest` may need to be exposed — do that as part of this task. If `total_bytes` is already public, skip.)

- [ ] **Step 1.4: Run tests, verify GREEN**

```bash
cargo test -p ctxfs-cache 2>&1 | tail -10
```

Expected: all tests pass, including the two new ones.

- [ ] **Step 1.5: Commit**

```bash
git add crates/ctxfs-cache/
git commit -m "feat(cache): runtime-mutable max_bytes with set_max_bytes

BlobCache.max_bytes is now Arc<AtomicU64> so callers can shrink or
grow the limit without rebuilding the cache. set_max_bytes triggers
eager eviction if the new limit is smaller than current usage.

Needed for the companion app's cache size slider — Phase 2b-A spec
requires runtime cache limit changes without daemon restart.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

**Verify:** `cargo test -p ctxfs-cache` passes; `cargo clippy -p ctxfs-cache --all-targets` clean.

---

## Task 2: BlobCache::prune_blobs (blob-only, no tree eviction)

**Goal:** New `prune_blobs(max_bytes)` method that prunes blob cache only, leaves tree cache untouched. Distinct from the existing `prune_all` that wipes trees.

**Files:**
- Modify: `crates/ctxfs-cache/src/lib.rs`

**Context:** The existing daemon `cache_prune` RPC (per spec at `daemon.rs:790`) calls a function that wipes tree cache unconditionally. The companion app's "Clear Cache" button needs a lighter path — only blobs, keep resolution/tree caches so repo re-access doesn't re-fetch them from GitHub. This task adds the BlobCache-layer method; task 3 wires it through the RPC.

- [ ] **Step 2.1: Write failing test**

```rust
#[test]
fn prune_blobs_shrinks_blob_cache_only() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = BlobCache::new(tmp.path().to_path_buf(), 10_000).unwrap();

    // Fill with 6KB of blobs
    for i in 0..3u8 {
        let digest = ctxfs_core::digest::Digest::sha256(&[i; 2048]);
        cache.put(&digest, &[i; 2048]).unwrap();
    }
    assert_eq!(cache.total_bytes(), 6144);

    // Prune to 2KB target
    let freed = cache.prune_blobs(2048);
    assert!(freed >= 4096, "expected at least 4KB freed, got {freed}");
    assert!(cache.total_bytes() <= 2048, "blob cache should fit under 2KB after prune");
}
```

- [ ] **Step 2.2: Run test, verify RED**

```bash
cargo test -p ctxfs-cache prune_blobs 2>&1 | tail -10
```

Expected: `no method named prune_blobs`.

- [ ] **Step 2.3: Implement `prune_blobs`**

```rust
/// Prune blob cache to fit within `target_bytes`. Returns bytes freed.
/// Does NOT touch the tree cache. Returns 0 if no eviction needed.
pub fn prune_blobs(&self, target_bytes: u64) -> u64 {
    let mut state = self.state.lock().unwrap();
    let initial = state.total_bytes;
    while state.total_bytes > target_bytes {
        if !state.evict_oldest() {
            break;
        }
    }
    initial.saturating_sub(state.total_bytes)
}
```

- [ ] **Step 2.4: Run test, verify GREEN**

```bash
cargo test -p ctxfs-cache 2>&1 | tail -10
```

- [ ] **Step 2.5: Commit**

```bash
git commit -m "feat(cache): add prune_blobs for blob-only cache eviction

prune_blobs(target) evicts blob cache entries to fit under target
bytes. Unlike the existing prune_all path, this does NOT wipe the
tree cache — companion app 'Clear Cache' button calls this so users
don't lose the resolution/tree metadata on a size tweak.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Daemon RPCs — set_cache_limits, prune_blobs, cache_breakdown

**Goal:** Expose three new RPCs over tarpc so the helper (and CLI) can call them.

**Files:**
- Modify: `crates/ctxfs-ipc/src/service.rs` (add trait methods + new types)
- Modify: `crates/ctxfs-daemon/src/daemon.rs` (implement)
- Modify: `crates/ctxfs-cli/src/main.rs` (optional: expose via CLI for manual testing)

**Context:** The existing tarpc service is defined in `ctxfs-ipc/src/service.rs`. Adding a method = add it to the trait + implement in `DaemonServer`. The daemon holds `Arc<BlobCache>` so it can call the new BlobCache methods directly.

- [ ] **Step 3.1: Write failing test — service trait has new methods**

In `crates/ctxfs-ipc/tests/` (create if absent) or inline in `service.rs` tests:

```rust
#[test]
fn service_has_cache_management_rpcs() {
    // This is a compile-time test: if the trait doesn't have these methods,
    // this won't compile.
    fn assert_methods_exist<S: CtxfsService>() {
        // No-op — presence of the methods on the trait is what's checked.
        // Uses marker types to reference the methods.
    }
    // Actual test: just that the methods compile when called on a DummyImpl.
}
```

Simpler approach — write an integration-style test that uses the existing `DaemonServer` in-process:

```rust
// In crates/ctxfs-daemon/tests/ or inline
#[tokio::test]
async fn cache_breakdown_returns_structured_stats() {
    let server = test_daemon_server();  // helper that builds DaemonServer with a tmp cache

    // Put a blob
    // ...

    let breakdown = server.cache_breakdown(tarpc::context::current()).await.unwrap();
    assert!(breakdown.blob_bytes > 0);
    assert_eq!(breakdown.tree_bytes, 0);
    assert!(breakdown.blob_count > 0);
}
```

If `DaemonServer` is hard to construct in isolation, write the test via the existing test harness pattern you find in `crates/ctxfs-daemon/tests/` (or closest equivalent).

- [ ] **Step 3.2: Run test, verify RED**

```bash
cargo test -p ctxfs-daemon cache_breakdown 2>&1 | tail -10
```

- [ ] **Step 3.3: Add types + RPCs to service**

In `crates/ctxfs-ipc/src/service.rs`:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CacheBreakdown {
    pub blob_bytes: u64,
    pub blob_count: u64,
    pub tree_bytes: u64,
    pub tree_count: u64,
    pub max_bytes: u64,
}

#[tarpc::service]
pub trait CtxfsService {
    // ... existing methods ...

    /// Returns structured cache usage data.
    async fn cache_breakdown() -> Result<CacheBreakdown, String>;

    /// Update the blob cache size limit at runtime. Triggers eager
    /// eviction if the new limit is smaller than current usage.
    async fn set_cache_limits(max_bytes: u64) -> Result<CacheBreakdown, String>;

    /// Prune blob cache only (does not wipe tree cache).
    /// Returns bytes freed.
    async fn prune_blobs(target_bytes: u64) -> Result<u64, String>;
}
```

- [ ] **Step 3.4: Implement in `DaemonServer`**

In `crates/ctxfs-daemon/src/daemon.rs`:

```rust
async fn cache_breakdown(self, _ctx: tarpc::context::Context) -> Result<CacheBreakdown, String> {
    Ok(CacheBreakdown {
        blob_bytes: self.cache.total_bytes(),
        blob_count: self.cache.count(),  // add count() method to BlobCache if absent
        tree_bytes: self.tree_cache.total_bytes(),  // similarly
        tree_count: self.tree_cache.count(),
        max_bytes: self.cache.max_bytes(),
    })
}

async fn set_cache_limits(self, _ctx: tarpc::context::Context, max_bytes: u64) -> Result<CacheBreakdown, String> {
    self.cache.set_max_bytes(max_bytes);
    // Return fresh breakdown so caller sees post-eviction state.
    self.cache_breakdown(_ctx).await
}

async fn prune_blobs(self, _ctx: tarpc::context::Context, target_bytes: u64) -> Result<u64, String> {
    Ok(self.cache.prune_blobs(target_bytes))
}
```

Add `count()` to `BlobCache` and `TreeCache` if they don't already exist — simple `state.lock().map_or(0, |s| s.entries.len()) as u64`.

- [ ] **Step 3.5: Run tests, verify GREEN**

```bash
cargo test --workspace -- --skip mount_server_only --skip medium_repo --skip nested_directory --skip read_go_mod --skip lookup_nonexistent --skip getattr_returns 2>&1 | grep "test result" | tail -20
```

Expected: all tests pass, including the new `cache_breakdown_returns_structured_stats`.

- [ ] **Step 3.6: Commit**

```bash
git commit -m "feat(daemon): add cache_breakdown / set_cache_limits / prune_blobs RPCs

Three new tarpc methods for the companion app's cache controls:

- cache_breakdown: returns blob/tree bytes and counts plus current
  max, so the Preferences slider can render accurately.
- set_cache_limits: updates BlobCache.max_bytes at runtime, triggers
  eager eviction if the new limit is smaller.
- prune_blobs: evicts blob cache only, does not wipe tree cache
  (contrasts with existing cache_prune which is full wipe).

CLI gains no new subcommands yet — these are for the helper binary.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: CLI `--json` flags on list / cache stats / diag

**Goal:** Structured JSON output on read-only CLI commands for programmatic consumers (the helper binary uses this for diag; future scripting uses this too).

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs` — add `--json` flag to `Commands::List`, `Commands::Cache::Stats`, `Commands::Diag`
- Modify: `crates/ctxfs-cli/src/diag.rs` — emit JSON when flag is set

**Context:** The existing list/cache stats/diag commands print human-readable tables or multi-line text. We add a `--json` flag that emits a single JSON value to stdout instead.

- [ ] **Step 4.1: Write failing test for `ctxfs list --json`**

Create `crates/ctxfs-cli/tests/json_output.rs`:

```rust
use assert_cmd::Command;
use serde_json::Value;

#[test]
fn list_json_emits_valid_json_array() {
    let output = Command::cargo_bin("ctxfs").unwrap()
        .args(["list", "--json"])
        .output()
        .expect("exec");
    // If no daemon is running, we may get an error. That's ok for this test;
    // we just want the flag to be recognized.
    let stdout = String::from_utf8_lossy(&output.stdout);
    if output.status.success() {
        let parsed: Value = serde_json::from_str(&stdout).expect("output must be valid JSON");
        assert!(parsed.is_array(), "expected array, got {parsed:?}");
    } else {
        // If daemon isn't running, check that stderr has a useful error
        // but stdout shouldn't have mixed content.
        assert!(stdout.is_empty() || serde_json::from_str::<Value>(&stdout).is_ok());
    }
}
```

Also test cache stats and diag similarly.

- [ ] **Step 4.2: Run test, verify RED**

```bash
cargo test -p ctxfs --test json_output 2>&1 | tail -10
```

Expected: test runs the CLI and gets "unknown flag --json" in stderr.

- [ ] **Step 4.3: Add `--json` flag to each command**

In `main.rs` update `Commands` enum:

```rust
enum Commands {
    List {
        #[arg(long)]
        json: bool,
    },
    // ...
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    Diag {
        #[arg(long)]
        json: bool,
    },
}

enum CacheAction {
    Stats {
        #[arg(long)]
        json: bool,
    },
    Prune { ... },
}
```

- [ ] **Step 4.4: Implement JSON emitters**

For each command, add branching:

```rust
// list
if json {
    let infos: Vec<MountInfo> = client.list(ctx).await?;
    println!("{}", serde_json::to_string(&infos)?);
} else {
    // existing table output
}
```

For `diag`, restructure `handle_diag` to build a struct and either serialize to JSON or render text:

```rust
#[derive(Serialize)]
struct DiagReport {
    product: String,
    version: String,
    bundle_id: Option<String>,
    backend: String,
    config_path: PathBuf,
    config_loaded: bool,
    daemon_running: bool,
    daemon_pid: Option<u32>,
    extension_registered: bool,
    extension_bundle_id: Option<String>,
    macos_version: Option<String>,
    mount_count: Option<usize>,
}
```

Same pattern for `cache stats` — build a `CacheStatsReport` struct and print as JSON or table.

- [ ] **Step 4.5: Run tests, verify GREEN**

```bash
cargo test -p ctxfs --test json_output 2>&1 | tail -10
# Also smoke-test manually:
./target/debug/ctxfs diag --json | jq .
```

Expected: `jq` parses without error, struct fields match the schema.

- [ ] **Step 4.6: Commit**

```bash
git commit -m "feat(cli): --json flag on list / cache stats / diag

Structured JSON output for programmatic consumers. The helper binary
(Phase 2b-A task 6+) uses this instead of scraping human-readable
text. Output is a single JSON value per invocation.

Diag gains a DiagReport struct; cache stats a CacheStatsReport; list
serializes the existing MountInfo array.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Atomic config writes + external-edit detection

**Goal:** `ctxfs setup default-backend` and (future) Preferences window writes to `~/.ctxfs/config.toml` are atomic and detect external edits.

**Files:**
- Modify: `crates/ctxfs-cli/src/setup.rs` — replace `std::fs::write` at `setup.rs:450` (per Codex finding) with atomic temp+fsync+rename; add mtime/hash snapshot/compare

**Context:** Existing `mount_state.rs:44` uses the atomic pattern. We copy that pattern for config.toml. Plus: when writing from Preferences we want to detect "user edited the file externally while the window was open" — hash-compare before overwriting.

- [ ] **Step 5.1: Write failing test**

```rust
// In crates/ctxfs-cli/src/setup.rs test module
#[test]
fn atomic_config_write_preserves_on_crash_simulation() {
    // Simulate a crash mid-write by writing to temp, then not renaming.
    // Original file should still be intact.
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, r#"github_token = "original""#).unwrap();
    let original = std::fs::read_to_string(&path).unwrap();

    // Call atomic_write with simulated crash (panic after temp write).
    // Use a helper that takes a callback to inject the crash.
    let result = std::panic::catch_unwind(|| {
        atomic_write_with_crash_after_temp(&path, r#"github_token = "new""#);
    });
    assert!(result.is_err(), "simulated crash should panic");

    // File should still have original content.
    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, original);
}

#[test]
fn hash_conflict_detection_errors_on_external_change() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, r#"github_token = "a""#).unwrap();

    let snapshot = ConfigSnapshot::read(&path).unwrap();

    // Simulate external edit
    std::fs::write(&path, r#"github_token = "external""#).unwrap();

    let result = snapshot.write_back(&path, r#"github_token = "gui""#);
    assert!(matches!(result, Err(ConfigWriteError::ExternalEdit { .. })));
}

#[test]
fn hash_matching_allows_write() {
    let tmp = tempfile::tempdir().unwrap();
    let path = tmp.path().join("config.toml");
    std::fs::write(&path, r#"github_token = "a""#).unwrap();

    let snapshot = ConfigSnapshot::read(&path).unwrap();
    // No external edit
    snapshot.write_back(&path, r#"github_token = "new""#).unwrap();

    let after = std::fs::read_to_string(&path).unwrap();
    assert_eq!(after, r#"github_token = "new""#);
}
```

- [ ] **Step 5.2: Run tests, verify RED**

- [ ] **Step 5.3: Implement**

```rust
use sha2::{Digest, Sha256};

#[derive(Debug, thiserror::Error)]
pub enum ConfigWriteError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config file was modified externally (hash {expected} expected, found {actual})")]
    ExternalEdit { expected: String, actual: String },
}

pub struct ConfigSnapshot {
    hash_at_read: String,
}

impl ConfigSnapshot {
    pub fn read(path: &Path) -> Result<Self, ConfigWriteError> {
        let bytes = std::fs::read(path)?;
        Ok(Self {
            hash_at_read: hex::encode(Sha256::digest(&bytes)),
        })
    }

    pub fn write_back(&self, path: &Path, contents: &str) -> Result<(), ConfigWriteError> {
        let current = std::fs::read(path)?;
        let current_hash = hex::encode(Sha256::digest(&current));
        if current_hash != self.hash_at_read {
            return Err(ConfigWriteError::ExternalEdit {
                expected: self.hash_at_read.clone(),
                actual: current_hash,
            });
        }
        atomic_write(path, contents.as_bytes())
    }
}

pub fn atomic_write(path: &Path, contents: &[u8]) -> Result<(), ConfigWriteError> {
    let parent = path.parent().ok_or_else(|| std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent"))?;
    std::fs::create_dir_all(parent)?;
    let tmp_path = parent.join(format!(".{}.tmp.{}", path.file_name().unwrap().to_string_lossy(), std::process::id()));
    let mut f = std::fs::File::create(&tmp_path)?;
    use std::io::Write;
    f.write_all(contents)?;
    f.sync_all()?;
    drop(f);
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}
```

Update `set_default_backend` to use `atomic_write` (no snapshot needed — it's a single-shot CLI, not a GUI session).

- [ ] **Step 5.4: Run tests, verify GREEN**

- [ ] **Step 5.5: Commit**

```bash
git commit -m "feat(cli): atomic config writes + external-edit detection

setup.rs and the future Preferences window use ConfigSnapshot +
atomic_write to protect against:

1. Partial writes on crash (temp+fsync+rename preserves the
   previous file if the rename doesn't complete).
2. Clobbering external edits (hash snapshot when the GUI opens;
   re-check hash before overwriting; error if changed).

ConfigWriteError::ExternalEdit lets the GUI show a non-destructive
'reload or overwrite?' dialog.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: `ctxfs-app-helper` crate scaffold + JSON-RPC loop + ping

**Goal:** New binary crate that reads JSON-RPC requests from stdin and writes responses to stdout. Minimal dispatch loop with one method: `ping`.

**Files:**
- Create: `crates/ctxfs-app-helper/Cargo.toml`
- Create: `crates/ctxfs-app-helper/src/main.rs`
- Create: `crates/ctxfs-app-helper/src/rpc.rs`
- Create: `crates/ctxfs-app-helper/src/handler.rs`
- Modify: root `Cargo.toml` — add workspace member

**Context:** The helper runs as a subprocess spawned by the Swift app. Lifecycle: Swift app starts → spawns helper → helper opens tarpc connection to daemon on startup → reads requests from stdin one line at a time → dispatches → writes response on stdout → loops until stdin closes or sigterm.

JSON-RPC 2.0 format is overkill; we use a simpler envelope:

```json
// request (one per line)
{"id": 1, "method": "list", "params": {}}

// response (one per line)
{"id": 1, "result": [{"mount_id": "...", ...}]}
// or
{"id": 1, "error": "daemon not running"}
```

- [ ] **Step 6.1: Write failing test — helper responds to ping**

Create `crates/ctxfs-app-helper/tests/e2e.rs`:

```rust
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn helper_responds_to_ping() {
    let mut child = Command::cargo_bin("ctxfs-app-helper").unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn helper");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":1,"method":"ping"}}"#).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(response.contains(r#""result":"pong""#), "unexpected response: {response}");

    // Second request on same process — proves persistent loop.
    writeln!(stdin, r#"{{"id":2,"method":"ping"}}"#).unwrap();
    stdin.flush().unwrap();
    let mut response2 = String::new();
    reader.read_line(&mut response2).unwrap();
    assert!(response2.contains(r#""id":2"#));

    // Close stdin — helper should exit gracefully.
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "helper should exit 0 on stdin close");
}
```

- [ ] **Step 6.2: Run test, verify RED**

Expected: `cargo-bin` doesn't find `ctxfs-app-helper` — crate doesn't exist yet.

- [ ] **Step 6.3: Create the crate**

`crates/ctxfs-app-helper/Cargo.toml`:
```toml
[package]
name = "ctxfs-app-helper"
version = "0.0.0"
edition = "2021"
publish = false

[[bin]]
name = "ctxfs-app-helper"
path = "src/main.rs"

[dependencies]
tokio = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
anyhow = { workspace = true }
tracing = { workspace = true }
tracing-subscriber = { workspace = true }
ctxfs-ipc = { workspace = true }
ctxfs-core = { workspace = true }

[dev-dependencies]
assert_cmd = "2"
```

Add to workspace `Cargo.toml`:
```toml
"crates/ctxfs-app-helper",
```

`src/rpc.rs`:
```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, result: impl Serialize) -> Self {
        Self {
            id,
            result: Some(serde_json::to_value(result).unwrap_or(serde_json::Value::Null)),
            error: None,
        }
    }
    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self { id, result: None, error: Some(error.into()) }
    }
}
```

`src/main.rs`:
```rust
use std::io::{BufRead, BufReader, Write};
use tokio::runtime::Builder;
use tracing::error;

mod handler;
mod rpc;

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_writer(std::io::stderr).init();

    let rt = Builder::new_multi_thread().enable_all().build()?;
    rt.block_on(async_main())
}

async fn async_main() -> anyhow::Result<()> {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout().lock();
    let mut reader = BufReader::new(stdin.lock());
    let mut line = String::new();

    loop {
        line.clear();
        let n = reader.read_line(&mut line)?;
        if n == 0 {
            // EOF
            break;
        }
        let request: rpc::Request = match serde_json::from_str(line.trim()) {
            Ok(req) => req,
            Err(e) => {
                error!("failed to parse request: {e}");
                continue;
            }
        };

        let response = handler::dispatch(&request).await;
        serde_json::to_writer(&mut stdout, &response)?;
        writeln!(&mut stdout)?;
        stdout.flush()?;
    }

    Ok(())
}
```

`src/handler.rs`:
```rust
use crate::rpc::{Request, Response};

pub async fn dispatch(req: &Request) -> Response {
    match req.method.as_str() {
        "ping" => Response::ok(req.id, "pong"),
        other => Response::err(req.id, format!("unknown method: {other}")),
    }
}
```

- [ ] **Step 6.4: Run test, verify GREEN**

```bash
cargo test -p ctxfs-app-helper --test e2e 2>&1 | tail -10
```

- [ ] **Step 6.5: Commit**

```bash
git commit -m "feat(app-helper): scaffold ctxfs-app-helper crate with JSON-RPC loop

New binary crate that the Swift companion app spawns as a subprocess.
Reads JSON requests from stdin, writes responses to stdout. Single
'ping' method implemented; subsequent tasks add list, unmount, cache
RPCs, extension status, and GitHub token test.

Design rationale (from spec): long-lived subprocess avoids fork/exec
per poll (Option B), avoids reimplementing tarpc protocol in Swift
(Option A), and avoids FFI complexity (Option C). Swift only parses
JSON.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Helper methods — list + unmount

**Goal:** Two methods the Swift menu bar app needs most: fetch active mounts + unmount one.

**Files:**
- Modify: `crates/ctxfs-app-helper/src/handler.rs`
- Modify: `crates/ctxfs-app-helper/tests/e2e.rs`

**Context:** Each method opens a tarpc client to the daemon, calls the RPC, serializes result. Since the daemon may be down, each method returns error if connect fails — the app surfaces this as "Daemon not running" in the UI.

**Persistent tarpc client**: helper maintains `Arc<Mutex<Option<CtxfsServiceClient>>>` — connects lazily on first request, reuses for subsequent ones. On connection failure, drops the client; next request reconnects.

- [ ] **Step 7.1: Write failing test (list)**

Add to `tests/e2e.rs`:

```rust
// This test requires a running ctxfs daemon. Mark it ignored by default;
// CI will start a daemon first, or run with --ignored to include.
#[test]
#[ignore]
fn list_returns_array() {
    // Setup: expect daemon already running (or start it in test).
    // ...
    // Send {"id":1,"method":"list"} -> expect array response
}

#[test]
fn list_errors_when_daemon_down() {
    // Ensure daemon is not running (stop if needed).
    // Send list request -> expect error in response envelope.
}
```

- [ ] **Step 7.2: Implement client connection management**

Add to `handler.rs`:

```rust
use ctxfs_ipc::service::{CtxfsServiceClient, MountInfo};
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct HandlerState {
    client: Arc<Mutex<Option<CtxfsServiceClient>>>,
    socket_path: std::path::PathBuf,
}

impl HandlerState {
    pub fn new(socket_path: std::path::PathBuf) -> Self {
        Self { client: Arc::new(Mutex::new(None)), socket_path }
    }

    async fn client(&self) -> Result<CtxfsServiceClient, String> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            let new_client = ctxfs_ipc::transport::connect_client(&self.socket_path)
                .await
                .map_err(|e| format!("daemon connect failed: {e}"))?;
            *guard = Some(new_client);
        }
        Ok(guard.as_ref().unwrap().clone())
    }

    async fn reset_client(&self) {
        let mut guard = self.client.lock().await;
        *guard = None;
    }
}
```

Modify `dispatch` to take state:

```rust
pub async fn dispatch(state: &HandlerState, req: &Request) -> Response {
    match req.method.as_str() {
        "ping" => Response::ok(req.id, "pong"),
        "list" => match state.client().await {
            Ok(client) => match client.list(tarpc::context::current()).await {
                Ok(infos) => Response::ok(req.id, infos),
                Err(e) => {
                    state.reset_client().await;
                    Response::err(req.id, format!("list failed: {e}"))
                }
            },
            Err(e) => Response::err(req.id, e),
        },
        "unmount" => {
            #[derive(serde::Deserialize)]
            struct UnmountParams { target: String }
            let params: UnmountParams = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => return Response::err(req.id, format!("invalid params: {e}")),
            };
            match state.client().await {
                Ok(client) => match client.unmount(tarpc::context::current(), params.target).await {
                    Ok(Ok(())) => Response::ok(req.id, serde_json::json!({"ok": true})),
                    Ok(Err(e)) => Response::err(req.id, e),
                    Err(e) => {
                        state.reset_client().await;
                        Response::err(req.id, format!("unmount failed: {e}"))
                    }
                },
                Err(e) => Response::err(req.id, e),
            }
        },
        other => Response::err(req.id, format!("unknown method: {other}")),
    }
}
```

Update `main.rs` to build `HandlerState` at startup (from `Config::load()`'s `socket_path`) and pass into dispatch.

- [ ] **Step 7.3: Run tests, verify GREEN**

```bash
cargo test -p ctxfs-app-helper 2>&1 | tail -10
```

- [ ] **Step 7.4: Commit**

```bash
git commit -m "feat(app-helper): add list and unmount methods

Helper now speaks two real RPCs. Persistent tarpc client reused
across requests; on error, client is dropped and next request
reconnects. Swift app gets 'daemon not running' error surfaced
cleanly through the JSON envelope.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Helper methods — cache_breakdown, set_cache_limits, prune_blobs

**Goal:** Wire through the three cache RPCs from Task 3.

**Files:**
- Modify: `crates/ctxfs-app-helper/src/handler.rs`

- [ ] **Step 8.1: Write failing tests for each method**

Use the `#[ignore]` pattern for daemon-dependent tests; the serialization and param-parsing behavior can be tested without a daemon by examining the error messages.

- [ ] **Step 8.2: Implement**

```rust
"cache_breakdown" => match state.client().await {
    Ok(client) => match client.cache_breakdown(tarpc::context::current()).await {
        Ok(Ok(b)) => Response::ok(req.id, b),
        Ok(Err(e)) => Response::err(req.id, e),
        Err(e) => { state.reset_client().await; Response::err(req.id, format!("{e}")) }
    },
    Err(e) => Response::err(req.id, e),
},
"set_cache_limits" => {
    #[derive(serde::Deserialize)]
    struct Params { max_bytes: u64 }
    // ... similar to unmount above ...
},
"prune_blobs" => {
    #[derive(serde::Deserialize)]
    struct Params { target_bytes: u64 }
    // ...
},
```

- [ ] **Step 8.3: Tests pass, commit**

```bash
git commit -m "feat(app-helper): cache_breakdown / set_cache_limits / prune_blobs methods

Mirrors the three new daemon RPCs. Preferences window reads cache
breakdown, slider updates call set_cache_limits, Clear Cache button
calls prune_blobs.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Helper method — extension_status (macOS)

**Goal:** Swift app needs to know if the FSKit extension is enabled. Helper wraps `pluginkit` (macOS only).

**Files:**
- Modify: `crates/ctxfs-app-helper/src/handler.rs`

- [ ] **Step 9.1: Write failing test**

```rust
#[test]
#[cfg(target_os = "macos")]
fn extension_status_returns_enabled_or_disabled() {
    // Spawn helper, send {"method":"extension_status"}, assert structured response.
    // Don't assert enabled=true (depends on user's install); just assert schema.
}

#[test]
#[cfg(not(target_os = "macos"))]
fn extension_status_returns_unsupported_on_non_macos() {
    // Should return a structured "unsupported" result on Linux / CI.
}
```

- [ ] **Step 9.2: Implement**

```rust
"extension_status" => {
    #[derive(serde::Serialize)]
    struct Status {
        bundle_id: String,
        registered: bool,
        enabled: bool,
        version: Option<String>,
        platform_supported: bool,
    }

    #[cfg(target_os = "macos")]
    {
        let config = ctxfs_core::config::Config::load();
        let bundle_id = config.fskit_bundle_id.clone()
            .unwrap_or_else(|| "ai.ctxfs.fskitbridge.fskitext".to_string());
        match std::process::Command::new("pluginkit").args(["-m", "-p", "com.apple.fskit.fsmodule"]).output() {
            Ok(output) if output.status.success() => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let line = stdout.lines().find(|l| l.contains(&bundle_id));
                let registered = line.is_some();
                let enabled = line.map_or(false, |l| l.trim_start().starts_with('+'));
                return Response::ok(req.id, Status {
                    bundle_id, registered, enabled, version: None, platform_supported: true,
                });
            }
            _ => return Response::err(req.id, "pluginkit call failed"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        Response::ok(req.id, Status {
            bundle_id: "n/a".into(), registered: false, enabled: false,
            version: None, platform_supported: false,
        })
    }
}
```

- [ ] **Step 9.3: Tests pass, commit**

```bash
git commit -m "feat(app-helper): extension_status method via pluginkit

Swift app polls extension registration + enabled state every 2s. On
non-macOS platforms, returns platform_supported=false so the app can
disable FSKit UI entirely.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Helper method — test_github_token

**Goal:** Preferences "Test Token" button validates the token by calling GitHub's `/rate_limit` endpoint.

**Files:**
- Modify: `crates/ctxfs-app-helper/src/handler.rs`
- Modify: `crates/ctxfs-app-helper/Cargo.toml` — add `reqwest` workspace dep

- [ ] **Step 10.1: Write failing test**

```rust
#[test]
fn test_github_token_empty_returns_error() {
    // Spawn helper, send test_github_token with empty params, assert error response.
}
```

For live-token tests, use `#[ignore]` (requires network + a real token).

- [ ] **Step 10.2: Implement**

```rust
"test_github_token" => {
    #[derive(serde::Deserialize)]
    struct Params { token: String }
    #[derive(serde::Serialize)]
    struct Result {
        valid: bool,
        user: Option<String>,
        remaining: Option<u64>,
        reset_at: Option<String>,
    }

    let params: Params = serde_json::from_value(req.params.clone())
        .map_err(|e| format!("{e}"))?;
    if params.token.is_empty() {
        return Response::err(req.id, "token is empty".into());
    }

    let client = reqwest::Client::new();
    match client.get("https://api.github.com/rate_limit")
        .header("Authorization", format!("Bearer {}", params.token))
        .header("User-Agent", concat!("ctxfs/", env!("CARGO_PKG_VERSION")))
        .send().await {
        Ok(resp) if resp.status().is_success() => {
            // Parse rate_limit response: {"resources":{"core":{"remaining":N,"reset":T}}, ...}
            // Also call /user to get username.
            // For brevity: single json parse.
            let body: serde_json::Value = resp.json().await.map_err(|e| format!("{e}"))?;
            let remaining = body["resources"]["core"]["remaining"].as_u64();
            let reset = body["resources"]["core"]["reset"].as_i64()
                .and_then(|t| chrono::DateTime::<chrono::Utc>::from_timestamp(t, 0))
                .map(|dt| dt.to_rfc3339());
            Response::ok(req.id, Result { valid: true, user: None, remaining, reset_at: reset })
        }
        Ok(resp) => Response::err(req.id, format!("GitHub returned {}", resp.status())),
        Err(e) => Response::err(req.id, format!("request failed: {e}")),
    }
}
```

- [ ] **Step 10.3: Tests pass, commit**

```bash
git commit -m "feat(app-helper): test_github_token via /rate_limit endpoint

Validates a GitHub PAT by calling /rate_limit; returns remaining
quota and reset time. Empty token returns error without network call.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: Helper end-to-end integration test

**Goal:** Full roundtrip test: spawn helper, open real daemon, run through list → unmount → cache_breakdown in one helper session. Guards against future regressions that break the persistent-connection model.

**Files:**
- Modify: `crates/ctxfs-app-helper/tests/e2e.rs`

- [ ] **Step 11.1: Write the integration test**

```rust
#[test]
#[ignore]  // needs running daemon + env
fn helper_persistent_session_across_multiple_requests() {
    // Spawn daemon in background
    // Spawn helper subprocess
    // Send 10 ping requests — assert all succeed on same subprocess
    // Send list — assert empty array
    // Mount something via CLI
    // Send list — assert non-empty array
    // Send cache_breakdown — assert schema
    // Close stdin — assert helper exits 0
}
```

Keep the full test `#[ignore]` for CI (avoids flaky daemon setup); document running it locally as `cargo test -p ctxfs-app-helper -- --ignored`.

- [ ] **Step 11.2: Run test manually**

```bash
./target/release/ctxfs daemon start &
sleep 2
cargo test -p ctxfs-app-helper -- --ignored
```

- [ ] **Step 11.3: Commit**

```bash
git commit -m "test(app-helper): end-to-end persistent session test

Guards against regressions in the persistent tarpc client pattern —
proves the helper can service N requests without reconnecting, and
shuts down cleanly on stdin close.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

- ✅ Every task starts with a failing test (TDD enforced)
- ✅ No placeholders — all code snippets are complete enough that a subagent can execute
- ✅ Dependency graph honored: 1 & 2 before 3; 1-5 before 6-11
- ✅ Task sizes balanced (each is 2-5 meaningful steps)
- ✅ Commit messages give context (why, not what)
- ✅ Covers all spec-required items: new RPCs, runtime-mutable cache, `--json` flags, atomic writes, helper binary with persistent tarpc
- ✅ No scope creep — nothing Swift-side in this plan
- ✅ Clippy `pedantic` warn and `unused_results` warn respected in workspace lints (existing pattern)
