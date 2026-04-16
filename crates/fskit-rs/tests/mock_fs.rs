//! Minimal `Filesystem` stub for integration tests.
//!
//! Every method returns a POSIX error (ENOSYS) so the test can confirm that
//! the auth gate prevents any handler dispatch before authentication.

use std::ffi::OsStr;
use std::sync::{Arc, Mutex};

use async_trait::async_trait;
use fskit_rs::{
    AccessMask, DirectoryEntries, Error, Filesystem, Item, ItemAttributes, ItemType, OpenMode,
    PathConfOperations, PreallocateFlag, ResourceIdentifier, Result, SetXattrPolicy, StatFsResult,
    SupportedCapabilities, SyncFlags, TaskOptions, VolumeBehavior, VolumeIdentifier, Xattrs,
};

#[derive(Clone)]
pub struct MockFs {
    non_auth_calls: Arc<Mutex<u32>>,
}

impl MockFs {
    pub fn new(counter: Arc<Mutex<u32>>) -> Self {
        Self {
            non_auth_calls: counter,
        }
    }

    fn record(&self) -> Error {
        *self.non_auth_calls.lock().unwrap() += 1;
        Error::Posix(libc::ENOSYS)
    }
}

#[async_trait]
impl Filesystem for MockFs {
    async fn get_resource_identifier(&mut self) -> Result<ResourceIdentifier> {
        Err(self.record())
    }
    async fn get_volume_identifier(&mut self) -> Result<VolumeIdentifier> {
        Err(self.record())
    }
    async fn get_volume_behavior(&mut self) -> Result<VolumeBehavior> {
        Err(self.record())
    }
    async fn get_path_conf_operations(&mut self) -> Result<PathConfOperations> {
        Err(self.record())
    }
    async fn get_volume_capabilities(&mut self) -> Result<SupportedCapabilities> {
        Err(self.record())
    }
    async fn get_volume_statistics(&mut self) -> Result<StatFsResult> {
        Err(self.record())
    }
    async fn mount(&mut self, _options: TaskOptions) -> Result<()> {
        Err(self.record())
    }
    async fn unmount(&mut self) -> Result<()> {
        Err(self.record())
    }
    async fn synchronize(&mut self, _flags: SyncFlags) -> Result<()> {
        Err(self.record())
    }
    async fn get_attributes(&mut self, _item_id: u64) -> Result<ItemAttributes> {
        Err(self.record())
    }
    async fn set_attributes(
        &mut self,
        _item_id: u64,
        _attributes: ItemAttributes,
    ) -> Result<ItemAttributes> {
        Err(self.record())
    }
    async fn lookup_item(&mut self, _name: &OsStr, _directory_id: u64) -> Result<Item> {
        Err(self.record())
    }
    async fn reclaim_item(&mut self, _item_id: u64) -> Result<()> {
        Err(self.record())
    }
    async fn read_symbolic_link(&mut self, _item_id: u64) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn create_item(
        &mut self,
        _name: &OsStr,
        _type: ItemType,
        _directory_id: u64,
        _attributes: ItemAttributes,
    ) -> Result<Item> {
        Err(self.record())
    }
    async fn create_symbolic_link(
        &mut self,
        _name: &OsStr,
        _directory_id: u64,
        _new_attributes: ItemAttributes,
        _contents: Vec<u8>,
    ) -> Result<Item> {
        Err(self.record())
    }
    async fn create_link(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn remove_item(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> Result<()> {
        Err(self.record())
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
        Err(self.record())
    }
    async fn enumerate_directory(
        &mut self,
        _directory_id: u64,
        _cookie: u64,
        _verifier: u64,
    ) -> Result<DirectoryEntries> {
        Err(self.record())
    }
    async fn activate(&mut self, _options: TaskOptions) -> Result<Item> {
        Err(self.record())
    }
    async fn deactivate(&mut self) -> Result<()> {
        Err(self.record())
    }
    async fn get_supported_xattr_names(&mut self, _item_id: u64) -> Result<Xattrs> {
        Err(self.record())
    }
    async fn get_xattr(&mut self, _name: &OsStr, _item_id: u64) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn set_xattr(
        &mut self,
        _name: &OsStr,
        _value: Option<Vec<u8>>,
        _item_id: u64,
        _policy: SetXattrPolicy,
    ) -> Result<()> {
        Err(self.record())
    }
    async fn get_xattrs(&mut self, _item_id: u64) -> Result<Xattrs> {
        Err(self.record())
    }
    async fn open_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        Err(self.record())
    }
    async fn close_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        Err(self.record())
    }
    async fn read(&mut self, _item_id: u64, _offset: i64, _length: i64) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn write(&mut self, _contents: Vec<u8>, _item_id: u64, _offset: i64) -> Result<i64> {
        Err(self.record())
    }
    async fn check_access(&mut self, _item_id: u64, _access: Vec<AccessMask>) -> Result<bool> {
        Err(self.record())
    }
    async fn set_volume_name(&mut self, _name: Vec<u8>) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn preallocate_space(
        &mut self,
        _item_id: u64,
        _offset: i64,
        _length: i64,
        _flags: Vec<PreallocateFlag>,
    ) -> Result<i64> {
        Err(self.record())
    }
    async fn deactivate_item(&mut self, _item_id: u64) -> Result<()> {
        Err(self.record())
    }
}
