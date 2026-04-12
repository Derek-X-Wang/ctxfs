# FSKit Backend Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add an FSKit-based filesystem backend for macOS 26+, eliminating sudo and Full Disk Access requirements. NFS remains the cross-platform fallback.

**Architecture:** Extract shared VFS logic from `ctxfs-nfs` into a new `ctxfs-vfs` crate. Create `ctxfs-fskit` as a thin adapter implementing `fskit_rs::Filesystem` over `VfsState`. The daemon dispatches between NFS and FSKit backends at mount time. Symlinks bridge user-specified paths to `/Volumes/ctxfs/` mount points.

**Tech Stack:** Rust workspace (15 crates), fskit-rs crate, vendored FSKitBridge Swift appex, dashmap, serde, tokio

**Spec:** `docs/superpowers/specs/2026-04-11-fskit-backend-design.md`

---

## Prerequisites

**Phase 0 (manual, not part of this plan):** Before executing any task below, complete the proof of concept described in the spec's "Phase 0" section. Clone FSKitBridge, build and sign the appex, write a minimal Rust binary implementing `fskit_rs::Filesystem` with hardcoded files, mount it on macOS 26, verify reads work. Only proceed if the PoC passes.

---

## File Map

### New Files

| File | Responsibility |
|---|---|
| `crates/ctxfs-vfs/Cargo.toml` | New crate manifest — depends on ctxfs-core, ctxfs-manifest, ctxfs-cache |
| `crates/ctxfs-vfs/src/lib.rs` | Re-exports `VfsState`, `NodeAttr`, `NodeType`, `VfsError` |
| `crates/ctxfs-vfs/src/state.rs` | `VfsState` struct: inode table, lazy population, blob fetching |
| `crates/ctxfs-vfs/src/types.rs` | `NodeAttr`, `NodeType`, `VfsError`, `StatFsResult` |
| `crates/ctxfs-vfs/tests/vfs_ops.rs` | Integration tests: lookup, read, readdir, readlink, subpath |
| `crates/ctxfs-fskit/Cargo.toml` | New crate manifest — depends on ctxfs-vfs, fskit-rs |
| `crates/ctxfs-fskit/src/lib.rs` | Re-exports `CtxfsFsKit` |
| `crates/ctxfs-fskit/src/fs.rs` | `CtxfsFsKit` implementing `fskit_rs::Filesystem` |
| `crates/ctxfs-fskit/src/auth.rs` | Per-mount auth token generation and validation |
| `crates/ctxfs-daemon/src/mount_state.rs` | `MountStateFile` — atomic JSON persistence with flock |
| `crates/ctxfs-cli/src/backend.rs` | `detect_backend()`, `Backend` re-export, macOS version check |
| `crates/ctxfs-cli/src/symlink.rs` | Symlink create/remove/verify helpers |

### Modified Files

| File | Changes |
|---|---|
| `Cargo.toml` (root) | Add ctxfs-vfs, ctxfs-fskit to workspace members + dependencies |
| `crates/ctxfs-core/src/lib.rs` | Re-export `Backend` |
| `crates/ctxfs-core/src/backend.rs` | `Backend` enum (Nfs, FsKit) with serde, clap, Display |
| `crates/ctxfs-core/src/config.rs` | Add `default_backend`, `fskit_app_path` fields, `CTXFS_BACKEND` env |
| `crates/ctxfs-ipc/src/service.rs` | Add `backend` field to `MountInfo`, make `nfs_port` optional, add `volume_path`, `symlink_paths` |
| `crates/ctxfs-nfs/Cargo.toml` | Replace direct core/manifest/cache deps with ctxfs-vfs |
| `crates/ctxfs-nfs/src/fs.rs` | Refactor to thin adapter: wrap `VfsState`, delegate core logic |
| `crates/ctxfs-daemon/Cargo.toml` | Add ctxfs-fskit dependency |
| `crates/ctxfs-daemon/src/daemon.rs` | `MountHandle` gains `Backend`, `FsKitHandle`, `symlink_paths`; `do_mount` dispatches; mount state persistence |
| `crates/ctxfs-cli/src/main.rs` | `--backend` flag, symlink management in mount/unmount, `setup install-fskit` |
| `crates/ctxfs-cli/src/setup.rs` | `install_fskit()`, `check` FSKit status, macOS 26 detection |

---

## Task 1: Backend Enum in ctxfs-core

**Files:**
- Create: `crates/ctxfs-core/src/backend.rs`
- Modify: `crates/ctxfs-core/src/lib.rs`

- [ ] **Step 1: Write the failing test**

Add to `crates/ctxfs-core/src/backend.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backend_display() {
        assert_eq!(Backend::Nfs.to_string(), "nfs");
        assert_eq!(Backend::FsKit.to_string(), "fskit");
    }

    #[test]
    fn backend_from_str() {
        assert_eq!("nfs".parse::<Backend>().unwrap(), Backend::Nfs);
        assert_eq!("fskit".parse::<Backend>().unwrap(), Backend::FsKit);
        assert!("invalid".parse::<Backend>().is_err());
    }

    #[test]
    fn backend_serde_roundtrip() {
        let nfs = Backend::Nfs;
        let json = serde_json::to_string(&nfs).unwrap();
        assert_eq!(json, "\"nfs\"");
        let back: Backend = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Backend::Nfs);

        let fskit = Backend::FsKit;
        let json = serde_json::to_string(&fskit).unwrap();
        assert_eq!(json, "\"fskit\"");
        let back: Backend = serde_json::from_str(&json).unwrap();
        assert_eq!(back, Backend::FsKit);
    }
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ctxfs-core backend`
Expected: Compilation error — `Backend` not defined yet.

- [ ] **Step 3: Write the implementation**

Create `crates/ctxfs-core/src/backend.rs`:

```rust
use serde::{Deserialize, Serialize};
use std::fmt;
use std::str::FromStr;

use crate::error::CtxfsError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Nfs,
    FsKit,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Backend::Nfs => write!(f, "nfs"),
            Backend::FsKit => write!(f, "fskit"),
        }
    }
}

impl FromStr for Backend {
    type Err = CtxfsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nfs" => Ok(Backend::Nfs),
            "fskit" => Ok(Backend::FsKit),
            other => Err(CtxfsError::InvalidSource(format!(
                "unsupported backend '{other}', expected 'nfs' or 'fskit'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    // ... tests from Step 1
}
```

Add to `crates/ctxfs-core/src/lib.rs`:

```rust
pub mod backend;
// ... existing modules ...
pub use backend::Backend;
```

- [ ] **Step 4: Run test to verify it passes**

Run: `cargo test -p ctxfs-core backend`
Expected: All 3 tests pass.

- [ ] **Step 5: Add Backend to Config**

In `crates/ctxfs-core/src/config.rs`, add field to `Config`:

```rust
pub struct Config {
    // ... existing fields ...
    pub default_backend: Option<Backend>,
}
```

Update `Default` impl to set `default_backend: None`.

Update `from_env()` to read `CTXFS_BACKEND`:

```rust
if let Ok(v) = std::env::var("CTXFS_BACKEND") {
    config.default_backend = v.parse().ok();
}
```

- [ ] **Step 6: Run all ctxfs-core tests**

Run: `cargo test -p ctxfs-core`
Expected: All tests pass. Fix any serde roundtrip test that fails due to the new field.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-core/src/backend.rs crates/ctxfs-core/src/lib.rs crates/ctxfs-core/src/config.rs
git commit -m "feat(core): add Backend enum (Nfs, FsKit) with serde/display/fromstr

Backend selection is the foundation for the FSKit backend. The enum lives
in ctxfs-core so both daemon and CLI can reference it. Config gains
default_backend field and CTXFS_BACKEND env var support."
```

---

## Task 2: Update MountInfo in ctxfs-ipc

**Files:**
- Modify: `crates/ctxfs-ipc/src/service.rs`

- [ ] **Step 1: Write the failing test**

Add to existing tests in `crates/ctxfs-ipc/src/service.rs`:

```rust
#[test]
fn mount_info_with_backend_and_volume_path() {
    let info = MountInfo {
        id: "react_19.1.0".into(),
        source: "npm:react@19.1.0".into(),
        mount_point: "/Users/derek/project/deps/react".into(),
        commit_sha: "abc123".into(),
        status: MountStatus::Ready,
        mounted_at: "2026-04-12T00:00:00Z".into(),
        nfs_port: None,
        backend: Backend::FsKit,
        volume_path: Some("/Volumes/ctxfs/react-19.1.0".into()),
        symlink_paths: vec!["/Users/derek/project/deps/react".into()],
    };

    let json = serde_json::to_string(&info).unwrap();
    let info2: MountInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(info2.backend, Backend::FsKit);
    assert_eq!(
        info2.volume_path.as_deref(),
        Some("/Volumes/ctxfs/react-19.1.0")
    );
    assert_eq!(info2.symlink_paths.len(), 1);
}

