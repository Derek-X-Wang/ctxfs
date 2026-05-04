# ctxfs — Mount any GitHub repo as a local directory

Mount Git repositories as read-only local directories without cloning. Files are fetched lazily from the GitHub API on first access and cached locally with LRU eviction.

```sh
ctxfs daemon start &
ctxfs mount github:rust-lang/rust@master -p ./mnt/rust
cat ./mnt/rust/README.md     # fetched on demand, cached locally
grep -r "fn main" ./mnt/rust/src/  # works with any Unix tool
ctxfs unmount ./mnt/rust

# Mount any npm/PyPI/crate package source — resolves to GitHub automatically
ctxfs mount npm:lodash@4.17.21 -p ./mnt/lodash
ctxfs mount pypi:requests@2.31.0 -p ./mnt/requests
ctxfs mount crate:serde@1.0.0 -p ./mnt/serde
```

**No macFUSE. No kernel extensions. No reboots.** macOS 26+ uses FSKit (no sudo, no Full Disk Access); older macOS and Linux fall back to a local NFSv3 loopback server that the OS mounts natively.

## Install

### Mac app (recommended)

Bundles the CLI and a menu-bar companion. On macOS 26 (Tahoe) and later, the bundled FSKit system extension is used and **no sudo / no Full Disk Access is required**. Sparkle handles in-app auto-updates.

```sh
brew install --cask Derek-X-Wang/ctxfs/contextfs
```

