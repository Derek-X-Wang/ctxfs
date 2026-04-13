//! FSKit Proof of Concept — minimal read-only filesystem with hardcoded files.
//!
//! This tests whether fskit-rs + FSKitBridge can mount a synthetic (non-block-device)
//! filesystem on macOS 26+.
//!
//! The filesystem contains:
//!   /README.md  (file, 45 bytes)
//!   /src/       (directory)
//!   /src/main.rs (file, 33 bytes)

use async_trait::async_trait;
use fskit_rs::{
    AccessMask, DirectoryEntries, Error, Filesystem, Item, ItemAttributes, ItemType,
    MountOptions, OpenMode, PathConfOperations, PreallocateFlag, ResourceIdentifier, Result,
    SetXattrPolicy, StatFsResult, SupportedCapabilities, SyncFlags, TaskOptions, VolumeBehavior,
    VolumeIdentifier, Xattrs, directory_entries, session,
};
use std::ffi::OsStr;

const ROOT_ID: u64 = 2; // FSKit root inode is typically 2
const README_ID: u64 = 3;
const SRC_DIR_ID: u64 = 4;
const MAIN_RS_ID: u64 = 5;

const README_CONTENT: &[u8] = b"# FSKit PoC\nThis file is served by fskit-rs.\n";
const MAIN_RS_CONTENT: &[u8] = b"fn main() { println!(\"hello\"); }\n";

#[derive(Clone)]
struct PocFs;

fn enosys() -> Error {
    Error::Posix(libc::ENOSYS)
}

fn enoent() -> Error {
    Error::Posix(libc::ENOENT)
}

fn erofs() -> Error {
    Error::Posix(libc::EROFS)
}

fn eisdir() -> Error {
    Error::Posix(libc::EISDIR)
}

fn make_item(name: &str, attrs: ItemAttributes) -> Item {
    Item {
        name: name.as_bytes().to_vec(),
        attributes: Some(attrs),
    }
}

fn dir_attrs(id: u64) -> ItemAttributes {
    ItemAttributes {
        file_id: Some(id),
        parent_id: Some(if id == ROOT_ID { ROOT_ID } else { ROOT_ID }),
        r#type: Some(ItemType::Directory as i32),
        mode: Some(0o755),
        uid: Some(501),
        gid: Some(20),
        link_count: Some(2),
        size: Some(0),
        alloc_size: Some(0),
        ..Default::default()
    }
}

fn file_attrs(id: u64, parent: u64, size: u64) -> ItemAttributes {
    ItemAttributes {
        file_id: Some(id),
        parent_id: Some(parent),
        r#type: Some(ItemType::File as i32),
        mode: Some(0o644),
        uid: Some(501),
        gid: Some(20),
        link_count: Some(1),
        size: Some(size),
        alloc_size: Some(size),
        ..Default::default()
    }
}

fn make_dir_entry(item: Item, cookie: u64) -> directory_entries::Entry {
    directory_entries::Entry {
        item: Some(item),
        next_cookie: cookie,
    }
}

#[async_trait]
impl Filesystem for PocFs {
    // --- Volume lifecycle ---

    async fn get_resource_identifier(&mut self) -> Result<ResourceIdentifier> {
        Ok(ResourceIdentifier {
            name: Some("ctxfs-poc".into()),
            container_id: Some("com.ctxfs.poc".into()),
        })
    }

    async fn get_volume_identifier(&mut self) -> Result<VolumeIdentifier> {
        Ok(VolumeIdentifier {
            id: Some("ctxfs-poc-vol".into()),
            name: Some("ctxfs-poc".into()),
        })
    }

    async fn get_volume_behavior(&mut self) -> Result<VolumeBehavior> {
        Ok(VolumeBehavior {
            is_open_close_inhibited: Some(true),
            is_access_check_inhibited: Some(true),
            is_volume_rename_inhibited: Some(true),
            is_preallocate_inhibited: Some(true),
            ..Default::default()
        })
    }

    async fn get_volume_capabilities(&mut self) -> Result<SupportedCapabilities> {
        Ok(SupportedCapabilities::default())
    }