#[test]
fn mount_info_nfs_backward_compat() {
    let info = MountInfo {
        id: "test".into(),
        source: "github:a/b@c".into(),
        mount_point: "/mnt/test".into(),
        commit_sha: "sha".into(),
        status: MountStatus::Ready,
        mounted_at: "now".into(),
        nfs_port: Some(12345),
        backend: Backend::Nfs,
        volume_path: None,
        symlink_paths: vec![],
    };

    let json = serde_json::to_string(&info).unwrap();
    let info2: MountInfo = serde_json::from_str(&json).unwrap();
    assert_eq!(info2.backend, Backend::Nfs);
    assert_eq!(info2.nfs_port, Some(12345));
    assert!(info2.volume_path.is_none());
    assert!(info2.symlink_paths.is_empty());
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ctxfs-ipc`
Expected: Compilation error — `MountInfo` doesn't have the new fields yet.

- [ ] **Step 3: Update MountInfo**

In `crates/ctxfs-ipc/src/service.rs`:

```rust
use ctxfs_core::Backend;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountInfo {
    pub id: String,
    pub source: String,
    pub mount_point: String,
    pub commit_sha: String,
    pub status: MountStatus,
    pub mounted_at: String,
    /// NFS loopback port (None for FSKit backend).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub nfs_port: Option<u16>,
    /// Which backend is serving this mount.
    #[serde(default = "default_backend")]
    pub backend: Backend,
    /// FSKit volume path (e.g. `/Volumes/ctxfs/react-19.1.0`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub volume_path: Option<String>,
    /// Symlink paths pointing to this volume (FSKit only).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub symlink_paths: Vec<String>,
}

fn default_backend() -> Backend {
    Backend::Nfs
}
```

Add `ctxfs-core` dependency to `crates/ctxfs-ipc/Cargo.toml` if not already present.

- [ ] **Step 4: Fix all compile errors**

The `nfs_port` field changed from `u16` to `Option<u16>`. Update all constructors in:
- `crates/ctxfs-daemon/src/daemon.rs` — wrap nfs_port in `Some(port)`
- `crates/ctxfs-ipc/src/service.rs` — update existing tests
- `crates/ctxfs-cli/src/main.rs` — update any `info.nfs_port` reads to unwrap/handle None

- [ ] **Step 5: Run all tests**

Run: `cargo test -p ctxfs-ipc && cargo test -p ctxfs-daemon && cargo test -p ctxfs-cli`
Expected: All tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-ipc/ crates/ctxfs-daemon/ crates/ctxfs-cli/
git commit -m "feat(ipc): add backend, volume_path, symlink_paths to MountInfo

MountInfo now carries backend type, optional volume path for FSKit mounts,
and symlink paths for tracking project-level links. nfs_port becomes
Option<u16> since FSKit mounts don't use NFS. Backward-compatible
serde defaults ensure existing NFS-only paths still work."
```

---

## Task 3: Create ctxfs-vfs Crate with Types

**Files:**
- Create: `crates/ctxfs-vfs/Cargo.toml`
- Create: `crates/ctxfs-vfs/src/lib.rs`
- Create: `crates/ctxfs-vfs/src/types.rs`
- Modify: `Cargo.toml` (root workspace)

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "ctxfs-vfs"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
ctxfs-core = { workspace = true }
ctxfs-manifest = { workspace = true }
ctxfs-cache = { workspace = true }
dashmap = { workspace = true }
serde = { workspace = true }
serde_json = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }

[dev-dependencies]
tokio = { workspace = true, features = ["rt-multi-thread", "macros"] }
async-trait = { workspace = true }
tempfile = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Create types.rs with tests**

```rust
use serde::{Deserialize, Serialize};
use thiserror::Error;

/// Protocol-agnostic node type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum NodeType {
    File,
    Directory,
    Symlink,
}

/// Protocol-agnostic file/directory attributes.
#[derive(Debug, Clone)]
pub struct NodeAttr {
    pub inode: u64,
    pub size: u64,
    pub kind: NodeType,
    pub executable: bool,
}

/// Result of a `statfs` call.
#[derive(Debug, Clone)]
pub struct StatFsResult {
    pub total_bytes: u64,
    pub free_bytes: u64,
    pub block_size: u64,
    pub total_files: u64,
}

/// Errors from VFS operations.
#[derive(Debug, Error)]
pub enum VfsError {
    #[error("not found")]
    NotFound,
    #[error("not a directory")]
    NotDir,
    #[error("is a directory")]
    IsDir,
    #[error("invalid argument")]
    Invalid,
    #[error("read-only filesystem")]
    ReadOnly,
    #[error("I/O error: {0}")]
    Io(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn node_type_debug() {
        assert_eq!(format!("{:?}", NodeType::File), "File");
        assert_eq!(format!("{:?}", NodeType::Directory), "Directory");
        assert_eq!(format!("{:?}", NodeType::Symlink), "Symlink");
    }

    #[test]
    fn vfs_error_display() {
        assert_eq!(VfsError::NotFound.to_string(), "not found");
        assert_eq!(VfsError::ReadOnly.to_string(), "read-only filesystem");
        assert_eq!(
            VfsError::Io("disk full".into()).to_string(),
            "I/O error: disk full"
        );
    }

    #[test]
    fn node_attr_properties() {
        let attr = NodeAttr {
            inode: 42,
            size: 1024,
            kind: NodeType::File,
            executable: true,
        };
        assert_eq!(attr.inode, 42);
        assert!(attr.executable);
    }
}
```

- [ ] **Step 3: Create lib.rs**

```rust
pub mod state;
pub mod types;

pub use state::VfsState;
pub use types::{NodeAttr, NodeType, StatFsResult, VfsError};
```

Create a placeholder `state.rs`:

```rust
/// Placeholder — implemented in Task 4.
pub struct VfsState;
```

- [ ] **Step 4: Register in workspace**

Add to root `Cargo.toml`:

In `[workspace] members`:
```
"crates/ctxfs-vfs",
```

In `[workspace.dependencies]`:
```
ctxfs-vfs = { path = "crates/ctxfs-vfs", version = "0.0.0" }
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo test -p ctxfs-vfs`
Expected: 3 tests pass.

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-vfs/ Cargo.toml
git commit -m "feat(vfs): create ctxfs-vfs crate with protocol-agnostic types

New crate for shared VFS logic between NFS and FSKit backends.
Defines NodeAttr, NodeType, VfsError, StatFsResult. VfsState
implementation follows in the next commit."
```

---

## Task 4: Implement VfsState (Extract from ctxfs-nfs)

**Files:**
- Modify: `crates/ctxfs-vfs/src/state.rs`

This is the core extraction. The logic moves from `crates/ctxfs-nfs/src/fs.rs` into `VfsState`, with NFS-specific types (nfsstat3, fileid3, fattr3) replaced by VFS-agnostic equivalents.

- [ ] **Step 1: Write the failing integration test**

Create `crates/ctxfs-vfs/tests/vfs_ops.rs`:

```rust
#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

use async_trait::async_trait;
use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::Digest;
use ctxfs_manifest::{DirEntry, Directory, DirectoryEntry, FileEntry, Snapshot, SymlinkEntry};
use ctxfs_vfs::{NodeType, VfsError, VfsState};
use std::sync::Arc;

/// A mock provider that serves pre-built directory manifests and file blobs
/// from in-memory maps, no network needed.
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
        unimplemented!("not needed for VFS tests")
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

/// Build a test fixture: root dir with README.md (100 bytes), src/ subdir, and a symlink.
fn build_test_fixture() -> (SharedProvider, Snapshot, Arc<BlobCache>) {
    let readme_digest = make_digest("readme_sha256");
    let readme_content = b"# Hello World\nThis is a test README.\n".to_vec();

    let src_file_digest = make_digest("main_rs_sha256");
    let src_file_content = b"fn main() { println!(\"hello\"); }\n".to_vec();

    let src_dir = Directory {
        digest: make_digest("src_dir_sha256"),
        entries: vec![DirEntry::File(FileEntry {
            name: "main.rs".into(),
            digest: src_file_digest.clone(),
            size: src_file_content.len() as u64,
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
            DirEntry::Symlink(SymlinkEntry {
                name: "link".into(),
                target: "README.md".into(),
            }),
        ],
    };

    let mut directories = std::collections::HashMap::new();
    directories.insert(
        root_dir.digest.hex.clone(),
        serde_json::to_vec(&root_dir).unwrap(),
    );
    directories.insert(
        src_dir.digest.hex.clone(),
        serde_json::to_vec(&src_dir).unwrap(),
    );

    let mut blobs = std::collections::HashMap::new();
    blobs.insert(readme_digest.hex.clone(), readme_content);
    blobs.insert(src_file_digest.hex.clone(), src_file_content);

    let provider: SharedProvider = Arc::new(MockProvider { directories, blobs });

    let snapshot = Snapshot {
        source: "github:test/repo@main".into(),
        commit_sha: "abc123".into(),
        root_directory: root_dir.digest,
        created_at: "2026-04-12T00:00:00Z".into(),
    };

    let tmp = tempfile::tempdir().unwrap();
    let cache = Arc::new(BlobCache::new(tmp.path().to_path_buf(), 64 * 1024 * 1024).unwrap());

    (provider, snapshot, cache)
}

#[tokio::test]
async fn lookup_root_children() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    // Root is inode 1
    let (readme_id, readme_attr) = vfs.lookup(1, "README.md").await.unwrap();
    assert!(readme_id > 1);
    assert_eq!(readme_attr.kind, NodeType::File);

    let (src_id, src_attr) = vfs.lookup(1, "src").await.unwrap();
    assert!(src_id > 1);
    assert_eq!(src_attr.kind, NodeType::Directory);

    let (link_id, link_attr) = vfs.lookup(1, "link").await.unwrap();
    assert!(link_id > 1);
    assert_eq!(link_attr.kind, NodeType::Symlink);
}

#[tokio::test]
async fn lookup_not_found() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let err = vfs.lookup(1, "nonexistent").await.unwrap_err();
    assert!(matches!(err, VfsError::NotFound));
}

