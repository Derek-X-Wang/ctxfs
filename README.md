# ctxfs — Mount any GitHub repo as a local directory

Mount Git repositories as read-only local directories without cloning. Files are fetched lazily from the GitHub API on first access and cached locally with LRU eviction.

```sh
ctxfs daemon start &
ctxfs mount github:rust-lang/rust@master /mnt/rust
cat /mnt/rust/README.md     # fetched on demand, cached locally
grep -r "fn main" /mnt/rust/src/  # works with any Unix tool
ctxfs unmount /mnt/rust

# Mount any npm/PyPI/crate package source — resolves to GitHub automatically
ctxfs mount npm:lodash@4.17.21 /mnt/lodash
ctxfs mount pypi:requests@2.31.0 /mnt/requests
ctxfs mount crate:serde@1.0.0 /mnt/serde
```

**No macFUSE. No kernel extensions. No reboots.** Uses a local NFSv3 loopback server that macOS and Linux mount natively.

## Install

### From source (requires Rust toolchain)

```sh
git clone https://github.com/<owner>/ctxfs.git
cd ctxfs
cargo build --release
# Binary at target/release/ctxfs
```

### Requirements

- **macOS**: No extra dependencies. Uses the built-in `mount_nfs`.
- **Linux**: `nfs-common` package (`sudo apt install nfs-common` on Debian/Ubuntu).
- **GitHub token** (optional but recommended): Set `GITHUB_TOKEN` for 5000 req/hr instead of 60.

## Usage

```sh
# Start the background daemon
ctxfs daemon start &

# Mount a repo (will prompt for sudo password once for mount_nfs)
ctxfs mount github:owner/repo@branch /path/to/mountpoint

# Browse like a local directory
ls /path/to/mountpoint/
cat /path/to/mountpoint/README.md
find /path/to/mountpoint -name "*.rs"

# List active mounts
ctxfs list

# Unmount
ctxfs unmount /path/to/mountpoint

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
ctxfs mount --server-only github:owner/repo@main /mnt/repo
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
| `CTXFS_CACHE_DIR` | `~/.ctxfs/cache` | Cache directory |
| `CTXFS_CACHE_MAX_BYTES` | `536870912` (512MB) | Max cache size |
| `CTXFS_PID_FILE` | `~/.ctxfs/ctxfs.pid` | Daemon PID file |
| `CTXFS_LOG_LEVEL` | `info` | Log level (trace/debug/info/warn/error) |

## Why NFS instead of FUSE?

FUSE on macOS requires **macFUSE**, a kernel extension that needs approval in Recovery mode (shut down → hold Touch ID → Startup Security Utility → Reduced Security). That's a non-starter for a dev tool.

We run a tiny NFSv3 server on `127.0.0.1` and let the OS mount it with its built-in `mount_nfs`. No kernel extensions, no reboots, no third-party system installs. The tradeoff is a `sudo` prompt when mounting (kernel restriction on macOS), but that's just a password — not a reboot cycle.

## Development

```sh
# Run all tests (115 total — unit, integration, e2e)
cargo test

# Run without network tests
CTXFS_E2E_SKIP_NETWORK=1 cargo test

# Clippy
cargo clippy --all-targets
```

## License

MIT