    async fn get_volume_statistics(&mut self) -> Result<StatFsResult> {
        Ok(StatFsResult {
            block_size: 4096,
            io_size: 4096,
            total_blocks: 1024,
            available_blocks: 0,
            free_blocks: 0,
            used_blocks: 1024,
            total_bytes: 4_194_304,
            available_bytes: 0,
            free_bytes: 0,
            used_bytes: 4_194_304,
            total_files: 4,
            free_files: 0,
        })
    }

    async fn mount(&mut self, _options: TaskOptions) -> Result<()> {
        println!("[poc] mount called");
        Ok(())
    }

    async fn unmount(&mut self) -> Result<()> {
        println!("[poc] unmount called");
        Ok(())
    }

    async fn synchronize(&mut self, _flags: SyncFlags) -> Result<()> {
        Ok(())
    }

    async fn activate(&mut self, _options: TaskOptions) -> Result<Item> {
        println!("[poc] activate — returning root item");
        Ok(make_item("/", dir_attrs(ROOT_ID)))
    }

    async fn deactivate(&mut self) -> Result<()> {
        println!("[poc] deactivate");
        Ok(())
    }

    async fn set_volume_name(&mut self, _name: Vec<u8>) -> Result<Vec<u8>> {
        Err(erofs())
    }

    // --- Item attributes ---

    async fn get_attributes(&mut self, item_id: u64) -> Result<ItemAttributes> {
        match item_id {
            ROOT_ID => Ok(dir_attrs(ROOT_ID)),
            README_ID => Ok(file_attrs(README_ID, ROOT_ID, README_CONTENT.len() as u64)),
            SRC_DIR_ID => Ok(dir_attrs(SRC_DIR_ID)),
            MAIN_RS_ID => Ok(file_attrs(MAIN_RS_ID, SRC_DIR_ID, MAIN_RS_CONTENT.len() as u64)),
            _ => Err(enoent()),
        }
    }

    async fn set_attributes(&mut self, _item_id: u64, _attributes: ItemAttributes) -> Result<ItemAttributes> {
        Err(erofs())
    }

    // --- Directory operations ---

    async fn lookup_item(&mut self, name: &OsStr, directory_id: u64) -> Result<Item> {
        let name_str = name.to_str().unwrap_or("");
        println!("[poc] lookup: '{name_str}' in dir {directory_id}");

        match (directory_id, name_str) {
            (ROOT_ID, "README.md") => Ok(make_item(
                "README.md",
                file_attrs(README_ID, ROOT_ID, README_CONTENT.len() as u64),
            )),
            (ROOT_ID, "src") => Ok(make_item("src", dir_attrs(SRC_DIR_ID))),
            (SRC_DIR_ID, "main.rs") => Ok(make_item(
                "main.rs",
                file_attrs(MAIN_RS_ID, SRC_DIR_ID, MAIN_RS_CONTENT.len() as u64),
            )),
            _ => Err(enoent()),
        }
    }

    async fn enumerate_directory(&mut self, directory_id: u64, cookie: u64, _verifier: u64) -> Result<DirectoryEntries> {
        println!("[poc] enumerate dir {directory_id}, cookie {cookie}");

        match directory_id {
            ROOT_ID => {
                let entries = if cookie == 0 {
                    vec![
                        make_dir_entry(
                            make_item("README.md", file_attrs(README_ID, ROOT_ID, README_CONTENT.len() as u64)),
                            1,
                        ),
                        make_dir_entry(make_item("src", dir_attrs(SRC_DIR_ID)), 2),
                    ]
                } else {
                    vec![]
                };
                Ok(DirectoryEntries {
                    entries,
                    verifier: 0,
                })
            }
            SRC_DIR_ID => {
                let entries = if cookie == 0 {
                    vec![make_dir_entry(
                        make_item("main.rs", file_attrs(MAIN_RS_ID, SRC_DIR_ID, MAIN_RS_CONTENT.len() as u64)),
                        1,
                    )]
                } else {
                    vec![]
                };
                Ok(DirectoryEntries {
                    entries,
                    verifier: 0,
                })
            }
            _ => Err(enoent()),
        }
    }

    async fn reclaim_item(&mut self, _item_id: u64) -> Result<()> {
        Ok(())
    }

    async fn deactivate_item(&mut self, _item_id: u64) -> Result<()> {
        Ok(())
    }