#[tokio::test]
async fn read_file_inline() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let (readme_id, _) = vfs.lookup(1, "README.md").await.unwrap();
    let data = vfs.read(readme_id, 0, 4096).await.unwrap();
    assert!(data.starts_with(b"# Hello World"));
}

#[tokio::test]
async fn read_file_from_provider() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let (src_id, _) = vfs.lookup(1, "src").await.unwrap();
    let (main_id, _) = vfs.lookup(src_id, "main.rs").await.unwrap();
    let data = vfs.read(main_id, 0, 4096).await.unwrap();
    assert!(data.starts_with(b"fn main()"));
}

#[tokio::test]
async fn read_with_offset() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let (readme_id, _) = vfs.lookup(1, "README.md").await.unwrap();
    let data = vfs.read(readme_id, 2, 5).await.unwrap();
    assert_eq!(&data, b"Hello");
}

#[tokio::test]
async fn readdir_root() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let entries = vfs.readdir(1).await.unwrap();
    assert_eq!(entries.len(), 3);
    let names: Vec<&str> = entries.iter().map(|(_, name, _)| name.as_str()).collect();
    assert!(names.contains(&"README.md"));
    assert!(names.contains(&"src"));
    assert!(names.contains(&"link"));
}

#[tokio::test]
async fn readlink() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let (link_id, _) = vfs.lookup(1, "link").await.unwrap();
    let target = vfs.readlink(link_id).await.unwrap();
    assert_eq!(target, "README.md");
}

#[tokio::test]
async fn getattr_root() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, None).await.unwrap();

    let attr = vfs.getattr(1).await.unwrap();
    assert_eq!(attr.kind, NodeType::Directory);
    assert_eq!(attr.inode, 1);
}

#[tokio::test]
async fn subpath_reroots() {
    let (provider, snapshot, cache) = build_test_fixture();
    let vfs = VfsState::new(provider, cache, snapshot, Some("src".into()))
        .await
        .unwrap();

    // Root should now be the src/ directory
    let entries = vfs.readdir(1).await.unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0].1, "main.rs");
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p ctxfs-vfs`
Expected: Compilation error — `VfsState` is a placeholder.

- [ ] **Step 3: Implement VfsState**

Replace `crates/ctxfs-vfs/src/state.rs` with the full implementation. This is extracted from `crates/ctxfs-nfs/src/fs.rs`, replacing NFS-specific types:

```rust
use crate::types::{NodeAttr, NodeType, StatFsResult, VfsError};
use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::Digest;
use ctxfs_manifest::{DirEntry, Directory, Snapshot};
use dashmap::DashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tracing::{debug, error};

const ROOT_ID: u64 = 1;
const BLOCK_SIZE: u64 = 4096;

#[derive(Debug, Clone)]
enum NodeKind {
    Directory {
        digest: Digest,
        populated: bool,
    },
    File {
        digest: Digest,
        size: u64,
        executable: bool,
        inline_content: Option<Vec<u8>>,
    },
    Symlink {
        target: String,
    },
}

#[derive(Debug, Clone)]
struct Node {
    id: u64,
    parent: u64,
    name: String,
    kind: NodeKind,
}

/// Protocol-agnostic VFS state shared between NFS and FSKit backends.
pub struct VfsState {
    provider: SharedProvider,
    cache: Arc<BlobCache>,
    #[allow(dead_code)]
    snapshot: Snapshot,

    next_id: AtomicU64,
    nodes: DashMap<u64, Node>,
    dir_cache: DashMap<(u64, String), u64>,
    dir_children: DashMap<u64, Vec<u64>>,
}

impl std::fmt::Debug for VfsState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VfsState")
            .field("node_count", &self.nodes.len())
            .finish_non_exhaustive()
    }
}

impl VfsState {
    /// Create a new VFS, optionally scoped to a subpath.
    pub async fn new(
        provider: SharedProvider,
        cache: Arc<BlobCache>,
        snapshot: Snapshot,
        subpath: Option<String>,
    ) -> Result<Self, VfsError> {
        let state = Self {
            provider,
            cache,
            snapshot: snapshot.clone(),
            next_id: AtomicU64::new(2),
            nodes: DashMap::new(),
            dir_cache: DashMap::new(),
            dir_children: DashMap::new(),
        };

        let _ = state.nodes.insert(
            ROOT_ID,
            Node {
                id: ROOT_ID,
                parent: ROOT_ID,
                name: "/".into(),
                kind: NodeKind::Directory {
                    digest: snapshot.root_directory,
                    populated: false,
                },
            },
        );

        if let Some(sp) = subpath {
            state.resolve_subpath(&sp).await?;
        }

        Ok(state)
    }

    async fn resolve_subpath(&self, subpath: &str) -> Result<(), VfsError> {
        let mut current_digest = match &self.nodes.get(&ROOT_ID) {
            Some(n) => match &n.kind {
                NodeKind::Directory { digest, .. } => digest.clone(),
                _ => return Err(VfsError::NotDir),
            },
            None => return Err(VfsError::NotFound),
        };

        for component in subpath.split('/').filter(|s| !s.is_empty()) {
            let data = self
                .provider
                .fetch_directory(&current_digest)
                .await
                .map_err(|e| VfsError::Io(format!("fetching directory: {e}")))?;

            let directory: Directory = serde_json::from_slice(&data)
                .map_err(|e| VfsError::Io(format!("parsing directory: {e}")))?;

            let child_dir = directory
                .entries
                .iter()
                .find_map(|entry| match entry {
                    DirEntry::Directory(d) if d.name == component => Some(d.digest.clone()),
                    _ => None,
                })
                .ok_or(VfsError::NotFound)?;

            current_digest = child_dir;
        }

        if let Some(mut root_node) = self.nodes.get_mut(&ROOT_ID) {
            root_node.kind = NodeKind::Directory {
                digest: current_digest,
                populated: false,
            };
        }

        Ok(())
    }

    fn alloc_id(&self) -> u64 {
        self.next_id.fetch_add(1, Ordering::Relaxed)
    }

    fn node_to_attr(node: &Node) -> NodeAttr {
        match &node.kind {
            NodeKind::Directory { .. } => NodeAttr {
                inode: node.id,
                size: BLOCK_SIZE,
                kind: NodeType::Directory,
                executable: false,
            },
            NodeKind::File {
                size, executable, ..
            } => NodeAttr {
                inode: node.id,
                size: *size,
                kind: NodeType::File,
                executable: *executable,
            },
            NodeKind::Symlink { target } => NodeAttr {
                inode: node.id,
                size: target.len() as u64,
                kind: NodeType::Symlink,
                executable: false,
            },
        }
    }

    /// Ensure a directory's children are loaded into the node table.
    async fn ensure_populated(&self, dirid: u64) -> Result<Vec<u64>, VfsError> {
        // Fast path: already populated.
        {
            let node = self.nodes.get(&dirid).ok_or(VfsError::NotFound)?;
            if let NodeKind::Directory {
                populated: true, ..
            } = &node.kind
            {
                drop(node);
                if let Some(children) = self.dir_children.get(&dirid) {
                    return Ok(children.clone());
                }
            }
        }

        let digest = {
            let node = self.nodes.get(&dirid).ok_or(VfsError::NotFound)?;
            match &node.kind {
                NodeKind::Directory { digest, .. } => digest.clone(),
                _ => return Err(VfsError::NotDir),
            }
        };

        let data = self.provider.fetch_directory(&digest).await.map_err(|e| {
            error!("fetch_directory({}) failed: {}", digest, e);
            VfsError::Io(format!("fetch_directory: {e}"))
        })?;

        let directory: Directory = serde_json::from_slice(&data).map_err(|e| {
            error!("parse directory {}: {}", digest, e);
            VfsError::Io(format!("parse directory: {e}"))
        })?;

        let mut child_ids = Vec::with_capacity(directory.entries.len());
        for entry in &directory.entries {
            let name = entry.name().to_string();
            let cache_key = (dirid, name.clone());

            let child_id = if let Some(existing) = self.dir_cache.get(&cache_key) {
                *existing
            } else {
                let new_id = self.alloc_id();
                let node = match entry {
                    DirEntry::File(f) => Node {
                        id: new_id,
                        parent: dirid,
                        name: name.clone(),
                        kind: NodeKind::File {
                            digest: f.digest.clone(),
                            size: f.size,
                            executable: f.executable,
                            inline_content: f.inline_content.clone(),
                        },
                    },
                    DirEntry::Directory(d) => Node {
                        id: new_id,
                        parent: dirid,
                        name: name.clone(),
                        kind: NodeKind::Directory {
                            digest: d.digest.clone(),
                            populated: false,
                        },
                    },
                    DirEntry::Symlink(s) => Node {
                        id: new_id,
                        parent: dirid,
                        name: name.clone(),
                        kind: NodeKind::Symlink {
                            target: s.target.clone(),
                        },
                    },
                };
                let _ = self.nodes.insert(new_id, node);
                let _ = self.dir_cache.insert(cache_key, new_id);
                new_id
            };
            child_ids.push(child_id);
        }

        if let Some(mut node_ref) = self.nodes.get_mut(&dirid) {
            if let NodeKind::Directory {
                ref mut populated, ..
            } = node_ref.kind
            {
                *populated = true;
            }
        }
        let _ = self.dir_children.insert(dirid, child_ids.clone());

        Ok(child_ids)
    }

