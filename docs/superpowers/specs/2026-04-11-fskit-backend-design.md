# FSKit Backend for macOS — Design Spec

**Date**: 2026-04-11 (phases revised 2026-04-16)
**Status**: Phase 0 ✅ validated 2026-04-13. Phase 1 ✅ shipped 2026-04-14. Phase 1.5 ✅ shipped 2026-04-16. Phase 2 next.
**Reserved bundle IDs**: `ai.ctxfs.fskitbridge` (host app), `ai.ctxfs.fskitbridge.fskitext` (appex).
**Scope**: Add an FSKit-based filesystem backend for macOS 26+, eliminating the need for sudo and Full Disk Access on modern Macs. NFS remains the cross-platform fallback.

## Phase 0 Evidence (2026-04-13)

The proof of concept at `docs/poc/fskit-poc/` confirmed on macOS 26.4:

| Test | Result |
|---|---|
| Mount a synthetic (non-block-device) filesystem via FSKitBridge + fskit-rs | ✅ Works |
| `ls`, `cat`, `find`, `grep`, `stat` on mounted files | ✅ All work |
| Read latency | **2ms** per read — imperceptible |
| No sudo per mount | ✅ Confirmed |
| No Full Disk Access required | ✅ Confirmed (the huge win) |

Mount reports as `fskit` type, `mounted by <user>` (not root). See `docs/poc/fskit-poc/README.md` for gotchas encountered during Phase 0 (swift-protobuf 1.36+ required, bundle ID must be overridden from fskit-rs default).

---

## Motivation

The current NFS loopback backend works cross-platform but has two macOS UX pain points:

1. **sudo on every mount** — `mount_nfs` is a kernel operation requiring root. We mitigate with passwordless sudoers, but the setup is friction.
2. **Full Disk Access required** — macOS TCC treats NFS mounts (even loopback) as "network volumes" and blocks reads without FDA granted to the terminal app. This is the same issue affecting macFUSE, s3fs-fuse, and HuggingFace's hf-mount (see [macfuse#690](https://github.com/macfuse/macfuse/issues/690)).

Apple's FSKit framework (public since macOS 15.4, non-local volume support since macOS 26) provides a user-space filesystem API that requires neither sudo nor FDA. The filesystem runs as a sandboxed app extension — the user enables it once in System Settings, and mounts happen without privilege escalation.

---

## Architecture

### Two-Process Model

```
                                     ┌─────────────────────┐
                                     │  CtxfsFS.appex       │
                                     │  (Swift, signed)     │
                                     │                      │
                       XPC           │  FSKit VFS calls     │
              fskitd ◄──────────────►│    ↓                 │
                ↑                    │  Protobuf/TCP        │
                │                    │  localhost:PORT       │
           kernel VFS                └──────────┬───────────┘
                ↓                               │
     /Volumes/ctxfs/react-19.1.0                │ TCP
     (symlink from ./deps/react)                │
                                     ┌──────────▼───────────┐
                ┌──────────┐   UDS   │   ctxfs-daemon       │
                │ ctxfs CLI├────────►│                       │
                └──────────┘  tarpc  │  fskit-rs trait impl  │
                                     │  Provider + Cache     │
                                     │  NFS server (fallback)│
                                     └──────────────────────┘
```

### Data Flow (FSKit Read)

1. `cat ./deps/react/src/hooks.js` — kernel resolves symlink to `/Volumes/ctxfs/react-19.1.0/src/hooks.js`
2. Kernel VFS → `fskitd` → XPC → `CtxfsFS.appex` (Swift)
3. Appex serializes `read(inode, offset, size)` as Protobuf → TCP to daemon
4. Daemon's `fskit-rs` handler: check blob cache → if miss, fetch from GitHub API → return bytes
5. Bytes flow back: daemon → TCP → appex → XPC → fskitd → kernel → userland

### What Changes vs Current Architecture

| Component | Change |
|---|---|
| New crate: `ctxfs-vfs` | Shared VFS logic extracted from `ctxfs-nfs` |
| New crate: `ctxfs-fskit` | Implements `fskit_rs::Filesystem` trait over `VfsState` |
| New directory: `swift/CtxfsFS/` | Vendored FSKitBridge appex, signed with developer account |
| `ctxfs-nfs` | Becomes thin adapter over `VfsState` (refactor, no behavior change) |
| `ctxfs-daemon` | Gains `FsKitHandle`, backend dispatch in `do_mount()`, mount state persistence |
| `ctxfs-cli` | Backend detection, `--backend` flag, symlink management, setup flow updates |
| `ctxfs-core` | `Backend` enum, config file support for `default_backend` |
| `ctxfs-ipc` | `MountInfo` gains `backend`, `volume_path`, `symlink_path` fields |

### What Stays the Same

