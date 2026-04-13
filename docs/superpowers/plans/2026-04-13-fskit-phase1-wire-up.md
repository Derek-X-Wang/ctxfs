# FSKit Phase 1: Wire-Up Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the existing `ctxfs-fskit` stub into a working FSKit backend by implementing `fskit_rs::Filesystem` on top of `VfsState`, adding daemon dispatch, CLI integration, and symlink management so users can mount via `ctxfs mount ... --backend fskit`.

**Architecture:** `ctxfs-fskit` gains a `FilesystemAdapter` that wraps `VfsState` and implements `fskit_rs::Filesystem` by translating VFS types to FSKit protobuf types. The daemon's `do_mount()` dispatches on `Backend` — the FSKit path constructs the adapter, calls `fskit_rs::mount()` to start the TCP server and trigger the mount, tracks the session handle in `MountHandle`, and persists state to `mounts.json`. The CLI extends to create `/Volumes/ctxfs/<slug>` symlinks when `-p` is specified.

**Tech Stack:** Rust (fskit-rs 0.1, tokio, async-trait), reuses existing ctxfs-vfs/ctxfs-cache/ctxfs-manifest/ctxfs-core stack

**Spec:** `docs/superpowers/specs/2026-04-11-fskit-backend-design.md`

**Phase 0 evidence:** `docs/poc/fskit-poc/README.md` — PoC validated 2026-04-13 on macOS 26.4.

---

## Scope

This plan covers only what's needed to mount repos via FSKit and read files from them. It explicitly defers:
- Auth token handshake (requires modifying FSKitBridge Swift code — tracked for Phase 1.5)
- Finder polish (custom icon, volume display name) — Phase 2
- `ctxfs setup default-backend` persistence — Phase 2
- `ctxfs setup uninstall-fskit` — Phase 2

Phase 1 delivers the feature gate: a user with FSKitBridge installed can run `ctxfs mount npm:react@19.1.0 -p ./deps/react --backend fskit` and successfully browse React source code.

---

## Prerequisites for Implementation

The implementer must have:
1. macOS 26.x
2. `FSKitBridge.app` installed at `/Applications/FSKitBridge.app` with the extension enabled in System Settings
3. The bundle ID of the installed appex (check with `pluginkit -m -p com.apple.fskit.fsmodule | grep fskitbridge`)
4. `sudo mkdir -p /Volumes/ctxfs && sudo chown $(whoami):staff /Volumes/ctxfs` (one-time)
5. Protoc: `brew install protobuf` (for fskit-rs build script)

Without these, the FSKit-dependent e2e tests will fail. All Rust unit tests and the trait adapter tests can still run.

---

## File Map

### New Files

| File | Responsibility |
|---|---|
| `crates/ctxfs-fskit/src/adapter.rs` | `FilesystemAdapter` struct: implements `fskit_rs::Filesystem` by delegating to `VfsState`. Contains all VFS→FSKit type translation (NodeAttr→ItemAttributes, NodeType→ItemType, VfsError→fskit_rs::Error). |
| `crates/ctxfs-fskit/src/slug.rs` | `volume_slug(source: &SourceSpec) -> String` — produces the `/Volumes/ctxfs/<slug>` directory name. |
| `crates/ctxfs-fskit/tests/adapter_ops.rs` | Unit tests for the adapter: trait methods return correct types, error translation, item attribute mapping. Uses a mock `VfsState` (same fixture pattern as `crates/ctxfs-vfs/tests/vfs_ops.rs`). |
| `crates/ctxfs-daemon/src/fskit_mount.rs` | `start_fskit_mount()` helper: builds adapter, calls `fskit_rs::mount()`, returns `(Session, volume_path)`. Keeps FSKit-specific code out of `daemon.rs`. |

### Modified Files

| File | Changes |
|---|---|
| `crates/ctxfs-fskit/Cargo.toml` | Add `fskit-rs`, `async-trait`, `libc`, `tracing`, `tokio`; add `dev-dependencies` for testing the adapter with a mock VfsState. |
| `crates/ctxfs-fskit/src/lib.rs` | Re-export `FilesystemAdapter`, `volume_slug`. |
| `crates/ctxfs-fskit/src/fs.rs` | Remove the stub — `CtxfsFsKit` struct replaced by `adapter::FilesystemAdapter`. |
| `crates/ctxfs-daemon/Cargo.toml` | Add `ctxfs-fskit` dependency. |
| `crates/ctxfs-daemon/src/daemon.rs` | (a) Add `FsKitHandle` type, (b) extend `MountHandle` with `fskit_handle: Option<FsKitHandle>` and `volume_path: Option<PathBuf>`, (c) change `do_mount()` to dispatch on `Backend` — call `start_fskit_mount()` for FSKit, existing NFS path unchanged, (d) set `volume_path` / `backend` on `MountInfo` return, (e) add `fskit_bundle_id` to `Config`-read env var. |
| `crates/ctxfs-daemon/src/lib.rs` | Register `pub mod fskit_mount;`. |
| `crates/ctxfs-ipc/src/service.rs` | Extend the `mount` RPC to accept `backend: Backend` as a third argument. (Current signature: `mount(source, mount_point) -> MountInfo`. New: `mount(source, mount_point, backend) -> MountInfo`.) |
| `crates/ctxfs-core/src/config.rs` | Add `fskit_bundle_id: Option<String>` field (from `CTXFS_FSKIT_BUNDLE_ID` env var). Needed because the bundle ID depends on the signing team and fskit-rs's default (`network.debox.fskitbridge.fskitext`) won't match self-signed installs. |
| `crates/ctxfs-cli/src/main.rs` | (a) Pass detected `backend` as the third arg to `client.mount()`, (b) when FSKit path: create symlink from user's `-p` to returned `volume_path`, print both paths, (c) in `handle_unmount`: resolve `-p` through symlink to volume_path before calling daemon, remove symlink after. |
| `crates/ctxfs-cli/src/deps/mount.rs` | Same `client.mount()` signature update for the batch-mount path. |
| `crates/ctxfs-ipc/tests/rpc_roundtrip.rs` | Update mock server's `mount` signature. |

---

## Task 1: Add fskit-rs Dependency to ctxfs-fskit

**Files:**
- Modify: `crates/ctxfs-fskit/Cargo.toml`
- Modify: `Cargo.toml` (root) — add `fskit-rs` to workspace deps

- [ ] **Step 1: Add fskit-rs to workspace dependencies**

Edit root `Cargo.toml`, add under `[workspace.dependencies]` alongside `rand`:

```toml
# FSKit backend (macOS 26+)
fskit-rs = "0.1"
```

- [ ] **Step 2: Update ctxfs-fskit/Cargo.toml**

Replace the `[dependencies]` section of `crates/ctxfs-fskit/Cargo.toml` with:

```toml
[dependencies]
ctxfs-vfs = { workspace = true }
ctxfs-core = { workspace = true }
ctxfs-manifest = { workspace = true }
async-trait = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
rand = { workspace = true }
hex = { workspace = true }
libc = { workspace = true }
fskit-rs = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
ctxfs-cache = { workspace = true }
async-trait = { workspace = true }
serde_json = { workspace = true }
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p ctxfs-fskit`
Expected: compiles. Note: `fskit-rs` triggers a `protoc` build — if `protoc` is missing, the error message says `brew install protobuf`. Install and retry.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/ctxfs-fskit/Cargo.toml
git commit -m "feat(fskit): add fskit-rs dependency

Pulls in fskit-rs 0.1 for the FSKit TCP/Protobuf client.
The build requires protoc (brew install protobuf).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: VFS → FSKit Type Translation Helpers

**Files:**
- Create: `crates/ctxfs-fskit/src/adapter.rs`
- Modify: `crates/ctxfs-fskit/src/lib.rs`

This task implements only the pure translation functions. No trait impl yet — that comes in Task 3.

- [ ] **Step 1: Write the failing test**

Create `crates/ctxfs-fskit/src/adapter.rs`:

```rust
//! Adapter translating between `ctxfs-vfs` types and `fskit-rs` (FSKit protobuf) types.

use ctxfs_vfs::{NodeAttr, NodeType, VfsError};
use fskit_rs::{Error as FsKitError, Item, ItemAttributes, ItemType};

/// The root directory inode ID. FSKit conventionally uses 2 for the root.
pub(crate) const FSKIT_ROOT_ID: u64 = 2;

/// Translate a VFS `NodeType` to an FSKit `ItemType`.
pub(crate) fn node_type_to_item_type(kind: NodeType) -> ItemType {
    match kind {
        NodeType::File => ItemType::File,
        NodeType::Directory => ItemType::Directory,
        NodeType::Symlink => ItemType::Symlink,
    }
}

/// Translate a VFS `NodeAttr` to an FSKit `ItemAttributes` for the given parent.
pub(crate) fn node_attr_to_item_attributes(attr: &NodeAttr, parent_id: u64) -> ItemAttributes {
    let mode = match attr.kind {
        NodeType::Directory => 0o555,
        NodeType::File => {
            if attr.executable {
                0o555
            } else {
                0o444
            }
        }
        NodeType::Symlink => 0o777,
    };
    let link_count = match attr.kind {
        NodeType::Directory => 2,
        _ => 1,
    };
    ItemAttributes {
        file_id: Some(attr.inode),
        parent_id: Some(parent_id),
        r#type: Some(node_type_to_item_type(attr.kind) as i32),
        mode: Some(mode),
        uid: Some(unsafe { libc::getuid() }),
        gid: Some(unsafe { libc::getgid() }),
        link_count: Some(link_count),
        size: Some(attr.size),
        alloc_size: Some(attr.size),
        ..Default::default()
    }
}

/// Build an FSKit `Item` for the given name and attributes.
pub(crate) fn make_item(name: &str, attr: &NodeAttr, parent_id: u64) -> Item {
    Item {
        name: name.as_bytes().to_vec(),
        attributes: Some(node_attr_to_item_attributes(attr, parent_id)),
    }
}

/// Translate a `VfsError` to an `fskit_rs::Error` (POSIX errno).
pub(crate) fn vfs_err_to_fskit(err: VfsError) -> FsKitError {
    let errno = match err {
        VfsError::NotFound => libc::ENOENT,
        VfsError::NotDir => libc::ENOTDIR,
        VfsError::IsDir => libc::EISDIR,
        VfsError::Invalid => libc::EINVAL,
        VfsError::ReadOnly => libc::EROFS,
        VfsError::Io(_) => libc::EIO,
    };
    FsKitError::Posix(errno)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file_attr(inode: u64, size: u64, executable: bool) -> NodeAttr {
        NodeAttr {
            inode,
            size,
            kind: NodeType::File,
            executable,
        }
    }

    #[test]
    fn node_type_translation() {
        assert!(matches!(
            node_type_to_item_type(NodeType::File),
            ItemType::File
        ));
        assert!(matches!(
            node_type_to_item_type(NodeType::Directory),
            ItemType::Directory
        ));
        assert!(matches!(
            node_type_to_item_type(NodeType::Symlink),
            ItemType::Symlink
        ));
    }

    #[test]
    fn file_attr_translates_correctly() {
        let attr = file_attr(5, 1024, false);
        let item_attr = node_attr_to_item_attributes(&attr, 2);
        assert_eq!(item_attr.file_id, Some(5));
        assert_eq!(item_attr.parent_id, Some(2));
        assert_eq!(item_attr.r#type, Some(ItemType::File as i32));
        assert_eq!(item_attr.mode, Some(0o444));
        assert_eq!(item_attr.size, Some(1024));
        assert_eq!(item_attr.alloc_size, Some(1024));
        assert_eq!(item_attr.link_count, Some(1));
    }

    #[test]
    fn executable_file_gets_exec_mode() {
        let attr = file_attr(5, 10, true);
        let item_attr = node_attr_to_item_attributes(&attr, 2);
        assert_eq!(item_attr.mode, Some(0o555));
    }

    #[test]
    fn directory_attrs() {
        let attr = NodeAttr {
            inode: 2,
            size: 4096,
            kind: NodeType::Directory,
            executable: false,
        };
        let item_attr = node_attr_to_item_attributes(&attr, 2);
        assert_eq!(item_attr.mode, Some(0o555));
        assert_eq!(item_attr.link_count, Some(2));
        assert_eq!(item_attr.r#type, Some(ItemType::Directory as i32));
    }

    #[test]
    fn symlink_attrs() {
        let attr = NodeAttr {
            inode: 5,
            size: 10,
            kind: NodeType::Symlink,
            executable: false,
        };
        let item_attr = node_attr_to_item_attributes(&attr, 2);
        assert_eq!(item_attr.mode, Some(0o777));
        assert_eq!(item_attr.r#type, Some(ItemType::Symlink as i32));
    }

    #[test]
    fn make_item_name_bytes() {
        let attr = file_attr(5, 10, false);
        let item = make_item("README.md", &attr, 2);
        assert_eq!(item.name, b"README.md");
        assert!(item.attributes.is_some());
    }

    #[test]
    fn error_translation() {
        assert!(matches!(
            vfs_err_to_fskit(VfsError::NotFound),
            FsKitError::Posix(e) if e == libc::ENOENT
        ));
        assert!(matches!(
            vfs_err_to_fskit(VfsError::NotDir),
            FsKitError::Posix(e) if e == libc::ENOTDIR
        ));
        assert!(matches!(
            vfs_err_to_fskit(VfsError::IsDir),
            FsKitError::Posix(e) if e == libc::EISDIR
        ));
        assert!(matches!(
            vfs_err_to_fskit(VfsError::Invalid),
            FsKitError::Posix(e) if e == libc::EINVAL
        ));
        assert!(matches!(
            vfs_err_to_fskit(VfsError::ReadOnly),
            FsKitError::Posix(e) if e == libc::EROFS
        ));
        assert!(matches!(
            vfs_err_to_fskit(VfsError::Io("test".into())),
            FsKitError::Posix(e) if e == libc::EIO
        ));
    }
}
```

- [ ] **Step 2: Register module**

Edit `crates/ctxfs-fskit/src/lib.rs`:

```rust
pub mod adapter;
pub mod auth;
pub mod fs;

pub use auth::AuthToken;
pub use fs::CtxfsFsKit;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ctxfs-fskit adapter`
Expected: 7 tests pass.

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets -p ctxfs-fskit`
Expected: clean. The `unsafe { libc::getuid() }` / `getgid()` calls should be fine — they're canonical POSIX calls with no UB. We use `#[allow(unsafe_code)]` if the workspace lint complains.

If clippy complains about unsafe_code, wrap the calls:

```rust
#[allow(unsafe_code)]
let uid = unsafe { libc::getuid() };
```

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-fskit/src/adapter.rs crates/ctxfs-fskit/src/lib.rs
git commit -m "feat(fskit): add VFS → FSKit type translation

Pure translation helpers for NodeAttr → ItemAttributes,
NodeType → ItemType, and VfsError → fskit_rs::Error (POSIX errno).
No trait impl yet — that comes next. All tests are pure function
calls, no FSKit runtime required.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: FilesystemAdapter — fskit_rs::Filesystem Implementation

**Files:**
- Modify: `crates/ctxfs-fskit/src/adapter.rs`
- Modify: `crates/ctxfs-fskit/src/fs.rs` (delete stub, replace with re-export)
- Modify: `crates/ctxfs-fskit/src/lib.rs`
- Create: `crates/ctxfs-fskit/tests/adapter_ops.rs`

- [ ] **Step 1: Extend adapter.rs with FilesystemAdapter struct**

Append to `crates/ctxfs-fskit/src/adapter.rs`:

```rust
use async_trait::async_trait;
use ctxfs_vfs::VfsState;
use fskit_rs::{
    directory_entries, AccessMask, DirectoryEntries, Filesystem, OpenMode, PathConfOperations,
    PreallocateFlag, ResourceIdentifier, Result as FsKitResult, SetXattrPolicy, StatFsResult,
    SupportedCapabilities, SyncFlags, TaskOptions, VolumeBehavior, VolumeIdentifier, Xattrs,
};
use std::ffi::OsStr;
use std::sync::Arc;
use tracing::{debug, warn};

/// Adapter implementing `fskit_rs::Filesystem` on top of a shared `VfsState`.
///
/// The adapter is cloneable because `fskit_rs::mount` requires `Clone`. All state
/// lives in the `Arc<VfsState>`, so clones are cheap and share the inode table.
#[derive(Clone, Debug)]
pub struct FilesystemAdapter {
    vfs: Arc<VfsState>,
    /// Human-readable volume name, e.g. "react-19.1.0" — used by Finder.
    volume_name: String,
    /// Internal volume identifier (kept stable across mount lifecycle).
    volume_id: String,
}

impl FilesystemAdapter {
    pub fn new(vfs: Arc<VfsState>, volume_name: String) -> Self {
        let volume_id = format!("ctxfs-{volume_name}");
        Self {
            vfs,
            volume_name,
            volume_id,
        }
    }
}

#[async_trait]
impl Filesystem for FilesystemAdapter {
    // --- Volume lifecycle ---

    async fn get_resource_identifier(&mut self) -> FsKitResult<ResourceIdentifier> {
        Ok(ResourceIdentifier {
            name: Some(self.volume_name.clone()),
            container_id: Some("com.ctxfs.volume".into()),
        })
    }

    async fn get_volume_identifier(&mut self) -> FsKitResult<VolumeIdentifier> {
        Ok(VolumeIdentifier {
            id: Some(self.volume_id.clone()),
            name: Some(self.volume_name.clone()),
        })
    }

    async fn get_volume_behavior(&mut self) -> FsKitResult<VolumeBehavior> {
        Ok(VolumeBehavior {
            is_open_close_inhibited: Some(true),
            is_access_check_inhibited: Some(true),
            is_volume_rename_inhibited: Some(true),
            is_preallocate_inhibited: Some(true),
            ..Default::default()
        })
    }

    async fn get_volume_capabilities(&mut self) -> FsKitResult<SupportedCapabilities> {
        Ok(SupportedCapabilities::default())
    }

    async fn get_volume_statistics(&mut self) -> FsKitResult<StatFsResult> {
        let stats = self.vfs.statfs();
        Ok(StatFsResult {
            block_size: stats.block_size as i64,
            io_size: stats.block_size as i64,
            total_blocks: stats.total_bytes / stats.block_size,
            available_blocks: 0,
            free_blocks: 0,
            used_blocks: stats.total_bytes / stats.block_size,
            total_bytes: stats.total_bytes,
            available_bytes: 0,
            free_bytes: 0,
            used_bytes: stats.total_bytes,
            total_files: stats.total_files,
            free_files: 0,
        })
    }

    async fn get_path_conf_operations(&mut self) -> FsKitResult<PathConfOperations> {
        Ok(PathConfOperations::default())
    }

    async fn mount(&mut self, _options: TaskOptions) -> FsKitResult<()> {
        debug!("fskit mount called for volume {}", self.volume_name);
        Ok(())
    }

    async fn unmount(&mut self) -> FsKitResult<()> {
        debug!("fskit unmount called for volume {}", self.volume_name);
        Ok(())
    }

    async fn synchronize(&mut self, _flags: SyncFlags) -> FsKitResult<()> {
        Ok(())
    }

    async fn activate(&mut self, _options: TaskOptions) -> FsKitResult<fskit_rs::Item> {
        let root_id = self.vfs.root_id();
        // FSKit expects root to be inode 2, but VfsState uses 1. We map VFS root 1
        // to FSKit's FSKIT_ROOT_ID on the way out. The VfsState internally stays
        // at inode 1; translation happens at the FSKit boundary.
        let attr = self.vfs.getattr(root_id).await.map_err(vfs_err_to_fskit)?;
        let remapped = NodeAttr {
            inode: FSKIT_ROOT_ID,
            ..attr
        };
        Ok(make_item("/", &remapped, FSKIT_ROOT_ID))
    }

    async fn deactivate(&mut self) -> FsKitResult<()> {
        Ok(())
    }

    async fn set_volume_name(&mut self, _name: Vec<u8>) -> FsKitResult<Vec<u8>> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    // --- Item attributes ---

    async fn get_attributes(&mut self, item_id: u64) -> FsKitResult<ItemAttributes> {
        let inode = fskit_to_vfs_inode(item_id, self.vfs.root_id());
        let attr = self.vfs.getattr(inode).await.map_err(vfs_err_to_fskit)?;
        let parent_id = if inode == self.vfs.root_id() {
            FSKIT_ROOT_ID
        } else {
            // We don't track parent in VfsState's public API; use root as a fallback.
            // The attributes are still correct for FSKit's purposes — parent_id is
            // informational, not load-bearing for reads.
            FSKIT_ROOT_ID
        };
        let remapped = NodeAttr {
            inode: item_id,
            ..attr
        };
        Ok(node_attr_to_item_attributes(&remapped, parent_id))
    }

    async fn set_attributes(
        &mut self,
        _item_id: u64,
        _attributes: ItemAttributes,
    ) -> FsKitResult<ItemAttributes> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    // --- Directory operations ---

    async fn lookup_item(&mut self, name: &OsStr, directory_id: u64) -> FsKitResult<fskit_rs::Item> {
        let name_str = name.to_str().ok_or(FsKitError::Posix(libc::EINVAL))?;
        let parent = fskit_to_vfs_inode(directory_id, self.vfs.root_id());
        let (child_id, attr) = self
            .vfs
            .lookup(parent, name_str)
            .await
            .map_err(vfs_err_to_fskit)?;
        let remapped = NodeAttr {
            inode: vfs_to_fskit_inode(child_id, self.vfs.root_id()),
            ..attr
        };
        Ok(make_item(name_str, &remapped, directory_id))
    }

    async fn enumerate_directory(
        &mut self,
        directory_id: u64,
        cookie: u64,
        _verifier: u64,
    ) -> FsKitResult<DirectoryEntries> {
        let parent = fskit_to_vfs_inode(directory_id, self.vfs.root_id());
        let children = self
            .vfs
            .readdir(parent)
            .await
            .map_err(vfs_err_to_fskit)?;

        // FSKit uses `cookie` as an offset into the entry list. We return entries
        // starting at `cookie`, and set `next_cookie` to `index + 1`.
        let start = cookie as usize;
        let entries: Vec<directory_entries::Entry> = children
            .into_iter()
            .enumerate()
            .skip(start)
            .map(|(index, (child_inode, name, kind))| {
                let attr = NodeAttr {
                    inode: vfs_to_fskit_inode(child_inode, self.vfs.root_id()),
                    size: 0,
                    kind,
                    executable: false,
                };
                directory_entries::Entry {
                    item: Some(make_item(&name, &attr, directory_id)),
                    next_cookie: (index + 1) as u64,
                }
            })
            .collect();

        Ok(DirectoryEntries {
            entries,
            verifier: 0,
        })
    }

    async fn reclaim_item(&mut self, _item_id: u64) -> FsKitResult<()> {
        Ok(())
    }

    async fn deactivate_item(&mut self, _item_id: u64) -> FsKitResult<()> {
        Ok(())
    }

    // --- File operations (read-only) ---

    async fn create_item(
        &mut self,
        _name: &OsStr,
        _type: ItemType,
        _dir_id: u64,
        _attrs: ItemAttributes,
    ) -> FsKitResult<fskit_rs::Item> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn remove_item(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _dir_id: u64,
    ) -> FsKitResult<()> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn rename_item(
        &mut self,
        _item_id: u64,
        _to_dir_id: u64,
        _name: &OsStr,
        _to_name: &OsStr,
        _to_item_id: u64,
        _to_item_existing_id: Option<u64>,
    ) -> FsKitResult<Vec<u8>> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn open_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> FsKitResult<()> {
        Ok(())
    }

    async fn close_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> FsKitResult<()> {
        Ok(())
    }

    async fn read(&mut self, item_id: u64, offset: i64, length: i64) -> FsKitResult<Vec<u8>> {
        if offset < 0 || length < 0 {
            return Err(FsKitError::Posix(libc::EINVAL));
        }
        let inode = fskit_to_vfs_inode(item_id, self.vfs.root_id());
        let data = self
            .vfs
            .read(inode, offset as u64, length as u32)
            .await
            .map_err(vfs_err_to_fskit)?;
        Ok(data)
    }

    async fn write(
        &mut self,
        _contents: Vec<u8>,
        _item_id: u64,
        _offset: i64,
    ) -> FsKitResult<i64> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn preallocate_space(
        &mut self,
        _item_id: u64,
        _offset: i64,
        _length: i64,
        _flags: Vec<PreallocateFlag>,
    ) -> FsKitResult<i64> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    // --- Link operations ---

    async fn create_symbolic_link(
        &mut self,
        _name: &OsStr,
        _directory_id: u64,
        _attributes: ItemAttributes,
        _contents: Vec<u8>,
    ) -> FsKitResult<fskit_rs::Item> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn create_link(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> FsKitResult<Vec<u8>> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn read_symbolic_link(&mut self, item_id: u64) -> FsKitResult<Vec<u8>> {
        let inode = fskit_to_vfs_inode(item_id, self.vfs.root_id());
        let target = self.vfs.readlink(inode).await.map_err(vfs_err_to_fskit)?;
        Ok(target.into_bytes())
    }

    // --- Access control ---

    async fn check_access(
        &mut self,
        _item_id: u64,
        _access: Vec<AccessMask>,
    ) -> FsKitResult<bool> {
        Ok(true)
    }

    // --- Extended attributes (not supported) ---

    async fn get_supported_xattr_names(&mut self, _item_id: u64) -> FsKitResult<Xattrs> {
        Ok(Xattrs::default())
    }

    async fn get_xattr(&mut self, _name: &OsStr, _item_id: u64) -> FsKitResult<Vec<u8>> {
        Err(FsKitError::Posix(libc::ENOATTR))
    }

    async fn set_xattr(
        &mut self,
        _name: &OsStr,
        _value: Option<Vec<u8>>,
        _item_id: u64,
        _policy: SetXattrPolicy,
    ) -> FsKitResult<()> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    async fn get_xattrs(&mut self, _item_id: u64) -> FsKitResult<Xattrs> {
        Ok(Xattrs::default())
    }
}

/// Map an FSKit inode ID to a VFS inode ID.
/// FSKit expects the root at `FSKIT_ROOT_ID` (2). VfsState uses its own
/// `root_id()` (1 by convention). Everything else passes through unchanged.
fn fskit_to_vfs_inode(fskit_id: u64, vfs_root: u64) -> u64 {
    if fskit_id == FSKIT_ROOT_ID {
        vfs_root
    } else {
        fskit_id
    }
}

/// Inverse of `fskit_to_vfs_inode`.
fn vfs_to_fskit_inode(vfs_id: u64, vfs_root: u64) -> u64 {
    if vfs_id == vfs_root {
        FSKIT_ROOT_ID
    } else {
        vfs_id
    }
}

// Re-import NodeAttr for the `..attr` struct-update syntax used above.
use ctxfs_vfs::NodeAttr;
use fskit_rs::Error as FsKitError;
```

