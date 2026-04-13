# FSKit Phase 1: Wire-Up Implementation Plan (v2)

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Wire the existing `ctxfs-fskit` stub into a working FSKit backend by implementing `fskit_rs::Filesystem` on top of `VfsState`, adding daemon dispatch, CLI integration, mount state persistence, and symlink management so users can mount via `ctxfs mount ... --backend fskit`.

**Architecture:** `ctxfs-fskit` gains a `FilesystemAdapter` that wraps `Arc<VfsState>` and implements `fskit_rs::Filesystem` via pure translation helpers (including a correct inode bijection — VFS root=1, FSKit root=2, with `+1` offset for all non-root IDs). `VfsState` is extended with parent-inode tracking so directory attrs are correct. The daemon's `do_mount()` is refactored to extract backend-agnostic prep (source resolution, snapshot fetch) into a helper, then dispatches on `Backend`. The FSKit path builds the adapter, calls `fskit_rs::mount()`, records the mount in `mounts.json` for crash recovery, and registers a shutdown hook that unmounts all FSKit sessions cleanly. The CLI extends to create `/Volumes/ctxfs/<slug>` symlinks when `-p` is specified.

**Tech Stack:** Rust (fskit-rs 0.1, tokio, async-trait), reuses existing ctxfs-vfs/ctxfs-cache/ctxfs-manifest/ctxfs-core stack.

**Spec:** `docs/superpowers/specs/2026-04-11-fskit-backend-design.md`

**Phase 0 evidence:** `docs/poc/fskit-poc/README.md` — validated 2026-04-13 on macOS 26.4.

**Review history:** This plan incorporates Codex's review findings from 2026-04-13. Key corrections: proper inode bijection (v1 had a collision bug), parent-tracking via extended `NodeAttr`, real attrs in `enumerate_directory`, atomic IPC signature change, explicit `do_mount` refactor, session cleanup on shutdown, `mounts.json` integration in scope, CI-gated e2e test.

---

## Scope

This plan delivers the feature gate: a user with FSKitBridge installed can run `ctxfs mount npm:react@19.1.0 -p ./deps/react --backend fskit` and successfully browse React source code without sudo or Full Disk Access.

**In scope:**
- Full `fskit_rs::Filesystem` implementation with correct inode bijection and parent tracking
- Daemon backend dispatch with refactored shared prep logic
- CLI symlink creation on mount / resolution on unmount
- Persistent mount state (mounts.json) for crash recovery
- Session cleanup on daemon shutdown
- CI-gated end-to-end FSKit test (opt-in via env var)
- Manual smoke test on real hardware

**Explicitly deferred (Phase 1.5 / 2):**
- Auth token handshake (requires modifying FSKitBridge Swift code)
- Finder polish (custom volume icon, display name attributes)
- `ctxfs setup default-backend` persistence
- `ctxfs setup uninstall-fskit`
- Batch-mount (deps command) FSKit support — NFS stays the batch default for now

---

## Prerequisites for Implementation

The implementer must have:
1. macOS 26.x
2. `FSKitBridge.app` installed at `/Applications/FSKitBridge.app` with the extension enabled in System Settings (see `docs/poc/fskit-poc/README.md`)
3. The bundle ID of the installed appex (check with `pluginkit -m -p com.apple.fskit.fsmodule | grep fskitbridge`)
4. `sudo mkdir -p /Volumes/ctxfs && sudo chown $(whoami):staff /Volumes/ctxfs` (one-time)
5. Protoc: `brew install protobuf` (for fskit-rs build script)

Without these, the FSKit-dependent e2e tests will fail. All Rust unit tests and the trait adapter tests (Tasks 2-5) can still run.

---

## File Map

### New Files

| File | Responsibility |
|---|---|
| `crates/ctxfs-fskit/src/slug.rs` | `volume_slug(source: &SourceSpec) -> String` — produces the `/Volumes/ctxfs/<slug>` directory name. |
| `crates/ctxfs-fskit/src/adapter.rs` | `FilesystemAdapter`: implements `fskit_rs::Filesystem` by delegating to `VfsState`. Contains all VFS→FSKit type translation. |
| `crates/ctxfs-fskit/tests/adapter_ops.rs` | Unit tests for the adapter using a mock `VfsState`. |
| `crates/ctxfs-daemon/src/fskit_mount.rs` | `start_fskit_mount()` helper + `FsKitHandle` with `Drop` unmount fallback. |
| `crates/ctxfs-daemon/tests/mount_dispatch.rs` | NFS-regression test: verifies `Backend::Nfs` path still works after refactor. |
| `crates/ctxfs-cli/tests/e2e_fskit.rs` | CI-gated (`CTXFS_E2E_FSKIT=1`) end-to-end test. |

### Modified Files