    async fn fetch_file_bytes(&self, node: &Node) -> Result<Vec<u8>, VfsError> {
        if let NodeKind::File {
            digest,
            inline_content: Some(content),
            ..
        } = &node.kind
        {
            debug!("inline content for inode {} ({} bytes)", node.id, content.len());
            let _ = digest;
            return Ok(content.clone());
        }

        let digest = match &node.kind {
            NodeKind::File { digest, .. } => digest.clone(),
            _ => return Err(VfsError::IsDir),
        };

        if let Some(data) = self.cache.get(&digest) {
            return Ok(data);
        }

        let data = self.provider.fetch_blob(&digest).await.map_err(|e| {
            error!("fetch_blob({}) failed: {}", digest, e);
            VfsError::Io(format!("fetch_blob: {e}"))
        })?;

        if let Err(e) = self.cache.put(&digest, &data) {
            error!("cache put failed for {}: {}", digest, e);
        }
        Ok(data)
    }

    // ── Public API ──────────────────────────────────────────────────────

    pub async fn lookup(&self, parent: u64, name: &str) -> Result<(u64, NodeAttr), VfsError> {
        if name == "." {
            let node = self.nodes.get(&parent).ok_or(VfsError::NotFound)?;
            return Ok((parent, Self::node_to_attr(&node)));
        }
        if name == ".." {
            let parent_of_parent = self
                .nodes
                .get(&parent)
                .map_or(ROOT_ID, |n| n.parent);
            let node = self
                .nodes
                .get(&parent_of_parent)
                .ok_or(VfsError::NotFound)?;
            return Ok((parent_of_parent, Self::node_to_attr(&node)));
        }

        let cache_key = (parent, name.to_string());
        if let Some(existing) = self.dir_cache.get(&cache_key) {
            let id = *existing;
            let node = self.nodes.get(&id).ok_or(VfsError::NotFound)?;
            return Ok((id, Self::node_to_attr(&node)));
        }

        let _ = self.ensure_populated(parent).await?;

        let id = self
            .dir_cache
            .get(&cache_key)
            .map(|id| *id)
            .ok_or(VfsError::NotFound)?;
        let node = self.nodes.get(&id).ok_or(VfsError::NotFound)?;
        Ok((id, Self::node_to_attr(&node)))
    }

    pub async fn getattr(&self, inode: u64) -> Result<NodeAttr, VfsError> {
        let node = self.nodes.get(&inode).ok_or(VfsError::NotFound)?;
        Ok(Self::node_to_attr(&node))
    }

    pub async fn read(&self, inode: u64, offset: u64, count: u32) -> Result<Vec<u8>, VfsError> {
        let node = self.nodes.get(&inode).ok_or(VfsError::NotFound)?.clone();
        let data = self.fetch_file_bytes(&node).await?;

        let total = data.len() as u64;
        let start = offset.min(total) as usize;
        let end = (offset + u64::from(count)).min(total) as usize;
        Ok(data[start..end].to_vec())
    }

    /// Returns `(inode, name, kind)` tuples for all children.
    pub async fn readdir(&self, dirid: u64) -> Result<Vec<(u64, String, NodeType)>, VfsError> {
        let child_ids = self.ensure_populated(dirid).await?;

        let mut entries = Vec::with_capacity(child_ids.len());
        for child_id in &child_ids {
            if let Some(node) = self.nodes.get(child_id) {
                let kind = match &node.kind {
                    NodeKind::File { .. } => NodeType::File,
                    NodeKind::Directory { .. } => NodeType::Directory,
                    NodeKind::Symlink { .. } => NodeType::Symlink,
                };
                entries.push((*child_id, node.name.clone(), kind));
            }
        }
        Ok(entries)
    }

    pub async fn readlink(&self, inode: u64) -> Result<String, VfsError> {
        let node = self.nodes.get(&inode).ok_or(VfsError::NotFound)?;
        match &node.kind {
            NodeKind::Symlink { target } => Ok(target.clone()),
            _ => Err(VfsError::Invalid),
        }
    }

    pub fn statfs(&self) -> StatFsResult {
        StatFsResult {
            total_bytes: 1024 * 1024 * 1024, // synthetic 1GB
            free_bytes: 0,                     // read-only
            block_size: BLOCK_SIZE,
            total_files: self.nodes.len() as u64,
        }
    }