- [ ] **Step 2: Replace fs.rs stub**

Overwrite `crates/ctxfs-fskit/src/fs.rs` with a compatibility shim:

```rust
//! Legacy module name — kept so external references don't break.
//! The adapter lives in `crate::adapter`.

pub use crate::adapter::FilesystemAdapter as CtxfsFsKit;
```

Update `crates/ctxfs-fskit/src/lib.rs`:

```rust
pub mod adapter;
pub mod auth;
pub mod fs;

pub use adapter::FilesystemAdapter;
pub use auth::AuthToken;
pub use fs::CtxfsFsKit;
```

- [ ] **Step 3: Write the adapter integration test**

Create `crates/ctxfs-fskit/tests/adapter_ops.rs`:

```rust
//! Integration tests exercising `FilesystemAdapter` trait methods against a mock VFS.
//!
//! These tests do NOT require FSKit to be installed — they call trait methods directly.

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

use async_trait::async_trait;
use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::Digest;
use ctxfs_fskit::FilesystemAdapter;
use ctxfs_manifest::{DirEntry, Directory, DirectoryEntry, FileEntry, Snapshot};
use ctxfs_vfs::VfsState;
use fskit_rs::{Filesystem, ItemType, TaskOptions};
use std::ffi::OsStr;
use std::sync::Arc;

struct MockProvider {
    directories: std::collections::HashMap<String, Vec<u8>>,
    blobs: std::collections::HashMap<String, Vec<u8>>,
}

#[async_trait]
impl ctxfs_core::provider::Provider for MockProvider {
    async fn fetch_snapshot(
        &self,
        _source: &ctxfs_core::source::SourceSpec,
    ) -> Result<Vec<u8>, ctxfs_core::error::CtxfsError> {
        unimplemented!("not needed for adapter tests")
    }

    async fn fetch_directory(
        &self,
        digest: &Digest,
    ) -> Result<Vec<u8>, ctxfs_core::error::CtxfsError> {
        self.directories
            .get(&digest.hex)
            .cloned()
            .ok_or_else(|| ctxfs_core::error::CtxfsError::NotFound(digest.hex.clone()))
    }

    async fn fetch_blob(
        &self,
        digest: &Digest,
    ) -> Result<Vec<u8>, ctxfs_core::error::CtxfsError> {
        self.blobs
            .get(&digest.hex)
            .cloned()
            .ok_or_else(|| ctxfs_core::error::CtxfsError::NotFound(digest.hex.clone()))
    }
}

fn make_digest(hex: &str) -> Digest {
    Digest {
        algorithm: ctxfs_core::digest::HashAlgorithm::Sha256,
        hex: hex.to_string(),
    }
}

async fn build_adapter() -> FilesystemAdapter {
    let readme_digest = make_digest("readme_sha256");
    let readme_content = b"# Hello\n".to_vec();

    let root_dir = Directory {
        digest: make_digest("root_dir_sha256"),
        entries: vec![
            DirEntry::File(FileEntry {
                name: "README.md".into(),
                digest: readme_digest.clone(),
                size: readme_content.len() as u64,
                executable: false,
                inline_content: Some(readme_content.clone()),
            }),
            DirEntry::Directory(DirectoryEntry {
                name: "src".into(),
                digest: make_digest("empty_dir_sha256"),
            }),
        ],
    };
    let empty_dir = Directory {
        digest: make_digest("empty_dir_sha256"),
        entries: vec![],
    };

    let mut directories = std::collections::HashMap::new();
    directories.insert(
        root_dir.digest.hex.clone(),
        serde_json::to_vec(&root_dir).unwrap(),
    );
    directories.insert(
        empty_dir.digest.hex.clone(),
        serde_json::to_vec(&empty_dir).unwrap(),
    );

    let mut blobs = std::collections::HashMap::new();
    blobs.insert(readme_digest.hex.clone(), readme_content);

    let provider: SharedProvider = Arc::new(MockProvider { directories, blobs });

    let snapshot = Snapshot {
        source: "github:test/repo@main".into(),
        commit_sha: "abc123".into(),
        root_directory: root_dir.digest,
        created_at: "2026-04-13T00:00:00Z".into(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp.path().to_path_buf(), 64 * 1024 * 1024).unwrap());
    let vfs = Arc::new(VfsState::new(provider, cache, snapshot, None).await.unwrap());

    FilesystemAdapter::new(vfs, "test-vol".into())
}

#[tokio::test]
async fn activate_returns_root_at_fskit_id_2() {
    let mut adapter = build_adapter().await;
    let root = adapter.activate(TaskOptions::default()).await.unwrap();
    let attrs = root.attributes.unwrap();
    assert_eq!(attrs.file_id, Some(2)); // FSKIT_ROOT_ID
    assert_eq!(attrs.r#type, Some(ItemType::Directory as i32));
}

#[tokio::test]
async fn lookup_finds_child_from_fskit_root() {
    let mut adapter = build_adapter().await;
    // FSKit sends directory_id=2 for root lookups
    let item = adapter
        .lookup_item(OsStr::new("README.md"), 2)
        .await
        .unwrap();
    assert_eq!(item.name, b"README.md");
    let attrs = item.attributes.unwrap();
    assert_eq!(attrs.r#type, Some(ItemType::File as i32));
    assert_eq!(attrs.size, Some(8));
}

#[tokio::test]
async fn lookup_missing_returns_enoent() {
    let mut adapter = build_adapter().await;
    let err = adapter
        .lookup_item(OsStr::new("does-not-exist"), 2)
        .await
        .unwrap_err();
    match err {
        fskit_rs::Error::Posix(e) => assert_eq!(e, libc::ENOENT),
    }
}

#[tokio::test]
async fn read_file_contents() {
    let mut adapter = build_adapter().await;
    let readme = adapter
        .lookup_item(OsStr::new("README.md"), 2)
        .await
        .unwrap();
    let file_id = readme.attributes.unwrap().file_id.unwrap();

    let bytes = adapter.read(file_id, 0, 1024).await.unwrap();
    assert_eq!(bytes, b"# Hello\n");
}

#[tokio::test]
async fn enumerate_root_returns_children() {
    let mut adapter = build_adapter().await;
    let dir = adapter.enumerate_directory(2, 0, 0).await.unwrap();
    assert_eq!(dir.entries.len(), 2);
    let names: Vec<_> = dir
        .entries
        .iter()
        .filter_map(|e| e.item.as_ref().map(|i| i.name.clone()))
        .collect();
    assert!(names.iter().any(|n| n == b"README.md"));
    assert!(names.iter().any(|n| n == b"src"));
}

#[tokio::test]
async fn write_returns_erofs() {
    let mut adapter = build_adapter().await;
    let err = adapter.write(vec![1, 2, 3], 5, 0).await.unwrap_err();
    match err {
        fskit_rs::Error::Posix(e) => assert_eq!(e, libc::EROFS),
    }
}

#[tokio::test]
async fn getattr_root_returns_directory() {
    let mut adapter = build_adapter().await;
    let attrs = adapter.get_attributes(2).await.unwrap();
    assert_eq!(attrs.r#type, Some(ItemType::Directory as i32));
    assert_eq!(attrs.file_id, Some(2));
}
```

