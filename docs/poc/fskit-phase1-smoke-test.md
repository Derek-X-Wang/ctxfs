# FSKit Phase 1 End-to-End Smoke Test

**Date**: 2026-04-14
**Hardware**: MacBook Pro (Apple Silicon)
**OS**: macOS 26.4 (Tahoe)
**ctxfs commit range**: `017db00..5ec5fb4` (Phase 1 wire-up + post-smoke-test fixes)

## Setup

1. FSKitBridge installed at `/Applications/FSKitBridge.app`, signed with personal Apple Developer team
2. Extension enabled in System Settings → Login Items & Extensions → File System Extensions → FSKitBridge ON
3. `/Volumes/ctxfs/` auto-created by `ctxfs mount --backend fskit` on first use (one sudo prompt)
4. Bundle ID: `com.derekxwang.fskitbridge.fskitext` (self-signed team prefix)

Note: `pluginkit -m -p com.apple.fskit.fsmodule` prints the bundle ID followed by `(0.1)` (the version). That needs to be stripped when setting `CTXFS_FSKIT_BUNDLE_ID`:

```sh
export CTXFS_FSKIT_BUNDLE_ID=$(pluginkit -m -p com.apple.fskit.fsmodule | grep -i fskitbridge | awk '{print $1}' | sed 's/([^)]*)//')
```

## Test Run

### Mount

```sh
./target/release/ctxfs mount github:octocat/Hello-World@master -p ./test-mnt --backend fskit
```

Output:
```
Mounted FSKit volume at /Volumes/ctxfs/github-octocat-hello-world-master
Linked from: /Users/derekxwang/Development/incubator/ContextFS/ctxfs/test-mnt
  Source:   github:octocat/Hello-World@master
  Commit:   7fd1a60b01f91b314f59955a4e4d4e80d8edf11d
  ID:       github_octocat_Hello-World_master
```

### Kernel mount table

```
/dev/disk8 on /Volumes/ctxfs/github-octocat-hello-world-master (fskitbridge, local, nodev, nosuid, noowners, noatime, fskit, mounted by derekxwang)
```

Key attributes:
- **`fskitbridge`** as filesystem type (not `nfs`), with `fskit` listed as a mount option
- **`mounted by derekxwang`** (not root — no sudo per mount)
- No Full Disk Access requirement

### Symlink

```
./test-mnt → /Volumes/ctxfs/github-octocat-hello-world-master
```

### Reads

Both paths work:

```sh
cat ./test-mnt/README                                           # → Hello World!
cat /Volumes/ctxfs/github-octocat-hello-world-master/README     # → Hello World!
```

### Latency

Repeated `cat` on the 13-byte README:

| Try | Time |
|---|---|
| Cold | 0.003s |
| Warm 1 | 0.002s |
| Warm 2 | 0.002s |
| Warm 3 | 0.002s |
| Warm 4 | 0.002s |
| Warm 5 | 0.002s |

**2-3ms per read**, consistent with Phase 0 PoC. No difference between cold and warm visible at this granularity.

### Unmount

```sh
./target/release/ctxfs unmount ./test-mnt
```

Output: `Unmounted ./test-mnt`

Post-unmount:
- Symlink removed (✅ `ls ./test-mnt` → No such file or directory)
- Volume unmounted (✅ `mount | grep ctxfs/` → empty)
- Daemon tracking cleared (✅ `ctxfs list` → No active mounts)

## Issues Found and Fixed During Smoke Test

### 1. Pre-existing `-p` path broke symlink creation

First attempt:
```
warning: mounted at /Volumes/ctxfs/... but failed to create symlink ./test-mnt: File exists (os error 17)
```

**Root cause**: `handle_mount` used `std::fs::create_dir_all(&mp)` unconditionally, which works for NFS (mounts AT the path) but breaks FSKit (which needs to create a symlink AT the path).

**Fix** (commit `ea601d5`): For FSKit, only create the parent directory. If `-p` already exists, call `handle_existing_fskit_mount_point` which intelligently removes stale ctxfs symlinks and empty directories, or errors with a clear message for non-empty dirs.

### 2. Unmount race: kernel `umount` called for FSKit mounts

First attempt output:
```
umount: ./test-mnt: not currently mounted
warning: kernel umount failed: umount exited with exit status: 1
```

**Root cause**: `handle_unmount` called `run_umount()` unconditionally, but FSKit mounts are torn down by the daemon dropping the `fskit-rs::Session`. The kernel `umount` call raced with fskit-rs's own unmount.

**Fix** (commit `ea601d5`): Skip `run_umount` for targets under `/Volumes/ctxfs/`.

### 3. Unmount RPC timeout + dangling symlink

After fixes 1 & 2:
```
Error: the request exceeded its deadline
```

**Root cause**: `hdiutil detach disk8` can block >10s when multiple FSKit volumes share `/dev/disk8` (FSKitBridge reuses the sentinel device). The daemon eventually succeeded, but the CLI's 10s deadline fired first, skipping symlink cleanup.

**Fix** (commit `5ec5fb4`):
- Use `long_context()` (60s) for unmount RPC
- Always attempt symlink cleanup after the RPC, regardless of outcome
- On RPC timeout, check `mount` to see if the volume is actually gone — if so, report success

## Stale PoC mount

The original Phase 0 PoC mount at `/Volumes/ctxfs-poc` remained across all testing because the PoC process was SIGKILLed without a clean unmount. This is a **Phase 0 PoC testing artifact**, not a Phase 1 issue. Cleanup requires `sudo diskutil unmount force /Volumes/ctxfs-poc` or a reboot.

## Auto-creation of `/Volumes/ctxfs/`

First-time FSKit mount on a fresh machine:

```
/Volumes/ctxfs/ does not exist yet — creating it (requires sudo)...
Password: ****
Created /Volumes/ctxfs/ (owned by derekxwang:staff)
```

Single sudo prompt, then the directory is writable by the user for all subsequent mounts.

## Verdict

**Phase 1 is complete.** All acceptance criteria from the design spec are met:

- ✅ `ctxfs mount <source> -p ./path --backend fskit` works on macOS 26+
- ✅ No sudo per mount (only one-time for `/Volumes/ctxfs/` creation)
- ✅ **No Full Disk Access required** — the primary motivation for FSKit
- ✅ 2-3ms read latency (imperceptible)
- ✅ Symlink lifecycle handled (create on mount, resolve + remove on unmount)
- ✅ Unmount cleanly tears down the volume and removes the symlink
- ✅ Daemon state accurate across mount/unmount cycle

Remaining Phase 1.5 / Phase 2 work is polish (Finder icons, volume display names, batch-mount FSKit support, auth token) — nothing blocks usage today.

## Gotchas for Users

1. **Bundle ID must be stripped of version suffix** — `pluginkit` returns `com.<TEAM>.fskitbridge.fskitext(<VERSION>)`. The `(version)` must be removed. See the setup command above.

2. **Daemon must be started with `CTXFS_FSKIT_BUNDLE_ID` in its environment**, not just the shell that invokes `ctxfs mount`. Export it before `ctxfs daemon start`.

3. **The `--backend fskit` flag must be on the same command line** — shell continuation artifacts (like zsh wrapping or missing `\`) silently cause the flag to be dropped, leaving you on the NFS fallback. Look for `(backend=fskit)` in the daemon log to confirm.

4. **Multiple FSKit mounts share `/dev/disk8`**, which is a fskit-rs/FSKitBridge limitation. Unmounting one can take a few seconds when others are still live.

5. **Existing `-p` path**: non-empty directories block the mount with a clear error message; ctxfs symlinks and empty directories are auto-removed.
