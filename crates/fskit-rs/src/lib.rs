// TODO(phase1.5): upstream fskit-rs has pedantic lint violations; suppress
// here to avoid churn on vendored code. Remove after upstream PR merges or
// this fork stabilizes on its own lint budget.
#![allow(clippy::pedantic)]
// Suppress all clippy::all denies from workspace lints — upstream code is
// unmodified; errors here are noise until we own the codebase changes.
#![allow(clippy::all)]
// Suppress workspace rust lint: upstream spawns tasks without awaiting handles
#![allow(unused_results)]

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use async_trait::async_trait;

pub use crate::pb::check_access::AccessMask;
pub use crate::pb::preallocate_space::PreallocateFlag;
pub use crate::pb::set_xattr::SetXattrPolicy;
pub use crate::pb::supported_capabilities::CaseFormat;
pub use crate::pb::synchronize::SyncFlags;
pub use crate::pb::{
    DirectoryEntries, Item, ItemAttributes, ItemType, OpenMode, PathConfOperations,
    ResourceIdentifier, StatFsResult, SupportedCapabilities, TaskOptions, VolumeBehavior,
    VolumeIdentifier, Xattrs, directory_entries,
};
use crate::session::Session;

pub mod auth;
mod handler;
mod info;
pub mod installer;
pub mod mounter;
pub mod session;
pub mod socket;

mod pb {
    include!(concat!(env!("OUT_DIR"), "/pb.rs"));
}

/// Re-export of all generated protobuf types for use by downstream crates.
pub mod protocol {
    pub use super::pb::*;
}

const FSKIT_ID: &str = "network.debox.fskitbridge.fskitext";
const DEFAULT_MOUNT_POINT: &str = "/tmp/fskitbridge";

pub type Result<T> = std::result::Result<T, Error>;

#[async_trait]
pub trait Filesystem {
    /// Get the resource identifier and name.
    async fn get_resource_identifier(&mut self) -> Result<ResourceIdentifier>;

    /// Get the volume identifier and name.
    async fn get_volume_identifier(&mut self) -> Result<VolumeIdentifier>;

    /// Get options that tell FSKit to declare behaviors and selectively inhibit
    /// operation protocols.
    async fn get_volume_behavior(&mut self) -> Result<VolumeBehavior>;

    /// Get properties implemented by volumes that support providing the values of
    /// system limits or options.
    async fn get_path_conf_operations(&mut self) -> Result<PathConfOperations>;

    /// Get properties that provide the supported capabilities of the volume.
    async fn get_volume_capabilities(&mut self) -> Result<SupportedCapabilities>;

    /// Get properties that provide up-to-date statistics of the volume.
    async fn get_volume_statistics(&mut self) -> Result<StatFsResult>;

    /// Mounts this volume, using the specified options.
    async fn mount(&mut self, options: TaskOptions) -> Result<()>;

    /// Unmounts this volume.
    async fn unmount(&mut self) -> Result<()>;

    /// Synchronizes the volume with its underlying resource.
    async fn synchronize(&mut self, flags: SyncFlags) -> Result<()>;

    /// Fetches attributes for the given item.
    async fn get_attributes(&mut self, item_id: u64) -> Result<ItemAttributes>;

    /// Sets the given attributes on an item.
    async fn set_attributes(
        &mut self,
        item_id: u64,
        attributes: ItemAttributes,
    ) -> Result<ItemAttributes>;

    /// Looks up an item within a directory.
    async fn lookup_item(&mut self, name: &OsStr, directory_id: u64) -> Result<Item>;

    /// Reclaims an item, releasing any resources allocated for the item.
    async fn reclaim_item(&mut self, item_id: u64) -> Result<()>;

    /// Reads a symbolic link.
    async fn read_symbolic_link(&mut self, item_id: u64) -> Result<Vec<u8>>;

    /// Creates a new file or directory item.
    async fn create_item(
        &mut self,
        name: &OsStr,
        r#type: ItemType,
        directory_id: u64,
        attributes: ItemAttributes,
    ) -> Result<Item>;

    /// Creates a new symbolic link.
    async fn create_symbolic_link(
        &mut self,
        name: &OsStr,
        directory_id: u64,
        new_attributes: ItemAttributes,
        contents: Vec<u8>,
    ) -> Result<Item>;

    /// Creates a new hard link.
    async fn create_link(
        &mut self,
        item_id: u64,
        name: &OsStr,
        directory_id: u64,
    ) -> Result<Vec<u8>>;

    /// Removes an existing item from a given directory.
    async fn remove_item(&mut self, item_id: u64, name: &OsStr, directory_id: u64) -> Result<()>;

    /// Renames an item from one path in the file system to another.
    async fn rename_item(
        &mut self,
        item_id: u64,
        source_directory_id: u64,
        source_name: &OsStr,
        destination_name: &OsStr,
        destination_directory_id: u64,
        over_item_id: Option<u64>,
    ) -> Result<Vec<u8>>;

    /// Enumerates the contents of the given directory.
    async fn enumerate_directory(
        &mut self,
        directory_id: u64,
        cookie: u64,
        verifier: u64,
    ) -> Result<DirectoryEntries>;

    /// Activates the volume using the specified options.
    async fn activate(&mut self, options: TaskOptions) -> Result<Item>;

    /// Tears down a previously initialized volume instance.
    async fn deactivate(&mut self) -> Result<()>;

    /// Returns an array that specifies the extended attribute names the given
    /// item supports.
    async fn get_supported_xattr_names(&mut self, item_id: u64) -> Result<Xattrs>;