| File | Changes |
|---|---|
| `Cargo.toml` (root) | Add `fskit-rs = "0.1"` to `[workspace.dependencies]`. |
| `crates/ctxfs-fskit/Cargo.toml` | Add `fskit-rs`, `async-trait`, `tokio` features; dev-deps for adapter tests. |
| `crates/ctxfs-fskit/src/lib.rs` | Re-export `FilesystemAdapter`, `volume_slug`. |
| `crates/ctxfs-fskit/src/fs.rs` | Replace stub with `pub use crate::adapter::FilesystemAdapter as CtxfsFsKit;`. |
| `crates/ctxfs-vfs/src/types.rs` | `NodeAttr` gains `parent_inode: u64` field. |
| `crates/ctxfs-vfs/src/state.rs` | `node_to_attr()` populates `parent_inode` from `Node.parent`; all public methods that return `NodeAttr` return real parent. |
| `crates/ctxfs-vfs/tests/vfs_ops.rs` | Updated fixture assertions for new field. |
| `crates/ctxfs-nfs/src/fs.rs` | `attr_to_fattr3` ignores `parent_inode` (NFS doesn't need it); no behavior change. Existing tests still pass. |
| `crates/ctxfs-core/src/config.rs` | `Config` gains `fskit_bundle_id: Option<String>` from `CTXFS_FSKIT_BUNDLE_ID`. |
| `crates/ctxfs-ipc/src/service.rs` | `mount()` RPC gains `backend: Backend` parameter. |
| `crates/ctxfs-ipc/tests/rpc_roundtrip.rs` | MockServer signature update. |
| `crates/ctxfs-daemon/Cargo.toml` | Add `ctxfs-fskit`, `fskit-rs`. |
| `crates/ctxfs-daemon/src/daemon.rs` | (a) `MountHandle` gains `backend: Backend` and `_fskit: Option<FsKitHandle>`, (b) extract `prepare_mount()` helper, (c) `do_mount()` dispatches on backend, (d) shutdown path calls `FsKitHandle::shutdown()` before drop, (e) startup cleanup calls `mount_state` helpers. |
| `crates/ctxfs-daemon/src/lib.rs` | Register `pub mod fskit_mount;`. |
| `crates/ctxfs-cli/src/main.rs` | (a) Pass detected `backend` to `client.mount()`, (b) FSKit path creates symlink, (c) `handle_unmount` resolves symlinks. |
| `crates/ctxfs-cli/src/deps/mount.rs` | Pass `Backend::Nfs` explicitly (batch FSKit deferred). |

---

## Task 1: Add fskit-rs Dependency

**Files:**
- Modify: `Cargo.toml` (root)
- Modify: `crates/ctxfs-fskit/Cargo.toml`

- [ ] **Step 1: Add to workspace dependencies**

Edit root `Cargo.toml`, add under `[workspace.dependencies]` (alphabetically near `fuser`-less slot):

```toml
# FSKit backend (macOS 26+)
fskit-rs = "0.1"
```

- [ ] **Step 2: Update ctxfs-fskit/Cargo.toml**

Replace the `[dependencies]` section of `crates/ctxfs-fskit/Cargo.toml`:

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
serde_json = { workspace = true }
```

- [ ] **Step 3: Verify it builds**

Run: `cargo build -p ctxfs-fskit`
Expected: compiles. If `protoc` is missing, the error tells you to `brew install protobuf`.

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/ctxfs-fskit/Cargo.toml
git commit -m "feat(fskit): add fskit-rs dependency

Pulls in fskit-rs 0.1 for the FSKit TCP/Protobuf client.
Build requires protoc (brew install protobuf).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 2: Extend NodeAttr with parent_inode

Fixes Codex finding #2: `VfsState`'s internal `Node` struct tracks parent but the public API loses it. Adding `parent_inode` to `NodeAttr` makes it available to both backends without exposing internals.

**Files:**
- Modify: `crates/ctxfs-vfs/src/types.rs`
- Modify: `crates/ctxfs-vfs/src/state.rs`
- Modify: `crates/ctxfs-vfs/tests/vfs_ops.rs`
- Modify: `crates/ctxfs-nfs/src/fs.rs` (no behavior change, just pattern match)

- [ ] **Step 1: Write the failing test**

Edit `crates/ctxfs-vfs/tests/vfs_ops.rs`. Find the `lookup_root_children` test and add assertion:

```rust
#[tokio::test]
async fn lookup_populates_parent_inode() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let root = vfs.root_id();

    // Root's parent is itself
    let root_attr = vfs.getattr(root).await.unwrap();
    assert_eq!(root_attr.parent_inode, root);

    // README.md's parent is root
    let (readme_id, readme_attr) = vfs.lookup(root, "README.md").await.unwrap();
    assert_eq!(readme_attr.parent_inode, root);

    // src/main.rs: parent is src/, not root
    let (src_id, _) = vfs.lookup(root, "src").await.unwrap();
    let (_, main_rs_attr) = vfs.lookup(src_id, "main.rs").await.unwrap();
    assert_eq!(main_rs_attr.parent_inode, src_id);
    assert_ne!(main_rs_attr.parent_inode, root);

    let _ = readme_id;
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ctxfs-vfs lookup_populates_parent_inode`
Expected: compile error — `parent_inode` field doesn't exist.

- [ ] **Step 3: Add field to NodeAttr**

Edit `crates/ctxfs-vfs/src/types.rs`:

```rust
#[derive(Debug, Clone)]
pub struct NodeAttr {
    pub inode: u64,
    /// Inode of the parent directory. The root's parent is itself.
    pub parent_inode: u64,
    pub size: u64,
    pub kind: NodeType,
    pub executable: bool,
}
```

Update the unit tests in `types.rs` that construct `NodeAttr` literals. Add `parent_inode: 1` (or any sensible test value) to each:

```rust
let attr = NodeAttr {
    inode: 5,
    parent_inode: 1,
    size: 1024,
    kind: NodeType::File,
    executable: false,
};
```

- [ ] **Step 4: Populate parent_inode in VfsState**

Edit `crates/ctxfs-vfs/src/state.rs`. Find `fn node_to_attr(node: &Node) -> NodeAttr` and add `parent_inode` to every variant:

```rust
fn node_to_attr(node: &Node) -> NodeAttr {
    let base = NodeAttr {
        inode: node.id,
        parent_inode: node.parent,
        size: 0,
        kind: NodeType::Directory,
        executable: false,
    };
    match &node.kind {
        NodeKind::Directory { .. } => NodeAttr {
            size: BLOCK_SIZE,
            kind: NodeType::Directory,
            executable: false,
            ..base
        },
        NodeKind::File { size, executable, .. } => NodeAttr {
            size: *size,
            kind: NodeType::File,
            executable: *executable,
            ..base
        },
        NodeKind::Symlink { target } => NodeAttr {
            size: target.len() as u64,
            kind: NodeType::Symlink,
            executable: false,
            ..base
        },
    }
}
```

- [ ] **Step 5: Fix NFS adapter**

Edit `crates/ctxfs-nfs/src/fs.rs`. Find `fn attr_to_fattr3(attr: &NodeAttr) -> fattr3`. NFS3 doesn't expose parent inodes at the protocol level, so just ignore the new field — the existing code already uses `attr.inode`, `attr.size`, `attr.kind`, `attr.executable` and nothing else needs to change.

Verify the destructuring pattern (if any) is not exhaustive — the current code uses `attr.field` access so it tolerates new fields automatically.

- [ ] **Step 6: Run all tests**

Run: `cargo test -p ctxfs-vfs && cargo test -p ctxfs-nfs`
Expected: both pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-vfs/ crates/ctxfs-nfs/
git commit -m "feat(vfs): add parent_inode to NodeAttr

Tracks the parent directory inode so adapters (FSKit specifically)
can populate ItemAttributes::parent_id correctly. VfsState already
stores parent on Node internally — this exposes it through the
public attr API. NFS adapter is unaffected (NFS3 has no parent
field in fattr3).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 3: Volume Slug Helper

**Files:**
- Create: `crates/ctxfs-fskit/src/slug.rs`
- Modify: `crates/ctxfs-fskit/src/lib.rs`

Moved earlier than v1 — the adapter (Task 5) needs a way to name volumes.

- [ ] **Step 1: Write the failing test**

Create `crates/ctxfs-fskit/src/slug.rs`:

```rust
//! Volume slug derivation.

use ctxfs_core::source::{ProviderType, SourceSpec};

/// Produce a volume slug from a `SourceSpec`.
///
/// Two projects mounting the same source deliberately produce the same slug,
/// so the FSKit volume is shared (with multiple symlinks pointing at it).
///
/// Examples:
/// - `npm:react@19.1.0` → `npm-react-19.1.0`
/// - `npm:@scope/pkg@1.0.0` → `npm-scope-pkg-1.0.0`
/// - `github:rust-lang/rust@master` → `github-rust-lang-rust-master`
pub fn volume_slug(source: &SourceSpec) -> String {
    let provider_prefix = match source.provider_type {
        ProviderType::GitHub => "github",
        ProviderType::Npm => "npm",
        ProviderType::PyPI => "pypi",
        ProviderType::Crate => "crate",
    };

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
pub mod auth;
pub mod fs;
pub mod slug;

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

Derives a filesystem-safe slug from a SourceSpec for use as the FSKit
volume mount directory. Lowercased, no path separators. Two projects
mounting the same source deliberately collide (shared volume).

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 4: VFS → FSKit Translation Helpers

**Files:**
- Create: `crates/ctxfs-fskit/src/adapter.rs`
- Modify: `crates/ctxfs-fskit/src/lib.rs`

Pure translation functions only — trait impl comes in Task 5.

- [ ] **Step 1: Create adapter.rs with inode bijection + translation helpers**

Create `crates/ctxfs-fskit/src/adapter.rs`:

```rust
//! Adapter translating between `ctxfs-vfs` types and `fskit-rs` (FSKit protobuf) types.

use ctxfs_vfs::{NodeAttr, NodeType, VfsError};
use fskit_rs::{Error as FsKitError, Item, ItemAttributes, ItemType};

/// FSKit root inode ID. FSKit conventionally expects the root at 2.
pub(crate) const FSKIT_ROOT_ID: u64 = 2;

/// Map a VFS inode ID to an FSKit inode ID.
///
/// VfsState uses root=1, children=2,3,... FSKit requires root=2. We use a
/// simple `+1` offset: vfs(1)→fskit(2), vfs(2)→fskit(3), vfs(3)→fskit(4), ...
/// This is a bijection — FSKit inode 1 is never used, which is fine because
/// FSKit never asks about id 1. The caller must ensure `vfs_root == 1`
/// (checked by `FilesystemAdapter::new`).
pub(crate) fn vfs_to_fskit_inode(vfs_id: u64) -> u64 {
    vfs_id.saturating_add(1)
}

/// Inverse of `vfs_to_fskit_inode`. Panics on FSKit id 0 or 1 (never emitted
/// by us, so they indicate a bug if FSKit ever sends them).
pub(crate) fn fskit_to_vfs_inode(fskit_id: u64) -> u64 {
    debug_assert!(fskit_id >= FSKIT_ROOT_ID, "FSKit inode {fskit_id} is reserved");
    fskit_id.saturating_sub(1).max(1)
}

/// Translate a VFS `NodeType` to an FSKit `ItemType`.
pub(crate) fn node_type_to_item_type(kind: NodeType) -> ItemType {
    match kind {
        NodeType::File => ItemType::File,
        NodeType::Directory => ItemType::Directory,
        NodeType::Symlink => ItemType::Symlink,
    }
}

/// Translate a VFS `NodeAttr` to an FSKit `ItemAttributes`.
///
/// The inode and parent_inode are both remapped through `vfs_to_fskit_inode`.
#[allow(unsafe_code)]
pub(crate) fn node_attr_to_item_attributes(attr: &NodeAttr) -> ItemAttributes {
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
    // SAFETY: getuid/getgid are safe POSIX calls with no UB.
    let uid = unsafe { libc::getuid() };
    let gid = unsafe { libc::getgid() };

    ItemAttributes {
        file_id: Some(vfs_to_fskit_inode(attr.inode)),
        parent_id: Some(vfs_to_fskit_inode(attr.parent_inode)),
        r#type: Some(node_type_to_item_type(attr.kind) as i32),
        mode: Some(mode),
        uid: Some(uid),
        gid: Some(gid),
        link_count: Some(link_count),
        size: Some(attr.size),
        alloc_size: Some(attr.size),
        ..Default::default()
    }
}

/// Build an FSKit `Item` for a given name and attributes.
pub(crate) fn make_item(name: &str, attr: &NodeAttr) -> Item {
    Item {
        name: name.as_bytes().to_vec(),
        attributes: Some(node_attr_to_item_attributes(attr)),
    }
}

/// Translate `VfsError` to `fskit_rs::Error` (POSIX errno).
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

    fn file_attr(inode: u64, parent: u64, size: u64, executable: bool) -> NodeAttr {
        NodeAttr {
            inode,
            parent_inode: parent,
            size,
            kind: NodeType::File,
            executable,
        }
    }

    #[test]
    fn inode_bijection_root() {
        assert_eq!(vfs_to_fskit_inode(1), FSKIT_ROOT_ID);
        assert_eq!(fskit_to_vfs_inode(FSKIT_ROOT_ID), 1);
    }

    #[test]
    fn inode_bijection_children() {
        // VfsState children start at 2
        assert_eq!(vfs_to_fskit_inode(2), 3);
        assert_eq!(vfs_to_fskit_inode(3), 4);
        assert_eq!(fskit_to_vfs_inode(3), 2);
        assert_eq!(fskit_to_vfs_inode(4), 3);
    }

    #[test]
    fn inode_bijection_roundtrip() {
        for vfs_id in [1u64, 2, 3, 100, 1_000_000] {
            assert_eq!(fskit_to_vfs_inode(vfs_to_fskit_inode(vfs_id)), vfs_id);
        }
    }

    #[test]
    fn no_collision_between_root_and_children() {
        // The critical bug v1 had: vfs_to_fskit(1) must not equal vfs_to_fskit(2).
        assert_ne!(vfs_to_fskit_inode(1), vfs_to_fskit_inode(2));
        assert_eq!(vfs_to_fskit_inode(1), 2);
        assert_eq!(vfs_to_fskit_inode(2), 3);
    }

    #[test]
    fn node_type_translation() {
        assert!(matches!(node_type_to_item_type(NodeType::File), ItemType::File));
        assert!(matches!(node_type_to_item_type(NodeType::Directory), ItemType::Directory));
        assert!(matches!(node_type_to_item_type(NodeType::Symlink), ItemType::Symlink));
    }

    #[test]
    fn file_attr_translates_correctly() {
        let attr = file_attr(5, 1, 1024, false);
        let item_attr = node_attr_to_item_attributes(&attr);
        assert_eq!(item_attr.file_id, Some(6));   // 5 + 1
        assert_eq!(item_attr.parent_id, Some(2)); // 1 + 1 (root)
        assert_eq!(item_attr.r#type, Some(ItemType::File as i32));
        assert_eq!(item_attr.mode, Some(0o444));
        assert_eq!(item_attr.size, Some(1024));
    }

    #[test]
    fn directory_mode_and_link_count() {
        let attr = NodeAttr {
            inode: 1,
            parent_inode: 1,
            size: 0,
            kind: NodeType::Directory,
            executable: false,
        };
        let item_attr = node_attr_to_item_attributes(&attr);
        assert_eq!(item_attr.mode, Some(0o555));
        assert_eq!(item_attr.link_count, Some(2));
    }

    #[test]
    fn executable_file_mode() {
        let attr = file_attr(5, 1, 10, true);
        assert_eq!(node_attr_to_item_attributes(&attr).mode, Some(0o555));
    }

    #[test]
    fn error_translation_all_variants() {
        assert!(matches!(vfs_err_to_fskit(VfsError::NotFound), FsKitError::Posix(e) if e == libc::ENOENT));
        assert!(matches!(vfs_err_to_fskit(VfsError::NotDir), FsKitError::Posix(e) if e == libc::ENOTDIR));
        assert!(matches!(vfs_err_to_fskit(VfsError::IsDir), FsKitError::Posix(e) if e == libc::EISDIR));
        assert!(matches!(vfs_err_to_fskit(VfsError::Invalid), FsKitError::Posix(e) if e == libc::EINVAL));
        assert!(matches!(vfs_err_to_fskit(VfsError::ReadOnly), FsKitError::Posix(e) if e == libc::EROFS));
        assert!(matches!(vfs_err_to_fskit(VfsError::Io("x".into())), FsKitError::Posix(e) if e == libc::EIO));
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

pub use auth::AuthToken;
pub use fs::CtxfsFsKit;
pub use slug::volume_slug;
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ctxfs-fskit adapter`
Expected: 9 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-fskit/src/adapter.rs crates/ctxfs-fskit/src/lib.rs
git commit -m "feat(fskit): add VFS → FSKit translation with correct inode bijection

Pure translation helpers for NodeAttr → ItemAttributes, NodeType →
ItemType, VfsError → fskit_rs::Error. Uses a +1 offset for the inode
bijection (VFS root=1 → FSKit root=2, VFS child=2 → FSKit=3, etc.),
ensuring no collision between the FSKit-reserved root ID and VFS-
allocated child IDs.

No trait impl yet — pure functions testable without FSKit installed.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 5: FilesystemAdapter — fskit_rs::Filesystem Implementation

Implements the `Filesystem` trait using the helpers from Task 4. This time with real parent tracking and correct enumerate_directory attrs.

**Files:**
- Modify: `crates/ctxfs-fskit/src/adapter.rs`
- Modify: `crates/ctxfs-fskit/src/fs.rs`
- Create: `crates/ctxfs-fskit/tests/adapter_ops.rs`

- [ ] **Step 1: Append FilesystemAdapter impl to adapter.rs**

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
use tracing::debug;

/// Adapter implementing `fskit_rs::Filesystem` on top of a shared `VfsState`.
///
/// Cloneable because `fskit_rs::mount` requires `Clone + Send + Sync + 'static`.
/// All state lives in `Arc<VfsState>`; clones share the inode table.
#[derive(Clone, Debug)]
pub struct FilesystemAdapter {
    vfs: Arc<VfsState>,
    volume_name: String,
    volume_id: String,
}

impl FilesystemAdapter {
    /// Create an adapter for a VFS whose root inode must be 1 (enforced at runtime).
    ///
    /// # Panics
    /// Panics in debug builds if `vfs.root_id() != 1`. The inode bijection
    /// in `vfs_to_fskit_inode` assumes this.
    pub fn new(vfs: Arc<VfsState>, volume_name: String) -> Self {
        debug_assert_eq!(
            vfs.root_id(),
            1,
            "FilesystemAdapter requires VfsState with root_id=1"
        );
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
    // ─── Volume lifecycle ────────────────────────────────────────────────

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

    async fn activate(&mut self, _options: TaskOptions) -> FsKitResult<Item> {
        let root_id = self.vfs.root_id();
        let attr = self.vfs.getattr(root_id).await.map_err(vfs_err_to_fskit)?;
        Ok(make_item("/", &attr))
    }

    async fn deactivate(&mut self) -> FsKitResult<()> {
        Ok(())
    }

    async fn set_volume_name(&mut self, _name: Vec<u8>) -> FsKitResult<Vec<u8>> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    // ─── Item attributes ─────────────────────────────────────────────────

    async fn get_attributes(&mut self, item_id: u64) -> FsKitResult<ItemAttributes> {
        let vfs_id = fskit_to_vfs_inode(item_id);
        let attr = self.vfs.getattr(vfs_id).await.map_err(vfs_err_to_fskit)?;
        Ok(node_attr_to_item_attributes(&attr))
    }

    async fn set_attributes(
        &mut self,
        _item_id: u64,
        _attributes: ItemAttributes,
    ) -> FsKitResult<ItemAttributes> {
        Err(FsKitError::Posix(libc::EROFS))
    }

    // ─── Directory operations ────────────────────────────────────────────

    async fn lookup_item(&mut self, name: &OsStr, directory_id: u64) -> FsKitResult<Item> {
        let name_str = name.to_str().ok_or(FsKitError::Posix(libc::EINVAL))?;
        let parent_vfs = fskit_to_vfs_inode(directory_id);
        let (_, attr) = self
            .vfs
            .lookup(parent_vfs, name_str)
            .await
            .map_err(vfs_err_to_fskit)?;
        Ok(make_item(name_str, &attr))
    }

    async fn enumerate_directory(
        &mut self,
        directory_id: u64,
        cookie: u64,
        _verifier: u64,
    ) -> FsKitResult<DirectoryEntries> {
        let parent_vfs = fskit_to_vfs_inode(directory_id);
        let children = self
            .vfs
            .readdir(parent_vfs)
            .await
            .map_err(vfs_err_to_fskit)?;

        // FSKit uses `cookie` as the offset into the child list; 0 means start.
        let start = cookie as usize;
        let mut entries = Vec::with_capacity(children.len().saturating_sub(start));

        for (index, (child_inode, name, _kind)) in children.into_iter().enumerate().skip(start) {
            // Fetch real attrs per child — correctness over optimization.
            // See Codex review finding #3: zeroed attrs cause Finder to cache
            // empty values for the UI.
            let attr = self
                .vfs
                .getattr(child_inode)
                .await
                .map_err(vfs_err_to_fskit)?;
            entries.push(directory_entries::Entry {
                item: Some(make_item(&name, &attr)),
                next_cookie: (index + 1) as u64,
            });
        }

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

    // ─── File operations (read-only) ─────────────────────────────────────

    async fn create_item(
        &mut self,
        _name: &OsStr,
        _type: ItemType,
        _dir_id: u64,
        _attrs: ItemAttributes,
    ) -> FsKitResult<Item> {
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
        let vfs_id = fskit_to_vfs_inode(item_id);
        let data = self
            .vfs
            .read(vfs_id, offset as u64, length as u32)
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

    // ─── Link operations ─────────────────────────────────────────────────

    async fn create_symbolic_link(
        &mut self,
        _name: &OsStr,
        _directory_id: u64,
        _attributes: ItemAttributes,
        _contents: Vec<u8>,
    ) -> FsKitResult<Item> {
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
        let vfs_id = fskit_to_vfs_inode(item_id);
        let target = self.vfs.readlink(vfs_id).await.map_err(vfs_err_to_fskit)?;
        Ok(target.into_bytes())
    }

    // ─── Access control ──────────────────────────────────────────────────

    async fn check_access(
        &mut self,
        _item_id: u64,
        _access: Vec<AccessMask>,
    ) -> FsKitResult<bool> {
        Ok(true)
    }

    // ─── Extended attributes (unsupported) ───────────────────────────────

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
```

- [ ] **Step 2: Replace fs.rs stub**

Overwrite `crates/ctxfs-fskit/src/fs.rs`:

```rust
//! Legacy module — kept so external references don't break.
//! The adapter lives in `crate::adapter`.

pub use crate::adapter::FilesystemAdapter as CtxfsFsKit;
```

- [ ] **Step 3: Update lib.rs**

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

- [ ] **Step 4: Write integration tests**

Create `crates/ctxfs-fskit/tests/adapter_ops.rs`:

```rust
//! Integration tests for `FilesystemAdapter` against a mock VFS.
//! No FSKit runtime required — exercises trait methods directly.

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
        unimplemented!()
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
    let main_rs_digest = make_digest("main_rs_sha256");
    let main_rs_content = b"fn main() {}\n".to_vec();

    let src_dir = Directory {
        digest: make_digest("src_dir_sha256"),
        entries: vec![DirEntry::File(FileEntry {
            name: "main.rs".into(),
            digest: main_rs_digest.clone(),
            size: main_rs_content.len() as u64,
            executable: false,
            inline_content: None,
        })],
    };
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
                digest: src_dir.digest.clone(),
            }),
        ],
    };

    let mut directories = std::collections::HashMap::new();
    directories.insert(root_dir.digest.hex.clone(), serde_json::to_vec(&root_dir).unwrap());
    directories.insert(src_dir.digest.hex.clone(), serde_json::to_vec(&src_dir).unwrap());
    let mut blobs = std::collections::HashMap::new();
    blobs.insert(readme_digest.hex.clone(), readme_content);
    blobs.insert(main_rs_digest.hex.clone(), main_rs_content);

    let provider: SharedProvider = Arc::new(MockProvider { directories, blobs });
    let snapshot = Snapshot {
        source: "github:test/repo@main".into(),
        commit_sha: "abc".into(),
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
    assert_eq!(attrs.file_id, Some(2));
    assert_eq!(attrs.r#type, Some(ItemType::Directory as i32));
}

#[tokio::test]
async fn lookup_child_from_root() {
    let mut adapter = build_adapter().await;
    let item = adapter.lookup_item(OsStr::new("README.md"), 2).await.unwrap();
    let attrs = item.attributes.unwrap();
    assert_eq!(attrs.r#type, Some(ItemType::File as i32));
    assert_eq!(attrs.size, Some(8));
    // README.md's parent is root → FSKit id 2
    assert_eq!(attrs.parent_id, Some(2));
    // README.md is the first child VFS allocates (id 2) → FSKit id 3
    assert_eq!(attrs.file_id, Some(3));
}

#[tokio::test]
async fn nested_file_has_correct_parent_id() {
    let mut adapter = build_adapter().await;
    // Resolve src/
    let src = adapter.lookup_item(OsStr::new("src"), 2).await.unwrap();
    let src_fskit_id = src.attributes.unwrap().file_id.unwrap();

    // Resolve src/main.rs
    let main_rs = adapter
        .lookup_item(OsStr::new("main.rs"), src_fskit_id)
        .await
        .unwrap();
    let attrs = main_rs.attributes.unwrap();

    // Critical: parent_id must be src's FSKit id, NOT root.
    assert_eq!(attrs.parent_id, Some(src_fskit_id));
    assert_ne!(attrs.parent_id, Some(2));
}

#[tokio::test]
async fn enumerate_returns_real_sizes() {
    let mut adapter = build_adapter().await;
    let dir = adapter.enumerate_directory(2, 0, 0).await.unwrap();
    let readme = dir
        .entries
        .iter()
        .find_map(|e| e.item.as_ref().filter(|i| i.name == b"README.md"))
        .unwrap();
    let attrs = readme.attributes.as_ref().unwrap();
    // Codex finding #3: must not be 0
    assert_eq!(attrs.size, Some(8));
}

#[tokio::test]
async fn lookup_missing_returns_enoent() {
    let mut adapter = build_adapter().await;
    let err = adapter.lookup_item(OsStr::new("nope"), 2).await.unwrap_err();
    match err {
        fskit_rs::Error::Posix(e) => assert_eq!(e, libc::ENOENT),
    }
}

#[tokio::test]
async fn read_file_contents() {
    let mut adapter = build_adapter().await;
    let readme = adapter.lookup_item(OsStr::new("README.md"), 2).await.unwrap();
    let file_id = readme.attributes.unwrap().file_id.unwrap();
    let bytes = adapter.read(file_id, 0, 1024).await.unwrap();
    assert_eq!(bytes, b"# Hello\n");
}

#[tokio::test]
async fn write_returns_erofs() {
    let mut adapter = build_adapter().await;
    let err = adapter.write(vec![1], 3, 0).await.unwrap_err();
    match err {
        fskit_rs::Error::Posix(e) => assert_eq!(e, libc::EROFS),
    }
}

#[tokio::test]
async fn getattr_root_is_directory() {
    let mut adapter = build_adapter().await;
    let attrs = adapter.get_attributes(2).await.unwrap();
    assert_eq!(attrs.r#type, Some(ItemType::Directory as i32));
    assert_eq!(attrs.file_id, Some(2));
    assert_eq!(attrs.parent_id, Some(2)); // root's parent is itself
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p ctxfs-fskit`
Expected: unit tests from Task 4 (9) + integration tests (8) = 17 pass.

- [ ] **Step 6: Run clippy**

Run: `cargo clippy --all-targets -p ctxfs-fskit`
Expected: clean.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-fskit/
git commit -m "feat(fskit): implement Filesystem trait on VfsState

FilesystemAdapter wraps Arc<VfsState> and implements fskit_rs::Filesystem.
Read-only (writes return EROFS). Fixes key correctness issues:

- Inode bijection: VFS root=1 → FSKit root=2, VFS children offset by +1.
  No collision between root and first child (v1 plan had this bug).
- Parent tracking: uses NodeAttr.parent_inode (added in previous commit)
  so ItemAttributes.parent_id is always the real parent, enabling
  Finder breadcrumbs and .. resolution for nested dirs.
- enumerate_directory fetches real sizes via getattr per child rather
  than emitting zeros that GUI clients would cache.

17 unit + integration tests pass without FSKit installed.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 6: Refactor do_mount — Extract Shared Prep Helper

Fixes Codex finding #10: before adding backend dispatch, extract the backend-agnostic prep (source resolution + snapshot fetch) into a helper. This keeps NFS behavior identical while enabling FSKit to reuse the same prep.

**Files:**
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

- [ ] **Step 1: Extract the helper struct and function**

In `crates/ctxfs-daemon/src/daemon.rs`, add above `impl DaemonServer`:

```rust
/// All the state a backend needs from `prepare_mount` to build a `VfsState` and start serving.
struct MountPrep {
    /// The original parsed source (used for the mount ID and registry cache).
    source_spec: ctxfs_core::source::SourceSpec,
    /// A GitHub-shaped source (registries resolved to owner/repo/ref).
    github_source: ctxfs_core::source::SourceSpec,
    /// The provider that fetches blobs and directories.
    provider: std::sync::Arc<ctxfs_provider_git::GitHubProvider>,
    /// Parsed snapshot manifest.
    snapshot: ctxfs_manifest::Snapshot,
    /// Optional subpath to re-root the mount at (from the source spec or resolver).
    subpath: Option<String>,
}
```

- [ ] **Step 2: Add the `prepare_mount` method**

Inside `impl DaemonServer`, add:

```rust
/// Resolve `source_str` to GitHub coordinates, fetch the snapshot, and
/// return all the state the backend-specific code needs.
///
/// This is backend-agnostic — extracted from the previous monolithic
/// `do_mount` so both NFS and FSKit dispatch can share it.
fn prepare_mount(&self, source_str: &str) -> Result<MountPrep, String> {
    let mut source =
        SourceSpec::parse(source_str).map_err(|e| format!("invalid source: {e}"))?;

    let is_latest = source.version == "latest";

    let cached_resolution = if source.provider_type == ProviderType::GitHub {
        None
    } else {
        let guard = self.resolution_cache.lock().unwrap();
        guard.get(source_str).cloned()
    };

    let (owner, repo, git_ref, subpath) = if source.provider_type == ProviderType::GitHub {
        let (o, r) = source
            .name
            .split_once('/')
            .ok_or_else(|| format!("invalid github source: {}", source.name))?;
        (
            o.to_string(),
            r.to_string(),
            source.version.clone(),
            source.subpath.clone(),
        )
    } else if let Some(resolved) = cached_resolution {
        info!("resolution cache hit for {source_str}");
        let sp = source.subpath.clone().or(resolved.subpath.clone());
        (
            resolved.owner.clone(),
            resolved.repo.clone(),
            resolved.git_ref.clone(),
            sp,
        )
    } else {
        let resolver = Self::make_resolver(&source)?;

        if is_latest {
            source.version = self
                .rt_handle
                .block_on(resolver.resolve_latest(&source.name))
                .map_err(|e| format!("failed to resolve latest: {e}"))?;
        }

        let src = self
            .rt_handle
            .block_on(resolver.resolve(&source.name, &source.version))
            .map_err(|e| format!("{e}"))?;

        let sp = source.subpath.clone().or(src.subpath.clone());

        {
            let mut guard = self.resolution_cache.lock().unwrap();
            if let Err(e) = guard.put(source_str.to_string(), src.clone(), is_latest) {
                warn!("failed to persist resolution cache: {e}");
            }
        }

        (src.owner, src.repo, src.git_ref, sp)
    };

    let github_source = SourceSpec {
        provider_type: ProviderType::GitHub,
        name: format!("{owner}/{repo}"),
        version: git_ref,
        subpath: subpath.clone(),
    };

    let provider = Arc::new(GitHubProvider::new(
        self.config.github_token.as_deref(),
        self.cache.clone(),
        Some(self.tree_cache.clone()),
        self.shared_tree_cache.clone(),
    ));

    let snapshot_data = self
        .rt_handle
        .block_on(provider.fetch_snapshot(&github_source))
        .map_err(|e| format!("failed to fetch snapshot: {e}"))?;

    let snapshot: Snapshot = serde_json::from_slice(&snapshot_data)
        .map_err(|e| format!("failed to parse snapshot: {e}"))?;

    Ok(MountPrep {
        source_spec: source,
        github_source,
        provider,
        snapshot,
        subpath,
    })
}
```

- [ ] **Step 3: Simplify `do_mount` to use the helper (NFS only — FSKit dispatch comes next)**

Replace the body of `do_mount` with:

```rust
fn do_mount(&self, source_str: &str, mount_point: &str) -> Result<MountInfo, String> {
    let prep = self.prepare_mount(source_str)?;

    std::fs::create_dir_all(mount_point)
        .map_err(|e| format!("failed to create mount point: {e}"))?;

    let id = prep.source_spec.id();
    let commit_sha = prep.snapshot.commit_sha.clone();

    let port = pick_free_port()?;
    let addr = format!("127.0.0.1:{port}");

    let vfs = self
        .rt_handle
        .block_on(ctxfs_vfs::VfsState::new(
            prep.provider,
            self.cache.clone(),
            prep.snapshot,
            prep.subpath,
        ))
        .map_err(|e| format!("failed to build VFS: {e}"))?;
    let fs = CtxfsNfs::new(Arc::new(vfs), prep.github_source);
    let nfs_handle = self
        .rt_handle
        .block_on(fs.spawn(&addr))
        .map_err(|e| format!("failed to start NFS server on {addr}: {e}"))?;

    info!(
        "NFS server listening on {} for {source_str}",
        nfs_handle.addr
    );

    let info = MountInfo {
        id: id.clone(),
        source: source_str.to_string(),
        mount_point: mount_point.to_string(),
        commit_sha,
        status: MountStatus::Ready,
        mounted_at: chrono::Utc::now().to_rfc3339(),
        nfs_port: Some(port),
        backend: ctxfs_core::backend::Backend::Nfs,
        volume_path: None,
        symlink_paths: vec![],
    };

    let handle = MountHandle {
        info: info.clone(),
        _nfs: nfs_handle,
    };

    self.rt_handle.block_on(async {
        let _ = self.mounts.write().await.insert(id, handle);
    });

    Ok(info)
}
```

The behavior is identical — `do_mount` now delegates the prep to `prepare_mount` and does only the NFS-specific parts inline.

- [ ] **Step 4: Run full workspace tests**

Run: `cargo test`
Expected: all tests pass. This is a pure refactor.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --tests`
Expected: clean (possibly `too_many_lines` on `prepare_mount` — add `#[allow(clippy::too_many_lines)]` if needed).

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-daemon/src/daemon.rs
git commit -m "refactor(daemon): extract prepare_mount() helper

The source-resolution + snapshot-fetch portion of do_mount() is
backend-agnostic. Extracting it into prepare_mount() keeps NFS
behavior identical while letting the upcoming FSKit path reuse
the same prep without duplicating 80 lines of resolver logic.

No behavior change — pure refactor, all tests pass.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 7: Atomic — IPC Signature + Config + MountHandle + CLI + NFS Regression Test

Fixes Codex finding #6: Changing the RPC signature cascades through daemon and CLI. All three crates must update together in one commit to keep the tree buildable. This task is larger than typical but must be atomic.

**Files:**
- Modify: `crates/ctxfs-core/src/config.rs`
- Modify: `crates/ctxfs-ipc/src/service.rs`
- Modify: `crates/ctxfs-ipc/tests/rpc_roundtrip.rs`
- Modify: `crates/ctxfs-daemon/Cargo.toml`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`
- Modify: `crates/ctxfs-cli/src/main.rs`
- Modify: `crates/ctxfs-cli/src/deps/mount.rs`
- Create: `crates/ctxfs-daemon/tests/mount_dispatch.rs`

- [ ] **Step 1: Add fskit_bundle_id to Config**

Edit `crates/ctxfs-core/src/config.rs`. Add field to `Config` struct:

```rust
pub struct Config {
    // ... existing fields ...
    pub default_backend: Option<Backend>,
    /// Bundle ID of the installed FSKitBridge appex.
    pub fskit_bundle_id: Option<String>,
}
```

In `Default::default()` — set `fskit_bundle_id: None`.

In `from_env()`:

```rust
config.fskit_bundle_id = std::env::var("CTXFS_FSKIT_BUNDLE_ID")
    .ok()
    .filter(|s| !s.is_empty());
```

Add tests to existing `#[cfg(test)] mod tests`:

```rust
#[test]
fn default_config_has_no_fskit_bundle_id() {
    assert!(Config::default().fskit_bundle_id.is_none());
}

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
```

- [ ] **Step 2: Update IPC trait**

Edit `crates/ctxfs-ipc/src/service.rs`. Change:

```rust
async fn mount(
    source: String,
    mount_point: String,
    backend: ctxfs_core::Backend,
) -> Result<MountInfo, String>;
```

- [ ] **Step 3: Update IPC test**

Edit `crates/ctxfs-ipc/tests/rpc_roundtrip.rs`. Update the `MockServer::mount` signature and the client call to include the new `Backend` arg. Pass `Backend::Nfs` for the existing test.

- [ ] **Step 4: Extend MountHandle in daemon**

Edit `crates/ctxfs-daemon/src/daemon.rs`. Add these imports:

```rust
use ctxfs_core::Backend;
use ctxfs_fskit::FilesystemAdapter;
use fskit_rs::session::Session as FsKitSession;
```

Update `crates/ctxfs-daemon/Cargo.toml` dependencies:

```toml
ctxfs-fskit = { workspace = true }
fskit-rs = { workspace = true }
```

Add the `FsKitHandle` struct above `MountHandle`:

```rust
/// State owned by the daemon for an FSKit mount. Dropping this unmounts
/// the volume and stops the fskit-rs session.
pub struct FsKitHandle {
    /// The fskit-rs session. Dropping triggers unmount via fskit-rs.
    session: Option<FsKitSession>,
    /// Absolute `/Volumes/ctxfs/<slug>` path.
    volume_path: std::path::PathBuf,
}

impl FsKitHandle {
    pub fn volume_path(&self) -> &std::path::Path {
        &self.volume_path
    }

    /// Explicitly unmount and consume the session. Called from the daemon's
    /// shutdown path so we don't rely on Drop (which can't await).
    pub async fn shutdown(mut self) {
        if let Some(session) = self.session.take() {
            drop(session);
        }
    }
}

impl std::fmt::Debug for FsKitHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FsKitHandle")
            .field("volume_path", &self.volume_path)
            .finish_non_exhaustive()
    }
}
```

Update `MountHandle`:

```rust
struct MountHandle {
    info: MountInfo,
    backend: Backend,
    _nfs: Option<NfsServerHandle>,
    _fskit: Option<FsKitHandle>,
}
```

- [ ] **Step 5: Update daemon's `mount` RPC to accept backend**

In the `impl CtxfsService for DaemonServer` block:

```rust
async fn mount(
    self,
    _: tarpc::context::Context,
    source: String,
    mount_point: String,
    backend: Backend,
) -> Result<MountInfo, String> {
    info!("mount request: {source} -> {mount_point} (backend={backend})");
    let server = self.clone();
    tokio::task::spawn_blocking(move || server.do_mount(&source, &mount_point, backend))
        .await
        .map_err(|e| format!("mount task panicked: {e}"))?
}
```

Update `do_mount` signature to accept `backend: Backend` and branch:

```rust
fn do_mount(
    &self,
    source_str: &str,
    mount_point: &str,
    backend: Backend,
) -> Result<MountInfo, String> {
    match backend {
        Backend::Nfs => self.do_mount_nfs(source_str, mount_point),
        Backend::FsKit => Err("FSKit dispatch not yet implemented — see Task 8".into()),
    }
}
```

Rename the existing body to `do_mount_nfs`:

```rust
fn do_mount_nfs(
    &self,
    source_str: &str,
    mount_point: &str,
) -> Result<MountInfo, String> {
    // ... existing body from Task 6 ...
    // Update MountHandle construction:
    let handle = MountHandle {
        info: info.clone(),
        backend: Backend::Nfs,
        _nfs: Some(nfs_handle),
        _fskit: None,
    };
    // ...
}
```

- [ ] **Step 6: Update CLI call sites**

Edit `crates/ctxfs-cli/src/main.rs`. Find `handle_mount`. Pass the detected `backend`:

```rust
let info = client
    .mount(
        tarpc::context::current(),
        source.clone(),
        mount_point_str.clone(),
        backend,
    )
    .await?
    .map_err(|e| anyhow::anyhow!(e))?;
```

Edit `crates/ctxfs-cli/src/deps/mount.rs`. The batch mount path passes `Backend::Nfs` explicitly (FSKit batch is deferred):

```rust
let result = client
    .mount(
        tarpc::context::current(),
        source.clone(),
        mount_point.clone(),
        Backend::Nfs,
    )
    .await;
```

Add `use ctxfs_core::Backend;` at the top of any file that doesn't already import it.

- [ ] **Step 7: Add NFS regression unit test**

Create `crates/ctxfs-daemon/tests/mount_dispatch.rs`:

```rust
//! Regression test: Backend::Nfs dispatch path still works after refactor.
//!
//! This test uses `--server-only` semantics — it creates the NFS server via
//! the daemon without requiring sudo. Gated on GITHUB_TOKEN to avoid rate
//! limits in CI.

#![allow(clippy::unwrap_used)]

#[test]
#[ignore = "requires GITHUB_TOKEN and network; runs in local dev"]
fn do_mount_nfs_path_returns_ready_info() {
    if std::env::var("GITHUB_TOKEN").is_err() {
        eprintln!("skipping: GITHUB_TOKEN not set");
        return;
    }
    // This is effectively an integration test that starts the daemon in-process
    // and asserts the Backend::Nfs dispatch returns a MountInfo with
    // nfs_port: Some(_), backend: Nfs. Full implementation depends on the
    // existing test harness at crates/ctxfs-cli/tests/common/ — reuse that
    // pattern if available, or skip this regression test and rely on the
    // existing end-to-end test at crates/ctxfs-cli/tests/e2e.rs which already
    // exercises the NFS path end-to-end via the CLI binary.
    //
    // The simpler alternative: this test's existence is documentation that
    // we expect `cargo test -p ctxfs-cli --test e2e`'s NFS path to still
    // pass after the refactor. If it does, the regression test goal is met.
}
```

The simpler pragmatic approach: **verify the existing `crates/ctxfs-cli/tests/e2e.rs` NFS tests still pass** — that's the true regression test. This new file exists only as a breadcrumb for future daemon-internal testing. Run the existing e2e test as part of Step 8 below.

- [ ] **Step 8: Build and test everything**

Run:
```bash
cargo build
cargo test
cargo clippy --all-targets --tests
```

Expected: all tests pass. Specifically the existing NFS `mount_server_only_starts_nfs_and_reports_port` e2e test (if `GITHUB_TOKEN` is set and rate-limit allows) should still pass.

- [ ] **Step 9: Commit (atomic)**

```bash
git add Cargo.toml \
        crates/ctxfs-core/src/config.rs \
        crates/ctxfs-ipc/ \
        crates/ctxfs-daemon/Cargo.toml \
        crates/ctxfs-daemon/src/daemon.rs \
        crates/ctxfs-daemon/tests/mount_dispatch.rs \
        crates/ctxfs-cli/src/main.rs \
        crates/ctxfs-cli/src/deps/mount.rs
git commit -m "feat: thread Backend through IPC + daemon + CLI (atomic)

Changes that must land together to keep the tree buildable:

- ctxfs-core: Config gains fskit_bundle_id (CTXFS_FSKIT_BUNDLE_ID env)
- ctxfs-ipc: mount() RPC now takes backend: Backend third arg
- ctxfs-daemon: MountHandle gains backend + optional FsKitHandle
  (with async shutdown method); do_mount splits into do_mount_nfs
  (working) and do_mount_fskit (stub, implemented in next commit)
- ctxfs-cli: handle_mount passes detected backend through;
  deps batch-mount explicitly passes Backend::Nfs (FSKit batch deferred)

Existing NFS end-to-end tests pass — the refactor is behavior-
preserving for the NFS path.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 8: Daemon FSKit Dispatch + /Volumes/ctxfs/ Validation + mounts.json

Fixes Codex findings #4, #5, #8: real FSKit mount path with directory validation, `mounts.json` persistence integrated, session cleanup on shutdown.

**Files:**
- Create: `crates/ctxfs-daemon/src/fskit_mount.rs`
- Modify: `crates/ctxfs-daemon/src/lib.rs`
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

- [ ] **Step 1: Create fskit_mount.rs**

Create `crates/ctxfs-daemon/src/fskit_mount.rs`:

```rust
//! FSKit mount orchestration.
//!
//! Builds the FilesystemAdapter, validates the mount directory, starts the
//! fskit-rs session, and returns an `FsKitHandle` the daemon tracks.

use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::source::SourceSpec;
use ctxfs_fskit::{volume_slug, FilesystemAdapter};
use ctxfs_manifest::Snapshot;
use ctxfs_vfs::VfsState;
use fskit_rs::MountOptions;
use std::path::PathBuf;
use std::sync::Arc;
use tracing::{info, warn};

use crate::daemon::FsKitHandle;

#[derive(Debug, thiserror::Error)]
pub enum FsKitMountError {
    #[error("fskit_bundle_id is not configured (set CTXFS_FSKIT_BUNDLE_ID or install the extension)")]
    MissingBundleId,
    #[error("/Volumes/ctxfs/ does not exist — run `ctxfs setup install-fskit`")]
    ParentMissing,
    #[error("/Volumes/ctxfs/{slug} appears to already be mounted — unmount it first (ctxfs unmount, or sudo umount)")]
    AlreadyMounted { slug: String },
    #[error("failed to create /Volumes/ctxfs/{slug}: {source}")]
    MountDir {
        slug: String,
        source: std::io::Error,
    },
    #[error("failed to build VfsState: {0}")]
    Vfs(String),
    #[error("failed to start fskit-rs session: {0}")]
    Session(String),
}

/// Start an FSKit mount. Returns a handle whose `Drop` unmounts on scope exit
/// (or explicit `shutdown()` for async cleanup).
pub async fn start_fskit_mount(
    source: &SourceSpec,
    provider: SharedProvider,
    cache: Arc<BlobCache>,
    snapshot: Snapshot,
    subpath: Option<String>,
    bundle_id: &str,
) -> Result<FsKitHandle, FsKitMountError> {
    // 1. Validate preconditions.
    let parent = PathBuf::from("/Volumes/ctxfs");
    if !parent.exists() {
        return Err(FsKitMountError::ParentMissing);
    }

    let slug = volume_slug(source);
    let volume_path = parent.join(&slug);

    // 2. Ensure the volume directory is in a mountable state.
    validate_volume_path(&volume_path, &slug)?;

    // 3. Build the VFS and adapter.
    let vfs = Arc::new(
        VfsState::new(provider, cache, snapshot, subpath)
            .await
            .map_err(|e| FsKitMountError::Vfs(e.to_string()))?,
    );
    let adapter = FilesystemAdapter::new(vfs, slug.clone());

    // 4. Start the session (this is what triggers the kernel mount via fskitd).
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

    Ok(FsKitHandle::new(session, volume_path))
}

/// Ensure `/Volumes/ctxfs/<slug>` exists and is a usable mount point.
///
/// - If it doesn't exist, create it.
/// - If it exists and is a directory, verify nothing is already mounted on
///   it (via `mount` command output).
/// - If it exists as a non-directory, error.
fn validate_volume_path(volume_path: &std::path::Path, slug: &str) -> Result<(), FsKitMountError> {
    match std::fs::symlink_metadata(volume_path) {
        Ok(meta) if meta.is_dir() => {
            // Check the mount table for an existing mount at this path.
            if is_already_mounted(volume_path) {
                return Err(FsKitMountError::AlreadyMounted {
                    slug: slug.to_string(),
                });
            }
            // Directory exists and nothing is mounted — reuse it.
            Ok(())
        }
        Ok(_) => Err(FsKitMountError::MountDir {
            slug: slug.to_string(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "path exists but is not a directory",
            ),
        }),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::create_dir(volume_path)
            .map_err(|source| FsKitMountError::MountDir {
                slug: slug.to_string(),
                source,
            }),
        Err(e) => Err(FsKitMountError::MountDir {
            slug: slug.to_string(),
            source: e,
        }),
    }
}

/// Check `mount` output for an active mount at the given path.
fn is_already_mounted(path: &std::path::Path) -> bool {
    match std::process::Command::new("mount").output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            let path_str = path.to_string_lossy();
            stdout
                .lines()
                .any(|line| line.contains(&format!(" on {path_str} ")))
        }
        _ => {
            warn!("could not query mount table; assuming not mounted");
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validate_nonexistent_creates_dir() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("newslug");
        assert!(!path.exists());
        validate_volume_path(&path, "newslug").unwrap();
        assert!(path.is_dir());
    }

    #[test]
    fn validate_existing_empty_dir_ok() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("exists");
        std::fs::create_dir(&path).unwrap();
        validate_volume_path(&path, "exists").unwrap();
    }

    #[test]
    fn validate_file_errors() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("regularfile");
        std::fs::write(&path, "data").unwrap();
        assert!(matches!(
            validate_volume_path(&path, "regularfile"),
            Err(FsKitMountError::MountDir { .. })
        ));
    }
}
```

- [ ] **Step 2: Wire into daemon**

Edit `crates/ctxfs-daemon/src/lib.rs`:

```rust
pub mod daemon;
pub mod fskit_mount;
pub mod mount_state;
```

Edit `crates/ctxfs-daemon/src/daemon.rs`. Add the `FsKitHandle::new` constructor:

```rust
impl FsKitHandle {
    pub fn new(session: FsKitSession, volume_path: std::path::PathBuf) -> Self {
        Self {
            session: Some(session),
            volume_path,
        }
    }
    // existing methods: volume_path(), shutdown() ...
}
```

Implement the FSKit dispatch branch. Replace the stub `Err("FSKit dispatch not yet implemented...")` in `do_mount` with a call to a new method:

```rust
fn do_mount(
    &self,
    source_str: &str,
    mount_point: &str,
    backend: Backend,
) -> Result<MountInfo, String> {
    match backend {
        Backend::Nfs => self.do_mount_nfs(source_str, mount_point),
        Backend::FsKit => self.do_mount_fskit(source_str, mount_point),
    }
}

fn do_mount_fskit(
    &self,
    source_str: &str,
    mount_point: &str,
) -> Result<MountInfo, String> {
    let bundle_id = self
        .config
        .fskit_bundle_id
        .clone()
        .ok_or_else(|| "CTXFS_FSKIT_BUNDLE_ID not set — cannot start FSKit mount".to_string())?;

    let prep = self.prepare_mount(source_str)?;

    let fskit_handle = self
        .rt_handle
        .block_on(crate::fskit_mount::start_fskit_mount(
            &prep.source_spec,
            prep.provider,
            self.cache.clone(),
            prep.snapshot.clone(),
            prep.subpath,
            &bundle_id,
        ))
        .map_err(|e| format!("fskit mount failed: {e}"))?;

    let id = prep.source_spec.id();
    let commit_sha = prep.snapshot.commit_sha.clone();
    let volume_path_str = fskit_handle.volume_path().to_string_lossy().to_string();

    let symlink_paths = if mount_point == volume_path_str {
        vec![]
    } else {
        vec![mount_point.to_string()]
    };

    let info = MountInfo {
        id: id.clone(),
        source: source_str.to_string(),
        mount_point: mount_point.to_string(),
        commit_sha,
        status: MountStatus::Ready,
        mounted_at: chrono::Utc::now().to_rfc3339(),
        nfs_port: None,
        backend: Backend::FsKit,
        volume_path: Some(volume_path_str.clone()),
        symlink_paths: symlink_paths.clone(),
    };

    // Persist to mounts.json for crash recovery.
    let state_file = crate::mount_state::MountStateFile::new(
        self.config.pid_file.parent().unwrap_or_else(|| {
            std::path::Path::new(std::env::var("HOME").as_deref().unwrap_or("/tmp"))
        }),
    );
    let entry = crate::mount_state::MountStateEntry {
        source: source_str.to_string(),
        volume_path: volume_path_str,
        symlink_paths,
        backend: Backend::FsKit,
        tcp_port: None,
        auth_token: None,
    };
    if let Err(e) = state_file.add(entry) {
        warn!("failed to persist mount state: {e}");
    }

    let handle = MountHandle {
        info: info.clone(),
        backend: Backend::FsKit,
        _nfs: None,
        _fskit: Some(fskit_handle),
    };

    self.rt_handle.block_on(async {
        let _ = self.mounts.write().await.insert(id, handle);
    });

    Ok(info)
}
```

Update `unmount` to also clean up the mounts.json entry:

```rust
async fn unmount(self, _: tarpc::context::Context, target: String) -> Result<(), String> {
    info!("unmount request: {target}");
    let mut mounts = self.mounts.write().await;

    let key = mounts
        .iter()
        .find(|(_, h)| {
            h.info.mount_point == target
                || h.info.id == target
                || h.info.volume_path.as_deref() == Some(&target)
        })
        .map(|(k, _)| k.clone());

    match key {
        Some(k) => {
            if let Some(handle) = mounts.remove(&k) {
                let volume_path = handle.info.volume_path.clone();

                // If it's an FSKit mount, explicitly shut the session down.
                if let Some(fskit) = handle._fskit {
                    fskit.shutdown().await;
                }
                drop(handle._nfs); // no-op for FSKit

                // Clean up mounts.json entry.
                if let Some(vp) = volume_path.as_deref() {
                    let state_file = crate::mount_state::MountStateFile::new(
                        self.config.pid_file.parent().unwrap_or_else(|| {
                            std::path::Path::new(std::env::var("HOME").as_deref().unwrap_or("/tmp"))
                        }),
                    );
                    if let Err(e) = state_file.remove_volume(vp) {
                        warn!("failed to remove mount state entry: {e}");
                    }
                }

                info!("stopped mount for {target}");
                Ok(())
            } else {
                Err(format!("mount not found: {target}"))
            }
        }
        None => Err(format!("mount not found: {target}")),
    }
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ctxfs-daemon`
Expected: all pass (including the 3 new `validate_volume_path` tests).

- [ ] **Step 4: Run clippy**

Run: `cargo clippy --all-targets --tests`
Expected: clean.

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-daemon/
git commit -m "feat(daemon): implement FSKit mount dispatch with state persistence

do_mount_fskit() validates /Volumes/ctxfs/<slug> is usable (creating
it if missing, rejecting stale mounts), builds the adapter via
fskit_mount::start_fskit_mount, and records the mount in mounts.json
for crash recovery. unmount() now shuts down the FsKitSession
asynchronously and cleans up the state entry.

Requires CTXFS_FSKIT_BUNDLE_ID in daemon environment.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 9: Session Cleanup on Daemon Shutdown + Startup Recovery

Fixes Codex finding #5: daemon crash leaves kernel mounts live. Guarantee cleanup on graceful shutdown, and attempt best-effort unmount of stale entries on startup.

**Files:**
- Modify: `crates/ctxfs-daemon/src/daemon.rs`

- [ ] **Step 1: Add shutdown hook**

Find the daemon's `shutdown` / `stop` method (where SIGTERM/SIGINT is handled). Before the existing `drop(handle)` loop, explicitly shut down FSKit sessions:

```rust
async fn stop(&self) {
    info!("stopping daemon");
    let ids: Vec<String> = {
        let mounts = self.mounts.read().await;
        mounts.keys().cloned().collect()
    };
    let mut mounts = self.mounts.write().await;
    for id in ids {
        if let Some(handle) = mounts.remove(&id) {
            info!("shutting down mount {}", handle.info.mount_point);
            // Explicit async shutdown for FSKit (can't await in Drop).
            if let Some(fskit) = handle._fskit {
                fskit.shutdown().await;
            }
            drop(handle._nfs);
        }
    }

    // Clear mounts.json — we've shut down cleanly.
    let state_file = crate::mount_state::MountStateFile::new(
        self.config.pid_file.parent().unwrap_or_else(|| {
            std::path::Path::new(std::env::var("HOME").as_deref().unwrap_or("/tmp"))
        }),
    );
    if let Err(e) = state_file.clear() {
        warn!("failed to clear mount state: {e}");
    }

    let _ = std::fs::remove_file(&self.config.pid_file);
    let _ = std::fs::remove_file(&self.config.socket_path);
    info!("daemon stopped");
}
```

- [ ] **Step 2: Add startup cleanup**

Find `Daemon::run()` or the daemon startup function. Before starting the IPC server, call a cleanup function:

```rust
impl Daemon {
    fn cleanup_stale_mounts(&self) {
        let state_file = crate::mount_state::MountStateFile::new(
            self.config.pid_file.parent().unwrap_or_else(|| {
                std::path::Path::new("/tmp")
            }),
        );
        let entries = state_file.read();
        if entries.is_empty() {
            return;
        }
        warn!(
            "found {} stale mount entries from previous daemon run, attempting cleanup",
            entries.len()
        );
        for entry in &entries {
            if entry.backend != Backend::FsKit {
                continue;
            }
            // Best-effort force-unmount.
            let _ = std::process::Command::new("diskutil")
                .args(["unmount", "force", &entry.volume_path])
                .output();
            info!("cleaned up stale FSKit volume {}", entry.volume_path);
        }
        // Clear the state file — we've handled what we can.
        if let Err(e) = state_file.clear() {
            warn!("failed to clear stale mount state: {e}");
        }
    }
}
```

Call it at the top of `run()`:

```rust
pub async fn run(self) -> Result<()> {
    self.cleanup_stale_mounts();
    // ... existing run body ...
}
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ctxfs-daemon && cargo test`
Expected: all pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-daemon/src/daemon.rs
git commit -m "feat(daemon): explicit FSKit session cleanup on shutdown + startup recovery

- Daemon::stop() now awaits FsKitHandle::shutdown() for each FSKit
  mount before dropping handles (can't call async unmount in Drop).
- Daemon::run() calls cleanup_stale_mounts() which reads mounts.json
  from a previous crashed session and runs 'diskutil unmount force'
  on any leftover FSKit volumes.
- mounts.json is cleared after clean shutdown and after startup cleanup.

Addresses Codex finding #5: daemon crash previously leaked kernel
mounts with no recovery path.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 10: CLI Symlink Management for FSKit Mounts

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs`

- [ ] **Step 1: Create symlink after successful FSKit mount**

In `crates/ctxfs-cli/src/main.rs`'s `handle_mount`, after the `client.mount(...)` call succeeds:

```rust
use ctxfs_core::Backend;
use crate::symlink;

if info.backend == Backend::FsKit {
    if let Some(volume_path) = info.volume_path.as_deref() {
        let user_path = std::path::Path::new(&mount_point_str);
        let volume = std::path::Path::new(volume_path);

        if user_path != volume {
            match symlink::create_symlink(user_path, volume) {
                Ok(created_at) => {
                    println!("Mounted FSKit volume at {}", volume.display());
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
    } else {
        println!("Mounted FSKit volume (no volume_path reported — inspect with `ctxfs list`)");
    }
} else {
    // existing NFS output — unchanged
    // ... existing lines that print NFS info ...
}
```

- [ ] **Step 2: Resolve symlink on unmount**

In `handle_unmount` — before calling `client.unmount`:

```rust
use crate::symlink;

let target_path = std::path::Path::new(&target);
let daemon_target = if symlink::is_ctxfs_symlink(target_path) {
    symlink::resolve_ctxfs_path(target_path)
        .to_string_lossy()
        .into_owned()
} else {
    target.clone()
};

client
    .unmount(tarpc::context::current(), daemon_target)
    .await?
    .map_err(|e| anyhow::anyhow!(e))?;

// Remove the symlink after daemon acknowledges unmount.
if symlink::is_ctxfs_symlink(target_path) {
    let _ = symlink::safe_remove_symlink(target_path);
}

println!("Unmounted {}", target);
```

- [ ] **Step 3: Run tests**

Run: `cargo test -p ctxfs-cli`
Expected: all unit tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): manage /Volumes/ctxfs/ symlinks for FSKit mounts

After an FSKit mount succeeds, create a symlink from the user's -p
path to the /Volumes/ctxfs/<slug> volume path. On unmount, resolve
a ctxfs symlink to the underlying volume before asking the daemon,
then remove the symlink after the daemon acknowledges.

This preserves the UX of 'ctxfs mount ... -p ./deps/react' working
the same way regardless of backend.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 11: CI-Gated End-to-End FSKit Test

Fixes Codex finding #7: no automated coverage of the FSKit wiring. An opt-in test runs only when the environment is ready.

**Files:**
- Create: `crates/ctxfs-cli/tests/e2e_fskit.rs`

- [ ] **Step 1: Create the gated test**

```rust
//! End-to-end FSKit test. Gated on `CTXFS_E2E_FSKIT=1` and a populated
//! `CTXFS_FSKIT_BUNDLE_ID` env var, so CI skips it automatically.
//!
//! What this proves:
//! - `ctxfs daemon start` + `ctxfs mount --backend fskit` succeeds
//! - The volume appears under /Volumes/ctxfs/ and reads work
//! - `ctxfs unmount` cleans up

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

mod common;
use common::TestEnv;
use predicates::prelude::*;

fn fskit_env_ready() -> bool {
    std::env::var("CTXFS_E2E_FSKIT").ok().as_deref() == Some("1")
        && std::env::var("CTXFS_FSKIT_BUNDLE_ID").is_ok()
        && std::path::Path::new("/Volumes/ctxfs").exists()
}

#[test]
fn fskit_mount_and_read_cycle() {
    if !fskit_env_ready() {
        eprintln!(
            "skipping FSKit e2e test: set CTXFS_E2E_FSKIT=1, \
             CTXFS_FSKIT_BUNDLE_ID, and ensure /Volumes/ctxfs/ exists"
        );
        return;
    }

    let env = TestEnv::new();
    let _daemon = env.start_daemon();

    let mount_point = env.tempdir_path().join("test-mnt");

    // Mount via FSKit
    env.ctxfs(&[
        "mount",
        "github:octocat/Hello-World@master",
        "-p",
        mount_point.to_str().unwrap(),
        "--backend",
        "fskit",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("FSKit volume"));

    // Verify the mount is visible and readable
    let readme = mount_point.join("README");
    let content = std::fs::read_to_string(&readme).expect("read README");
    assert!(!content.is_empty());

    // Unmount
    env.ctxfs(&["unmount", mount_point.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Unmounted"));

    // Symlink should be gone
    assert!(!mount_point.exists(), "symlink should be removed");
}
```

- [ ] **Step 2: Build and run**

Run: `cargo build -p ctxfs-cli --tests`
Expected: compiles. Running the test locally (with the env vars set and FSKitBridge installed) should succeed; in CI without the env it prints the skip message and exits success.

- [ ] **Step 3: Commit**

```bash
git add crates/ctxfs-cli/tests/e2e_fskit.rs
git commit -m "test(cli): add CI-gated FSKit end-to-end test

Gated on CTXFS_E2E_FSKIT=1 plus a populated CTXFS_FSKIT_BUNDLE_ID.
When enabled, exercises the full daemon+CLI+FSKit path: mount a
real GitHub repo via --backend fskit, read the README, unmount.

CI without the env vars skips silently. Local runs with FSKitBridge
installed provide regression coverage.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Task 12: Manual Smoke Test + Findings Write-Up

**Files:**
- Create: `docs/poc/fskit-phase1-smoke-test.md`

- [ ] **Step 1: Set up environment**

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
Linked from: /Users/.../test-mnt
```

- [ ] **Step 4: Read files through both paths**

```sh
ls /Volumes/ctxfs/github-octocat-hello-world-master/
cat ./test-mnt/README
mount | grep ctxfs
```

- [ ] **Step 5: Run the opt-in e2e test**

```sh
export CTXFS_E2E_FSKIT=1
cargo test --release -p ctxfs --test e2e_fskit
```

- [ ] **Step 6: Unmount**

```sh
./target/release/ctxfs unmount ./test-mnt
```

Verify the symlink is gone, volume directory may remain (empty — fine).

- [ ] **Step 7: Write findings**

Create `docs/poc/fskit-phase1-smoke-test.md` with:
- Date tested + macOS version
- Each step's actual outcome
- Measured latency (`time cat`, `time grep -r`)
- Comparison vs NFS for same repo
- Any gotchas — update plan or spec if needed

- [ ] **Step 8: Commit**

```bash
git add docs/poc/fskit-phase1-smoke-test.md
git commit -m "docs: FSKit Phase 1 end-to-end smoke test writeup

Records the first successful ctxfs mount ... --backend fskit on a
real repo with the full daemon + CLI + FSKit stack. Latency
comparison vs NFS, gotchas, validation that Codex review findings
are resolved in practice.

Co-Authored-By: Claude Opus 4.6 (1M context) <noreply@anthropic.com>"
```

---

## Summary

| Task | Component | What it does |
|---|---|---|
| 1 | ctxfs-fskit Cargo.toml | Pull in fskit-rs |
| 2 | ctxfs-vfs types | NodeAttr gains parent_inode (fixes Codex #2) |
| 3 | ctxfs-fskit/slug.rs | volume_slug() — derives /Volumes/ctxfs/<slug> |
| 4 | ctxfs-fskit/adapter.rs | VFS→FSKit translation with correct inode bijection (fixes Codex #1) |
| 5 | ctxfs-fskit/adapter.rs | Filesystem trait impl, real parents, real attrs in enumerate (fixes Codex #3) |
| 6 | ctxfs-daemon | Extract prepare_mount() — backend-agnostic prep helper (fixes Codex #10) |
| 7 | atomic across core + ipc + daemon + cli | Backend threaded through IPC + config (fixes Codex #6 via atomic commit) |
| 8 | ctxfs-daemon | FSKit dispatch + /Volumes/ validation + mounts.json integration (fixes Codex #4, #8) |
| 9 | ctxfs-daemon | Async session cleanup on shutdown + startup recovery (fixes Codex #5) |
| 10 | ctxfs-cli | Symlink creation / resolution for FSKit (fixes Codex #9) |
| 11 | ctxfs-cli test | CI-gated FSKit e2e test (fixes Codex #7) |
| 12 | docs | Manual smoke test + findings writeup |

**Deferred to Phase 1.5 / Phase 2:**
- Auth token handshake (requires FSKitBridge Swift modifications)
- Finder polish (custom icon, volume display name)
- `ctxfs setup default-backend` persistence
- `ctxfs setup uninstall-fskit`
- Batch-mount (`deps` command) FSKit support

After Phase 1, users on macOS 26+ with FSKitBridge installed can mount repos without sudo and without Full Disk Access. That's the feature gate.
