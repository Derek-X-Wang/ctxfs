# FSKit Backend for macOS — Design Spec

**Date**: 2026-04-11
**Status**: Draft
**Scope**: Add an FSKit-based filesystem backend for macOS 26+, eliminating the need for sudo and Full Disk Access on modern Macs. NFS remains the cross-platform fallback.

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

### Backend Adapters

Each backend is a thin translation layer:

- `ctxfs-nfs`: wraps `VfsState`, implements `nfsserve::NFSFileSystem`, translates `NodeAttr` → `fattr3`
- `ctxfs-fskit`: wraps `VfsState`, implements `fskit_rs::Filesystem`, translates `NodeAttr` → FSKit attributes

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

When the user specifies `-p` or `-d`, the CLI creates a symlink from the user-specified path to the `/Volumes/ctxfs/` mount:

```sh
ctxfs mount npm:react@19.1.0 -p ./deps/react
# Creates: ./deps/react → /Volumes/ctxfs/react-19.1.0
# Prints:  Mounted at /Volumes/ctxfs/react-19.1.0 (linked from ./deps/react)
```

When no `-p` or `-d` is specified, no symlink is created — the user uses the `/Volumes/ctxfs/` path directly.

NFS mounts do not use symlinks — they mount directly at the `-p` path (unchanged behavior).

### Symlink Lifecycle

The daemon tracks symlink paths in `MountHandle`:

```rust
pub struct MountHandle {
    pub info: MountInfo,
    pub backend: Backend,
    pub symlink_path: Option<PathBuf>,
    pub nfs_handle: Option<NfsServerHandle>,
    pub fskit_handle: Option<FsKitHandle>,
}
```

**Unmount (any path)**: The daemon resolves the target whether the user passes the symlink path, the `/Volumes/` path, or uses `--all`. Both the FSKit volume and the symlink are cleaned up.

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

Both backends can run simultaneously. Switching the default only affects new mounts. Existing mounts stay on their original backend. The same source cannot be mounted twice — the daemon rejects duplicates regardless of backend.

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

For crash recovery, the daemon persists active mount metadata to `~/.ctxfs/mounts.json`:

```json
[
  {
    "source": "npm:react@19.1.0",
    "volume_path": "/Volumes/ctxfs/react-19.1.0",
    "symlink_path": "/Users/derek/project/deps/react",
    "backend": "fskit"
  }
]
```

Written on mount, entry removed on unmount. On daemon startup, used for cleanup only — mounts are not restored, but dangling symlinks and stale volume directories are removed.

### Startup Cleanup

```rust
fn cleanup_stale_fskit_state(&self) {
    // 1. Read ~/.ctxfs/mounts.json (if it exists)
    // 2. For each entry: if volume_path no longer exists, remove symlink_path if dangling
    // 3. Clear mounts.json (daemon is starting fresh)
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
  1. Next to the `ctxfs` binary (release archive)
  2. `CTXFS_FSKIT_APP_PATH` env var
  3. Already installed at `~/Applications/CtxfsFS.app`

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

## Finder Polish

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

Clicking "Eject" in Finder triggers an FSKit unmount. The appex forwards this to the daemon via TCP, which performs full cleanup: stop TCP listener, remove symlink, drop mount handle.

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
| `ctxfs-fskit` | Attribute translation (NodeAttr → FSKit attrs), volume naming/slug, read-only enforcement |
| `ctxfs-cli` | Backend detection (mock OS version + extension state), symlink creation/removal, unmount path resolution |
| `ctxfs-core` | `Backend` enum serialization, config file parsing with `default_backend` |

### Integration Tests

| Test | Coverage | Requirements |
|---|---|---|
| `ctxfs-vfs/tests/` | VfsState with mock Provider — full read flow | None |
| `ctxfs-nfs/tests/` | NFS-specific translation, existing tests (refactored) | None |
| `ctxfs-fskit/tests/tcp_roundtrip.rs` | fskit-rs TCP listener + mock client, exercise lookup/read/readdir | None |

### E2E Tests

```rust
#[test]
fn backend_flag_selects_nfs() { /* --server-only --backend nfs → NFS port */ }

#[test]
fn backend_flag_selects_fskit() { /* --server-only --backend fskit → TCP port */ }

#[test]
fn symlink_created_and_cleaned_on_unmount() { /* mock volume path */ }

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
```

### TDD Order

1. `ctxfs-vfs` extraction + migrated tests
2. `ctxfs-fskit` unit tests (Filesystem trait)
3. CLI backend detection tests
4. Integration tests (TCP roundtrip)
5. E2E tests (server-only paths)
6. Manual FSKit smoke test

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

1. **fskit-rs maturity**: The `fskit-rs` crate exists on crates.io but is relatively new. Need to evaluate: does it support non-local volumes on macOS 26? Does it handle the full VFS surface we need? Fallback: vendor and extend.

2. **FSKitBridge customization depth**: How much do we need to customize the Swift appex beyond branding? Volume naming, icon, and read-only flags likely require changes to the FSKit bridge protocol or the appex itself.

3. **Notarization**: For distribution outside the App Store, the `.app` bundle needs notarization via `notarytool`. This is a one-time CI setup, not a design question, but it's a prerequisite for shipping.

4. **Performance**: The extra XPC → TCP hop adds latency compared to NFS direct loopback. Need to benchmark on real workloads (large `grep` across a mounted repo). If latency is problematic, consider the direct C FFI approach (Option 2) as a future optimization.