    /// Gets the specified extended attribute of the given item.
    async fn get_xattr(&mut self, name: &OsStr, item_id: u64) -> Result<Vec<u8>>;

    /// Sets the specified extended attribute data on the given item.
    async fn set_xattr(
        &mut self,
        name: &OsStr,
        value: Option<Vec<u8>>,
        item_id: u64,
        policy: SetXattrPolicy,
    ) -> Result<()>;

    /// Gets the list of extended attributes currently set on the given item.
    async fn get_xattrs(&mut self, item_id: u64) -> Result<Xattrs>;

    /// Opens a file for access.
    async fn open_item(&mut self, item_id: u64, modes: Vec<OpenMode>) -> Result<()>;

    /// Closes a file from further access.
    async fn close_item(&mut self, item_id: u64, modes: Vec<OpenMode>) -> Result<()>;

    /// Reads the contents of the given file item.
    async fn read(&mut self, item_id: u64, offset: i64, length: i64) -> Result<Vec<u8>>;

    /// Writes contents to the given file item.
    async fn write(&mut self, contents: Vec<u8>, item_id: u64, offset: i64) -> Result<i64>;

    /// Checks whether the file system allows access to the given item.
    async fn check_access(&mut self, item_id: u64, access: Vec<AccessMask>) -> Result<bool>;

    /// Sets a new name for the volume.
    async fn set_volume_name(&mut self, name: Vec<u8>) -> Result<Vec<u8>>;

    /// Preallocate disk space for the given item.
    async fn preallocate_space(
        &mut self,
        item_id: u64,
        offset: i64,
        length: i64,
        flags: Vec<PreallocateFlag>,
    ) -> Result<i64>;

    /// Notifies the file system that the kernel is no longer making immediate use of
    /// the given item.
    async fn deactivate_item(&mut self, item_id: u64) -> Result<()>;
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error("POSIX error: {0}")]
    Posix(std::ffi::c_int),
}

/// Configuration for mounting the file system and connecting between `FSKitExt` and `fskit-rs`.
///
/// # Parameters
/// * `fskit_id` — Bundle identifier of the FSKit extension used for registration
///   and election. Default: `network.debox.fskitbridge.fskitext`.
/// * `mount_point` — Existing (usually empty) directory to mount onto. Use `/Volumes/<Name>`
///   (may require `sudo`) or a user-owned path. Default: `/tmp/fskitbridge`.
/// * `force` — If `true`, preflight **unmounts** anything already mounted at `mount_point`
///   before mounting. Default: `true`.
/// * `auth_token` — Optional 32-byte per-mount shared secret. When set, every TCP
///   connection must send an `AuthenticateRequest` as its first frame before any VFS
///   requests are dispatched. If `None`, all connections are accepted (backward-compatible
///   with upstream behavior).
/// * `task_options` — Extra strings passed to fskitd as `FSTaskOptions`. The daemon uses
///   this to forward the auth token hex to the appex (e.g. `"token=<hex>"`).
#[derive(Debug, Clone)]
pub struct MountOptions {
    pub fskit_id: String,
    pub mount_point: PathBuf,
    pub force: bool,
    /// Raw 32-byte auth token; `None` means no auth enforcement (backward-compatible).
    pub auth_token: Option<Vec<u8>>,
    /// Extra strings forwarded to the appex as `FSTaskOptions` (equivalent to argv).
    pub task_options: Vec<String>,
}

impl Default for MountOptions {
    fn default() -> Self {
        Self {
            fskit_id: FSKIT_ID.to_string(),
            mount_point: PathBuf::from(DEFAULT_MOUNT_POINT),
            force: true,
            auth_token: None,
            task_options: Vec::new(),
        }
    }
}

/// Mounts a user-space file system at `opts.mount_point` and returns a `Session` that
/// keeps the mount alive. Non-blocking: background workers serve kernel requests;
/// dropping `Session` cleanly unmounts.
///
/// # Parameters
/// * `fs` — Your `Filesystem` impl. Must be `Send + Sync + Clone + 'static`.
///   Prefer keeping a heavy state in `Arc<_>`.
/// * `opts` — Combined mount/connection configuration.
///
/// # Returns
/// A `Session` handle; while it’s alive, the mount remains active. Dropping it unmounts.
///
/// # macOS (FSKit) notes
/// * The extension must be **enabled** in System Settings (File System Extensions).
/// * FSKit mounts use `noowners`; you can store/report uid/gid in metadata,
///   but host POSIX enforcement will still be disabled.
pub async fn mount<FS>(fs: FS, opts: MountOptions) -> session::Result<Session>
where
    FS: Filesystem + Send + Sync + Clone + 'static,
{
    Session::new(fs, opts).await
}

/// Installs the FSKit host application into `/Applications` and registers its extension.
///
/// # Behavior
/// - If `force` is `true`, removes any existing app from `/Applications`.
/// - Copies the app bundle from `path` to `/Applications`.
/// - Removes the quarantine attribute from the installed app.
/// - Launches the installed host app once to trigger extension discovery and registration.
///
/// # Commands
/// ```text
/// rm -rf /Applications/<app>
/// cp -r <path_to_app> /Applications
/// xattr -dr com.apple.quarantine /Applications/<app>
/// open -a /Applications/<app> --args -s
/// ```
pub fn install<P: AsRef<Path>>(path: P, force: bool) -> installer::Result<()> {
    installer::run(path.as_ref(), force)
}

#[macro_export]
macro_rules! path {
    ($arg:expr) => {
        $arg.as_os_str().to_str().unwrap()
    };
}
