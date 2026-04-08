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

### Argument Disambiguation

The presence of `--mount-dir` (or `-d`) switches the parser into multi-mount mode:

- **Without `--mount-dir`**: exactly two positionals required — `<source>` and `<mount_point>`. This is the existing behavior.
- **With `--mount-dir`**: all positionals are treated as sources. `<mount_point>` is not accepted. Mount points are derived automatically.

In clap terms, `source` is `Vec<String>` (min 1) and `mount_point` is `Option<PathBuf>`. A validation step enforces:
- If `mount_point` is `None` and `mount_dir` is `None` → error: "provide either a mount point or --mount-dir".
- If `mount_point` is `Some` and `mount_dir` is `Some` → error: "cannot use both a mount point and --mount-dir".
- If `mount_point` is `Some` and sources has more than one entry → error: "use --mount-dir for multiple sources".

### Mount Directory Naming

Mount points are derived as `<mount-dir>/<slug>/` where the slug is:

- **npm packages**: `react` → `react`, `@types/node` → `types-node`
- **GitHub sources**: `github:owner/repo@ref` → `repo-ref` (e.g., `lodash-main`). If no ref, just `repo`.
- **crate/pypi packages**: package name as-is (e.g., `serde`, `requests`)

Collision handling: if two sources produce the same slug, append the ecosystem prefix (`npm-react`, `crate-react`). If still colliding (same ecosystem, different refs), the ref suffix already differentiates them.

### Concurrency

- Daemon RPC calls (`mount`) are issued **concurrently** via `tokio::join!` / `FuturesUnordered`.
- Kernel mounts (`mount_nfs`) run **sequentially** (each needs sudo).
- Partial success is allowed: mounts that succeed stay up. Failures are reported in a summary.
- Exit code 0 if all succeed, 1 if any fail.

---

## 2. `ctxfs deps` Subcommand Group

### `ctxfs deps list <project-dir>`

Scans for manifest files and prints detected dependencies.

- Groups by ecosystem (JS, Python, Rust).
- Labels each dep as `[dep]` or `[dev]`.
- `--json` flag for machine-readable output.
- `--include-dev` flag to include dev dependencies (excluded by default in non-interactive output).
- No mounting.

**JSON schema** (when `--json` is passed):

```json
{
  "manifests": ["package.json", "Cargo.toml"],
  "dependencies": [
    {
      "name": "react",
      "version": "19.1.0",
      "ecosystem": "npm",
      "is_dev": false,
      "source_spec": "npm:react@19.1.0"
    }
  ]
}
```

### `ctxfs deps mount <project-dir>`

Detects dependencies and mounts selected ones.

**Interactive mode** (default when TTY):
- `dialoguer::MultiSelect` checkbox picker.
- All deps shown, grouped by ecosystem, dev deps labeled `[dev]`.
- Dev deps shown but not pre-selected.

**Non-interactive mode** (non-TTY or when flags are provided):
- `--all` — mount all production deps.
- `--all --include-dev` — mount everything including dev deps.
- `--select react,lodash,serde` — mount specific packages by name.
- `--select` with `--include-dev` — select from both production and dev pools.
- **If non-TTY and no `--all`/`--select` flag**: error with "use --all or --select in non-interactive mode".

**`--select` name resolution**: Names are matched as bare names when unambiguous. If the same name exists in multiple ecosystems, the user must qualify with the ecosystem prefix (e.g., `npm:react`, `pypi:requests`, `crate:requests`). Ambiguous bare names produce an error listing the options.

**Mount directory**:
- `--mount-dir <dir>` specifies where to mount. Defaults to `./ctxfs-deps/` if omitted.
- Reuses the multi-mount flow from Section 1.

### `ctxfs deps unmount`

- No positional argument required.
- `--mount-dir <dir>` specifies which directory to scan. Defaults to `./ctxfs-deps/`.
- Queries the daemon via `list()` RPC to find active mounts whose mount points are under the target directory.
- Unmounts all matching mounts in batch (kernel umount + daemon cleanup).
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
| `requirements.txt` | pypi | all lines (treated as production) | (none) |
| `pyproject.toml` | pypi | `[project.dependencies]` | `[project.optional-dependencies]` (see note) |

**`pyproject.toml` optional-dependencies note**: Only extras named `dev`, `test`, or `testing` are classified as dev dependencies. All other optional-dependency groups are treated as production dependencies. This is a heuristic — not all projects follow this convention. The limitation is documented in `--help` output.

**`requirements.txt` note**: All entries are treated as production dependencies. There is no dev/prod distinction. `--include-dev` has no effect on requirements.txt entries.

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
- **Cargo.toml**: Handle both string (`"1.0"`) and table (`{ version = "1.0" }`) forms. Skip `path = "..."` and `git = "..."` deps (local/custom sources, not on crates.io).
- **requirements.txt**: Parse `package==version`. Unpinned deps use `latest`. Skip lines starting with `-` (flags) or `#` (comments).
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

**Non-TTY without flags**: Error with "use --all or --select in non-interactive mode" and exit 1.

**Packages without GitHub repos**: Surfaced as error in summary, dep is skipped.

**Duplicate names across ecosystems**: Mount dirs include ecosystem prefix only on collision: `./deps/requests/` if unique, `./deps/pypi-requests/` and `./deps/crate-requests/` if not.

**Already mounted**: Detected via daemon `list()` RPC by checking if any active mount's `mount_point` matches the derived path. If already mounted, skip and note "already mounted" in output summary.

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