- [ ] **Step 4: Build the test crate**

Run: `cargo build -p ctxfs-fskit --tests`
Expected: compiles. If you get errors about `ItemType` being an enum without a `PartialEq` impl, the test uses `matches!` or `as i32` comparison — check against the trait method return types.

- [ ] **Step 5: Run the tests**

Run: `cargo test -p ctxfs-fskit`
Expected: 7 unit tests (from Task 2) + 7 integration tests (this task) = 14 total, all pass.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets -p ctxfs-fskit`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-fskit/
git commit -m "feat(fskit): implement Filesystem trait on VfsState

FilesystemAdapter wraps Arc<VfsState> and implements fskit_rs::Filesystem
via the translation helpers from adapter.rs. Read-only (writes return
EROFS). Handles the FSKit root inode convention (id=2) vs VfsState
convention (id=1) with a thin mapping function.

All 14 unit + integration tests pass without FSKit installed — the
adapter is testable against a mock VfsState.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: Volume Slug Helper

**Files:**
- Create: `crates/ctxfs-fskit/src/slug.rs`
- Modify: `crates/ctxfs-fskit/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ctxfs-fskit/src/slug.rs`:

```rust
//! Volume slug derivation — produces the `/Volumes/ctxfs/<slug>` directory name
//! for an FSKit mount.

use ctxfs_core::source::{ProviderType, SourceSpec};

/// Produce a volume slug from a `SourceSpec`.
///
/// Slugs are used as directory names under `/Volumes/ctxfs/` and must be:
/// - lowercase ASCII
/// - no path separators
/// - unique per source (so the same package version collides deliberately —
///   two projects mounting `npm:react@19.1.0` share the same volume)
///
/// Examples:
/// - `npm:react@19.1.0` → `npm-react-19.1.0`
/// - `npm:@scope/pkg@1.0.0` → `npm-scope-pkg-1.0.0`
/// - `github:rust-lang/rust@master` → `github-rust-rust-master`
/// - `crate:serde@1.0.219` → `crate-serde-1.0.219`
pub fn volume_slug(source: &SourceSpec) -> String {
    let provider_prefix = match source.provider_type {
        ProviderType::GitHub => "github",
        ProviderType::Npm => "npm",
        ProviderType::PyPI => "pypi",
        ProviderType::Crate => "crate",
    };

    // Flatten `owner/repo` or `@scope/pkg` names to use `-` as separator.
    let name_flat = source
        .name
        .trim_start_matches('@')
        .replace('/', "-")
        .to_lowercase();

    let version_flat = source.version.replace('/', "-");

    format!("{provider_prefix}-{name_flat}-{version_flat}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(provider: ProviderType, name: &str, version: &str) -> SourceSpec {
        SourceSpec {
            provider_type: provider,
            name: name.into(),
            version: version.into(),
            subpath: None,
        }
    }

    #[test]
    fn npm_simple() {
        assert_eq!(
            volume_slug(&spec(ProviderType::Npm, "react", "19.1.0")),
            "npm-react-19.1.0"
        );
    }

    #[test]
    fn npm_scoped() {
        assert_eq!(
            volume_slug(&spec(ProviderType::Npm, "@scope/pkg", "1.0.0")),
            "npm-scope-pkg-1.0.0"
        );
    }

    #[test]
    fn github_owner_repo() {
        assert_eq!(
            volume_slug(&spec(ProviderType::GitHub, "rust-lang/rust", "master")),
            "github-rust-lang-rust-master"
        );
    }

    #[test]
    fn pypi_package() {
        assert_eq!(
            volume_slug(&spec(ProviderType::PyPI, "requests", "2.31.0")),
            "pypi-requests-2.31.0"
        );
    }

    #[test]
    fn crate_package() {
        assert_eq!(
            volume_slug(&spec(ProviderType::Crate, "serde", "1.0.219")),
            "crate-serde-1.0.219"
        );
    }

    #[test]
    fn uppercase_normalized() {
        assert_eq!(
            volume_slug(&spec(ProviderType::GitHub, "Facebook/React", "v19")),
            "github-facebook-react-v19"
        );
    }
}
```

- [ ] **Step 2: Register module**

Update `crates/ctxfs-fskit/src/lib.rs`:

```rust
pub mod adapter;
pub mod auth;
pub mod fs;
pub mod slug;

pub use adapter::FilesystemAdapter;
pub use auth::AuthToken;
pub use fs::CtxfsFsKit;
pub use slug::volume_slug;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ctxfs-fskit slug`
Expected: 6 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-fskit/src/slug.rs crates/ctxfs-fskit/src/lib.rs
git commit -m "feat(fskit): add volume_slug() for /Volumes/ctxfs/ paths

Derives a filesystem-safe slug from a SourceSpec for use as the
FSKit volume mount directory. Two projects mounting the same
source deliberately collide (shared volume, multiple symlinks).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: IPC — Extend mount() RPC with Backend Parameter

**Files:**
- Modify: `crates/ctxfs-ipc/src/service.rs`
- Modify: `crates/ctxfs-ipc/tests/rpc_roundtrip.rs`

- [ ] **Step 1: Update the tarpc service trait**

Edit `crates/ctxfs-ipc/src/service.rs`:

Change:
```rust
async fn mount(source: String, mount_point: String) -> Result<MountInfo, String>;
```

To:
```rust
async fn mount(
    source: String,
    mount_point: String,
    backend: ctxfs_core::Backend,
) -> Result<MountInfo, String>;
```

- [ ] **Step 2: Update the rpc_roundtrip test**

Edit `crates/ctxfs-ipc/tests/rpc_roundtrip.rs`. Find `async fn mount` in the `MockServer` impl and update the signature:

```rust
async fn mount(
    self,
    _: tarpc::context::Context,
    source: String,
    mount_point: String,
    _backend: ctxfs_core::Backend,
) -> Result<MountInfo, String> {
    // ... existing body unchanged ...
}
```

Find the test's client call and add the backend argument:

```rust
let info = client
    .mount(
        tarpc::context::current(),
        "github:test/repo@main".into(),
        "/tmp/mnt".into(),
        ctxfs_core::Backend::Nfs,
    )
    .await
    .unwrap()
    .unwrap();
