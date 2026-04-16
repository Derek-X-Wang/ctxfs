use std::ffi::OsStr;
use std::fs;

use async_trait::async_trait;
use tokio::signal;

use fskit_rs::{
    AccessMask, DirectoryEntries, Error, Filesystem, Item, ItemAttributes, ItemType, MountOptions,
    OpenMode, PathConfOperations, PreallocateFlag, ResourceIdentifier, Result, SetXattrPolicy,
    StatFsResult, SupportedCapabilities, SyncFlags, TaskOptions, VolumeBehavior, VolumeIdentifier,
    Xattrs, session,
};

#[derive(Clone)]
struct FsHandler;

#[async_trait]
impl Filesystem for FsHandler {
    async fn get_resource_identifier(&mut self) -> Result<ResourceIdentifier> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_volume_identifier(&mut self) -> Result<VolumeIdentifier> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_volume_behavior(&mut self) -> Result<VolumeBehavior> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_path_conf_operations(&mut self) -> Result<PathConfOperations> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_volume_capabilities(&mut self) -> Result<SupportedCapabilities> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_volume_statistics(&mut self) -> Result<StatFsResult> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn mount(&mut self, _options: TaskOptions) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn unmount(&mut self) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn synchronize(&mut self, _flags: SyncFlags) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_attributes(&mut self, _item_id: u64) -> Result<ItemAttributes> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn set_attributes(
        &mut self,
        _item_id: u64,
        _attributes: ItemAttributes,
    ) -> Result<ItemAttributes> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn lookup_item(&mut self, _name: &OsStr, _directory_id: u64) -> Result<Item> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn reclaim_item(&mut self, _item_id: u64) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn read_symbolic_link(&mut self, _item_id: u64) -> Result<Vec<u8>> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn create_item(
        &mut self,
        _name: &OsStr,
        _type: ItemType,
        _directory_id: u64,
        _attributes: ItemAttributes,
    ) -> Result<Item> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn create_symbolic_link(
        &mut self,
        _name: &OsStr,
        _directory_id: u64,
        _new_attributes: ItemAttributes,
        _contents: Vec<u8>,
    ) -> Result<Item> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn create_link(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> Result<Vec<u8>> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn remove_item(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn rename_item(
        &mut self,
        _item_id: u64,
        _source_directory_id: u64,
        _source_name: &OsStr,
        _destination_name: &OsStr,
        _destination_directory_id: u64,
        _over_item_id: Option<u64>,
    ) -> Result<Vec<u8>> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn enumerate_directory(
        &mut self,
        _directory_id: u64,
        _cookie: u64,
        _verifier: u64,
    ) -> Result<DirectoryEntries> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn activate(&mut self, _options: TaskOptions) -> Result<Item> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn deactivate(&mut self) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_supported_xattr_names(&mut self, _item_id: u64) -> Result<Xattrs> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_xattr(&mut self, _name: &OsStr, _item_id: u64) -> Result<Vec<u8>> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn set_xattr(
        &mut self,
        _name: &OsStr,
        _value: Option<Vec<u8>>,
        _item_id: u64,
        _policy: SetXattrPolicy,
    ) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn get_xattrs(&mut self, _item_id: u64) -> Result<Xattrs> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn open_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn close_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn read(&mut self, _item_id: u64, _offset: i64, _length: i64) -> Result<Vec<u8>> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn write(&mut self, _contents: Vec<u8>, _item_id: u64, _offset: i64) -> Result<i64> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn check_access(&mut self, _item_id: u64, _access: Vec<AccessMask>) -> Result<bool> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn set_volume_name(&mut self, _name: Vec<u8>) -> Result<Vec<u8>> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn preallocate_space(
        &mut self,
        _item_id: u64,
        _offset: i64,
        _length: i64,
        _flags: Vec<PreallocateFlag>,
    ) -> Result<i64> {
        Err(Error::Posix(libc::ENOSYS))
    }

    async fn deactivate_item(&mut self, _item_id: u64) -> Result<()> {
        Err(Error::Posix(libc::ENOSYS))
    }
}

#[tokio::main]
async fn main() -> session::Result<()> {
    let _ = env_logger::try_init();

    let handler = FsHandler;

    let opts = MountOptions::default();

    if !opts.mount_point.exists() {
        let _ = fs::create_dir(opts.mount_point.clone());
    }

    println!(
        "Mounting example filesystem at {}...",
        opts.mount_point.display()
    );

    let session = match fskit_rs::mount(handler, opts.clone()).await {
        Ok(session) => session,
        Err(err) => {
            eprintln!("Mount failed. Ensure the FSKit host app is installed and enabled.");
            return Err(err);
        }
    };

    println!(
        "Mounted. Press Ctrl+C to unmount {}.",
        opts.mount_point.display()
    );

    signal::ctrl_c().await?;

    drop(session);

    println!("Unmounted {}.", opts.mount_point.display());

    Ok(())
}
