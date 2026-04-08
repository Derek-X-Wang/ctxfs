# Multi-Mount and Dependency Detection

Two CLI features: (1) mount multiple sources in a single command, (2) auto-detect project dependencies and mount them interactively or programmatically.

---

## 1. Multi-Mount

### Syntax

Existing single-mount form is unchanged (backwards compatible):

```
ctxfs mount <source> <mount_point>
```

New multi-mount form:

```
ctxfs mount <source1> [source2 ...] --mount-dir <dir>
```

- `--mount-dir` (or `-d`) specifies the base directory for all mounts.
- Mount points are derived automatically: `<mount-dir>/<package-name>/`.
  - Scoped npm packages: `@types/node` becomes `types-node`.
  - GitHub sources: `github:owner/repo@ref` uses `repo` as directory name.
- Daemon RPC calls (`mount`) are issued **concurrently**. Kernel mounts (`mount_nfs`) run sequentially (each needs sudo).
- Partial success is allowed: mounts that succeed stay up. Failures are reported in a summary. Exit code 0 if all succeed, 1 if any fail.

---

## 2. `ctxfs deps` Subcommand Group

### `ctxfs deps list <project-dir>`

Scans for manifest files and prints detected dependencies.

- Groups by ecosystem (JS, Python, Rust).
- Labels each dep as `[dep]` or `[dev]`.
- `--json` flag for machine-readable output.
- No mounting.

### `ctxfs deps mount <project-dir>`

Detects dependencies and mounts selected ones.

**Interactive mode** (default when TTY):
- `dialoguer::MultiSelect` checkbox picker.
- All deps shown, grouped by ecosystem, dev deps labeled `[dev]`.
- Dev deps shown but not pre-selected.

**Non-interactive flags**:
- `--all` — mount all production deps.
- `--all --include-dev` — mount everything including dev deps.
- `--select react,lodash,serde` — mount specific packages by name.
- `--select` with `--include-dev` — select from both production and dev pools.

**Mount directory**:
- `--mount-dir <dir>` specifies where to mount. Defaults to `./ctxfs-deps/` if omitted.
- Reuses the multi-mount flow from Section 1.

### `ctxfs deps unmount <project-dir>`

- Scans the mount dir (default `./ctxfs-deps/`) for active ctxfs mounts.
- Unmounts all in batch (kernel umount + daemon cleanup).
- Supports `--mount-dir <dir>` for custom directories.
- Summary output with per-mount status.

### `ctxfs unmount --all`

Separate addition to the existing `unmount` command. Unmounts every active mount tracked by the daemon (calls `list()` then unmounts each). Works regardless of how mounts were created.

---

## 3. Manifest Parsing

### Supported Manifests

| File | Ecosystem | Deps section | Dev deps section |
|------|-----------|-------------|-----------------|
| `package.json` | npm | `dependencies` | `devDependencies` |
| `Cargo.toml` | crate | `[dependencies]` | `[dev-dependencies]` |
| `requirements.txt` | pypi | all lines | (none, all treated as production) |
| `pyproject.toml` | pypi | `[project.dependencies]` | `[project.optional-dependencies]` |

Multiple manifest files can coexist in one project. All are detected and results merged.

### Data Model

```rust
enum Ecosystem { Npm, PyPI, Crate }

struct DetectedDep {
    name: String,
    version: String,       // resolved base version or "latest"
    ecosystem: Ecosystem,
    is_dev: bool,
    source_spec: String,   // e.g., "npm:react@19.1.0"
}
```

### Version Handling

- **package.json**: Strip range operators (`^`, `~`, `>=`, etc.), take base version. `*` or complex ranges become `latest`.
- **Cargo.toml**: Handle both string (`"1.0"`) and table (`{ version = "1.0" }`) forms. Skip `path = "..."` and `git = "..."` deps.
- **requirements.txt**: Parse `package==version`. Unpinned deps use `latest`.
- **pyproject.toml**: Parse PEP 508 specifiers, take pinned version or `latest`.

---

## 4. Error Handling and Edge Cases

**Mount failures in batch**: Each mount is independent. Summary printed at end:
```
Mounted 3/5 dependencies:
  ok  npm:react@19.1.0 -> ./deps/react
  ok  npm:lodash@4.17.21 -> ./deps/lodash
  ok  crate:serde@1.0.219 -> ./deps/serde
  ERR npm:some-private-pkg@1.0.0 -- resolution failed: no GitHub repository found
  ERR pypi:internal-lib@2.0 -- resolution failed: package not found
```

**No manifest found**: Print "No supported manifest files found in `<dir>`" and exit 1.

**Empty dependency list**: After filtering, print "No matching dependencies found" and exit 0.

**Packages without GitHub repos**: Surfaced as error in summary, dep is skipped.

**Duplicate names across ecosystems**: Mount dirs include ecosystem prefix only on collision: `./deps/requests/` if unique, `./deps/pypi-requests/` and `./deps/crate-requests/` if not.

**Already mounted**: If a mount point already has an active mount, skip and note in output.

---

## 5. Code Organization

### New Dependencies (`ctxfs-cli/Cargo.toml`)

- `dialoguer` — MultiSelect interactive picker.
- `toml` — parse Cargo.toml and pyproject.toml.

### File Layout

```
crates/ctxfs-cli/src/
  main.rs          -- extended with multi-mount, deps subcommands, unmount --all
  setup.rs         -- existing (unchanged)
  deps/
    mod.rs         -- DetectedDep, Ecosystem, detect_all() orchestrator
    npm.rs         -- package.json parser
    cargo.rs       -- Cargo.toml parser
    python.rs      -- requirements.txt + pyproject.toml parser
    mount.rs       -- batch mount/unmount logic (concurrent RPC + sequential kernel mount)
```

### What Does NOT Change

No modifications to daemon, IPC, or provider crates. The CLI constructs source specs from manifest data and uses the existing `mount` RPC. The daemon's resolution cache and registry resolvers handle resolution.

### Testing

- Unit tests for each manifest parser with known fixture data.
- Integration test for batch mount flow.