```

- [ ] **Step 3: Run ipc tests**

Run: `cargo test -p ctxfs-ipc`
Expected: all tests pass. Other crates (daemon, cli) will be broken by this change — fixed in Tasks 6 and 7.

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-ipc/
git commit -m "feat(ipc): add backend parameter to mount() RPC

The CtxfsService::mount RPC now takes an explicit Backend
(Nfs or FsKit). Updated MockServer and rpc_roundtrip test.
Daemon and CLI will be updated in the next commits.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Config — Add fskit_bundle_id Field

**Files:**
- Modify: `crates/ctxfs-core/src/config.rs`

- [ ] **Step 1: Write the failing test**

Add to the `#[cfg(test)] mod tests` block in `crates/ctxfs-core/src/config.rs`:

```rust
#[test]
#[allow(unsafe_code)]
fn from_env_reads_fskit_bundle_id() {
    let prev = std::env::var("CTXFS_FSKIT_BUNDLE_ID").ok();
    unsafe {
        std::env::set_var("CTXFS_FSKIT_BUNDLE_ID", "com.example.fskitbridge.fskitext");
    }

    let config = Config::from_env();

    match prev {
        Some(v) => unsafe { std::env::set_var("CTXFS_FSKIT_BUNDLE_ID", v) },
        None => unsafe { std::env::remove_var("CTXFS_FSKIT_BUNDLE_ID") },
    }

    assert_eq!(
        config.fskit_bundle_id.as_deref(),
        Some("com.example.fskitbridge.fskitext")
    );
}

#[test]
fn default_config_has_no_fskit_bundle_id() {
    let config = Config::default();
    assert!(config.fskit_bundle_id.is_none());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ctxfs-core config`
Expected: compile error — `fskit_bundle_id` doesn't exist.

- [ ] **Step 3: Add the field**

Edit `crates/ctxfs-core/src/config.rs`:

In the struct:
```rust
pub struct Config {
    // ... existing fields ...
    pub default_backend: Option<Backend>,
    /// Bundle ID of the installed FSKitBridge appex. Needed because self-signed
    /// builds use the developer's team prefix instead of fskit-rs's default
    /// (`network.debox.fskitbridge.fskitext`).
    pub fskit_bundle_id: Option<String>,
}
```

In `Default::default()`:
```rust
Self {
    // ... existing ...
    default_backend: None,
    fskit_bundle_id: None,
}
```

In `from_env()`:
```rust
config.fskit_bundle_id = std::env::var("CTXFS_FSKIT_BUNDLE_ID")
    .ok()
    .filter(|s| !s.is_empty());
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ctxfs-core`
Expected: all tests pass (53 old + 2 new = 55).

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-core/src/config.rs
git commit -m "feat(core): add fskit_bundle_id to Config

Adds CTXFS_FSKIT_BUNDLE_ID env var. Needed because self-signed
FSKitBridge installs use the developer's team prefix, not the
fskit-rs default. Phase 0 surfaced this gotcha.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Daemon — FsKitHandle and MountHandle Extension

**Files:**
- Modify: `crates/ctxfs-daemon/Cargo.toml`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

Before adding the full FSKit mount path, extend the daemon's data structures to carry backend-specific state.

- [ ] **Step 1: Add ctxfs-fskit dependency**

Edit `crates/ctxfs-daemon/Cargo.toml`, add to `[dependencies]`:

```toml
ctxfs-fskit = { workspace = true }
fskit-rs = { workspace = true }
```

- [ ] **Step 2: Extend MountHandle**

In `crates/ctxfs-daemon/src/daemon.rs`, near the top where `MountHandle` is defined:

```rust
use ctxfs_core::Backend;
use fskit_rs::session::Session as FsKitSession;

/// State owned by the daemon for an FSKit-backed mount.
/// When dropped, the `FsKitSession` unmounts the volume.
pub(crate) struct FsKitHandle {
    /// The fskit-rs session. Drop-triggers unmount.
    pub(crate) session: FsKitSession,
    /// Absolute path to the FSKit volume, e.g. `/Volumes/ctxfs/npm-react-19.1.0`.
    pub(crate) volume_path: std::path::PathBuf,
}

impl std::fmt::Debug for FsKitHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsKitHandle")
            .field("volume_path", &self.volume_path)
            .finish_non_exhaustive()
    }
}

struct MountHandle {
    info: MountInfo,
    backend: Backend,
    /// For NFS mounts: keeps the NFS server task alive.
    _nfs: Option<NfsServerHandle>,
    /// For FSKit mounts: keeps the session alive; drop triggers unmount.
    _fskit: Option<FsKitHandle>,
}
```

- [ ] **Step 3: Update existing NFS mount construction**

Find where `MountHandle` is constructed in `do_mount()` (the NFS path). Update:

```rust
let handle = MountHandle {
    info: info.clone(),
    backend: Backend::Nfs,
    _nfs: Some(nfs_handle),
    _fskit: None,
};
```

- [ ] **Step 4: Build**

Run: `cargo build -p ctxfs-daemon`
Expected: compiles. The `do_mount` RPC signature will be fixed in the next task; right now it'll complain about the missing `backend` parameter from the IPC trait. If so, add a stub arg:

Inside the `CtxfsService` impl:
```rust
async fn mount(
    self,
    _: tarpc::context::Context,
    source: String,
    mount_point: String,
    _backend: Backend, // will be used in the next task
) -> Result<MountInfo, String> {
    // existing body — still only does NFS for now
    ...
}
```

- [ ] **Step 5: Run daemon tests**

Run: `cargo test -p ctxfs-daemon`
Expected: all tests pass (existing NFS path still works).

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-daemon/
git commit -m "feat(daemon): extend MountHandle with backend + optional FsKitHandle

MountHandle now tracks which backend owns the mount and holds either
the NFS handle or an FsKitHandle (with the fskit-rs Session). The IPC
trait's mount() signature is updated with the new backend parameter,
but the daemon still only executes the NFS path — the FSKit
implementation comes in the next commit.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Daemon — FSKit Mount Path

**Files:**
- Create: `crates/ctxfs-daemon/src/fskit_mount.rs`
- Modify: `crates/ctxfs-daemon/src/lib.rs`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

- [ ] **Step 1: Create the fskit_mount module**

Create `crates/ctxfs-daemon/src/fskit_mount.rs`:

```rust
//! FSKit mount orchestration — builds the adapter, starts the session, returns
//! the handle the daemon tracks.

use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::source::SourceSpec;
use ctxfs_fskit::{volume_slug, FilesystemAdapter};
use ctxfs_manifest::Snapshot;
use ctxfs_vfs::VfsState;
use fskit_rs::MountOptions;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::info;

use crate::daemon::FsKitHandle;

/// Errors specific to starting an FSKit mount.
#[derive(Debug, thiserror::Error)]
pub enum FsKitMountError {
    #[error("fskit_bundle_id is not configured. Set CTXFS_FSKIT_BUNDLE_ID or ensure the daemon Config has it")]
    MissingBundleId,
    #[error("failed to build VfsState: {0}")]
    Vfs(String),
    #[error("failed to start fskit-rs session: {0}")]
    Session(String),
    #[error("failed to create /Volumes/ctxfs/ directory: {0}")]
    MountDir(String),
}

/// Start an FSKit mount for `source` using the given provider and snapshot.
///
/// Returns a handle the daemon stores in its `MountHandle` plus the volume path
/// that the CLI uses to create a symlink. Dropping the returned `FsKitHandle`
/// unmounts the volume.
///
/// Preconditions:
/// - `/Volumes/ctxfs/` must exist (created by `ctxfs setup install-fskit`)
/// - `FSKitBridge.app` must be installed and the extension enabled
/// - `bundle_id` must match the installed appex's bundle identifier
pub async fn start_fskit_mount(
    source: &SourceSpec,
    provider: SharedProvider,
    cache: Arc<BlobCache>,
    snapshot: Snapshot,
    subpath: Option<String>,
    bundle_id: &str,
) -> Result<FsKitHandle, FsKitMountError> {
    // 1. Build the VFS.
    let vfs = Arc::new(
        VfsState::new(provider, cache, snapshot, subpath)
            .await
            .map_err(|e| FsKitMountError::Vfs(e.to_string()))?,
    );

    // 2. Derive the volume slug and mount path.
    let slug = volume_slug(source);
    let volume_path = PathBuf::from("/Volumes/ctxfs").join(&slug);

    // 3. Ensure /Volumes/ctxfs/ exists and create the per-volume directory.
    let parent = PathBuf::from("/Volumes/ctxfs");
    if !parent.exists() {
        return Err(FsKitMountError::MountDir(format!(
            "{} does not exist — run `ctxfs setup install-fskit`",
            parent.display()
        )));
    }
    if !volume_path.exists() {
        std::fs::create_dir(&volume_path).map_err(|e| {
            FsKitMountError::MountDir(format!("create {}: {e}", volume_path.display()))
        })?;
    }

    // 4. Build the adapter.
    let adapter = FilesystemAdapter::new(vfs, slug.clone());

    // 5. Start the fskit-rs session.
    let opts = MountOptions {
        fskit_id: bundle_id.to_string(),
        mount_point: volume_path.clone(),
        force: true,
    };
    info!(
        "starting FSKit mount at {} (bundle_id={})",
        volume_path.display(),
        bundle_id
    );
    let session = fskit_rs::mount(adapter, opts)
        .await
        .map_err(|e| FsKitMountError::Session(e.to_string()))?;

    Ok(FsKitHandle {
        session,
        volume_path,
    })
}
```

- [ ] **Step 2: Register the module**

Edit `crates/ctxfs-daemon/src/lib.rs`:

```rust
pub mod daemon;
pub mod fskit_mount;
pub mod mount_state;
```

Make `FsKitHandle` and `MountHandle` visible to the new module:

In `crates/ctxfs-daemon/src/daemon.rs`, change:
```rust
pub(crate) struct FsKitHandle { ... }
```
to:
```rust
pub struct FsKitHandle {
    pub(crate) session: FsKitSession,
    pub(crate) volume_path: std::path::PathBuf,
}
```

Note: the inner fields stay `pub(crate)` — only the struct itself is public.

- [ ] **Step 3: Wire dispatch into do_mount**

