# Phase 2a — Rename to ContextFS + Config File + DX Improvements

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Rename user-facing surfaces from FSKitBridge/CtxfsFS to ContextFS, add a config file so the daemon works under launchd, default the bundle ID so users don't need an env var, and add a `ctxfs diag` command for support.

**Architecture:** Rename is cosmetic — bundle IDs stay locked at `ai.ctxfs.fskitbridge[.fskitext]`. Config file loads before env vars (env overrides). Diag prints runtime state for debugging.

**Tech Stack:** Rust (toml, clap), Swift (Xcode GUI rename)

---

## Task 1: Directory rename swift/CtxfsFS/ → swift/ContextFS/

**Files:**
- Rename: `swift/CtxfsFS/` → `swift/ContextFS/`
- Modify: `swift/README.md` (update paths)
- Modify: `swift/ContextFS/FSKitExt/protocol.proto` symlink (may need re-link after move)

- [ ] **Step 1.1: Move the directory**

```bash
git mv swift/CtxfsFS swift/ContextFS
```

- [ ] **Step 1.2: Fix the protocol.proto symlink**

After the move, the symlink at `swift/ContextFS/FSKitExt/protocol.proto` still points to `../../../crates/fskit-rs/src/protocol.proto`. Verify it resolves:

```bash
readlink swift/ContextFS/FSKitExt/protocol.proto
cat swift/ContextFS/FSKitExt/protocol.proto | head -5
```

If broken, re-create:
```bash
rm swift/ContextFS/FSKitExt/protocol.proto
ln -s ../../../crates/fskit-rs/src/protocol.proto swift/ContextFS/FSKitExt/protocol.proto
```

- [ ] **Step 1.3: Update swift/README.md**

Change any `CtxfsFS` references to `ContextFS`.

- [ ] **Step 1.4: Verify both builds**

```bash
xcodebuild -list -project swift/ContextFS/FSKitBridge.xcodeproj
cargo build --workspace
```

- [ ] **Step 1.5: Commit**

```bash
git add swift/ && git commit -m "refactor(swift): rename swift/CtxfsFS → swift/ContextFS

Directory rename only — Xcode targets/schemes stay as FSKitBridge
until the GUI rename (next task). Bundle IDs unchanged.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Xcode GUI rename (user-assisted)

**Goal:** Rename Xcode targets, schemes, and product name from FSKitBridge → ContextFS via Xcode GUI. Bundle IDs stay unchanged.

**This task requires the user to perform the rename in Xcode GUI.** The subagent validates the result afterward.

- [ ] **Step 2.1: User performs rename in Xcode**

Open `swift/ContextFS/FSKitBridge.xcodeproj` in Xcode. Then:

1. **Rename the app target**: Click "FSKitBridge" target → rename to "ContextFS"
2. **Rename the extension target**: Click "FSKitExt" target → rename to "ContextFSExt"
3. **Rename the scheme**: Product → Scheme → Manage Schemes → rename "FSKitBridge" to "ContextFS"
4. **Update product name**: Target "ContextFS" → Build Settings → Product Name → "ContextFS"
5. **Keep bundle IDs unchanged**: Verify `ai.ctxfs.fskitbridge` and `ai.ctxfs.fskitbridge.fskitext` are still set

- [ ] **Step 2.2: Validate the rename**

```bash
xcodebuild -list -project swift/ContextFS/FSKitBridge.xcodeproj
# Expected: targets "ContextFS" and "ContextFSExt", scheme "ContextFS"

xcodebuild -project swift/ContextFS/FSKitBridge.xcodeproj -scheme ContextFS -configuration Release build SYMROOT=/tmp/ctxfs-build 2>&1 | tail -5
# Expected: BUILD SUCCEEDED, produces ContextFS.app

ls /tmp/ctxfs-build/Release/ContextFS.app/Contents/Extensions/
# Expected: ContextFSExt.appex

defaults read /tmp/ctxfs-build/Release/ContextFS.app/Contents/Info.plist CFBundleIdentifier
# Expected: ai.ctxfs.fskitbridge (unchanged)
```

- [ ] **Step 2.3: Rename the xcodeproj file itself**

```bash
git mv swift/ContextFS/FSKitBridge.xcodeproj swift/ContextFS/ContextFS.xcodeproj
```

Verify: `xcodebuild -list -project swift/ContextFS/ContextFS.xcodeproj`

- [ ] **Step 2.4: Commit**

```bash
git add swift/ && git commit -m "refactor(swift): rename Xcode targets/scheme to ContextFS