    /// The root inode ID (always 1).
    pub fn root_id(&self) -> u64 {
        ROOT_ID
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ctxfs-vfs`
Expected: All 9 integration tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-vfs/
git commit -m "feat(vfs): implement VfsState with inode table and lazy population

Extracts protocol-agnostic VFS logic from ctxfs-nfs: inode allocation,
DashMap-based node table, lazy directory population from provider,
blob fetching through cache, and subpath re-rooting. Both NFS and
FSKit backends will delegate to VfsState for core filesystem operations."
```

---

## Task 5: Refactor ctxfs-nfs to Use VfsState

**Files:**
- Modify: `crates/ctxfs-nfs/Cargo.toml`
- Modify: `crates/ctxfs-nfs/src/fs.rs`

- [ ] **Step 1: Update Cargo.toml**

Add `ctxfs-vfs` dependency, keep `nfsserve` and `dashmap`:

```toml
[dependencies]
ctxfs-vfs = { workspace = true }
ctxfs-core = { workspace = true }
ctxfs-cache = { workspace = true }
ctxfs-manifest = { workspace = true }
nfsserve = { workspace = true }
dashmap = { workspace = true }
async-trait = { workspace = true }
tracing = { workspace = true }
serde_json = { workspace = true }
tokio = { workspace = true }
```

- [ ] **Step 2: Refactor fs.rs to wrap VfsState**

Replace the internal `Node`, `NodeKind`, inode table, `ensure_populated`, `fetch_file_bytes`, and `resolve_subpath` with delegation to `VfsState`. Keep only NFS-specific code:

```rust
use async_trait::async_trait;
use ctxfs_cache::BlobCache;
use ctxfs_core::provider::SharedProvider;
use ctxfs_core::source::SourceSpec;
use ctxfs_manifest::Snapshot;
use ctxfs_vfs::{NodeAttr, NodeType, VfsError, VfsState};
use nfsserve::nfs::{
    fattr3, fileid3, filename3, ftype3, nfspath3, nfsstat3, nfstime3, sattr3, specdata3,
};
use nfsserve::tcp::{NFSTcp, NFSTcpListener};
use nfsserve::vfs::{DirEntry as NfsDirEntry, NFSFileSystem, ReadDirResult, VFSCapabilities};
use std::sync::Arc;
use tracing::error;

const BLOCK_SIZE: u64 = 4096;
const FSID: u64 = 0x6374_7866_7300_0001;

#[derive(Debug)]
pub struct NfsServerHandle {
    pub addr: String,
}

pub struct CtxfsNfs {
    vfs: Arc<VfsState>,
    #[allow(dead_code)]
    source: SourceSpec,
}

impl std::fmt::Debug for CtxfsNfs {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("CtxfsNfs")
            .field("source", &self.source.to_string())
            .field("vfs", &self.vfs)
            .finish_non_exhaustive()
    }
}

impl CtxfsNfs {
    pub fn new(
        provider: SharedProvider,
        source: SourceSpec,
        cache: Arc<BlobCache>,
        snapshot: Snapshot,
    ) -> Self {
        Self::new_with_subpath(provider, source, cache, snapshot, None)
    }

    #[must_use]
    pub fn new_with_subpath(
        provider: SharedProvider,
        source: SourceSpec,
        cache: Arc<BlobCache>,
        snapshot: Snapshot,
        subpath: Option<String>,
    ) -> Self {
        // VfsState::new is async; we store the params and init lazily in spawn().
        // For now, store a placeholder — spawn() will construct VfsState.
        Self {
            vfs: Arc::new_cyclic(|_| {
                // This is a workaround — we need async init.
                // We'll refactor spawn() to create VfsState.
                unreachable!("VfsState must be initialized via spawn()")
            }),
            source,
        }
        // Actually, let's change the API: make new_with_subpath return a builder
        // and spawn() does the actual construction.
    }
}
```

Wait — `VfsState::new` is async but `CtxfsNfs::new_with_subpath` is sync. The cleanest approach: change `CtxfsNfs` to accept a pre-built `VfsState`:

```rust
impl CtxfsNfs {
    /// Create from a pre-built VfsState. The caller (daemon) handles async construction.
    pub fn new(vfs: Arc<VfsState>, source: SourceSpec) -> Self {
        Self { vfs, source }
    }

    pub async fn spawn(self, addr: &str) -> std::io::Result<NfsServerHandle> {
        let listener = NFSTcpListener::bind(addr, self)
            .await
            .map_err(|e| std::io::Error::other(format!("bind failed: {e}")))?;

        let addr = addr.to_string();
        let _task = tokio::spawn(async move {
            if let Err(e) = listener.handle_forever().await {
                error!("NFS server exited: {e}");
            }
        });

        Ok(NfsServerHandle { addr })
    }

    fn attr_to_fattr3(attr: &NodeAttr) -> fattr3 {
        let epoch = nfstime3 { seconds: 0, nseconds: 0 };
        match attr.kind {
            NodeType::Directory => fattr3 {
                ftype: ftype3::NF3DIR,
                mode: 0o555,
                nlink: 2,
                uid: 0, gid: 0,
                size: BLOCK_SIZE, used: BLOCK_SIZE,
                rdev: specdata3::default(),
                fsid: FSID,
                fileid: attr.inode,
                atime: epoch, mtime: epoch, ctime: epoch,
            },
            NodeType::File => fattr3 {
                ftype: ftype3::NF3REG,
                mode: if attr.executable { 0o555 } else { 0o444 },
                nlink: 1,
                uid: 0, gid: 0,
                size: attr.size, used: attr.size,
                rdev: specdata3::default(),
                fsid: FSID,
                fileid: attr.inode,
                atime: epoch, mtime: epoch, ctime: epoch,
            },
            NodeType::Symlink => fattr3 {
                ftype: ftype3::NF3LNK,
                mode: 0o777,
                nlink: 1,
                uid: 0, gid: 0,
                size: attr.size, used: attr.size,
                rdev: specdata3::default(),
                fsid: FSID,
                fileid: attr.inode,
                atime: epoch, mtime: epoch, ctime: epoch,
            },
        }
    }

    fn vfs_err_to_nfs(e: VfsError) -> nfsstat3 {
        match e {
            VfsError::NotFound => nfsstat3::NFS3ERR_NOENT,
            VfsError::NotDir => nfsstat3::NFS3ERR_NOTDIR,
            VfsError::IsDir => nfsstat3::NFS3ERR_ISDIR,
            VfsError::Invalid => nfsstat3::NFS3ERR_INVAL,
            VfsError::ReadOnly => nfsstat3::NFS3ERR_ROFS,
            VfsError::Io(_) => nfsstat3::NFS3ERR_IO,
        }
    }
}

#[async_trait]
impl NFSFileSystem for CtxfsNfs {
    fn root_dir(&self) -> fileid3 { self.vfs.root_id() }
    fn capabilities(&self) -> VFSCapabilities { VFSCapabilities::ReadOnly }

    async fn lookup(&self, dirid: fileid3, filename: &filename3) -> Result<fileid3, nfsstat3> {
        let name = std::str::from_utf8(filename.as_ref()).map_err(|_| nfsstat3::NFS3ERR_INVAL)?;
        let (id, _) = self.vfs.lookup(dirid, name).await.map_err(Self::vfs_err_to_nfs)?;
        Ok(id)
    }

    async fn getattr(&self, id: fileid3) -> Result<fattr3, nfsstat3> {
        let attr = self.vfs.getattr(id).await.map_err(Self::vfs_err_to_nfs)?;
        Ok(Self::attr_to_fattr3(&attr))
    }

    async fn read(&self, id: fileid3, offset: u64, count: u32) -> Result<(Vec<u8>, bool), nfsstat3> {
        let attr = self.vfs.getattr(id).await.map_err(Self::vfs_err_to_nfs)?;
        let data = self.vfs.read(id, offset, count).await.map_err(Self::vfs_err_to_nfs)?;
        let eof = (offset + data.len() as u64) >= attr.size;
        Ok((data, eof))
    }

    async fn readdir(&self, dirid: fileid3, start_after: fileid3, max_entries: usize) -> Result<ReadDirResult, nfsstat3> {
        let all_children = self.vfs.readdir(dirid).await.map_err(Self::vfs_err_to_nfs)?;

        let mut entries: Vec<NfsDirEntry> = Vec::new();
        let mut started = start_after == 0;

        for (child_id, name, _kind) in &all_children {
            if !started {
                if *child_id == start_after { started = true; }
                continue;
            }
            if entries.len() >= max_entries { break; }
            let attr = self.vfs.getattr(*child_id).await.map_err(Self::vfs_err_to_nfs)?;
            entries.push(NfsDirEntry {
                fileid: *child_id,
                name: filename3::from(name.as_bytes().to_vec()),
                attr: Self::attr_to_fattr3(&attr),
            });
        }

        let last_returned = entries.last().map(|e| e.fileid);
        let end = match (last_returned, all_children.last()) {
            (Some(last), Some((total_last, _, _))) => last == *total_last,
            _ => true,
        };

        Ok(ReadDirResult { entries, end })
    }

    async fn readlink(&self, id: fileid3) -> Result<nfspath3, nfsstat3> {
        let target = self.vfs.readlink(id).await.map_err(Self::vfs_err_to_nfs)?;
        Ok(nfspath3::from(target.as_bytes().to_vec()))
    }

    // --- Read-only stubs (unchanged) ---
    async fn setattr(&self, _id: fileid3, _setattr: sattr3) -> Result<fattr3, nfsstat3> { Err(nfsstat3::NFS3ERR_ROFS) }
    async fn write(&self, _id: fileid3, _offset: u64, _data: &[u8]) -> Result<fattr3, nfsstat3> { Err(nfsstat3::NFS3ERR_ROFS) }
    async fn create(&self, _dirid: fileid3, _filename: &filename3, _attr: sattr3) -> Result<(fileid3, fattr3), nfsstat3> { Err(nfsstat3::NFS3ERR_ROFS) }
    async fn create_exclusive(&self, _dirid: fileid3, _filename: &filename3) -> Result<fileid3, nfsstat3> { Err(nfsstat3::NFS3ERR_ROFS) }
    async fn mkdir(&self, _dirid: fileid3, _dirname: &filename3) -> Result<(fileid3, fattr3), nfsstat3> { Err(nfsstat3::NFS3ERR_ROFS) }
    async fn remove(&self, _dirid: fileid3, _filename: &filename3) -> Result<(), nfsstat3> { Err(nfsstat3::NFS3ERR_ROFS) }
    async fn rename(&self, _from_dirid: fileid3, _from_filename: &filename3, _to_dirid: fileid3, _to_filename: &filename3) -> Result<(), nfsstat3> { Err(nfsstat3::NFS3ERR_ROFS) }
    async fn symlink(&self, _dirid: fileid3, _linkname: &filename3, _symlink: &nfspath3, _attr: &sattr3) -> Result<(fileid3, fattr3), nfsstat3> { Err(nfsstat3::NFS3ERR_ROFS) }
}
```

- [ ] **Step 3: Update daemon to construct VfsState**

In `crates/ctxfs-daemon/src/daemon.rs`, change `do_mount` to construct `VfsState` first, then pass it to `CtxfsNfs::new`:

```rust
// Replace:
let fs = CtxfsNfs::new_with_subpath(provider, github_source, self.cache.clone(), snapshot, subpath);
let nfs_handle = self.rt_handle.block_on(fs.spawn(&addr))...

// With:
let vfs = self.rt_handle.block_on(VfsState::new(
    provider, self.cache.clone(), snapshot, subpath,
)).map_err(|e| format!("failed to build VFS: {e}"))?;
let fs = CtxfsNfs::new(Arc::new(vfs), github_source);
let nfs_handle = self.rt_handle.block_on(fs.spawn(&addr))...
```

Add `use ctxfs_vfs::VfsState;` and `use std::sync::Arc;` to imports.
Add `ctxfs-vfs` to `crates/ctxfs-daemon/Cargo.toml` dependencies.

- [ ] **Step 4: Run all tests**

Run: `cargo test`
Expected: All existing tests pass. The refactor is behavior-preserving.

- [ ] **Step 5: Run clippy**

Run: `cargo clippy --all-targets --tests`
Expected: No new warnings.

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-nfs/ crates/ctxfs-daemon/ Cargo.toml
git commit -m "refactor(nfs): delegate to VfsState, NFS is now a thin adapter

CtxfsNfs no longer owns the inode table or fetch logic — it wraps
VfsState and translates between VFS types and NFS3 types. The daemon
constructs VfsState (async) and passes it to CtxfsNfs. This enables
the FSKit backend to share the same VfsState in the next step.

All existing tests pass — the refactor is behavior-preserving."
```

---

## Task 6: Create ctxfs-fskit Crate (Stub)

**Files:**
- Create: `crates/ctxfs-fskit/Cargo.toml`
- Create: `crates/ctxfs-fskit/src/lib.rs`
- Create: `crates/ctxfs-fskit/src/fs.rs`
- Create: `crates/ctxfs-fskit/src/auth.rs`
- Modify: `Cargo.toml` (root)

This task creates the crate structure. The actual `fskit_rs::Filesystem` implementation depends on the Phase 0 PoC confirming the `fskit-rs` crate API. For now, we implement the auth token module and a `CtxfsFsKit` struct that wraps `VfsState` with a `serve_tcp` method that starts a listener (using a mock protocol for testing — real fskit-rs integration comes after Phase 0).

- [ ] **Step 1: Create Cargo.toml**

```toml
[package]
name = "ctxfs-fskit"
version = "0.0.0"
edition = "2021"
publish = false

[dependencies]
ctxfs-vfs = { workspace = true }
ctxfs-core = { workspace = true }
tokio = { workspace = true }
tracing = { workspace = true }
thiserror = { workspace = true }
rand = "0.8"
hex = { workspace = true }

[dev-dependencies]
tempfile = { workspace = true }
ctxfs-cache = { workspace = true }
ctxfs-manifest = { workspace = true }
async-trait = { workspace = true }
serde_json = { workspace = true }

[lints]
workspace = true
```

- [ ] **Step 2: Create auth.rs with tests**

```rust
use rand::RngCore;

/// A per-mount authentication token for the FSKit TCP bridge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthToken {
    bytes: [u8; 32],
}

impl AuthToken {
    /// Generate a new random 256-bit token.
    pub fn generate() -> Self {
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Create from hex string (for deserialization from mounts.json).
    pub fn from_hex(s: &str) -> Result<Self, hex::FromHexError> {
        let decoded = hex::decode(s)?;
        if decoded.len() != 32 {
            return Err(hex::FromHexError::InvalidStringLength);
        }
        let mut bytes = [0u8; 32];
        bytes.copy_from_slice(&decoded);
        Ok(Self { bytes })
    }

    /// Encode as hex string (for serialization to mounts.json).
    pub fn to_hex(&self) -> String {
        hex::encode(self.bytes)
    }

    /// Validate a candidate token against this one (constant-time comparison).
    pub fn validate(&self, candidate: &[u8]) -> bool {
        if candidate.len() != 32 {
            return false;
        }
        // Simple constant-time compare for a dev tool.
        let mut result = 0u8;
        for (a, b) in self.bytes.iter().zip(candidate.iter()) {
            result |= a ^ b;
        }
        result == 0
    }
}

impl std::fmt::Display for AuthToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_unique_tokens() {
        let a = AuthToken::generate();
        let b = AuthToken::generate();
        assert_ne!(a, b);
    }

