# ContextFS (ctxfs)

AI-native, read-only, mountable filesystem. Mount Git repos as local directories without cloning.

## Build & Test

```sh
cargo build --release
cargo clippy --all-targets --tests
cargo test
```

## Lints

Workspace-level lints in root `Cargo.toml` — all crates inherit with `[lints] workspace = true`.
- clippy::all = deny, clippy::pedantic = warn (with select overrides)
- rust: unsafe_code = warn, unused_results = warn, missing_debug_implementations = warn

## Testing

TDD workflow: write tests first, then implement. Run `cargo test` after every change.

- **Unit tests**: inline `#[cfg(test)]` modules in each source file
- **Integration tests**: `crates/*/tests/*.rs` — cross-crate and real-transport tests
  - `ctxfs-core/tests/cross_crate.rs`: SourceSpec/Digest/Config interop
  - `ctxfs-cache/tests/lifecycle.rs`: restart persistence, concurrent access, eviction under pressure
  - `ctxfs-ipc/tests/rpc_roundtrip.rs`: real tarpc client/server over UDS
  - `ctxfs-provider-git/tests/build_directories.rs`: snapshot construction → cache → resolution

## Architecture

7-crate workspace. Dependency graph:
- ctxfs-core: Digest, SourceSpec, Provider trait, Config, Error
- ctxfs-manifest: Snapshot, Directory, InodeTable (depends on core)
- ctxfs-cache: Content-addressable blob cache with LRU (depends on core, manifest)
- ctxfs-ipc: tarpc service trait + UDS transport (depends on core)
- ctxfs-provider-git: GitHub REST API provider (depends on core, manifest, cache)
- ctxfs-nfs: NFSv3 loopback server (depends on core, manifest, cache)
- ctxfs-daemon: Background service (depends on all above)
- ctxfs-cli: clap CLI binary (depends on core, ipc, daemon)

## Environment

- `GITHUB_TOKEN`: GitHub API token (5000 req/hr vs 60 unauthenticated)
- `CTXFS_SOCKET`: Override daemon socket path (default: ~/.ctxfs/ctxfs.sock)
- `CTXFS_CACHE_DIR`: Override cache directory (default: ~/.ctxfs/cache)
- `CTXFS_CACHE_MAX_BYTES`: Override max cache size (default: 512MB)
- `CTXFS_LOG_LEVEL`: Override log level (default: info)