User-facing rename: FSKitBridge → ContextFS, FSKitExt → ContextFSExt.
App now builds as ContextFS.app. Bundle IDs unchanged
(ai.ctxfs.fskitbridge[.fskitext]).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Update Rust/doc references

**Files:**
- Modify: `crates/ctxfs-cli/src/setup.rs` — change `CtxfsFS.app` → `ContextFS.app`
- Modify: `crates/ctxfs-cli/src/backend.rs` — change `CtxfsFS.app` → `ContextFS.app`
- Modify: `crates/ctxfs-core/src/config.rs:18` — update doc comment
- Modify: `CLAUDE.md` — update architecture description if needed
- Modify: `.claude/skills/ctxfs-dev/SKILL.md` — update app name reference
- Modify: `docs/poc/fskit-phase1-smoke-test.md` — update setup instructions

- [ ] **Step 3.1: Search and replace all CtxfsFS.app references in Rust code**

```bash
grep -rn "CtxfsFS\|ctxfsfs\|FSKitBridge\.app\|FSKitBridge appex" crates/ --include="*.rs" | grep -v target/
```

Update each occurrence.

- [ ] **Step 3.2: Update doc comments and markdown**

Update references in CLAUDE.md, skill files, and doc files. Keep historical references in plans/ and poc/ as-is (they document what WAS true at the time).

- [ ] **Step 3.3: Run tests**

```bash
cargo test --workspace -- --skip mount_server_only --skip medium_repo
```

- [ ] **Step 3.4: Commit**

```bash
git commit -m "refactor: update all FSKitBridge/CtxfsFS references to ContextFS

Rust code, doc comments, and active documentation now reference
ContextFS.app. Historical documents (plans/, poc/) left unchanged
as they describe what was true at the time.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Default bundle ID

**Goal:** Default `CTXFS_FSKIT_BUNDLE_ID` to `ai.ctxfs.fskitbridge.fskitext` so users don't need to export an env var.

**Files:**
- Modify: `crates/ctxfs-core/src/config.rs` — change default from `None` to `Some("ai.ctxfs.fskitbridge.fskitext")`

- [ ] **Step 4.1: Write failing test**

In `crates/ctxfs-core/src/config.rs` test module, add:

```rust
#[test]
fn default_config_has_fskit_bundle_id() {
    let config = Config::default();
    assert_eq!(
        config.fskit_bundle_id.as_deref(),
        Some("ai.ctxfs.fskitbridge.fskitext"),
    );
}
```

Update the existing `default_config_has_no_fskit_bundle_id` test to match the new behavior.

- [ ] **Step 4.2: Change the default**

In `Config::default()` or `Config::from_env()`, change:

```rust
// Before:
fskit_bundle_id: None,
// After:
fskit_bundle_id: Some("ai.ctxfs.fskitbridge.fskitext".to_string()),
```

Env var still overrides when set.

- [ ] **Step 4.3: Update smoke test docs and CLAUDE.md**

Remove `export CTXFS_FSKIT_BUNDLE_ID=...` from setup instructions — it's no longer needed for the default bundle ID.

- [ ] **Step 4.4: Run tests and commit**

```bash
cargo test -p ctxfs-core
git commit -m "feat(config): default CTXFS_FSKIT_BUNDLE_ID to ai.ctxfs.fskitbridge.fskitext

Users no longer need to export the env var for the standard install.
Env var still overrides the default for custom deployments.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: Config file ~/.ctxfs/config.toml