    #[test]
    fn hex_roundtrip() {
        let token = AuthToken::generate();
        let hex = token.to_hex();
        assert_eq!(hex.len(), 64); // 32 bytes = 64 hex chars
        let back = AuthToken::from_hex(&hex).unwrap();
        assert_eq!(token, back);
    }

    #[test]
    fn validate_correct_token() {
        let token = AuthToken::generate();
        assert!(token.validate(&token.bytes));
    }

    #[test]
    fn validate_wrong_token() {
        let token = AuthToken::generate();
        let wrong = AuthToken::generate();
        assert!(!token.validate(&wrong.bytes));
    }

    #[test]
    fn validate_wrong_length() {
        let token = AuthToken::generate();
        assert!(!token.validate(&[0u8; 16]));
        assert!(!token.validate(&[]));
    }

    #[test]
    fn from_hex_invalid() {
        assert!(AuthToken::from_hex("not_hex").is_err());
        assert!(AuthToken::from_hex("aabb").is_err()); // too short
    }
}
```

- [ ] **Step 3: Create fs.rs stub**

```rust
use ctxfs_vfs::VfsState;
use std::sync::Arc;

/// FSKit filesystem backend. Wraps VfsState and serves it over TCP
/// to the FSKitBridge Swift appex.
///
/// The actual `fskit_rs::Filesystem` trait implementation will be added
/// after the Phase 0 proof of concept confirms the fskit-rs API.
#[derive(Debug)]
pub struct CtxfsFsKit {
    vfs: Arc<VfsState>,
}

impl CtxfsFsKit {
    pub fn new(vfs: Arc<VfsState>) -> Self {
        Self { vfs }
    }

    /// Access the underlying VFS (for testing).
    pub fn vfs(&self) -> &VfsState {
        &self.vfs
    }
}
```

- [ ] **Step 4: Create lib.rs**

```rust
pub mod auth;
pub mod fs;

pub use auth::AuthToken;
pub use fs::CtxfsFsKit;
```

- [ ] **Step 5: Register in workspace**

Add to root `Cargo.toml`:

```toml
# In [workspace] members:
"crates/ctxfs-fskit",

# In [workspace.dependencies]:
ctxfs-fskit = { path = "crates/ctxfs-fskit", version = "0.0.0" }
rand = "0.8"
```

- [ ] **Step 6: Run tests**

Run: `cargo test -p ctxfs-fskit`
Expected: All 6 auth tests pass.

- [ ] **Step 7: Commit**

```bash
git add crates/ctxfs-fskit/ Cargo.toml
git commit -m "feat(fskit): create ctxfs-fskit crate with auth token and VFS wrapper

Adds the FSKit backend crate skeleton. AuthToken handles per-mount
256-bit token generation, hex serialization, and constant-time
validation. CtxfsFsKit wraps VfsState — the fskit_rs::Filesystem
trait implementation will be added after the Phase 0 PoC."
```

---

## Task 7: Backend Detection in CLI

**Files:**
- Create: `crates/ctxfs-cli/src/backend.rs`
- Modify: `crates/ctxfs-cli/src/main.rs`

- [ ] **Step 1: Create backend.rs with tests**

```rust
use ctxfs_core::Backend;

/// Check if macOS version is 26.0 or later.
#[cfg(target_os = "macos")]
fn macos_version_26_or_later() -> bool {
    use std::process::Command;
    let output = Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok();
    match output {
        Some(o) if o.status.success() => {
            let version = String::from_utf8_lossy(&o.stdout);
            let major: u32 = version.trim().split('.').next().and_then(|s| s.parse().ok()).unwrap_or(0);
            major >= 26
        }
        _ => false,
    }
}

#[cfg(not(target_os = "macos"))]
fn macos_version_26_or_later() -> bool {
    false
}

/// Check if the FSKit extension app is installed.
fn fskit_app_installed() -> bool {
    let candidates = [
        dirs::home_dir().map(|h| h.join("Applications/CtxfsFS.app")),
        Some(std::path::PathBuf::from("/Applications/CtxfsFS.app")),
    ];
    candidates.iter().any(|p| p.as_ref().is_some_and(|p| p.exists()))
}

/// Resolve which backend to use, following the priority chain:
/// `--backend` flag > `CTXFS_BACKEND` env > config file > auto-detect.
pub fn detect_backend(
    flag: Option<Backend>,
    config_default: Option<Backend>,
) -> Backend {
    // 1. Explicit flag
    if let Some(b) = flag {
        return b;
    }

    // 2. Environment variable (already parsed into Config)
    if let Ok(v) = std::env::var("CTXFS_BACKEND") {
        if let Ok(b) = v.parse() {
            return b;
        }
    }

    // 3. Config file default
    if let Some(b) = config_default {
        return b;
    }

    // 4. Auto-detect
    if macos_version_26_or_later() && fskit_app_installed() {
        return Backend::FsKit;
    }

    Backend::Nfs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_flag_wins() {
        assert_eq!(
            detect_backend(Some(Backend::FsKit), Some(Backend::Nfs)),
            Backend::FsKit
        );
    }

    #[test]
    fn config_default_used_when_no_flag() {
        // Clear env to avoid interference
        std::env::remove_var("CTXFS_BACKEND");
        let result = detect_backend(None, Some(Backend::Nfs));
        assert_eq!(result, Backend::Nfs);
    }

    #[test]
    fn no_flag_no_config_falls_back() {
        std::env::remove_var("CTXFS_BACKEND");
        let result = detect_backend(None, None);
        // On non-macOS or macOS < 26, should be Nfs
        // On macOS 26+ without app installed, should be Nfs
        // We can't predict the exact result in CI, but it should not panic
        assert!(result == Backend::Nfs || result == Backend::FsKit);
    }
}
```

- [ ] **Step 2: Add `--backend` flag to Mount command**

In `crates/ctxfs-cli/src/main.rs`, add to the `Mount` variant of `Commands`:

```rust
/// Override backend selection (auto-detected by default).
#[arg(long, value_enum)]
backend: Option<Backend>,
```

Add `use ctxfs_core::Backend;` and implement `clap::ValueEnum` for `Backend` (or use the existing `FromStr` with `value_parser`).

- [ ] **Step 3: Wire detection into handle_mount()**

At the start of mount handling:

```rust
let backend = backend::detect_backend(args.backend, config.default_backend);
```

For now, if `backend == Backend::FsKit`, print a message and fall back to NFS:

```rust
if backend == Backend::FsKit {
    eprintln!("FSKit backend selected but not yet implemented — falling back to NFS");
    // proceed with NFS path
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p ctxfs-cli`
Expected: All existing + new tests pass.

- [ ] **Step 5: Commit**

```bash
git add crates/ctxfs-cli/src/backend.rs crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): add --backend flag and auto-detection logic

CLI now supports --backend nfs|fskit to override backend selection.
Auto-detection checks macOS version >= 26 and CtxfsFS.app presence.
Priority chain: flag > env > config > auto-detect. FSKit path is
stubbed out with a fallback message until the backend is wired up."
```

---

## Task 8: Symlink Management

**Files:**
- Create: `crates/ctxfs-cli/src/symlink.rs`

- [ ] **Step 1: Create symlink.rs with tests**

```rust
use std::fs;
use std::path::{Path, PathBuf};

/// Create a symlink from `link_path` to `target_path`.
/// The `link_path` is canonicalized to an absolute path first.
/// Parent directories are created if they don't exist.
pub fn create_symlink(link_path: &Path, target_path: &Path) -> std::io::Result<PathBuf> {
    // Canonicalize the link's parent directory (must exist).
    let parent = link_path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no parent directory")
    })?;
    fs::create_dir_all(parent)?;
    let abs_parent = parent.canonicalize()?;
    let abs_link = abs_parent.join(link_path.file_name().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "no file name")
    })?);

    #[cfg(unix)]
    std::os::unix::fs::symlink(target_path, &abs_link)?;

    Ok(abs_link)
}

/// Remove a symlink, but only if it points into `/Volumes/ctxfs/`.
/// Returns Ok(true) if removed, Ok(false) if skipped (repointed or not a symlink).
pub fn safe_remove_symlink(link_path: &Path) -> std::io::Result<bool> {
    let meta = match fs::symlink_metadata(link_path) {
        Ok(m) => m,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e),
    };

    if !meta.is_symlink() {
        return Ok(false);
    }

    let target = fs::read_link(link_path)?;
    let target_str = target.to_string_lossy();
    if !target_str.starts_with("/Volumes/ctxfs/") {
        tracing::warn!(
            "symlink {} does not point to /Volumes/ctxfs/, skipping removal (target: {})",
            link_path.display(),
            target_str
        );
        return Ok(false);
    }

    fs::remove_file(link_path)?;
    Ok(true)
}

