use std::ffi::OsString;
use std::os::unix::ffi::OsStringExt;

use log::warn;

use crate::pb::check_access::AccessMask;
use crate::pb::preallocate_space::PreallocateFlag;
use crate::pb::set_xattr::SetXattrPolicy;
use crate::pb::synchronize::SyncFlags;
use crate::pb::{Success, request, response};
use crate::{Error, Filesystem, ItemType, OpenMode, Result};

#[derive(Clone, Debug)]
pub(super) struct Handler<FS>
where
    FS: Filesystem + Send + Sync + Clone + 'static,
{
    filesystem: FS,
}

impl<FS> Handler<FS>
where
    FS: Filesystem + Send + Sync + Clone + 'static,
{
    pub(super) fn new(filesystem: FS) -> Self {
        Self { filesystem }
    }

    pub(super) async fn handle(&mut self, request: request::Content) -> Result<response::Content> {
        Ok(match request {
            request::Content::GetResourceIdentifier(_) => {
                match self.filesystem.get_resource_identifier().await {
                    Ok(res) => response::Content::ResourceIdentifier(res),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::GetVolumeIdentifier(_) => {
                match self.filesystem.get_volume_identifier().await {
                    Ok(res) => response::Content::VolumeIdentifier(res),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::GetVolumeBehavior(_) => {
                match self.filesystem.get_volume_behavior().await {
                    Ok(res) => response::Content::VolumeBehavior(res),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::GetPathConfOperations(_) => {
                match self.filesystem.get_path_conf_operations().await {
                    Ok(res) => response::Content::PathConfOperations(res),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::GetVolumeCapabilities(_) => {
                match self.filesystem.get_volume_capabilities().await {
                    Ok(res) => response::Content::SupportedCapabilities(res),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::GetVolumeStatistics(_) => {
                match self.filesystem.get_volume_statistics().await {
                    Ok(res) => response::Content::StatFsResult(res),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::Mount(msg) => {
                let Some(options) = msg.options else {
                    warn!("Mount request missing options");
                    return Ok(response::Content::PosixError(libc::EINVAL));
                };
                match self.filesystem.mount(options).await {
                    Ok(_) => response::Content::Success(Success {}),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::Unmount(_) => match self.filesystem.unmount().await {
                Ok(_) => response::Content::Success(Success {}),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::Synchronize(msg) => match self
                .filesystem
                .synchronize(SyncFlags::try_from(msg.flags).unwrap_or_default())
                .await
            {
                Ok(_) => response::Content::Success(Success {}),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::GetAttributes(msg) => {
                match self.filesystem.get_attributes(msg.item_id).await {
                    Ok(attrs) => response::Content::ItemAttributes(attrs),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::SetAttributes(msg) => {
                let Some(attributes) = msg.attributes else {
                    warn!("SetAttributes request missing attributes");
                    return Ok(response::Content::PosixError(libc::EINVAL));
                };
                match self
                    .filesystem
                    .set_attributes(msg.item_id, attributes)
                    .await
                {
                    Ok(attrs) => response::Content::ItemAttributes(attrs),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::LookupItem(msg) => match self
                .filesystem
                .lookup_item(&OsString::from_vec(msg.name), msg.directory_id)
                .await
            {
                Ok(item) => response::Content::Item(item),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::ReclaimItem(msg) => {
                match self.filesystem.reclaim_item(msg.item_id).await {
                    Ok(_) => response::Content::Success(Success {}),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::ReadSymbolicLink(msg) => {
                match self.filesystem.read_symbolic_link(msg.item_id).await {
                    Ok(data) => response::Content::Data(data),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::CreateItem(msg) => {
                let Ok(item_type) = ItemType::try_from(msg.r#type) else {
                    warn!("CreateItem request contained unknown type: {}", msg.r#type);
                    return Ok(response::Content::PosixError(libc::EINVAL));
                };
                let Some(attributes) = msg.attributes else {
                    warn!("CreateItem request missing attributes");
                    return Ok(response::Content::PosixError(libc::EINVAL));
                };
                match self
                    .filesystem
                    .create_item(
                        &OsString::from_vec(msg.name),
                        item_type,
                        msg.directory_id,
                        attributes,
                    )
                    .await
                {
                    Ok(item) => response::Content::Item(item),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::CreateSymbolicLink(msg) => {
                let Some(attributes) = msg.new_attributes else {
                    warn!("CreateSymbolicLink request missing attributes");
                    return Ok(response::Content::PosixError(libc::EINVAL));
                };
                match self
                    .filesystem
                    .create_symbolic_link(
                        &OsString::from_vec(msg.name),
                        msg.directory_id,
                        attributes,
                        msg.contents,
                    )
                    .await
                {
                    Ok(item) => response::Content::Item(item),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::CreateLink(msg) => match self
                .filesystem
                .create_link(msg.item_id, &OsString::from_vec(msg.name), msg.directory_id)
                .await
            {
                Ok(data) => response::Content::Data(data),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::RemoveItem(msg) => match self
                .filesystem
                .remove_item(msg.item_id, &OsString::from_vec(msg.name), msg.directory_id)
                .await
            {
                Ok(_) => response::Content::Success(Success {}),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::RenameItem(msg) => match self
                .filesystem
                .rename_item(
                    msg.item_id,
                    msg.source_directory_id,
                    &OsString::from_vec(msg.source_name),
                    &OsString::from_vec(msg.destination_name),
                    msg.destination_directory_id,
                    msg.over_item_id,
                )
                .await
            {
                Ok(data) => response::Content::Data(data),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::EnumerateDirectory(msg) => match self
                .filesystem
                .enumerate_directory(msg.directory_id, msg.cookie, msg.verifier)
                .await
            {
                Ok(entries) => response::Content::DirectoryEntries(entries),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::Activate(msg) => {
                let Some(options) = msg.options else {
                    warn!("Activate request missing options");
                    return Ok(response::Content::PosixError(libc::EINVAL));
                };
                match self.filesystem.activate(options).await {
                    Ok(item) => response::Content::Item(item),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::Deactivate(_) => match self.filesystem.deactivate().await {
                Ok(_) => response::Content::Success(Success {}),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::GetSupportedXattrNames(msg) => {
                match self.filesystem.get_supported_xattr_names(msg.item_id).await {
                    Ok(xattrs) => response::Content::Xattrs(xattrs),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::GetXattr(msg) => match self
                .filesystem
                .get_xattr(&OsString::from_vec(msg.name), msg.item_id)
                .await
            {
                Ok(data) => response::Content::Data(data),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::SetXattr(msg) => match self
                .filesystem
                .set_xattr(
                    &OsString::from_vec(msg.name),
                    msg.value,
                    msg.item_id,
                    SetXattrPolicy::try_from(msg.policy).unwrap_or_default(),
                )
                .await
            {
                Ok(_) => response::Content::Success(Success {}),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::GetXattrs(msg) => match self.filesystem.get_xattrs(msg.item_id).await
            {
                Ok(xattrs) => response::Content::Xattrs(xattrs),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::OpenItem(msg) => match self
                .filesystem
                .open_item(
                    msg.item_id,
                    msg.modes
                        .iter()
                        .filter_map(|&raw| OpenMode::try_from(raw).ok())
                        .collect(),
                )
                .await
            {
                Ok(_) => response::Content::Success(Success {}),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::CloseItem(msg) => match self
                .filesystem
                .close_item(
                    msg.item_id,
                    msg.modes
                        .iter()
                        .filter_map(|&raw| OpenMode::try_from(raw).ok())
                        .collect(),
                )
                .await
            {
                Ok(_) => response::Content::Success(Success {}),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::Read(msg) => match self
                .filesystem
                .read(msg.item_id, msg.offset, msg.length)
                .await
            {
                Ok(data) => response::Content::Data(data),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::Write(msg) => match self
                .filesystem
                .write(msg.contents, msg.item_id, msg.offset)
                .await
            {
                Ok(count) => response::Content::ByteCount(count),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::CheckAccess(msg) => match self
                .filesystem
                .check_access(
                    msg.item_id,
                    msg.access
                        .iter()
                        .filter_map(|&raw| AccessMask::try_from(raw).ok())
                        .collect(),
                )
                .await
            {
                Ok(allow) => response::Content::Allow(allow),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::SetVolumeName(msg) => {
                match self.filesystem.set_volume_name(msg.name).await {
                    Ok(data) => response::Content::Data(data),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
            request::Content::PreallocateSpace(msg) => match self
                .filesystem
                .preallocate_space(
                    msg.item_id,
                    msg.offset,
                    msg.length,
                    msg.flags
                        .iter()
                        .filter_map(|&raw| PreallocateFlag::try_from(raw).ok())
                        .collect(),
                )
                .await
            {
                Ok(count) => response::Content::ByteCount(count),
                Err(Error::Posix(code)) => response::Content::PosixError(code),
            },
            request::Content::DeactivateItem(msg) => {
                match self.filesystem.deactivate_item(msg.item_id).await {
                    Ok(_) => response::Content::Success(Success {}),
                    Err(Error::Posix(code)) => response::Content::PosixError(code),
                }
            }
        })
    }
}