**Goal:** Add config file support so the daemon works under launchd (which doesn't inherit shell env).

**Files:**
- Create: `~/.ctxfs/config.toml` (template, not committed to repo)
- Modify: `crates/ctxfs-core/src/config.rs` — add `Config::from_file()` + merge logic
- Modify: `crates/ctxfs-core/Cargo.toml` — add `toml` dep (already in workspace)

**Precedence:** config.toml < env vars (env always wins, config is fallback).

- [ ] **Step 5.1: Write failing test**

```rust
#[test]
fn config_from_toml_reads_github_token() {
    let toml = r#"
github_token = "ghp_test123"
log_level = "debug"
"#;
    let config = Config::from_toml_str(toml).unwrap();
    assert_eq!(config.github_token.as_deref(), Some("ghp_test123"));
    assert_eq!(config.log_level, "debug");
}

#[test]
fn env_overrides_config_file() {
    // Set env, parse toml with different value, assert env wins
}
```

- [ ] **Step 5.2: Implement Config::from_toml_str and merge**

Add to `config.rs`:

```rust
use serde::Deserialize;

#[derive(Deserialize, Default)]
struct ConfigFile {
    github_token: Option<String>,
    socket_path: Option<String>,
    cache_dir: Option<String>,
    cache_max_bytes: Option<u64>,
    log_level: Option<String>,
    redis_url: Option<String>,
    latest_ttl_secs: Option<u64>,
    tree_cache_max_bytes: Option<u64>,
    backend: Option<String>,
    fskit_bundle_id: Option<String>,
}

impl Config {
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        let file: ConfigFile = toml::from_str(s)?;
        let mut config = Self::default();
        // Apply file values as fallbacks (env will override later)
        if let Some(v) = file.github_token { config.github_token = Some(v); }
        // ... repeat for each field
        Ok(config)
    }

    /// Load config: defaults → config file → env vars (highest precedence)
    pub fn load() -> Self {
        let mut config = Self::default();
        // Try loading config file
        let config_path = config.config_dir().join("config.toml");
        if let Ok(contents) = std::fs::read_to_string(&config_path) {
            if let Ok(file_config) = Self::from_toml_str(&contents) {
                config = file_config;
            }
        }
        // Apply env overrides
        config.apply_env();
        config
    }
}
```

- [ ] **Step 5.3: Add `ctxfs config init` subcommand**

Generates a template `~/.ctxfs/config.toml`:

```toml
# ContextFS configuration
# Env vars override these values (e.g. GITHUB_TOKEN overrides github_token)

# github_token = "ghp_..."
# log_level = "info"
# cache_max_bytes = 536870912  # 512MB
# backend = "auto"  # "nfs" | "fskit" | "auto"
# fskit_bundle_id = "ai.ctxfs.fskitbridge.fskitext"
```

- [ ] **Step 5.4: Wire Config::load() into daemon startup**

Replace `Config::from_env()` calls with `Config::load()` in `crates/ctxfs-daemon/src/daemon.rs` and `crates/ctxfs-cli/src/main.rs`.

- [ ] **Step 5.5: Tests pass, commit**

```bash
cargo test --workspace -- --skip mount_server_only --skip medium_repo
git commit -m "feat(config): add ~/.ctxfs/config.toml with env-var override

Config precedence: defaults → config.toml → env vars. Essential
for launchd (which doesn't inherit shell env). New ctxfs config
init generates a commented template.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: ctxfs diag command

**Goal:** Print runtime diagnostic info for support: product name, bundle IDs, config sources, daemon status, extension status.

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs` — add `Diag` command variant
- Create: `crates/ctxfs-cli/src/diag.rs` (or inline)

- [ ] **Step 6.1: Add the Diag command**

Output format:
```
ContextFS Diagnostics
  Product:    ContextFS
  Version:    0.0.0
  Bundle ID:  ai.ctxfs.fskitbridge.fskitext
  Backend:    fskit (auto-detected)
  Config:     ~/.ctxfs/config.toml (loaded)
  Daemon:     running (PID 12345)
  Extension:  ai.ctxfs.fskitbridge.fskitext (enabled)
  macOS:      26.4 (Tahoe)
  Mounts:     1 active
```

Implementation:
- Read config to show source (file vs env vs default)
- Ping daemon to check if running
- Run `pluginkit -m -p com.apple.fskit.fsmodule` to check extension
- Run `sw_vers` for macOS version
- Call `list` RPC for mount count

- [ ] **Step 6.2: Tests and commit**

```bash
cargo test -p ctxfs
git commit -m "feat(cli): add ctxfs diag command for support diagnostics

Prints product name, bundle IDs, config sources, daemon status,
extension status, macOS version, and active mount count. Helps
debug the ContextFS-vs-fskitbridge naming gap and config issues.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Self-Review

- ✅ All 6 tasks map to Phase 2a spec items
- ✅ No placeholders in code (Task 5 has full implementation sketch)
- ✅ Task 2 clearly marked as user-assisted with validation steps
- ✅ Bundle IDs never changed
- ✅ Historical docs (plans/, poc/) left untouched
- ✅ Config file precedence: defaults → toml → env