/// Check if a path is a symlink pointing into /Volumes/ctxfs/.
pub fn is_ctxfs_symlink(path: &Path) -> bool {
    fs::symlink_metadata(path)
        .ok()
        .filter(|m| m.is_symlink())
        .and_then(|_| fs::read_link(path).ok())
        .is_some_and(|target| target.to_string_lossy().starts_with("/Volumes/ctxfs/"))
}

/// Resolve a user-provided path: if it's a ctxfs symlink, return the target.
pub fn resolve_ctxfs_path(path: &Path) -> PathBuf {
    if is_ctxfs_symlink(path) {
        fs::read_link(path).unwrap_or_else(|_| path.to_path_buf())
    } else {
        path.to_path_buf()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn create_and_remove_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let target = tmp.path().join("Volumes/ctxfs/react-19.1.0");
        fs::create_dir_all(&target).unwrap();
        fs::write(target.join("README.md"), "hello").unwrap();

        let link = tmp.path().join("deps/react");
        let abs_link = create_symlink(&link, &target).unwrap();

        assert!(abs_link.is_absolute());
        assert!(fs::symlink_metadata(&abs_link).unwrap().is_symlink());

        // safe_remove won't remove because target doesn't start with /Volumes/ctxfs/
        // (it's in a tempdir). Test the path check:
        assert!(!safe_remove_symlink(&abs_link).unwrap()); // skipped — wrong prefix
    }

    #[test]
    fn safe_remove_nonexistent() {
        let tmp = tempfile::tempdir().unwrap();
        let result = safe_remove_symlink(&tmp.path().join("nope"));
        assert_eq!(result.unwrap(), false);
    }

    #[test]
    fn safe_remove_not_symlink() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("regular_file");
        fs::write(&file, "data").unwrap();
        assert_eq!(safe_remove_symlink(&file).unwrap(), false);
    }

    #[test]
    fn resolve_ctxfs_path_regular_file() {
        let tmp = tempfile::tempdir().unwrap();
        let file = tmp.path().join("regular");
        fs::write(&file, "data").unwrap();
        assert_eq!(resolve_ctxfs_path(&file), file);
    }
}
```

- [ ] **Step 2: Register module**

Add `pub mod symlink;` to `crates/ctxfs-cli/src/main.rs` (or a `mod.rs` if the CLI uses one).

- [ ] **Step 3: Run tests**

Run: `cargo test -p ctxfs-cli symlink`
Expected: All 4 tests pass.

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-cli/src/symlink.rs crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): add symlink management for FSKit mount paths

Symlink helpers for the FSKit backend: create_symlink canonicalizes
to absolute paths, safe_remove_symlink verifies target is /Volumes/ctxfs/
before deleting (prevents accidental removal of repointed links),
resolve_ctxfs_path resolves user paths through symlinks."
```

---

## Task 9: Mount State Persistence

**Files:**
- Create: `crates/ctxfs-daemon/src/mount_state.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ctxfs-daemon/src/mount_state.rs` with tests:

```rust
use ctxfs_core::Backend;
use serde::{Deserialize, Serialize};
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MountStateEntry {
    pub source: String,
    pub volume_path: String,
    pub symlink_paths: Vec<String>,
    pub backend: Backend,
    pub tcp_port: Option<u16>,
    pub auth_token: Option<String>,
}

/// Manages atomic persistence of mount state for crash recovery.
pub struct MountStateFile {
    path: PathBuf,
}

impl MountStateFile {
    pub fn new(base_dir: &Path) -> Self {
        Self {
            path: base_dir.join("mounts.json"),
        }
    }

    /// Read all entries. Returns empty vec if file doesn't exist or is corrupted.
    pub fn read(&self) -> Vec<MountStateEntry> {
        match fs::read_to_string(&self.path) {
            Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
            Err(_) => Vec::new(),
        }
    }

    /// Write entries atomically: write to .tmp, fsync, rename.
    pub fn write(&self, entries: &[MountStateEntry]) -> std::io::Result<()> {
        let tmp_path = self.path.with_extension("json.tmp");

        let data = serde_json::to_string_pretty(entries)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::Other, e))?;

        {
            let mut file = fs::File::create(&tmp_path)?;
            file.write_all(data.as_bytes())?;
            file.sync_all()?;
        }

        fs::rename(&tmp_path, &self.path)?;
        Ok(())
    }

    /// Add an entry and persist.
    pub fn add(&self, entry: MountStateEntry) -> std::io::Result<()> {
        let mut entries = self.read();
        // Remove existing entry for same volume_path (idempotent)
        entries.retain(|e| e.volume_path != entry.volume_path);
        entries.push(entry);
        self.write(&entries)
    }

    /// Add a symlink to an existing entry.
    pub fn add_symlink(&self, volume_path: &str, symlink: &str) -> std::io::Result<()> {
        let mut entries = self.read();
        if let Some(entry) = entries.iter_mut().find(|e| e.volume_path == volume_path) {
            if !entry.symlink_paths.contains(&symlink.to_string()) {
                entry.symlink_paths.push(symlink.to_string());
            }
        }
        self.write(&entries)
    }

    /// Remove a symlink from an entry. Returns true if the entry has no more symlinks.
    pub fn remove_symlink(&self, symlink: &str) -> std::io::Result<bool> {
        let mut entries = self.read();
        let mut no_more_links = false;

        for entry in &mut entries {
            entry.symlink_paths.retain(|s| s != symlink);
            if entry.symlink_paths.is_empty() {
                no_more_links = true;
            }
        }

        self.write(&entries)?;
        Ok(no_more_links)
    }

    /// Remove an entry by volume path.
    pub fn remove_volume(&self, volume_path: &str) -> std::io::Result<()> {
        let mut entries = self.read();
        entries.retain(|e| e.volume_path != volume_path);
        self.write(&entries)
    }

    /// Clear all entries (daemon startup).
    pub fn clear(&self) -> std::io::Result<()> {
        self.write(&[])
    }

    /// Get the file path (for logging).
    pub fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_entry(source: &str, volume: &str) -> MountStateEntry {
        MountStateEntry {
            source: source.into(),
            volume_path: volume.into(),
            symlink_paths: vec![],
            backend: Backend::FsKit,
            tcp_port: Some(12345),
            auth_token: Some("abcd".into()),
        }
    }

    #[test]
    fn read_nonexistent_returns_empty() {
        let tmp = tempfile::tempdir().unwrap();
        let msf = MountStateFile::new(tmp.path());
        assert!(msf.read().is_empty());
    }

    #[test]
    fn write_and_read_roundtrip() {
        let tmp = tempfile::tempdir().unwrap();
        let msf = MountStateFile::new(tmp.path());

        let entry = make_entry("npm:react@19.1.0", "/Volumes/ctxfs/react-19.1.0");
        msf.add(entry).unwrap();

        let entries = msf.read();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].source, "npm:react@19.1.0");
    }

    #[test]
    fn add_symlink_to_entry() {
        let tmp = tempfile::tempdir().unwrap();
        let msf = MountStateFile::new(tmp.path());

        msf.add(make_entry("npm:react@19.1.0", "/Volumes/ctxfs/react-19.1.0"))
            .unwrap();
        msf.add_symlink("/Volumes/ctxfs/react-19.1.0", "/project-a/deps/react")
            .unwrap();
        msf.add_symlink("/Volumes/ctxfs/react-19.1.0", "/project-b/deps/react")
            .unwrap();

        let entries = msf.read();
        assert_eq!(entries[0].symlink_paths.len(), 2);
    }

    #[test]
    fn remove_symlink_returns_empty_flag() {
        let tmp = tempfile::tempdir().unwrap();
        let msf = MountStateFile::new(tmp.path());

        let mut entry = make_entry("npm:react@19.1.0", "/Volumes/ctxfs/react-19.1.0");
        entry.symlink_paths = vec!["/a".into(), "/b".into()];
        msf.add(entry).unwrap();

        assert!(!msf.remove_symlink("/a").unwrap()); // still has /b
        assert!(msf.remove_symlink("/b").unwrap()); // now empty
    }

    #[test]
    fn remove_volume() {
        let tmp = tempfile::tempdir().unwrap();
        let msf = MountStateFile::new(tmp.path());

        msf.add(make_entry("a", "/Volumes/ctxfs/a")).unwrap();
        msf.add(make_entry("b", "/Volumes/ctxfs/b")).unwrap();
        assert_eq!(msf.read().len(), 2);

        msf.remove_volume("/Volumes/ctxfs/a").unwrap();
        let entries = msf.read();
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].volume_path, "/Volumes/ctxfs/b");
    }

    #[test]
    fn atomic_write_survives_corruption() {
        let tmp = tempfile::tempdir().unwrap();
        let msf = MountStateFile::new(tmp.path());

        msf.add(make_entry("a", "/Volumes/ctxfs/a")).unwrap();

        // Corrupt the file
        fs::write(msf.path(), "not json{{{").unwrap();

        // Read should return empty (graceful degradation)
        assert!(msf.read().is_empty());

        // But we can still write a new valid state
        msf.add(make_entry("b", "/Volumes/ctxfs/b")).unwrap();
        assert_eq!(msf.read().len(), 1);
    }

    #[test]
    fn clear_removes_all() {
        let tmp = tempfile::tempdir().unwrap();
        let msf = MountStateFile::new(tmp.path());

        msf.add(make_entry("a", "/a")).unwrap();
        msf.add(make_entry("b", "/b")).unwrap();
        msf.clear().unwrap();
        assert!(msf.read().is_empty());
    }
}
```