In `crates/ctxfs-daemon/src/daemon.rs`, find the `CtxfsService::mount` RPC impl. After the snapshot has been fetched and parsed (the part that's shared with NFS), branch on backend:

```rust
async fn mount(
    self,
    _: tarpc::context::Context,
    source: String,
    mount_point: String,
    backend: Backend,
) -> Result<MountInfo, String> {
    // ... existing code that resolves source, fetches snapshot, etc.
    // stop before the part that constructs CtxfsNfs / spawns NFS server

    match backend {
        Backend::Nfs => {
            // existing NFS path — unchanged
            // ... build CtxfsNfs, spawn, build MountInfo with nfs_port=Some(...) ...
            // Construct MountHandle with backend=Backend::Nfs, _nfs=Some(...), _fskit=None.
        }
        Backend::FsKit => {
            let bundle_id = self
                .config
                .fskit_bundle_id
                .as_deref()
                .ok_or_else(|| {
                    "CTXFS_FSKIT_BUNDLE_ID not set — cannot start FSKit mount".to_string()
                })?;

            let handle = crate::fskit_mount::start_fskit_mount(
                &source_spec,
                provider.clone(),
                self.cache.clone(),
                snapshot.clone(),
                subpath.clone(),
                bundle_id,
            )
            .await
            .map_err(|e| format!("fskit mount failed: {e}"))?;

            let volume_path_str = handle.volume_path.to_string_lossy().to_string();
            let info = MountInfo {
                id: source.clone(),
                source: source.clone(),
                mount_point: mount_point.clone(),
                commit_sha: snapshot.commit_sha.clone(),
                status: MountStatus::Mounted,
                mounted_at: chrono::Utc::now().to_rfc3339(),
                nfs_port: None,
                backend: Backend::FsKit,
                volume_path: Some(volume_path_str.clone()),
                symlink_paths: if mount_point == volume_path_str {
                    vec![]
                } else {
                    vec![mount_point.clone()]
                },
            };

            let handle_entry = MountHandle {
                info: info.clone(),
                backend: Backend::FsKit,
                _nfs: None,
                _fskit: Some(handle),
            };

            let mut mounts = self.mounts.write().await;
            mounts.insert(source.clone(), handle_entry);

            Ok(info)
        }
    }
}
```

**Important**: the exact variable names (`source_spec`, `provider`, `snapshot`, `subpath`) must match the names used in the existing NFS code path. Read `daemon.rs` carefully before editing — adapt the snippet to whatever names the existing flow uses. Reuse the already-parsed source/snapshot; do not duplicate the fetch logic.

- [ ] **Step 4: Build**

Run: `cargo build -p ctxfs-daemon`
Expected: compiles. If the error is "cannot find value X in scope", it means a variable in the existing flow has a different name than expected — adjust the snippet.

- [ ] **Step 5: Run daemon tests**

Run: `cargo test -p ctxfs-daemon`
Expected: all tests pass. Existing NFS paths still work.

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-daemon/
git commit -m "feat(daemon): dispatch FSKit mounts via fskit_mount helper

do_mount() now branches on Backend — Nfs path unchanged, FsKit path
calls fskit_mount::start_fskit_mount() which builds the FilesystemAdapter,
starts the fskit-rs session, and creates the /Volumes/ctxfs/<slug>
volume directory. The returned FsKitHandle (owning the Session) is
stored in MountHandle; drop triggers unmount.

Requires CTXFS_FSKIT_BUNDLE_ID in the daemon environment because
fskit-rs's default bundle ID doesn't match self-signed builds.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: CLI — Backend Dispatch and Symlink Management

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs`
- Modify: `crates/ctxfs-cli/src/deps/mount.rs`

- [ ] **Step 1: Update client.mount() calls to pass backend**

In `crates/ctxfs-cli/src/main.rs`, find the `handle_mount` function. Currently it calls `client.mount(source, mount_point)` (or similar). Locate the `backend` variable already derived by `detect_backend()` and pass it:

```rust
let info = client
    .mount(
        tarpc::context::current(),
        source.clone(),
        mount_point.to_string_lossy().into_owned(),
        backend,
    )
    .await
    .map_err(anyhow::Error::from)?
    .map_err(|e| anyhow::anyhow!(e))?;
```

- [ ] **Step 2: Handle FSKit response: create symlink**

After the mount returns, check if `info.backend == Backend::FsKit` AND `info.volume_path` is set AND user-requested `mount_point != info.volume_path`. If so, create a symlink:

```rust
use crate::symlink;
use ctxfs_core::Backend;

if info.backend == Backend::FsKit {
    if let Some(volume_path) = info.volume_path.as_deref() {
        let user_path = std::path::Path::new(&mount_point_str);
        let volume = std::path::Path::new(volume_path);

        if user_path != volume {
            // Canonicalize parent before symlinking.
            match symlink::create_symlink(user_path, volume) {
                Ok(created_at) => {
                    println!(
                        "Mounted FSKit volume at {}",
                        volume.display()
                    );
                    println!("Linked from: {}", created_at.display());
                }
                Err(e) => {
                    eprintln!(
                        "warning: mounted at {} but failed to create symlink {}: {e}",
                        volume.display(),
                        user_path.display()
                    );
                }
            }
        } else {
            println!("Mounted FSKit volume at {}", volume.display());
        }
    }
} else {
    // existing NFS output — unchanged
}
```

- [ ] **Step 3: Unmount — resolve symlink, remove after daemon ack**

Find `handle_unmount`. Before calling `client.unmount`, resolve the user path through a ctxfs symlink if applicable:

```rust
use crate::symlink;

let target_path = std::path::Path::new(&target);
let canonical_target = if symlink::is_ctxfs_symlink(target_path) {
    symlink::resolve_ctxfs_path(target_path)
} else {
    target_path.to_path_buf()
};
let daemon_target = canonical_target.to_string_lossy().into_owned();

client
    .unmount(tarpc::context::current(), daemon_target.clone())
    .await?
    .map_err(|e| anyhow::anyhow!(e))?;

// After successful daemon unmount, remove the symlink if the original target was one.
if target_path != canonical_target.as_path() {
    let _ = symlink::safe_remove_symlink(target_path);
}

println!("Unmounted {}", target);
```

- [ ] **Step 4: Update batch mount in deps/mount.rs**

In `crates/ctxfs-cli/src/deps/mount.rs`, find the `client.mount(...)` call and add the backend parameter. The batch-mount path doesn't (yet) need FSKit-aware symlink handling because it already passes specific `-d <dir>` paths — but it does need the new signature. Pass the `backend` from the outer command handler down into `batch_mount`:

Update `pub async fn batch_mount(...)` signature to accept `backend: Backend`. Threading it through to the inner RPC call.

At each `client.mount(...)` call inside:
```rust
let result = client
    .mount(
        tarpc::context::current(),
        source.clone(),
        mount_point.clone(),
        backend,
    )
    .await;
```

If the batch path already has a `backend` variable: reuse it. If it uses `Backend::Nfs` hard-coded: add a parameter.

- [ ] **Step 5: Build**

Run: `cargo build -p ctxfs-cli`
Expected: compiles.

- [ ] **Step 6: Run tests**

Run: `cargo test -p ctxfs-cli`
Expected: all unit tests pass. (e2e tests may fail without GITHUB_TOKEN / without FSKitBridge — that's expected.)

- [ ] **Step 7: Run full workspace tests**

Run: `cargo test`
Expected: unit + integration tests pass. Ignore any tests marked `#[ignore]` requiring FSKit installed.

- [ ] **Step 8: Run clippy**

Run: `cargo clippy --all-targets --tests`
Expected: clean.

- [ ] **Step 9: Commit**

```bash
git add crates/ctxfs-cli/
git commit -m "feat(cli): wire FSKit backend dispatch and symlink management

handle_mount passes the detected backend to client.mount(). When the
daemon returns an FSKit MountInfo with volume_path and a different
user -p path, the CLI creates a symlink from the user path to the
/Volumes/ctxfs/<slug> volume path.

handle_unmount resolves the user-supplied path through a ctxfs
symlink before asking the daemon to unmount, then removes the
symlink after daemon acknowledgement.

batch_mount (deps) now accepts a backend parameter and threads it
through to the per-source client.mount() calls.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: Manual Smoke Test + Documentation

**Files:**
- Create: `docs/poc/fskit-phase1-smoke-test.md`

- [ ] **Step 1: Set up environment**

Ensure:
- `FSKitBridge.app` installed, extension enabled (see `docs/poc/fskit-poc/README.md`)
- `/Volumes/ctxfs/` exists and is writable by the current user
- Export `CTXFS_FSKIT_BUNDLE_ID=com.YOURID.fskitbridge.fskitext`
- `GITHUB_TOKEN` set (optional but helps with rate limits)

```sh
export CTXFS_FSKIT_BUNDLE_ID=$(pluginkit -m -p com.apple.fskit.fsmodule | grep -i fskitbridge | awk '{print $1}')
sudo mkdir -p /Volumes/ctxfs && sudo chown $(whoami):staff /Volumes/ctxfs
```

- [ ] **Step 2: Build and run the daemon**

```sh
cd /Users/derekxwang/Development/incubator/ContextFS/ctxfs
cargo build --release
./target/release/ctxfs daemon start &
sleep 1
```

- [ ] **Step 3: Mount a tiny repo via FSKit**

```sh
./target/release/ctxfs mount \
    github:octocat/Hello-World@master \
    -p ./test-mnt \
    --backend fskit
```

Expected output:
```
Mounted FSKit volume at /Volumes/ctxfs/github-octocat-hello-world-master
Linked from: /Users/derekxwang/.../ctxfs/test-mnt
```

- [ ] **Step 4: Verify mount and read**

```sh
ls /Volumes/ctxfs/
ls ./test-mnt/
cat ./test-mnt/README
```

Expected: reads succeed, `README` content visible.

- [ ] **Step 5: Verify mount shows as fskit in kernel**

```sh
mount | grep ctxfs
```

Expected: `...on /Volumes/ctxfs/github-octocat-hello-world-master (fskitbridge, ..., fskit, mounted by <user>)`

- [ ] **Step 6: Unmount**

```sh
./target/release/ctxfs unmount ./test-mnt
```

Expected: `Unmounted ./test-mnt`. The symlink should be gone. The volume directory under `/Volumes/ctxfs/` may remain (empty) — that's fine.

- [ ] **Step 7: Write up findings**

Create `docs/poc/fskit-phase1-smoke-test.md` summarizing:
- Date tested
- macOS version
- Whether each step succeeded
- Any gotchas encountered (update the plan / spec if needed)
- Latency measurement for `cat` / `grep` (compare to NFS for the same repo)

- [ ] **Step 8: Commit**

```bash
git add docs/poc/fskit-phase1-smoke-test.md
git commit -m "docs: add FSKit Phase 1 end-to-end smoke test results

Records the first successful ctxfs mount ... --backend fskit
on a real GitHub repo, including latency measurements and any
gotchas surfaced during end-to-end testing.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Summary

| Task | Component | What it does |
|---|---|---|
| 1 | ctxfs-fskit Cargo.toml | Pull in fskit-rs |
| 2 | ctxfs-fskit/adapter.rs | Pure translation helpers (NodeAttr ↔ ItemAttributes, errors) |
| 3 | ctxfs-fskit/adapter.rs | FilesystemAdapter: full fskit_rs::Filesystem impl over VfsState |
| 4 | ctxfs-fskit/slug.rs | volume_slug() — derives /Volumes/ctxfs/<slug> name |
| 5 | ctxfs-ipc | mount() RPC gains Backend parameter |
| 6 | ctxfs-core/config.rs | CTXFS_FSKIT_BUNDLE_ID env var |
| 7 | ctxfs-daemon/daemon.rs | MountHandle gains backend + FsKitHandle (IPC signature updated) |
| 8 | ctxfs-daemon/fskit_mount.rs | start_fskit_mount() — builds adapter, starts session |
| 9 | ctxfs-cli | Pass backend to mount RPC; create/remove symlinks on FSKit mounts |
| 10 | docs/poc | Manual end-to-end smoke test + findings writeup |

**Deferred to Phase 2 / 1.5:**
- Auth token handshake (requires FSKitBridge Swift modifications)
- Finder polish (custom icon, volume display name attributes)
- `ctxfs setup default-backend` persistence
- `ctxfs setup uninstall-fskit`
- Mount state persistence integration (the `MountStateFile` from the previous Phase 1 is already written but not yet called from `do_mount()` — wire it up after Task 8 is working)

After Phase 1, users on macOS 26+ with FSKitBridge installed can mount repos without sudo and without Full Disk Access. That's the feature gate.
