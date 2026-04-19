# ContextFS Companion App — Design Spec

**Date**: 2026-04-18 (revised 2026-04-18 after Codex review)
**Status**: Design validated via brainstorming session 2026-04-18. Codex review 2026-04-19 applied — 9 findings addressed.
**Scope**: Transform `ContextFS.app` from a stub host-app bundle for the FSKit extension into a full macOS menu bar companion app — status UI, extension management, and a minimal Preferences window.

## Motivation

After Phase 2a/2c, `ContextFS.app` exists but does nothing except host the `ContextFSExt.appex`. Users interact with ctxfs entirely via CLI. For a broader audience, we need a companion app that:

1. **Guides first-launch setup** — the two mandatory blockers (enable FSKit extension, add GitHub token) are invisible today unless the user reads docs.
2. **Makes status visible** — "is my mount healthy? is the daemon running?" should be answerable with one glance at the menu bar.
3. **Lowers the barrier for non-CLI users** — clicking a mount to unmount, editing cache size via a slider, etc.
4. **Matches expectations set by Docker Desktop and Tailscale** — power tools that ship a menu bar companion because it makes the product legible.

The CLI remains the primary interface for developers; the app is a *companion*, not a dashboard.

---

## Core Architectural Decisions

### 1. Daemon lifecycle — hybrid

- The daemon runs as a **launchd agent** (`~/Library/LaunchAgents/ai.ctxfs.daemon.plist`), persistent across app quits and logins.
- `ContextFS.app` can **start/stop the daemon** via launchd APIs (menu item: "Quit ContextFS Daemon" / "Start ContextFS Daemon").
- The `ctxfs` CLI **auto-starts the daemon** on first mount command if it's not already running.
- Quitting the app does NOT stop the daemon — mounts survive app quits and user logouts.

**Why**: Tailscale-like robustness (persistent service) + Docker-like "the app can manage it" affordance. Developers who never open the app still get a working ctxfs.

### 2. UI surface — menu bar + minimal Preferences window

- **Menu bar icon** (always visible when app is running) — glyph with status dot.
- **Dropdown menu** (click icon) — live mount list + actions + quit/preferences.
- **Preferences window** (small modal) — 5 settings only. Everything else stays in `~/.ctxfs/config.toml`.
- **Onboarding wizard** (first launch only) — modal with Quick/Custom path fork.

No dock icon, no main window. Pure menu bar app.

### 3. Mount management scope — observability only

The app displays and unmounts, but **does not mount new things**. Mounting remains a CLI action (`ctxfs mount <source> -p <path>`), because:
- Mount points are almost always project-relative paths (`./deps/react`) — terminal/IDE context is where they're chosen.
- A mount button without mount-point context is confusing.
- Scope creep — discovery UIs (search npm, browse crates.io) are a whole product direction, not companion-app territory.

Future: we may add "Mount to `~/ctxfs-scratch/<name>`" for ad-hoc browsing, but it's not shipping in Phase 2.

---

## UI Specs

### Menu bar icon

- **Glyph**: monochrome template image (Apple standard) — a simple 3-line filesystem icon. White in dark mode, black in light mode.
- **Status overlay**: 6px colored dot at the bottom-right corner.

