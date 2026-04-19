# ContextFS Companion App — Design Spec

**Date**: 2026-04-18
**Status**: Design validated via brainstorming session 2026-04-18.
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
- *Start at login* (toggle) — installs/removes launchd login item
- *Default backend* (dropdown: Auto / FSKit / NFS) — writes `backend = ...` to `~/.ctxfs/config.toml`

**Authentication**
- *GitHub Personal Access Token* (secure text field) — writes `github_token = ...` to config.toml
- *Test Token* button — calls GitHub API to validate, shows rate limit remaining on success

**Cache**
- *Maximum size* (slider, 256MB–8GB) + "Currently using XXX MB" display
- *Clear Cache* button (confirmation dialog) — calls daemon RPC to wipe cache

**Footer**
- Link: "Open config.toml in editor…" — opens `~/.ctxfs/config.toml` in the default text editor for power users to edit log_level, redis_url, TTLs, etc.

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

- **Swift + SwiftUI** (menu bar app via `MenuBarExtra` for macOS 13+)
- **Existing tarpc client** — either:
  - **Option 1**: link the Rust IPC client as a C-ABI library and call from Swift
  - **Option 2**: rewrite the IPC client in Swift using the proto/tarpc wire format
  - **Option 3**: ship a tiny helper binary (`ctxfs-app-helper`) that does IPC and exposes a Swift-friendly shell interface

**Recommended**: **Option 3**. Simplest to build, no FFI complexity, leverages existing CLI code. The helper is essentially `ctxfs list --json` + `ctxfs unmount <id>` + new `ctxfs cache-stats --json` wrapped in structured JSON output.

### Bundling

- The app binary and the FSKit appex both live inside `ContextFS.app`.
- The `ctxfs` CLI **does NOT** live inside the app (Homebrew formula owns the CLI — see spec `docs/superpowers/specs/2026-04-11-fskit-backend-design.md` Phase 2 section).
- Homebrew distribution:
  - `brew install --cask contextfs` → app (includes the appex + menu bar UI)
  - `brew install ctxfs` → CLI
  - Both can be installed together; they share the daemon via UDS socket

### Launchd agent

`ContextFS.app` installs `~/Library/LaunchAgents/ai.ctxfs.daemon.plist`:

```xml
<plist>
  <dict>
    <key>Label</key><string>ai.ctxfs.daemon</string>
    <key>ProgramArguments</key>
    <array>
      <string>/usr/local/bin/ctxfs</string>
      <string>daemon</string>
      <string>--foreground</string>
    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><true/>
    <key>EnvironmentVariables</key>
    <dict>
      <key>CTXFS_CONFIG_FILE</key>
      <string>/Users/&lt;user&gt;/.ctxfs/config.toml</string>
    </dict>
  </dict>
</plist>
```

Daemon reads config from the file (Phase 2a already added this support), not from shell env — critical for launchd.

---

## What's In Scope

For the Phase 2b implementation plan:

- Swift menu bar app (MenuBarExtra with dropdown)
- Preferences window (SwiftUI form)
- Onboarding wizard (SwiftUI modal, Welcome + Quick/Custom paths)
- Status icon with overlay dot states
- Launchd plist install/uninstall
- Helper binary `ctxfs-app-helper` with JSON output (or Option 1/2 if we revisit)
- Config.toml read/write from Swift
- `pluginkit` polling for extension state
- Deep-link to System Settings File System Extensions pane

Out of scope (deferred):

- Mount creation from the app (scope decision in brainstorming)
- Cache browser / blob viewer
- Per-source view (activity graphs, read rates)
- Notifications (may sneak in if it's <2 hours work)
- Login window / "Sign in to ContextFS" (no cloud service exists)
- Spotlight indexing toggles

---

## Success Criteria

- [ ] First-launch wizard gets a non-technical user from zero to a working mount via `ctxfs mount` CLI in under 2 minutes
- [ ] Menu bar status dot accurately reflects state within 500ms of the underlying change
- [ ] Preferences window writes to `~/.ctxfs/config.toml` and the change takes effect without restarting the daemon for reloadable settings (cache size, log level, token)
- [ ] Cache-size change does not lose cached blobs unless the new size is smaller than current usage
- [ ] Extension enable/disable state is polled from `pluginkit`, not cached — so flipping the System Settings toggle is reflected in the app within 2s
- [ ] App binary works on macOS 14+ (MenuBarExtra min version) and FSKit features gracefully degrade on pre-26