    // --- File operations ---

    async fn create_item(&mut self, _name: &OsStr, _type: ItemType, _dir_id: u64, _attrs: ItemAttributes) -> Result<Item> {
        Err(erofs())
    }

    async fn remove_item(&mut self, _item_id: u64, _name: &OsStr, _dir_id: u64) -> Result<()> {
        Err(erofs())
    }

    async fn rename_item(
        &mut self, _item_id: u64, _to_dir_id: u64, _name: &OsStr, _to_name: &OsStr,
        _to_item_id: u64, _to_item_existing_id: Option<u64>,
    ) -> Result<Vec<u8>> {
        Err(erofs())
    }

    async fn open_item(&mut self, item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        println!("[poc] open item {item_id}");
        Ok(())
    }

    async fn close_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        Ok(())
    }

    async fn read(&mut self, item_id: u64, offset: i64, length: i64) -> Result<Vec<u8>> {
        println!("[poc] read item {item_id}, offset {offset}, length {length}");

        let content = match item_id {
            README_ID => README_CONTENT,
            MAIN_RS_ID => MAIN_RS_CONTENT,
            _ => return Err(eisdir()),
        };

        let start = (offset as usize).min(content.len());
        let end = (start + length as usize).min(content.len());
        Ok(content[start..end].to_vec())
    }

    async fn write(&mut self, _contents: Vec<u8>, _item_id: u64, _offset: i64) -> Result<i64> {
        Err(erofs())
    }

    async fn preallocate_space(&mut self, _item_id: u64, _offset: i64, _length: i64, _flags: Vec<PreallocateFlag>) -> Result<i64> {
        Err(erofs())
    }

    // --- Link operations ---

    async fn create_symbolic_link(
        &mut self, _name: &OsStr, _directory_id: u64, _attributes: ItemAttributes, _contents: Vec<u8>,
    ) -> Result<Item> {
        Err(erofs())
    }

    async fn create_link(&mut self, _item_id: u64, _name: &OsStr, _directory_id: u64) -> Result<Vec<u8>> {
        Err(erofs())
    }

    async fn read_symbolic_link(&mut self, _item_id: u64) -> Result<Vec<u8>> {
        Err(enoent())
    }

    // --- Access control ---

    async fn check_access(&mut self, _item_id: u64, _access: Vec<AccessMask>) -> Result<bool> {
        Ok(true)
    }

    // --- Extended attributes ---

    async fn get_supported_xattr_names(&mut self, _item_id: u64) -> Result<Xattrs> {
        Ok(Xattrs::default())
    }

    async fn get_xattr(&mut self, _name: &OsStr, _item_id: u64) -> Result<Vec<u8>> {
        Err(enosys())
    }

    async fn set_xattr(&mut self, _name: &OsStr, _value: Option<Vec<u8>>, _item_id: u64, _policy: SetXattrPolicy) -> Result<()> {
        Err(erofs())
    }

    async fn get_xattrs(&mut self, _item_id: u64) -> Result<Xattrs> {
        Ok(Xattrs::default())
    }

    async fn get_path_conf_operations(&mut self) -> Result<PathConfOperations> {
        Ok(PathConfOperations::default())
    }
}

#[tokio::main]
async fn main() -> session::Result<()> {
    println!("=== FSKit PoC ===");
    println!("Filesystem: 2 files (README.md, src/main.rs)");
    println!();

    let opts = MountOptions {
        fskit_id: "com.derekxwang.fskitbridge.fskitext".into(),
        mount_point: "/Volumes/ctxfs-poc".into(),
        ..MountOptions::default()
    };

    println!("Mounting at {} ...", opts.mount_point.display());
    println!("(FSKitBridge must be installed and extension enabled)");
    println!();

    let _session = fskit_rs::mount(PocFs, opts).await?;

    println!("Mounted! Try:");
    println!("  ls /Volumes/ctxfs-poc/");
    println!("  cat /Volumes/ctxfs-poc/README.md");
    println!("  cat /Volumes/ctxfs-poc/src/main.rs");
    println!();
    println!("Press Ctrl+C to unmount.");

    tokio::signal::ctrl_c().await.unwrap();
    println!("Unmounting...");
    Ok(())
}