- All existing NFS code remains functional — it is the fallback
- Provider, Cache, Manifest crates unchanged
- `ctxfs mount` / `unmount` / `list` / `deps` CLI surface unchanged
- Daemon remains single source of truth for mount state
- tarpc over UDS for CLI-to-daemon IPC

---

## Workspace Layout (15 Crates)

```
crates/
  ctxfs-core/           # + Backend enum, config file parsing
  ctxfs-manifest/       # unchanged
  ctxfs-cache/          # unchanged
  ctxfs-cache-redis/    # unchanged
  ctxfs-ipc/            # + MountInfo backend/volume/symlink fields
  ctxfs-provider-common/# unchanged
  ctxfs-provider-git/   # unchanged
  ctxfs-provider-npm/   # unchanged
  ctxfs-provider-pypi/  # unchanged
  ctxfs-provider-crate/ # unchanged
  ctxfs-vfs/            # NEW — shared VFS logic
  ctxfs-nfs/            # refactored to thin adapter over ctxfs-vfs
  ctxfs-fskit/          # NEW — fskit-rs Filesystem impl
  ctxfs-daemon/         # + FsKitHandle, backend dispatch, mount persistence
  ctxfs-cli/            # + backend detection, symlinks, setup flow

swift/
  CtxfsFS/              # NEW — vendored FSKitBridge appex (Swift)
    CtxfsFS.xcodeproj/
    CtxfsFS/
      main.swift
      CtxfsFSModule.swift
      CtxfsFSVolume.swift
    CtxfsFS.appex.entitlements
```

### Dependency Graph (New Crates)

```
ctxfs-vfs (NEW)
  depends on: ctxfs-core, ctxfs-manifest, ctxfs-cache

ctxfs-nfs (refactored)
  depends on: ctxfs-vfs  (replaces direct core/manifest/cache deps)

ctxfs-fskit (NEW)
  depends on: ctxfs-vfs, fskit-rs
```

---

## Shared VFS Extraction (`ctxfs-vfs`)

The NFS and FSKit backends share ~80% of their logic. The shared core is extracted into `ctxfs-vfs`.

### `VfsState` — Protocol-Agnostic VFS

```rust
pub struct VfsState {
    provider: SharedProvider,
    cache: Arc<BlobCache>,
    snapshot: Snapshot,
    nodes: DashMap<u64, Node>,
    dir_cache: DashMap<(u64, String), u64>,
    dir_children: DashMap<u64, Vec<u64>>,
    next_id: AtomicU64,
}

impl VfsState {
    pub fn new(provider, cache, snapshot, subpath) -> Self;

    // Core operations — protocol-agnostic
    pub fn lookup(&self, parent: u64, name: &str) -> Result<(u64, NodeAttr)>;
    pub fn getattr(&self, inode: u64) -> Result<NodeAttr>;
    pub fn read(&self, inode: u64, offset: u64, size: u32) -> Result<Vec<u8>>;
    pub fn readdir(&self, inode: u64) -> Result<Vec<(u64, String, NodeType)>>;
    pub fn readlink(&self, inode: u64) -> Result<String>;
    pub fn statfs(&self) -> StatFsResult;
}

pub struct NodeAttr {
    pub size: u64,
    pub kind: NodeType,
    pub executable: bool,
}
```

### API Boundary: What Stays in the Adapters

The VFS layer is deliberately protocol-agnostic. Protocol-specific concerns stay in each adapter:

**NFS adapter (`ctxfs-nfs`) retains:**
- NFS cookie/cookieverf handling for paginated `readdir` (large directories)
- NFS3 file handle generation and mapping
- `fattr3` construction with NFS-specific UID/GID (0/0 for read-only)
- NFS3 error code translation (`NFS3ERR_ROFS`, `NFS3ERR_NOENT`, etc.)

**FSKit adapter (`ctxfs-fskit`) retains:**
- `FSItemAttributes` construction with FSKit-specific fields
- Volume metadata (display name, icon, read-only flags)
- FSKit-specific offset-based `readdir` pagination
- EROFS error translation for write operations

**Shared VFS provides:**
- Complete directory listing (adapters handle pagination/cookies)
- Inode allocation with stable IDs (both protocols need monotonic IDs, neither needs cross-mount stability)
- Lazy directory population with `DashMap` concurrency
- Blob fetching through cache → provider chain
- `NodeAttr` with protocol-agnostic fields (size, kind, executable)

This split means the "~80% shared" estimate is accurate for read-only semantics. The remaining ~20% is genuinely protocol-specific and belongs in the adapters.

---

## FSKit Backend (`ctxfs-fskit`)

### Rust Side