Or download the latest DMG from the [Releases page](https://github.com/Derek-X-Wang/ctxfs/releases).

### CLI only (headless / CI)

```sh
brew install Derek-X-Wang/ctxfs/contextfs
```

The cask and formula are mutually exclusive — the cask already ships the CLI bundled inside the app, on `$PATH`.

### From source (requires Rust toolchain)

```sh
git clone https://github.com/Derek-X-Wang/ctxfs.git
cd ctxfs
cargo build --release
# Binary at target/release/ctxfs
```

### Requirements

- **macOS 26+**: FSKit backend, no extra deps, no sudo, no Full Disk Access.
- **macOS 15.4–25**: Falls back to the NFS loopback backend. Uses the built-in `mount_nfs`. One-time sudo setup needed (see below).
- **Linux**: `nfs-common` package (`sudo apt install nfs-common` on Debian/Ubuntu). One-time sudo setup needed.
- **GitHub token** (optional but recommended): Set `GITHUB_TOKEN` for 5000 req/hr instead of 60.

### First-time setup

**macOS 26+ (FSKit backend — recommended)**: no sudo, no FDA. The cask installs the system extension automatically. If you built from source, run:

```sh
ctxfs setup install-fskit
```

**macOS 15.4–25 / Linux (NFS backend)**:

```sh
# Configure passwordless sudo for mount/umount (prompts for password once)
ctxfs setup install
```

On macOS in NFS mode, your terminal app also needs **Full Disk Access** to read NFS-mounted files. Without it, mounts succeed but reads fail with "Operation not permitted" — macOS treats NFS volumes as network volumes that require explicit permission ([macfuse#690](https://github.com/macfuse/macfuse/issues/690)).

```sh
# Opens the Full Disk Access settings pane directly
open "x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles"
```

Add your terminal app (Terminal, iTerm2, cmux, Ghostty, etc.), then **restart the terminal**. This is the same requirement that affects macFUSE, s3fs-fuse, and HuggingFace's [hf-mount](https://github.com/huggingface/hf-mount). FSKit on macOS 26+ avoids this entirely.

## Usage

```sh
# Start the background daemon
ctxfs daemon start &

# Mount a repo
ctxfs mount github:owner/repo@branch -p /path/to/mountpoint

# Browse like a local directory
ls /path/to/mountpoint/
cat /path/to/mountpoint/README.md
find /path/to/mountpoint -name "*.rs"

# Mount multiple sources at once (auto-derived mount points)
ctxfs mount npm:react@19.1.0 crate:serde@1.0.219 -d ./deps

# List active mounts
ctxfs list

# Unmount
ctxfs unmount /path/to/mountpoint
ctxfs unmount --all    # unmount everything

# Stop daemon
ctxfs daemon stop
```

### Source spec format

```
github:<owner>/<repo>@<ref>
github:<owner>/<repo>@<ref>:<subpath>
npm:<package>@<version>
npm:@<scope>/<package>@<version>
pypi:<package>@<version>
crate:<package>@<version>
```

`<ref>` can be a branch name, tag, or commit SHA. `@latest` is also supported for registry packages — resolves to the current version at mount time.

### Server-only mode (no sudo)

Start the NFS server without the kernel mount — useful for debugging or custom mount options:

```sh
ctxfs mount --server-only github:owner/repo@main -p /mnt/repo
# Prints the NFS port; mount manually:
sudo mount_nfs -o nolocks,vers=3,tcp,port=PORT,mountport=PORT 127.0.0.1:/ /mnt/repo
```

## How it works

```
┌──────────┐     UDS/tarpc      ┌──────────────┐
│ ctxfs CLI ├───────────────────►│ ctxfs daemon │
└──────────┘                    └──────┬───────┘
                                       │
                          ┌────────────┼────────────┐
                          ▼            ▼            ▼
                    ┌──────────┐ ┌──────────┐ ┌──────────┐
                    │ GitHub   │ │ LRU      │ │ NFSv3    │
                    │ REST API │ │ Cache    │ │ Server   │
                    └──────────┘ └──────────┘ └────┬─────┘
                                                   │ loopback
                                              ┌────▼─────┐
                                              │ mount_nfs │ (kernel)
                                              └────┬─────┘
                                              ┌────▼─────┐
                                              │ /mnt/repo │ (your files)
                                              └──────────┘
```

1. **`ctxfs mount`** tells the daemon to fetch the repo's tree from the GitHub API
2. The daemon builds a directory manifest (snapshot) and caches it
3. An in-process **NFSv3 server** starts on a random loopback port
4. The CLI invokes **`mount_nfs`** to connect the kernel to the loopback server
5. When you `ls` or `cat`, the kernel sends NFS requests to the daemon
6. The daemon fetches file content lazily from GitHub and caches it on disk

Files are cached in a content-addressable store with LRU eviction (default 512MB). Subsequent reads of the same file are served from disk.

On **macOS 26+**, the bottom three boxes (NFSv3 server / `mount_nfs` / kernel) are replaced by a single FSKit system extension that talks directly to the daemon over IPC — no NFS protocol, no kernel mount, no sudo, no Full Disk Access. The CLI/daemon/cache layers are identical.

## Package Registry Support

Mount source code from npm, PyPI, or crates.io — ctxfs resolves the package to its GitHub source repository and mounts it lazily:

1. `ctxfs mount npm:react@19.1.0 /mnt/react` hits the npm registry
2. Reads the `repository` field → `github.com/facebook/react`
3. Uses the existing GitHub provider to mount lazily (no clone)

If a package doesn't link to a GitHub repo, ctxfs tells you:
```
Error: no source repository found for npm:some-pkg@1.0.0
  Try: ctxfs mount github:owner/repo@ref /mnt/pkg
```

## Configuration

| Environment variable | Default | Description |
|---------------------|---------|-------------|
| `GITHUB_TOKEN` | none | GitHub API token (5000 req/hr vs 60) |
| `CTXFS_SOCKET` | `~/.ctxfs/ctxfs.sock` | Daemon IPC socket path |
| `CTXFS_CACHE_DIR` | `~/.ctxfs/cache` | Blob cache directory |
| `CTXFS_CACHE_MAX_BYTES` | `536870912` (512MB) | Max blob cache size |
| `CTXFS_TREE_CACHE_MAX_BYTES` | `524288000` (500MB) | Max local tree-manifest cache size |
| `CTXFS_LATEST_TTL_SECS` | `3600` | TTL for `@latest` resolution cache |
| `CTXFS_PID_FILE` | `~/.ctxfs/ctxfs.pid` | Daemon PID file |
| `CTXFS_LOG_LEVEL` | `info` | Log level (trace/debug/info/warn/error) |
| `CTXFS_BACKEND` | auto | Force backend selection (`fskit` or `nfs`) |
| `CTXFS_REDIS_URL` | none | Optional Redis URL for shared tree caching across machines |

## Why FSKit / NFS instead of FUSE?

FUSE on macOS requires **macFUSE**, a kernel extension that needs approval in Recovery mode (shut down → hold Touch ID → Startup Security Utility → Reduced Security). That's a non-starter for a dev tool.

ContextFS uses two backends, picked automatically:

- **FSKit** (macOS 26+, preferred): Apple's user-space filesystem framework. The cask installs the bundled system extension. No sudo, no Full Disk Access, no kernel extensions, no reboots.
- **NFSv3 loopback** (macOS 15.4–25, Linux): a tiny in-process NFSv3 server on `127.0.0.1` that the OS mounts via the built-in `mount_nfs`. No kernel extensions; one `sudo` cost is a passwordless sudoers entry installed by `ctxfs setup install`.

## Development

```sh
# Run all tests (unit, integration, e2e)
cargo test

# Run without network tests
CTXFS_E2E_SKIP_NETWORK=1 cargo test

# Clippy (workspace-level lints, clippy::all = deny)
cargo clippy --all-targets --tests
```

## License

MIT