| State | Dot color | Meaning |
|---|---|---|
| Idle | none (no dot) | Daemon running, no active mounts |
| Active | green (#38c172) | Daemon running, ≥1 active mount |
| Setup needed | amber (#f59e0b) | Extension disabled, no GitHub token, or similar fixable issue |
| Error | red (#ef4444) | Daemon crashed, mount failed, or unrecoverable state |
| Busy | blue spinner (#0ea5e9) | Mount/unmount in progress |

Tooltip: "ContextFS — 2 mounts active" / "ContextFS — Setup required" / etc.

### Menu bar dropdown (click icon)

Layout (top to bottom):

```
ContextFS                    ● (status dot)
2 mounts · fskit

━━━ Active Mounts ━━━
✓ react 19.1.0
  ./deps/react
✓ tokio 1.40.0
  ./deps/tokio

━━━ Actions ━━━
Unmount All
Diagnostics…

───────
Preferences…
Quit ContextFS
```

**Mount row interactions**:
- Hover → highlighted
- Click → unmount this mount (confirmation if >0 open file handles — out of scope for v1, just unmount)
- Right-click → context menu: "Unmount", "Open in Finder", "Copy mount path"

**Empty state** (no active mounts):
```
ContextFS                    ● (green or amber)
No active mounts

Use 'ctxfs mount …' in your terminal.
```

**Error state** (daemon not running, for example):
```
ContextFS                    ● (red)
Daemon not running

[Start Daemon]  (button)

───────
Preferences…
Quit ContextFS
```

### Preferences window (modal, ~560px wide)

**5 settings, three sections:**

**General**
- *Launch ContextFS at login* (toggle) — uses `SMAppService.mainApp` (macOS 13+) to register/unregister the app as a login item. **This only controls the menu bar app, NOT the daemon**. The daemon autostart is the separate launchd agent described above.
- *Default backend* (dropdown: Auto / FSKit / NFS) — writes `backend = ...` to `~/.ctxfs/config.toml`

**Authentication**
- *GitHub Personal Access Token* (secure text field) — writes `github_token = ...` to config.toml
- *Test Token* button — calls GitHub API to validate, shows rate limit remaining on success

**Cache**
- *Maximum size* (slider, 256MB–8GB) + "Currently using XXX MB" display
- *Clear Cache* button (confirmation dialog) — calls daemon `prune_blobs` RPC (see "Daemon changes" below)

**Footer**
- Link: "Open config.toml in editor…" — opens `~/.ctxfs/config.toml` in the default text editor for power users to edit log_level, redis_url, TTLs, etc.

**Config file write safety** (addresses Codex P1 #6):
- Preferences-driven writes use **atomic temp+fsync+rename** (same pattern as `mount_state.rs:44`), not plain `fs::write`, to prevent partial writes on crash.
- Before writing, the app **re-reads config.toml and hashes it**, compares to the hash taken when the Preferences window opened. If mismatched (user edited the file externally), show a non-destructive dialog: *"config.toml was modified outside ContextFS. Reload and lose your pending changes, or overwrite?"*
- `toml_edit` dep lets us preserve comments and unknown keys during GUI-originated writes.

### Onboarding wizard (first launch)

**Step 0 — Welcome (the fork)**
- Logo + "Welcome to ContextFS" + tagline
- Two large buttons:
  - **Quick Setup** (recommended) → 2-step path
  - **Custom Setup** → 5-step path
- "Skip for now" link (goes straight to menu bar)

**Quick path** (2 user-visible steps + auto-detected extension-poll):
1. Welcome (above)
2. Enable FSKit extension — button deep-links to System Settings, app polls `pluginkit` state, auto-advances when enabled
3. GitHub token — paste field, "Test" button, "Skip" link
4. "You're all set!"

**Custom path** (5 user-visible steps):
1. Welcome
2. Default backend (dropdown)
3. Enable FSKit extension (same as Quick)
4. GitHub token (optional)
5. Cache location + size
6. Notifications (mount/unmount toasts on/off)
7. Start at login (toggle)
8. "You're all set!"

Both paths persist progress — if the user quits mid-wizard, they resume on next launch unless they clicked "Skip for now" (in which case, no wizard until they explicitly click "Setup…" from the menu bar).

---

## Architecture

### Process model

```
┌─────────────────┐         ┌────────────────────┐
│  ContextFS.app  │   UDS   │  ctxfs daemon      │
│  (Swift/SwiftUI)│◄───────►│  (Rust, launchd)   │
└────────┬────────┘         └──────────┬─────────┘
         │                              │
         │ XPC to fskitd                │ spawns FSKit
         ▼                              ▼ mounts
┌─────────────────┐         ┌────────────────────┐
│ ContextFSExt    │         │   /Volumes/ctxfs/  │
│  .appex         │   TCP   │   <slug>           │
│ (Swift/NIO)     │◄────────┘                    │
└─────────────────┘                              │
```

- `ContextFS.app` (the menu bar app) talks to the ctxfs daemon over the **existing UDS socket** at `~/.ctxfs/ctxfs.sock` using the existing tarpc service.
- **No new protocol needed** for the app — it uses the same RPCs as `ctxfs` CLI: `list`, `unmount`, `ping`, `cache_stats`, `cache_prune`.
- The app may add **one new RPC** for "cache size breakdown" if the existing `cache_stats` doesn't include enough data for the slider + current-usage display.

### Languages and frameworks

- **Swift + SwiftUI** (menu bar app via `MenuBarExtra` for macOS 14+)
- **Host app is NOT sandboxed** (`ENABLE_APP_SANDBOX = NO`). Rationale: the app writes `~/Library/LaunchAgents/`, edits `~/.ctxfs/config.toml`, and spawns the helper subprocess. These operations are incompatible with App Sandbox. This matches Docker Desktop and other Homebrew-distributed developer utilities. The **appex remains sandboxed** (it only needs filesystem + network entitlements already granted).
- **IPC via long-lived helper subprocess**: `ctxfs-app-helper` binary speaks JSON-RPC over its stdin/stdout. The app spawns it once at launch, maintains a persistent tarpc connection inside the helper. Request latency: single pipe write + UDS round-trip (well under 500ms). No fork/exec per poll. The helper is a thin wrapper around the existing `ctxfs-ipc` client; no duplicated protocol work.
- The CLI also gets a `--json` flag on read-only commands (`ctxfs list --json`, `ctxfs cache stats --json`, `ctxfs diag --json`) so the helper can reuse CLI logic where the output format is already structured.

### Bundling

The app bundles the **full `ctxfs` CLI** inside its resources — same binary name as the Homebrew formula, built from the same source at the same tag:

```
ContextFS.app/
  Contents/
    MacOS/
      ContextFS           # Swift menu bar app (SwiftUI + MenuBarExtra)
      ctxfs               # Full Rust CLI (identical to formula build)
      ctxfs-app-helper    # Long-lived IPC helper (JSON-RPC over stdio)
    Extensions/
      ContextFSExt.appex  # FSKit extension (sandboxed)
```

**Why bundle `ctxfs` (same name, not `ctxfs-mac`)**: AI agents and human users invoke `ctxfs mount …` regardless of install method. One command name = one mental model. No documentation ambiguity.

**Homebrew distribution**:

| Package | Target | Contents | `/usr/local/bin/ctxfs` source |
|---|---|---|---|
| `brew install --cask contextfs` | macOS (FSKit) | App + appex + bundled CLI | symlink → `ContextFS.app/Contents/MacOS/ctxfs` |
| `brew install ctxfs` | macOS (NFS-only) / Linux / CI | CLI binary only | Homebrew-installed binary |

Both can be installed. Since `conflicts_with formula:` in cask DSL is not enforced (per Homebrew 5.1+), the resolution is:

- **Only one is installed**: that one owns `/usr/local/bin/ctxfs` (or `/opt/homebrew/bin/ctxfs` on Apple Silicon). Works without friction.
- **Both installed**: Homebrew's standard symlink collision behavior applies (last-installed typically wins with a warning). The cask's `caveats` text explicitly says: *"The formula `ctxfs` is redundant when ContextFS.app is installed. Use one or the other."* Formula's caveats say the reverse. Since both are built from the same monorepo tag, their `ctxfs` binaries are byte-identical — no behavior drift even if both exist.

### Launchd agent

`ContextFS.app` installs `~/Library/LaunchAgents/ai.ctxfs.daemon.plist` pointing at the **app-bundled `ctxfs` binary** (absolute path). This is independent of Homebrew install state — the daemon auto-starts whether or not the formula is installed:

```xml
<plist>
  <dict>
    <key>Label</key><string>ai.ctxfs.daemon</string>
    <key>ProgramArguments</key>
    <array>
      <string>/Applications/ContextFS.app/Contents/MacOS/ctxfs</string>
      <string>daemon</string>
      <string>start</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>StandardOutPath</key><string>/Users/&lt;user&gt;/Library/Logs/ContextFS/daemon.log</string>
    <key>StandardErrorPath</key><string>/Users/&lt;user&gt;/Library/Logs/ContextFS/daemon.err</string>
  </dict>
</plist>
```

Notes:
- Command is `ctxfs daemon start` — matches the existing CLI subcommand (no non-existent `--foreground` flag).
- No `CTXFS_CONFIG_FILE` env var: the daemon already reads `~/.ctxfs/config.toml` by default via `Config::load()`.
- If the user installed the app to `~/Applications/` instead of `/Applications/`, the plist path is resolved at install time and written correspondingly.
- **Daemon autostart is separate from "Start ContextFS.app at login"**. The daemon LaunchAgent keeps the daemon running regardless of whether the menu bar app is launched. The app uses **`SMAppService.mainApp`** (macOS 13+) to optionally launch itself at login — a distinct toggle, and arguably unnecessary since the daemon persists without the app.

---

## Daemon / CLI changes required

These must land before the Swift app can consume them:

1. **New daemon RPCs**:
   - `set_cache_limits { max_bytes: u64 }` — updates `BlobCache.max_bytes` at runtime (currently immutable after construction per `ctxfs-cache/src/lib.rs:27,44`). Triggers eager eviction if new limit < current usage; no-op otherwise.
   - `prune_blobs` — prune blob cache only (do NOT clear trees). The existing `cache_prune` wipes trees unconditionally (`daemon.rs:790`) which is too aggressive for the "Clear Cache" button; add a blob-only variant and keep the full prune for CLI power users.
   - `cache_breakdown` — returns blob-bytes, tree-bytes, blob-count, tree-count as structured values, so the Preferences slider can show "Currently using 247 MB" accurately.
2. **CLI `--json` flags**: `ctxfs list --json`, `ctxfs cache stats --json`, `ctxfs diag --json`. Helper binary uses these to avoid rescreenscraping text output.
3. **Atomic config writes** in `ctxfs-cli/src/setup.rs:450`: swap `std::fs::write` for temp+fsync+rename.
4. **`BlobCache` runtime-mutable limit**: make `max_bytes` an `Arc<AtomicU64>` or behind a Mutex so the new RPC can update it without restarting the daemon.

## Platform matrix (addresses Codex P2 #8)

| macOS version | App launches? | FSKit backend | Notes |
|---|---|---|---|
| < 14 | ❌ Refuses to launch | N/A | MenuBarExtra requires 14+ |
| 14 – 25 | ✅ Yes | ❌ Hidden/disabled | Forces NFS backend; FSKit onboarding steps skipped; menu shows "FSKit unavailable on this macOS" |
| 26+ | ✅ Yes | ✅ Available | Full feature set |

Implementation: `#available(macOS 26, *)` gates around FSKit-specific UI (extension toggle, Enable Extension wizard step, "FSKit" default backend option).

## What's In Scope

For the Phase 2b implementation plan:

- Swift menu bar app (MenuBarExtra with dropdown, macOS 14+)
- Preferences window (SwiftUI form with 5 settings, atomic config writes + external-edit detection)
- Onboarding wizard (SwiftUI modal, Welcome + Quick/Custom paths)
- Status icon with overlay dot states (template image + color dot)
- Launchd plist install/uninstall pointing at app-bundled `ctxfs`
- `ctxfs-app-helper` — long-lived subprocess with JSON-RPC over stdio
- Config.toml read/write from Swift via `toml_edit` (preserves comments)
- `pluginkit` polling for extension state (via helper binary on a 2s cadence; not hot-path)
- Deep-link to System Settings File System Extensions pane
- Host app target: disable `ENABLE_APP_SANDBOX`; keep appex sandboxed
- Daemon RPC additions (listed above)
- CLI `--json` flag additions (listed above)
- Homebrew cask + formula with mutual `caveats` explaining coexistence
- `SMAppService.mainApp` integration for "Launch at login" toggle

Out of scope for Phase 2b (explicit NO, not "maybe"):

- **Mount creation from the app** (CLI-only; decision from brainstorming)
- **Cache browser / blob viewer**
- **Per-source view** (activity graphs, read rates) — requires new daemon metrics RPCs
- **Notifications** — not in Phase 2b. Revisit in 2d or later if users request.
- **Login window / "Sign in to ContextFS"** — no cloud service exists
- **Spotlight indexing toggles**
- **Graceful downgrade to NFS on <26 with FSKit toggle UI** — on <26, the FSKit option is simply absent; no toggle.
- **"Config.toml migration" UI** — if config schema changes in the future, handle via the CLI `ctxfs config migrate` subcommand, not the app.

---

## Success Criteria

- [ ] First-launch wizard gets a non-technical user from zero to a working mount via `ctxfs mount` CLI in under 2 minutes
- [ ] Menu bar status dot accurately reflects state within 500ms of the underlying change (helper binary holds a persistent tarpc connection + streams state changes; no per-poll fork/exec)
- [ ] Preferences window writes to `~/.ctxfs/config.toml` atomically (temp+fsync+rename) and the change takes effect without restarting the daemon for reloadable settings (cache size, log level, token). External edits detected via hash comparison on save.
- [ ] Cache-size slider change calls the new `set_cache_limits` RPC; blob eviction is triggered if and only if the new limit is smaller than current usage. Tree cache is untouched.
- [ ] "Clear Cache" button calls `prune_blobs` (not the full `cache_prune`) — does not wipe tree cache
- [ ] Extension enable/disable state is polled from `pluginkit` every 2s (via helper) — so flipping the System Settings toggle is reflected in the app within 2s
- [ ] App launches on macOS 14+. On macOS 14-25, FSKit UI is hidden; backend forced to NFS. On 26+, full FSKit feature set.
- [ ] Cask and formula both install `ctxfs` at `/usr/local/bin/ctxfs` (or Homebrew prefix). Same-tag builds produce byte-identical binaries. `caveats` explain coexistence.
- [ ] Launchd agent uses absolute app-bundled path; daemon starts successfully regardless of whether the Homebrew formula is installed.
- [ ] Host app is NOT sandboxed; appex remains sandboxed. Entitlements files reflect this.