```rust
pub struct CtxfsFsKit {
    vfs: Arc<VfsState>,
}

impl fskit_rs::Filesystem for CtxfsFsKit {
    fn lookup(&self, parent: u64, name: &str) -> Result<Entry>;
    fn getattr(&self, inode: u64) -> Result<Attr>;
    fn read(&self, inode: u64, offset: u64, size: u32) -> Result<Vec<u8>>;
    fn readdir(&self, inode: u64, offset: u64) -> Result<Vec<DirEntry>>;
    fn readlink(&self, inode: u64) -> Result<String>;
    fn statfs(&self) -> Result<StatFs>;
    // write ops → return EROFS
}
```

### Swift Side (Vendored FSKitBridge)

The `CtxfsFS.appex` is a vendored and customized copy of [FSKitBridge](https://github.com/debox-network/FSKitBridge). It:

1. Implements `FSUnaryFileSystem` / `FSVolume` protocols
2. Translates FSKit XPC calls → length-delimited Protobuf messages over TCP localhost
3. Connects to the daemon's fskit-rs TCP listener on the port negotiated at mount time

The appex is signed with the `com.apple.developer.fskit.fsmodule` restricted entitlement using the project's Apple Developer provisioning profile.

### Bridge Protocol

FSKitBridge uses Protobuf over TCP with length-delimited framing:

```
[4-byte length][protobuf message]
```

The `fskit-rs` crate on the Rust side handles serialization/deserialization and maps Protobuf messages to `Filesystem` trait calls.

### Security Model (cross-backend)

ctxfs uses a **localhost-binding security model** consistent across both NFS and FSKit backends — the same trust boundary as `ssh-agent`, Docker, and GPG agent.

| Entry point | Binding | Protection |
|---|---|---|
| Daemon UDS socket (`~/.ctxfs/ctxfs.sock`) | User-owned (0700) | Same-user only — cross-user prevented by file permissions |
| NFS loopback server | `127.0.0.1:<port>` | Localhost only — network attacks prevented |
| FSKit TCP bridge | `127.0.0.1:35367` | Localhost only — network attacks prevented |

**Threat model:**

| Attacker | Mitigated? |
|---|---|
| Network attacker (remote) | ✅ Yes — all listeners bind `127.0.0.1` only; no IPv6 dual-stack |
| Other user on shared Mac | ✅ Yes — UDS socket is 0700; loopback listeners only accessible to same-user processes |
| Same-user process with localhost access | ❌ No — any same-user process that speaks protobuf (FSKit) or NFS can read mounted content |
| Root / same-user malware | ❌ No — can read from any listener or daemon memory |

**This is an explicit, intentional security posture.** Same-user trust is the standard for local developer tools. Adding per-connection auth requires a reliable secret delivery mechanism between the daemon and the FSKit appex; macOS `mount -o` flags do not propagate to `FSTaskOptions` (discovered during Phase 1.5 smoke testing), and the appex sandbox blocks filesystem reads from `~/.ctxfs/`.

**Auth infrastructure (opt-in, not active):** The fskit-rs fork includes full per-mount token enforcement (`SessionBuilder::with_auth_token`, `AuthenticateRequest` proto variant, constant-time validate in `socket.rs::handle_stream`, 9 passing tests). The Swift client supports optional token handshake in `Socket.getChannel()`. This activates when a reliable delivery mechanism (App Group shared container) is wired up in Phase 2a alongside the signing pipeline.

### Bridge Lifecycle

**Mount:**
1. Daemon starts fskit-rs TCP listener on the static port from `Info.plist` (35367)
2. Daemon signals fskitd to mount the volume via `mounter::mount(bundle_id, opts)`
3. fskitd calls appex `probeResource` → appex connects to daemon, gets resource identifier
4. fskitd calls appex `loadResource` → appex connects, gets volume identifier, returns volume
5. Kernel mounts the volume at `/Volumes/ctxfs/<slug>`

**Finder eject / external unmount:**
1. User clicks "Eject" in Finder → FSKit sends unmount to appex
2. Appex sends a `shutdown` message to daemon over the TCP channel
3. Daemon receives shutdown → tears down `MountHandle` (stops TCP listener, removes symlink, removes `mounts.json` entry)
4. Daemon updates its mount table atomically

**Appex crash / daemon restart:**
- If the appex crashes, fskitd restarts it. The appex reconnects to the daemon's TCP listener (same static port).
- **If the daemon restarts, all FSKit mounts must be remounted.** Daemon startup force-cleans stale FSKit mounts and removes dangling symlinks.
- The appex implements exponential backoff on TCP reconnection (up to 5 retries, then gives up and lets FSKit report errors).

---

## Mount Location and Symlinks

### Volume Naming

FSKit volumes mount under a ctxfs-owned directory:

```
/Volumes/ctxfs/<slug>
```

Examples:
- `/Volumes/ctxfs/react-19.1.0`
- `/Volumes/ctxfs/serde-1.0.219`
- `/Volumes/ctxfs/tokio-1.40.0`

The `/Volumes/ctxfs/` directory is created during `ctxfs setup install` (or `install-fskit`).

### Symlink Strategy

When the user specifies `-p` or `-d`, the CLI creates a symlink from the user-specified path to the `/Volumes/ctxfs/` mount. **All `-p` paths are canonicalized to absolute paths** before creating the symlink and storing in `MountHandle`, so symlinks work correctly regardless of the caller's working directory.

```sh
ctxfs mount npm:react@19.1.0 -p ./deps/react
# Canonicalizes ./deps/react → /Users/derek/project/deps/react
# Creates: /Users/derek/project/deps/react → /Volumes/ctxfs/react-19.1.0
# Prints:  Mounted at /Volumes/ctxfs/react-19.1.0 (linked from ./deps/react)
```

When no `-p` or `-d` is specified, no symlink is created — the user uses the `/Volumes/ctxfs/` path directly.

NFS mounts do not use symlinks — they mount directly at the `-p` path (unchanged behavior).

### Shared Volumes Across Projects

FSKit volumes live at a global path (`/Volumes/ctxfs/<slug>`), so they are inherently shared. If two projects mount the same source (e.g., `npm:react@19.1.0`), the daemon **reuses the existing volume** and creates an additional symlink:

```sh
# Project A
cd ~/project-a && ctxfs mount npm:react@19.1.0 -p ./deps/react
# Creates: /Volumes/ctxfs/react-19.1.0 (new volume)
# Creates: ~/project-a/deps/react → /Volumes/ctxfs/react-19.1.0

# Project B
cd ~/project-b && ctxfs mount npm:react@19.1.0 -p ./deps/react
# Reuses: /Volumes/ctxfs/react-19.1.0 (already mounted, same source+version)
# Creates: ~/project-b/deps/react → /Volumes/ctxfs/react-19.1.0
```

The daemon tracks **multiple symlinks per volume** in `MountHandle.symlink_paths: Vec<PathBuf>`. Unmounting via a symlink path removes only that symlink. The volume itself is only torn down when the last symlink is removed or the user unmounts via the `/Volumes/ctxfs/` path directly (or `--all`).

This is a UX improvement over NFS, where each project needed its own mount even for the same source.

### Symlink Lifecycle

The daemon tracks symlink paths in `MountHandle`:

```rust
pub struct MountHandle {
    pub info: MountInfo,
    pub backend: Backend,
    pub symlink_paths: Vec<PathBuf>,   // multiple projects can share one volume
    pub nfs_handle: Option<NfsServerHandle>,
    pub fskit_handle: Option<FsKitHandle>,
}
```

**Unmount by symlink path**: Removes the symlink. If other symlinks still reference the volume, the volume stays alive. If it was the last reference, the volume is torn down.

**Unmount by `/Volumes/ctxfs/` path or `--all`**: Tears down the volume and removes all associated symlinks.

**Symlink safety on unmount**: Before removing a symlink, the daemon verifies it still points into `/Volumes/ctxfs/`. If the user has repointed or replaced it, the daemon leaves it alone and logs a warning.

**Crash recovery**: Mount state (including symlink paths) is persisted to `~/.ctxfs/mounts.json`. On daemon startup, stale symlinks pointing to non-existent `/Volumes/ctxfs/*` paths are removed.

---

## Backend Detection and Selection

### Priority Chain

```
--backend flag  >  CTXFS_BACKEND env  >  config file  >  auto-detect
```

### Auto-Detection Logic

```
Is macOS 26+?
  ├─ No → NFS
  └─ Yes → Is CtxfsFS.appex installed and enabled?
       ├─ No → NFS
       └─ Yes → FSKit
```

### CLI Flag

```sh
ctxfs mount npm:react@19.1.0 -p ./deps/react --backend nfs
ctxfs mount npm:react@19.1.0 -p ./deps/react --backend fskit
```

### Environment Variable

```sh
CTXFS_BACKEND=nfs ctxfs mount npm:react@19.1.0 -p ./deps/react
```

### Persistent Default

```sh
ctxfs setup default-backend fskit   # writes to ~/.ctxfs/config.toml
ctxfs setup default-backend nfs
ctxfs setup default-backend auto    # restore auto-detect (default)
```

Stored in `~/.ctxfs/config.toml`:

```toml
default_backend = "fskit"
```

### Coexistence

Both backends can run simultaneously. Switching the default only affects new mounts. Existing mounts stay on their original backend. The same source mounted via FSKit can be shared across projects (multiple symlinks to one volume). The same source cannot be mounted via both backends simultaneously — the daemon rejects this to avoid confusion.

### `--server-only` with FSKit

Starts the fskit-rs TCP listener without signaling fskitd. Equivalent to NFS `--server-only` — exercises the daemon side without a real mount.

```sh
ctxfs mount github:octocat/Hello-World@master -p ./test --server-only --backend fskit
# Prints: FSKit TCP listener on 127.0.0.1:PORT (no volume mounted)
```

---

## Daemon Changes

### New Types

```rust
pub enum Backend {
    Nfs,
    FsKit,
}

pub struct FsKitHandle {
    pub tcp_listener: JoinHandle<()>,
    pub volume_path: PathBuf,
}
```

### Updated `do_mount()`

```rust
async fn do_mount(&self, source: &str, mount_point: &str, backend: Backend) -> Result<MountInfo> {
    // ... resolve source, fetch snapshot (unchanged) ...

    let vfs = VfsState::new(provider, cache, snapshot, subpath);

    match backend {
        Backend::Nfs => {
            let nfs = CtxfsNfs::new(vfs);
            let handle = nfs.spawn(&addr).await?;
            // store NfsServerHandle, return MountInfo with nfs_port
        }
        Backend::FsKit => {
            let fskit = CtxfsFsKit::new(vfs);
            let volume_path = PathBuf::from("/Volumes/ctxfs").join(&slug);
            let tcp_handle = fskit.serve_tcp(random_port).await?;
            // signal fskitd to mount the volume
            // store FsKitHandle + symlink_path, return MountInfo
        }
    }
}
```

### Mount State Persistence

For crash recovery, the daemon persists active mount metadata to `~/.ctxfs/mounts.json`. Writes use **atomic temp-file + rename** to prevent corruption from mid-write crashes, and an **advisory file lock** (`flock`) serializes concurrent mount/unmount operations.

```json
[
  {
    "source": "npm:react@19.1.0",
    "volume_path": "/Volumes/ctxfs/react-19.1.0",
    "symlink_paths": [
      "/Users/derek/project-a/deps/react",
      "/Users/derek/project-b/deps/react"
    ],
    "backend": "fskit",
    "tcp_port": 54321,
    "auth_token": "hex..."
  }
]
```

Written on every mount/unmount via: write to `mounts.json.tmp` → `fsync` → `rename` to `mounts.json`. On daemon startup, used for cleanup only — mounts are not restored, but dangling symlinks and stale volume directories are removed.

The file also stores enough metadata (port, token) to detect and reconcile FSKit volumes that may still exist in the kernel after a daemon crash — if a `/Volumes/ctxfs/*` directory exists but isn't tracked by the daemon, startup cleanup attempts to unmount it.

### Startup Cleanup

```rust
fn cleanup_stale_fskit_state(&self) {
    // 1. Read ~/.ctxfs/mounts.json (if it exists, may be absent or empty)
    // 2. Scan /Volumes/ctxfs/ for existing mount directories
    // 3. For each mounts.json entry:
    //    a. If volume_path no longer exists, remove dangling symlinks
    //    b. If volume_path exists but daemon didn't create it, attempt unmount
    // 4. For each /Volumes/ctxfs/* not in mounts.json:
    //    Attempt unmount (orphaned volume from a crashed daemon)
    // 5. Clear mounts.json (daemon is starting fresh)
}
```

---

## Setup Flow

### `ctxfs setup install` (Updated)

```
ctxfs setup install
  │
  ├─ [All platforms] Install NFS sudoers (/etc/sudoers.d/ctxfs)
  │
  ├─ [macOS 26+] Prompt for FSKit
  │   │
  │   │  "FSKit is available on your macOS 26. With FSKit:
  │   │   - No sudo needed per mount
  │   │   - No Full Disk Access needed
  │   │   - Better native macOS integration
  │   │
  │   │   Without FSKit, mounts use NFS (requires sudo + Full Disk Access).
  │   │
  │   │   Install CtxfsFS.app to enable FSKit? [Y/n]"
  │   │
  │   ├─ Y → install_fskit()
  │   │     1. Copy CtxfsFS.app to ~/Applications/
  │   │     2. Create /Volumes/ctxfs/ directory
  │   │     3. Open System Settings for extension toggle
  │   │     4. Verify extension is enabled
  │   │
  │   └─ n → print FDA guidance for NFS fallback
  │
  ├─ [macOS < 26] Print FDA guidance for NFS
  │
  └─ [Linux] Done (NFS sudoers only)
```

### `ctxfs setup install-fskit` (Standalone)

Same `install_fskit()` function. For users who declined during initial setup or upgraded to macOS 26 later.

### `ctxfs setup uninstall-fskit`

Removes `~/Applications/CtxfsFS.app` and prints instructions to disable the extension in System Settings.

### `ctxfs setup check` (Updated)

```
$ ctxfs setup check

NFS backend:
  Sudoers: Configured
  Full Disk Access: (see guidance below if reads fail)

FSKit backend:
  macOS version: 26.3.1 (supported)
  CtxfsFS.app: Installed (~/Applications/CtxfsFS.app)
  Extension enabled: Yes
  Mount directory: /Volumes/ctxfs/ exists

Active backend: FSKit (auto-detected)
```

### App Bundle Distribution

The signed `CtxfsFS.app` ships alongside the `ctxfs` binary:

- **GitHub Releases**: release archive contains both `ctxfs` binary and `CtxfsFS.app`
- **Homebrew**: formula installs CLI, cask installs the `.app` bundle
- `ctxfs setup install-fskit` looks for `CtxfsFS.app` at:
  1. Already installed at `~/Applications/CtxfsFS.app` or `/Applications/CtxfsFS.app`
  2. `CTXFS_FSKIT_APP_PATH` env var
  3. Next to the `ctxfs` binary (release archive layout)
- If FSKit is auto-detected but the appex is missing or not enabled, the CLI prints an actionable message: `"FSKit is supported on this macOS but CtxfsFS.app is not installed. Run 'ctxfs setup install-fskit' or install via Homebrew: 'brew install --cask ctxfs'. Falling back to NFS."`

---

## CLI Changes

### New Flags

```
ctxfs mount <sources...> [-p path] [-d dir] [--server-only] [--backend nfs|fskit]
ctxfs setup install
ctxfs setup install-fskit
ctxfs setup uninstall-fskit
ctxfs setup check
ctxfs setup default-backend <nfs|fskit|auto>
```

### `ctxfs list` Output

```
$ ctxfs list
SOURCE                           MOUNT               BACKEND  STATUS
github:facebook/react@v19.1.0   ./deps/react         fskit    ready
crate:serde@1.0.219             ./deps/serde         nfs      ready
```

### Symlink-Aware Unmount

`ctxfs unmount <path>` accepts either the symlink path or the `/Volumes/ctxfs/` path. The daemon resolves both. `--all` cleans up all mounts and all symlinks.

---

## Implementation Phases

The work is split into three phases to de-risk the FSKit dependency before investing in UX polish.

### Phase 0: Proof of Concept (do first, before any Rust code)

**Goal**: Prove that fskit-rs + FSKitBridge can mount a non-local volume on macOS 26 and serve reads.

1. Clone FSKitBridge, build and sign the appex with developer account
2. Write a minimal Rust binary implementing `fskit_rs::Filesystem` with hardcoded files
3. Mount it, `ls` and `cat` files from Finder and terminal
4. Measure read latency (single file, `grep` across 100 files)

**Gate**: If this doesn't work, or latency is unacceptable (>10x NFS), revisit the bridge strategy (direct FFI) or defer FSKit until the ecosystem matures. Do not proceed to Phase 1.

### Phase 1: Core Backend ✅ (2026-04-14)

Shipped. Validated end-to-end on macOS 26.4 — see `docs/poc/fskit-phase1-smoke-test.md`.

- ✅ `ctxfs-vfs` extraction from `ctxfs-nfs`
- ✅ `ctxfs-fskit` crate implementing `fskit_rs::Filesystem`
- ✅ Daemon backend dispatch (`do_mount` with `Backend::FsKit` path)
- ✅ `--backend nfs|fskit` flag, `CTXFS_BACKEND` env, auto-detection
- ✅ Symlink management (creation, absolute paths, shared volumes, safe removal)
- ✅ Mount state persistence (`mounts.json` with atomic writes)
- ✅ `ctxfs setup install-fskit` and `setup check` FSKit status
- ✅ `ctxfs setup install` FSKit prompt on macOS 26+
- ✅ Vendored FSKitBridge appex in `swift/CtxfsFS/` with `ai.ctxfs.fskitbridge[.fskitext]` bundle IDs
- ⏸ Bridge security (per-mount auth token) — **opt-in infrastructure built, not active** (macOS mount `-o` flags don't reach `FSTaskOptions`; needs App Group delivery in Phase 2a)

### Phase 1.5: Bridge Infrastructure + Security Model ✅ (2026-04-16)

Shipped. Validated end-to-end on macOS 26.4 with `ai.ctxfs.fskitbridge.fskitext` bundle ID.

**What shipped:**

- ✅ FSKitBridge vendored into `swift/CtxfsFS/` (bundle IDs already `ai.ctxfs.*`)
- ✅ `fskit-rs` forked into `crates/fskit-rs/` (workspace path dep, upstream PR track)
- ✅ Canonical `protocol.proto` on Rust side, Swift consumes via symlink
- ✅ `AuthenticateRequest` proto variant (field 50) + `pub mod protocol` re-export
- ✅ `SessionBuilder::with_auth_token()` API with per-connection auth enforcement in `handle_stream`
- ✅ Constant-time `verify_token_ct` in `crates/fskit-rs/src/auth.rs`
- ✅ Swift `Socket.getChannel()` supports optional auth handshake (skipped when token is nil)
- ✅ 9 auth tests (3 unit + 3 integration + 3 e2e) — all passing
- ✅ Cross-backend security model documented (localhost binding, same as ssh-agent/Docker)

**What was discovered and deferred:**

macOS `mount -o` flags do NOT propagate to `FSTaskOptions` in the FSKit appex (discovered during smoke testing). The appex sandbox also blocks filesystem reads from `~/.ctxfs/`. Token delivery requires App Group shared container, which requires the Phase 2a signing pipeline. Auth enforcement is opt-in until then.

**Security posture:** Localhost-binding-only, consistent across NFS and FSKit backends. See Security Model section above.

### Phase 2: Distribution + Finder Polish

Split into two sub-tracks that can progress in parallel once Phase 1.5 lands.

**2a. Distribution pipeline + auth activation**

Vendoring and bundle-ID rename shipped in Phase 1.5. Phase 2a handles signing, distribution, and activating auth.

- Apple Developer portal: register App ID + FSKit capability for `ai.ctxfs.fskitbridge` + `.fskitext`
- App Group shared container (`group.ai.ctxfs.shared`) for auth token delivery: daemon writes token, appex reads — activates the opt-in auth infrastructure from Phase 1.5
- Developer ID Application signing cert + App Store Connect API key in GitHub Actions secrets
- Release workflow on `macos-14`: `xcodebuild archive` → `-exportArchive` → `notarytool submit --wait` → `stapler staple`
- Homebrew tap (e.g. `derekxwang/tap`): `ctxfs` formula (CLI) + `ctxfs` cask (CtxfsFS.app)
- `ctxfs setup install-fskit` downloads the cask if not present
- `ctxfs setup uninstall-fskit`

**2b. Finder integration (UX polish)**

- Volume display name: `{source-spec} (ctxfs)` via `FSItemAttributes`
- Custom volume icon: `VolumeIcon.icns` in the appex bundle
- Read-only badge: `FSVolumeFlags.readOnly`
- Finder eject → daemon shutdown-RPC → symlink + state cleanup
- `ctxfs setup default-backend` persistent preference
- Swift unit tests for the appex
- Performance benchmarking on large dependency trees

**Release gating**: 2a ships before 2b. A user installing via `brew install --cask ctxfs` should get a working mount experience before we polish how it looks in Finder.

---

## Finder Polish (Phase 2)

### Volume Display Name

FSKit volume reports a human-readable name via `FSItemAttributes`:

```
react 19.1.0 (ctxfs)
```

Appears in Finder sidebar, `diskutil list`, and desktop if external disks are shown.

### Volume Icon

Custom ctxfs icon shipped inside `CtxfsFS.app/Contents/Resources/VolumeIcon.icns`. Visually distinguishes ctxfs volumes from real disks.

### Read-Only Indicator

Volume reports `FSVolumeFlags.readOnly`. Finder shows a lock badge and prevents drag-to-copy-into operations.

### Finder Eject

Clicking "Eject" in Finder triggers an FSKit unmount. The appex sends a `shutdown` message to the daemon over TCP (see Bridge Lifecycle). The daemon performs full cleanup: stop TCP listener, remove all associated symlinks, drop mount handle, update `mounts.json`.

### Not in Scope

- Spotlight indexing (not useful for read-only dependency source)
- Quick Look previews (standard text previews work already)
- Extended attributes (no need for MVP)

---

## Testing Strategy

### Unit Tests

| Crate | Tests |
|---|---|
| `ctxfs-vfs` | Inode allocation, lazy population, blob fetch, readdir ordering, symlink resolution, subpath re-rooting (migrated from `ctxfs-nfs`) |
| `ctxfs-fskit` | Attribute translation (NodeAttr → FSKit attrs), volume naming/slug, read-only enforcement, auth token validation |
| `ctxfs-cli` | Backend detection (mock OS version + extension state), symlink creation/removal (absolute path enforcement), unmount path resolution, shared volume symlink lifecycle |
| `ctxfs-core` | `Backend` enum serialization, config file parsing with `default_backend` |

### Integration Tests

| Test | Coverage | Requirements |
|---|---|---|
| `ctxfs-vfs/tests/` | VfsState with mock Provider — full read flow | None |
| `ctxfs-nfs/tests/` | NFS-specific translation, existing tests (refactored) | None |
| `ctxfs-fskit/tests/tcp_roundtrip.rs` | fskit-rs TCP listener + mock client, auth token handshake, exercise lookup/read/readdir | None |
| `ctxfs-daemon/tests/mounts_json.rs` | Atomic write + recovery: truncated file, concurrent writes, startup cleanup | None |

### E2E Tests

```rust
#[test]
fn backend_flag_selects_nfs() { /* --server-only --backend nfs → NFS port */ }

#[test]
fn backend_flag_selects_fskit() { /* --server-only --backend fskit → TCP port */ }

#[test]
fn symlink_created_and_cleaned_on_unmount() { /* mock volume path */ }

#[test]
fn shared_volume_multiple_symlinks() { /* same source, two -p paths, unmount one */ }

#[test]
fn symlink_repointed_by_user_not_deleted() { /* safety check */ }

#[test]
fn setup_check_reports_fskit_status() { /* macOS 26+ section in output */ }
```

### Manual Smoke Tests (Gated)

```rust
#[test]
#[ignore = "requires signed CtxfsFS.appex installed and enabled"]
fn fskit_full_mount_and_read() {
    // Real FSKit mount → read file → unmount → verify symlink cleanup
}

#[test]
#[ignore = "requires signed CtxfsFS.appex installed and enabled"]
fn fskit_shared_volume_two_projects() {
    // Mount same source with two different -p paths → one volume, two symlinks
}
```

### TDD Order

1. **Phase 0**: Proof of concept (manual, no TDD — just get a mount working)
2. `ctxfs-vfs` extraction + migrated tests
3. `ctxfs-fskit` unit tests (Filesystem trait, auth token)
4. CLI backend detection + symlink management tests
5. Integration tests (TCP roundtrip with auth, mounts.json atomicity)
6. E2E tests (server-only paths, shared volumes)
7. Manual FSKit smoke test
8. **Phase 2**: Finder polish tests (Swift unit tests for appex)

---

## Comparison: NFS vs FSKit

| Dimension | NFS (current) | FSKit (new) |
|---|---|---|
| Platforms | macOS + Linux | macOS 26+ only |
| sudo required | Yes (every mount) | No |
| Full Disk Access | Yes (macOS TCC) | No |
| Kernel extension | No | No |
| User setup | `setup install` + FDA grant + terminal restart | `setup install` → toggle extension once |
| Mount location | Arbitrary (`-p` path) | `/Volumes/ctxfs/<slug>`, symlink to `-p` path |
| Distribution | `cargo build` / binary | Requires signed `.app` bundle |
| Performance | Direct TCP loopback | XPC → TCP (extra hop) |
| Finder integration | Minimal | Volume icon, display name, eject support |

---

## Environment Variables (New/Updated)

| Variable | Default | Description |
|---|---|---|
| `CTXFS_BACKEND` | (auto-detect) | Force backend: `nfs` or `fskit` |
| `CTXFS_FSKIT_APP_PATH` | (next to binary, then `~/Applications/`) | Path to `CtxfsFS.app` bundle |

---

## Signing and Distribution

### Requirements

- **Apple Developer Program membership** (paid) — required for the `com.apple.developer.fskit.fsmodule` restricted entitlement
- **Xcode 16.3+** — for building the Swift appex

### Build Pipeline

```sh
# Rust workspace (all platforms)
cargo build --release

# Swift appex (macOS only, signed)
cd swift/CtxfsFS
xcodebuild -scheme CtxfsFS -configuration Release \
  CODE_SIGN_IDENTITY="Developer ID Application: ..." \
  build
```

### Distribution Matrix

| Channel | CLI binary | CtxfsFS.app | Backend available |
|---|---|---|---|
| `cargo build` / `cargo install` | Yes | No | NFS only |
| GitHub Releases | Yes | Yes (signed) | NFS + FSKit |
| Homebrew formula + cask | Yes | Yes (signed) | NFS + FSKit |

---

## Open Questions

### Resolved by Phase 0 (2026-04-13)

1. **fskit-rs maturity**: ✅ Works. API is complete for read-only filesystems. All methods we need (activate, lookup_item, enumerate_directory, get_attributes, read, read_symbolic_link, statfs) function correctly. Risk: v0.1.0, single contributor. Mitigation: vendor the crate in Phase 1.

2. **FSKitBridge customization depth**: The appex works unmodified for read-only filesystems. Customization needed: bundle ID per-deployment (trivial), volume display name (attribute), Finder icon (resource). Auth token handshake can be layered on top of the existing Protobuf protocol without modifying the Swift code. Mitigation: vendor the Swift source.

3. **Performance**: ✅ 2ms per small-file read on macOS 26.4 (MacBook Pro M-series). Comparable to NFS loopback. The XPC→TCP hop adds no meaningful overhead for interactive workloads. Direct C FFI optimization is NOT warranted.

### Resolve during implementation

4. **Notarization**: For distribution outside the App Store, the `.app` bundle needs notarization via `notarytool`. One-time CI setup, not a design blocker, but must be done before the first public release with FSKit.

5. **Reproducible builds and cert rotation**: The Swift appex build must be reproducible in CI. Pin the FSKitBridge commit in-tree (vendored, not a git submodule). Document how entitlements are provisioned, how Developer ID certs rotate, and how a second maintainer would be onboarded to sign releases. Avoid single-developer signing key as a bus factor.

6. **`cargo install` UX**: Users who install via `cargo install ctxfs` will get NFS-only. The CLI should detect macOS 26+ and print a one-time hint: `"Tip: FSKit backend is available for macOS 26+. Install CtxfsFS.app for a better experience (no sudo/FDA). See: ctxfs setup install-fskit"`. This avoids silent fallback confusion.