- [ ] **Step 2: Run tests**

Run: `cargo test -p ctxfs-daemon mount_state`
Expected: All 7 tests pass.

- [ ] **Step 3: Register module**

Add `pub mod mount_state;` to `crates/ctxfs-daemon/src/lib.rs` or the daemon's module tree.

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-daemon/src/mount_state.rs
git commit -m "feat(daemon): add atomic mount state persistence for crash recovery

MountStateFile manages ~/.ctxfs/mounts.json with atomic writes
(temp file + fsync + rename). Tracks volume paths, symlinks, backend
type, TCP port, and auth token per mount. Gracefully handles
corrupted files by returning empty state."
```

---

## Task 10: Setup FSKit Installation Flow

**Files:**
- Modify: `crates/ctxfs-cli/src/setup.rs`
- Modify: `crates/ctxfs-cli/src/main.rs`

- [ ] **Step 1: Add `install-fskit` and updated `check` to setup.rs**

Add these functions to the existing `setup.rs`:

```rust
/// Check if running on macOS 26+.
pub fn is_macos_26_or_later() -> bool {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        Command::new("sw_vers")
            .arg("-productVersion")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    let v = String::from_utf8_lossy(&o.stdout);
                    v.trim().split('.').next()?.parse::<u32>().ok()
                } else {
                    None
                }
            })
            .is_some_and(|major| major >= 26)
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Find CtxfsFS.app in known locations.
pub fn find_fskit_app() -> Option<std::path::PathBuf> {
    let candidates = [
        dirs::home_dir().map(|h| h.join("Applications/CtxfsFS.app")),
        Some(std::path::PathBuf::from("/Applications/CtxfsFS.app")),
        std::env::var("CTXFS_FSKIT_APP_PATH").ok().map(std::path::PathBuf::from),
        // Next to the current binary
        std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("CtxfsFS.app"))),
    ];
    candidates.into_iter().flatten().find(|p| p.exists())
}

/// Install the FSKit extension app.
pub fn install_fskit() -> Result<(), String> {
    let source = find_fskit_app()
        .ok_or("CtxfsFS.app not found. Download it from the ctxfs GitHub releases page.")?;

    let dest = dirs::home_dir()
        .ok_or("cannot determine home directory")?
        .join("Applications/CtxfsFS.app");

    if dest.exists() {
        println!("CtxfsFS.app already installed at {}", dest.display());
    } else {
        std::fs::create_dir_all(dest.parent().unwrap())
            .map_err(|e| format!("failed to create ~/Applications: {e}"))?;

        // Copy the .app bundle
        let status = std::process::Command::new("cp")
            .args(["-R", &source.to_string_lossy(), &dest.to_string_lossy()])
            .status()
            .map_err(|e| format!("failed to copy app: {e}"))?;

        if !status.success() {
            return Err("failed to copy CtxfsFS.app".into());
        }
        println!("Installed CtxfsFS.app to {}", dest.display());
    }

    // Create /Volumes/ctxfs/ if it doesn't exist
    let vol_dir = std::path::Path::new("/Volumes/ctxfs");
    if !vol_dir.exists() {
        println!("Creating /Volumes/ctxfs/ (requires sudo)...");
        let status = std::process::Command::new("sudo")
            .args(["mkdir", "-p", "/Volumes/ctxfs"])
            .status()
            .map_err(|e| format!("failed to create /Volumes/ctxfs: {e}"))?;
        if !status.success() {
            return Err("failed to create /Volumes/ctxfs".into());
        }
    }

    // Open System Settings for extension toggle
    println!("\nPlease enable the CtxfsFS extension in System Settings:");
    println!("  System Settings → General → Login Items & Extensions → File System Extensions");
    println!("  Toggle ON: CtxfsFS\n");

    #[cfg(target_os = "macos")]
    {
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.settings.General.LoginItems")
            .status();
    }

    Ok(())
}

/// Print FSKit status in `setup check`.
pub fn check_fskit_status() {
    if !is_macos_26_or_later() {
        println!("\nFSKit backend:");
        println!("  macOS version: < 26 (not supported)");
        return;
    }

    println!("\nFSKit backend:");

    // macOS version
    let version = std::process::Command::new("sw_vers")
        .arg("-productVersion")
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_else(|| "unknown".into());
    println!("  macOS version: {} (supported)", version);

    // App installed
    match find_fskit_app() {
        Some(path) => println!("  CtxfsFS.app: Installed ({})", path.display()),
        None => println!("  CtxfsFS.app: Not installed"),
    }

    // Mount directory
    if std::path::Path::new("/Volumes/ctxfs").exists() {
        println!("  Mount directory: /Volumes/ctxfs/ exists");
    } else {
        println!("  Mount directory: /Volumes/ctxfs/ not found");
    }
}
```

- [ ] **Step 2: Add `install-fskit` subcommand**

In `crates/ctxfs-cli/src/main.rs`, add to `SetupAction`:

```rust
/// Install FSKit extension for macOS 26+ (no sudo, no FDA).
InstallFskit,
```

Handle it:

```rust
SetupAction::InstallFskit => {
    setup::install_fskit().map_err(|e| anyhow::anyhow!(e))?;
}
```

- [ ] **Step 3: Update `setup install` to prompt for FSKit on macOS 26+**

After the existing NFS sudoers installation, add:

```rust
if setup::is_macos_26_or_later() {
    println!("\nFSKit is available on your macOS. With FSKit:");
    println!("  - No sudo needed per mount");
    println!("  - No Full Disk Access needed");
    println!("  - Better native macOS integration");
    println!();
    println!("Without FSKit, mounts use NFS (requires sudo + Full Disk Access).");
    println!();

    let install = dialoguer::Confirm::new()
        .with_prompt("Install CtxfsFS.app to enable FSKit?")
        .default(true)
        .interact()
        .unwrap_or(false);

    if install {
        if let Err(e) = setup::install_fskit() {
            eprintln!("FSKit install failed: {e}");
            eprintln!("You can retry later with: ctxfs setup install-fskit");
        }
    }
}
```

- [ ] **Step 4: Update `setup check` to include FSKit status**

Add `setup::check_fskit_status();` after the existing NFS check output.

- [ ] **Step 5: Run tests**

Run: `cargo test -p ctxfs-cli`
Expected: All tests pass. (The FSKit-specific functions are behind `#[cfg(target_os = "macos")]` where needed.)

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-cli/src/setup.rs crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): add FSKit setup flow and install-fskit subcommand

setup install now prompts for FSKit on macOS 26+. install-fskit copies
CtxfsFS.app to ~/Applications, creates /Volumes/ctxfs/, and opens
System Settings for extension toggle. setup check reports FSKit status
(app installed, macOS version, mount directory)."
```

---

## Task 11: Update CLAUDE.md and Skills

**Files:**
- Modify: `CLAUDE.md`
- Modify: `.claude/skills/ctxfs/SKILL.md`
- Modify: `.claude/skills/ctxfs-dev/SKILL.md`

- [ ] **Step 1: Update CLAUDE.md architecture section**

Add `ctxfs-vfs` and `ctxfs-fskit` to the crate list. Update count from 12 to 15. Add `CTXFS_BACKEND` to environment variables.

- [ ] **Step 2: Update ctxfs user skill**

Add a note about FSKit backend in the setup section:
- On macOS 26+, mention that FSKit is the preferred backend (no sudo/FDA)
- Add `ctxfs setup install-fskit` to the feasibility checks
- Note that `/Volumes/ctxfs/` paths may appear when FSKit is active

- [ ] **Step 3: Update ctxfs-dev skill**

Add `ctxfs-vfs` and `ctxfs-fskit` to the crate layout. Add FSKit-related gotchas. Update the "Where to Add X" table.

- [ ] **Step 4: Commit**

```bash
git add CLAUDE.md .claude/skills/
git commit -m "docs: update CLAUDE.md and skills for FSKit backend

Adds ctxfs-vfs and ctxfs-fskit to architecture docs, CTXFS_BACKEND
to environment variables, and FSKit guidance to both user and dev skills."
```

---

## Summary

| Task | Component | What it does |
|---|---|---|
| 1 | ctxfs-core | `Backend` enum with serde/display/fromstr |
| 2 | ctxfs-ipc | `MountInfo` gains backend, volume_path, symlink_paths |
| 3 | ctxfs-vfs | New crate with types (NodeAttr, VfsError) |
| 4 | ctxfs-vfs | `VfsState` implementation (extracted from ctxfs-nfs) |
| 5 | ctxfs-nfs | Refactor to thin adapter over VfsState |
| 6 | ctxfs-fskit | New crate with auth token + VFS wrapper stub |
| 7 | ctxfs-cli | `--backend` flag and auto-detection |
| 8 | ctxfs-cli | Symlink management helpers |
| 9 | ctxfs-daemon | Mount state persistence (mounts.json) |
| 10 | ctxfs-cli | Setup FSKit install flow |
| 11 | docs | CLAUDE.md + skills updates |

**Not in this plan (deferred to after Phase 0 PoC):**
- `fskit_rs::Filesystem` trait implementation in ctxfs-fskit
- TCP listener integration in daemon's do_mount FSKit path
- Swift appex vendoring and customization
- Finder eject support, volume icons, display names
- `setup default-backend` persistent preference
- `setup uninstall-fskit`

These depend on Phase 0 confirming the fskit-rs API and FSKitBridge behavior.
