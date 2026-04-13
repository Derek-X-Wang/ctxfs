# FSKit Phase 0 Proof of Concept

This is the **Phase 0** proof of concept referenced in `docs/superpowers/specs/2026-04-11-fskit-backend-design.md`.

It proves that FSKit can mount a **synthetic (non-block-device) filesystem** on macOS 26 using the FSKitBridge + fskit-rs bridge. This was the critical gate before committing to the FSKit backend implementation.

## Result: ✅ GO (2026-04-13)

All operations work:

| Test | Result |
|---|---|
| Mount synthetic filesystem at `/Volumes/ctxfs-poc` | ✅ |
| `ls`, `cat`, `find`, `grep`, `stat` | ✅ all work |
| Read latency (small file) | **2ms** per read (imperceptible) |
| No sudo per mount | ✅ confirmed |
| **No Full Disk Access required** | ✅ confirmed (the huge win) |
| Nested directory traversal | ✅ |

Mount report from kernel:
```
/dev/disk8 on /Volumes/ctxfs-poc (fskitbridge, local, nodev, nosuid, noowners, noatime, fskit, mounted by derekxwang)
```

Note: kernel reports filesystem type as `fskit`, `mounted by derekxwang` (not root).

## What this PoC does

Serves a hardcoded 3-entry filesystem over FSKit:
```
/README.md    (file, 45 bytes)
/src/         (directory)
/src/main.rs  (file, 33 bytes)
```

It implements the full `fskit_rs::Filesystem` trait — read-only stubs return `EROFS` for writes, `ENOENT` for unknown items.

## Prerequisites

1. **macOS 26+** (Tahoe). Tested on 26.4.
2. **Apple Developer account** (paid) for the `com.apple.developer.fskit.fsmodule` restricted entitlement.
3. **Xcode 16.3+** with Swift 5.10+.
4. **Homebrew packages**: `protobuf` and `swift-protobuf` (for `protoc-gen-swift`).

## Setup steps (for future reference)

### 1. Build and install FSKitBridge

```sh
git clone https://github.com/debox-network/FSKitBridge.git
cd FSKitBridge
```

Open `FSKitBridge.xcodeproj` in Xcode:

- Set signing team on both targets (FSKitBridge + FSKitExt)
- Bundle IDs will change to your team prefix (e.g., `com.YOURID.fskitbridge.fskitext`)
- **Important**: Update swift-protobuf to 1.36.1+ (File → Packages → Update to Latest Package Versions) — older pins break with newer `protoc-gen-swift`
- Build

Copy to `/Applications/`:
```sh
BUILD_DIR=$(ls -d ~/Library/Developer/Xcode/DerivedData/FSKitBridge-*/Build/Products/Debug/ | head -1)
cp -R "$BUILD_DIR/FSKitBridge.app" /Applications/
xattr -dr com.apple.quarantine /Applications/FSKitBridge.app
open /Applications/FSKitBridge.app   # registers the extension with PlugInKit
```

Verify registration:
```sh
pluginkit -m -p com.apple.fskit.fsmodule | grep fskitbridge
```

### 2. Enable the extension

System Settings → General → Login Items & Extensions → File System Extensions → toggle ON **FSKitBridge**.

### 3. Run the PoC

```sh
sudo mkdir -p /Volumes/ctxfs-poc
sudo chown $(whoami):staff /Volumes/ctxfs-poc

cd docs/poc/fskit-poc
cargo run
```

**Important**: Update the `fskit_id` in `src/main.rs` to match your actual bundle ID (from the pluginkit output). The fskit-rs default is `network.debox.fskitbridge.fskitext` which won't match if you signed with your own team.

### 4. Test

In another terminal:
```sh
ls /Volumes/ctxfs-poc/
cat /Volumes/ctxfs-poc/README.md
cat /Volumes/ctxfs-poc/src/main.rs
grep -r fskit /Volumes/ctxfs-poc/
```

Ctrl+C in the PoC terminal to unmount.

## Gotchas encountered

### 1. swift-protobuf version mismatch
FSKitBridge pins swift-protobuf 1.30.0, but Homebrew's `protoc-gen-swift` is 1.36.1. The generated code uses `SwiftProtobuf._NameMap(bytecode:)` which is a 1.36+ API. Fix: update the package dependency to 1.36.1+ in Xcode.

### 2. Bundle ID mismatch
The `fskit-rs` default `MountOptions::default()` uses bundle ID `network.debox.fskitbridge.fskitext`. When you sign with your own team, the bundle ID becomes `com.YOURTEAMID.fskitbridge.fskitext`. You must pass your actual bundle ID via `MountOptions::fskit_id`.

### 3. Mount point must exist and be writable
FSKit requires the `/Volumes/<name>` directory to exist before mounting. The fskit-rs mounter doesn't create it automatically. You need `sudo mkdir` once per mount point (or have `/Volumes/ctxfs/` pre-created by `ctxfs setup install-fskit`).

### 4. Stuck mounts on crash
If the Rust process is killed (SIGKILL, panic) without calling `unmount`, the kernel may keep the mount active but connection-less. `umount` may fail with "Invalid argument". Fix: `diskutil unmount force /Volumes/ctxfs-poc` or reboot. The daemon should always catch SIGTERM/SIGINT and clean up.

## Why this matters for ctxfs

This PoC proves the architectural decision in the FSKit design spec is sound:

- **ctxfs-fskit** can be implemented against fskit-rs exactly as described
- **No FDA requirement** eliminates the worst pain point of the NFS backend
- **No per-mount sudo** is a major UX improvement
- **2ms latency** is imperceptible; any optimization work (direct C FFI) would be premature

Phase 1 implementation can proceed with confidence.
