# Multi-Mount and Dependency Detection Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add multi-mount support and a `ctxfs deps` subcommand that detects project dependencies from manifest files, offers interactive selection, and batch-mounts them.

**Architecture:** All new code lives in `ctxfs-cli`. Manifest parsers extract dependency metadata into `DetectedDep` structs. Batch mount/unmount logic issues concurrent daemon RPCs then sequential kernel mounts. The daemon, IPC, and provider crates are unchanged.

**Tech Stack:** clap v4 derive, dialoguer (MultiSelect), toml (TOML parsing), serde_json (package.json), existing tarpc IPC client.

---

### Task 1: Add workspace dependencies (dialoguer, toml)

**Files:**
- Modify: `Cargo.toml:38-101` (workspace dependencies)
- Modify: `crates/ctxfs-cli/Cargo.toml:11-24` (crate dependencies)

- [ ] **Step 1: Add dialoguer and toml to workspace Cargo.toml**

In `Cargo.toml`, add after the `whoami = "1"` line (line 100):

```toml
dialoguer = "0.11"
toml = "0.8"
```

- [ ] **Step 2: Add dialoguer, toml, and serde to ctxfs-cli Cargo.toml**

In `crates/ctxfs-cli/Cargo.toml`, add after the `whoami = { workspace = true }` line (line 24):

```toml
dialoguer = { workspace = true }
toml = { workspace = true }
serde = { workspace = true }
futures = { workspace = true }
```

- [ ] **Step 3: Verify it compiles**

Run: `cargo check -p ctxfs`
Expected: compiles with no errors (unused dep warnings are OK at this stage)

- [ ] **Step 4: Commit**

```bash
git add Cargo.toml crates/ctxfs-cli/Cargo.toml
git commit -m "chore: add dialoguer and toml workspace dependencies for deps feature"
```

---

### Task 2: DetectedDep data model and Ecosystem enum

**Files:**
- Create: `crates/ctxfs-cli/src/deps/mod.rs`

- [ ] **Step 1: Write the failing test**

Create `crates/ctxfs-cli/src/deps/mod.rs`:

```rust
mod npm;
mod cargo_deps;
mod python;
pub mod mount;

use serde::Serialize;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Ecosystem {
    Npm,
    PyPI,
    Crate,
}

impl fmt::Display for Ecosystem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Ecosystem::Npm => write!(f, "npm"),
            Ecosystem::PyPI => write!(f, "pypi"),
            Ecosystem::Crate => write!(f, "crate"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DetectedDep {
    pub name: String,
    pub version: String,
    pub ecosystem: Ecosystem,
    pub is_dev: bool,
    pub source_spec: String,
}

impl DetectedDep {
    pub fn new(name: String, version: String, ecosystem: Ecosystem, is_dev: bool) -> Self {
        let source_spec = format!("{ecosystem}:{name}@{version}");
        Self {
            name,
            version,
            ecosystem,
            is_dev,
            source_spec,
        }
    }

    /// Display label for the interactive picker.
    pub fn picker_label(&self) -> String {
        let dev_tag = if self.is_dev { " [dev]" } else { "" };
        format!("{} {} @{}{}", self.ecosystem, self.name, self.version, dev_tag)
    }
}

/// Detect all dependencies from manifest files in the given directory.
pub fn detect_all(project_dir: &Path) -> Vec<DetectedDep> {
    let mut deps = Vec::new();

    let pkg_json = project_dir.join("package.json");
    if pkg_json.is_file() {
        if let Ok(mut d) = npm::parse_package_json(&pkg_json) {
            deps.append(&mut d);
        }
    }

    let cargo_toml = project_dir.join("Cargo.toml");
    if cargo_toml.is_file() {
        if let Ok(mut d) = cargo_deps::parse_cargo_toml(&cargo_toml) {
            deps.append(&mut d);
        }
    }

    let requirements_txt = project_dir.join("requirements.txt");
    if requirements_txt.is_file() {
        if let Ok(mut d) = python::parse_requirements_txt(&requirements_txt) {
            deps.append(&mut d);
        }
    }

    let pyproject_toml = project_dir.join("pyproject.toml");
    if pyproject_toml.is_file() {
        if let Ok(mut d) = python::parse_pyproject_toml(&pyproject_toml) {
            deps.append(&mut d);
        }
    }

    deps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detected_dep_source_spec() {
        let dep = DetectedDep::new("react".into(), "19.1.0".into(), Ecosystem::Npm, false);
        assert_eq!(dep.source_spec, "npm:react@19.1.0");
    }

    #[test]
    fn detected_dep_picker_label_prod() {
        let dep = DetectedDep::new("serde".into(), "1.0.0".into(), Ecosystem::Crate, false);
        assert_eq!(dep.picker_label(), "crate serde @1.0.0");
    }

    #[test]
    fn detected_dep_picker_label_dev() {
        let dep = DetectedDep::new("jest".into(), "29.0.0".into(), Ecosystem::Npm, true);
        assert!(dep.picker_label().contains("[dev]"));
    }

    #[test]
    fn ecosystem_display() {
        assert_eq!(Ecosystem::Npm.to_string(), "npm");
        assert_eq!(Ecosystem::PyPI.to_string(), "pypi");
        assert_eq!(Ecosystem::Crate.to_string(), "crate");
    }

    #[test]
    fn detect_all_empty_dir() {
        let dir = tempfile::tempdir().unwrap();
        let deps = detect_all(dir.path());
        assert!(deps.is_empty());
    }
}
```

- [ ] **Step 2: Register the module in main.rs**

At the top of `crates/ctxfs-cli/src/main.rs`, add after `mod setup;`:

```rust
mod deps;
```

- [ ] **Step 3: Run test to verify it compiles and passes**

Run: `cargo test -p ctxfs -- deps::tests`
Expected: tests fail because `npm`, `cargo_deps`, `python`, and `mount` modules don't exist yet.

- [ ] **Step 4: Create empty stub modules**

Create `crates/ctxfs-cli/src/deps/npm.rs`:

```rust
use super::{DetectedDep, Ecosystem};
use anyhow::Result;
use std::path::Path;

pub fn parse_package_json(_path: &Path) -> Result<Vec<DetectedDep>> {
    Ok(Vec::new())
}
```

Create `crates/ctxfs-cli/src/deps/cargo_deps.rs`:

```rust
use super::{DetectedDep, Ecosystem};
use anyhow::Result;
use std::path::Path;

pub fn parse_cargo_toml(_path: &Path) -> Result<Vec<DetectedDep>> {
    Ok(Vec::new())
}
```

Create `crates/ctxfs-cli/src/deps/python.rs`:

```rust
use super::{DetectedDep, Ecosystem};
use anyhow::Result;
use std::path::Path;

pub fn parse_requirements_txt(_path: &Path) -> Result<Vec<DetectedDep>> {
    Ok(Vec::new())
}

pub fn parse_pyproject_toml(_path: &Path) -> Result<Vec<DetectedDep>> {
    Ok(Vec::new())
}
```

Create `crates/ctxfs-cli/src/deps/mount.rs`:

```rust
// Batch mount/unmount logic — implemented in Task 8.
```

- [ ] **Step 5: Run tests to verify they pass**

Run: `cargo test -p ctxfs -- deps::tests`
Expected: 5 tests pass

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-cli/src/deps/ crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): add DetectedDep model and deps module skeleton"
```

---

### Task 3: npm manifest parser (package.json)

**Files:**
- Modify: `crates/ctxfs-cli/src/deps/npm.rs`

- [ ] **Step 1: Write the failing tests**

Replace the contents of `crates/ctxfs-cli/src/deps/npm.rs`:

```rust
use super::{DetectedDep, Ecosystem};
use anyhow::{Context, Result};
use std::path::Path;

/// Strip semver range operators and take the base version.
/// "^19.1.0" -> "19.1.0", "~4.17.0" -> "4.17.0", ">=2.0.0" -> "2.0.0"
/// "*" or complex ranges -> "latest"
fn strip_version_range(v: &str) -> String {
    let trimmed = v.trim();
    // Strip leading operator chars
    let stripped = trimmed.trim_start_matches(|c: char| matches!(c, '^' | '~' | '>' | '=' | '<'));
    let stripped = stripped.trim();

    if stripped.is_empty() || stripped == "*" || stripped.contains("||") || stripped.contains(' ') {
        return "latest".to_string();
    }

    stripped.to_string()
}

pub fn parse_package_json(path: &Path) -> Result<Vec<DetectedDep>> {
    let content = std::fs::read_to_string(path).context("failed to read package.json")?;
    let json: serde_json::Value =
        serde_json::from_str(&content).context("failed to parse package.json")?;

    let mut deps = Vec::new();

    if let Some(obj) = json.get("dependencies").and_then(|v| v.as_object()) {
        for (name, ver) in obj {
            let version = ver.as_str().map_or("latest".to_string(), |v| strip_version_range(v));
            deps.push(DetectedDep::new(
                name.clone(),
                version,
                Ecosystem::Npm,
                false,
            ));
        }
    }

    if let Some(obj) = json.get("devDependencies").and_then(|v| v.as_object()) {
        for (name, ver) in obj {
            let version = ver.as_str().map_or("latest".to_string(), |v| strip_version_range(v));
            deps.push(DetectedDep::new(
                name.clone(),
                version,
                Ecosystem::Npm,
                true,
            ));
        }
    }

    Ok(deps)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strip_caret() {
        assert_eq!(strip_version_range("^19.1.0"), "19.1.0");
    }

    #[test]
    fn strip_tilde() {
        assert_eq!(strip_version_range("~4.17.0"), "4.17.0");
    }

    #[test]
    fn strip_gte() {
        assert_eq!(strip_version_range(">=2.0.0"), "2.0.0");
    }

    #[test]
    fn star_becomes_latest() {
        assert_eq!(strip_version_range("*"), "latest");
    }

    #[test]
    fn complex_range_becomes_latest() {
        assert_eq!(strip_version_range(">=1.0.0 <2.0.0"), "latest");
        assert_eq!(strip_version_range("^1.0.0 || ^2.0.0"), "latest");
    }

    #[test]
    fn exact_version_unchanged() {
        assert_eq!(strip_version_range("19.1.0"), "19.1.0");
    }

    #[test]
    fn parse_basic_package_json() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(
            &pkg,
            r#"{
                "dependencies": {
                    "react": "^19.1.0",
                    "lodash": "4.17.21"
                },
                "devDependencies": {
                    "jest": "~29.0.0"
                }
            }"#,
        )
        .unwrap();

        let deps = parse_package_json(&pkg).unwrap();
        assert_eq!(deps.len(), 3);

        let react = deps.iter().find(|d| d.name == "react").unwrap();
        assert_eq!(react.version, "19.1.0");
        assert!(!react.is_dev);
        assert_eq!(react.source_spec, "npm:react@19.1.0");

        let jest = deps.iter().find(|d| d.name == "jest").unwrap();
        assert!(jest.is_dev);
    }

    #[test]
    fn parse_scoped_packages() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(
            &pkg,
            r#"{
                "dependencies": {
                    "@babel/core": "^7.24.0",
                    "@types/node": "^20.0.0"
                }
            }"#,
        )
        .unwrap();

        let deps = parse_package_json(&pkg).unwrap();
        assert_eq!(deps.len(), 2);

        let babel = deps.iter().find(|d| d.name == "@babel/core").unwrap();
        assert_eq!(babel.source_spec, "npm:@babel/core@7.24.0");
    }

    #[test]
    fn parse_empty_deps() {
        let dir = tempfile::tempdir().unwrap();
        let pkg = dir.path().join("package.json");
        std::fs::write(&pkg, r#"{"name": "my-app"}"#).unwrap();

        let deps = parse_package_json(&pkg).unwrap();
        assert!(deps.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p ctxfs -- deps::npm::tests`
Expected: 9 tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/ctxfs-cli/src/deps/npm.rs
git commit -m "feat(cli): implement package.json dependency parser"
```

---

### Task 4: Cargo.toml manifest parser

**Files:**
- Modify: `crates/ctxfs-cli/src/deps/cargo_deps.rs`

- [ ] **Step 1: Write the full implementation with tests**

Replace the contents of `crates/ctxfs-cli/src/deps/cargo_deps.rs`:

```rust
use super::{DetectedDep, Ecosystem};
use anyhow::{Context, Result};
use std::path::Path;

pub fn parse_cargo_toml(path: &Path) -> Result<Vec<DetectedDep>> {
    let content = std::fs::read_to_string(path).context("failed to read Cargo.toml")?;
    let doc: toml::Value = content.parse().context("failed to parse Cargo.toml")?;

    let mut deps = Vec::new();

    if let Some(table) = doc.get("dependencies").and_then(|v| v.as_table()) {
        parse_dep_table(table, false, &mut deps);
    }

    if let Some(table) = doc.get("dev-dependencies").and_then(|v| v.as_table()) {
        parse_dep_table(table, true, &mut deps);
    }

    Ok(deps)
}

fn parse_dep_table(table: &toml::map::Map<String, toml::Value>, is_dev: bool, deps: &mut Vec<DetectedDep>) {
    for (name, value) in table {
        let version = extract_version(value);
        let Some(version) = version else {
            // Skip path/git dependencies that have no version on crates.io
            continue;
        };
        deps.push(DetectedDep::new(
            name.clone(),
            version,
            Ecosystem::Crate,
            is_dev,
        ));
    }
}

/// Extract version from a dependency value.
/// - `"1.0"` (string) -> Some("1.0")
/// - `{ version = "1.0", ... }` (table with version) -> Some("1.0")
/// - `{ path = "...", ... }` (no version) -> None
/// - `{ git = "...", ... }` (no version) -> None
fn extract_version(value: &toml::Value) -> Option<String> {
    match value {
        toml::Value::String(v) => Some(v.clone()),
        toml::Value::Table(t) => {
            // Skip if path or git dependency without version
            if (t.contains_key("path") || t.contains_key("git")) && !t.contains_key("version") {
                return None;
            }
            t.get("version")
                .and_then(|v| v.as_str())
                .map(|s| s.to_string())
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_string_versions() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            r#"
[package]
name = "example"
version = "0.1.0"

[dependencies]
serde = "1.0"
anyhow = "1"

[dev-dependencies]
tempfile = "3"
"#,
        )
        .unwrap();

        let deps = parse_cargo_toml(&cargo).unwrap();
        assert_eq!(deps.len(), 3);

        let serde = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde.version, "1.0");
        assert!(!serde.is_dev);
        assert_eq!(serde.source_spec, "crate:serde@1.0");

        let tempfile = deps.iter().find(|d| d.name == "tempfile").unwrap();
        assert!(tempfile.is_dev);
    }

    #[test]
    fn parse_table_versions() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            r#"
[package]
name = "example"
version = "0.1.0"

[dependencies]
serde = { version = "1.0", features = ["derive"] }
tokio = { version = "1", features = ["full"] }
"#,
        )
        .unwrap();

        let deps = parse_cargo_toml(&cargo).unwrap();
        assert_eq!(deps.len(), 2);

        let serde = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde.version, "1.0");
    }

    #[test]
    fn skip_path_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            r#"
[package]
name = "example"
version = "0.1.0"

[dependencies]
my-local = { path = "../my-local" }
serde = "1.0"
"#,
        )
        .unwrap();

        let deps = parse_cargo_toml(&cargo).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "serde");
    }

    #[test]
    fn skip_git_dependencies() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            r#"
[package]
name = "example"
version = "0.1.0"

[dependencies]
my-git = { git = "https://github.com/example/repo" }
serde = "1.0"
"#,
        )
        .unwrap();

        let deps = parse_cargo_toml(&cargo).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "serde");
    }

    #[test]
    fn no_deps_section() {
        let dir = tempfile::tempdir().unwrap();
        let cargo = dir.path().join("Cargo.toml");
        std::fs::write(
            &cargo,
            r#"
[package]
name = "example"
version = "0.1.0"
"#,
        )
        .unwrap();

        let deps = parse_cargo_toml(&cargo).unwrap();
        assert!(deps.is_empty());
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p ctxfs -- deps::cargo_deps::tests`
Expected: 5 tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/ctxfs-cli/src/deps/cargo_deps.rs
git commit -m "feat(cli): implement Cargo.toml dependency parser"
```

---

### Task 5: Python manifest parsers (requirements.txt + pyproject.toml)

**Files:**
- Modify: `crates/ctxfs-cli/src/deps/python.rs`

- [ ] **Step 1: Write the full implementation with tests**

Replace the contents of `crates/ctxfs-cli/src/deps/python.rs`:

```rust
use super::{DetectedDep, Ecosystem};
use anyhow::{Context, Result};
use std::path::Path;

pub fn parse_requirements_txt(path: &Path) -> Result<Vec<DetectedDep>> {
    let content = std::fs::read_to_string(path).context("failed to read requirements.txt")?;
    let mut deps = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        // Skip comments, blank lines, and flags (-r, -e, --index-url, etc.)
        if line.is_empty() || line.starts_with('#') || line.starts_with('-') {
            continue;
        }

        // Parse "package==version" or "package>=version" etc.
        let (name, version) = if let Some(pos) = line.find("==") {
            (line[..pos].trim().to_string(), line[pos + 2..].trim().to_string())
        } else if let Some(pos) = line.find(">=") {
            (line[..pos].trim().to_string(), line[pos + 2..].trim().to_string())
        } else if let Some(pos) = line.find("<=") {
            (line[..pos].trim().to_string(), line[pos + 2..].trim().to_string())
        } else if let Some(pos) = line.find("~=") {
            (line[..pos].trim().to_string(), line[pos + 2..].trim().to_string())
        } else if let Some(pos) = line.find("!=") {
            (line[..pos].trim().to_string(), "latest".to_string())
        } else {
            // Bare package name, no version
            (line.to_string(), "latest".to_string())
        };

        // Strip any trailing version constraints (e.g., ",<3.0" from ">=2.0,<3.0")
        let version = version.split(',').next().unwrap_or("latest").trim().to_string();
        let version = if version.is_empty() { "latest".to_string() } else { version };

        if !name.is_empty() {
            deps.push(DetectedDep::new(name, version, Ecosystem::PyPI, false));
        }
    }

    Ok(deps)
}

/// Extra names that are classified as dev dependencies.
const DEV_EXTRA_NAMES: &[&str] = &["dev", "test", "testing"];

pub fn parse_pyproject_toml(path: &Path) -> Result<Vec<DetectedDep>> {
    let content = std::fs::read_to_string(path).context("failed to read pyproject.toml")?;
    let doc: toml::Value = content.parse().context("failed to parse pyproject.toml")?;

    let mut deps = Vec::new();

    // [project.dependencies]
    if let Some(arr) = doc
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(|d| d.as_array())
    {
        for item in arr {
            if let Some(s) = item.as_str() {
                if let Some(dep) = parse_pep508(s, false) {
                    deps.push(dep);
                }
            }
        }
    }

    // [project.optional-dependencies]
    if let Some(table) = doc
        .get("project")
        .and_then(|p| p.get("optional-dependencies"))
        .and_then(|d| d.as_table())
    {
        for (extra_name, arr) in table {
            let is_dev = DEV_EXTRA_NAMES.contains(&extra_name.to_lowercase().as_str());
            if let Some(arr) = arr.as_array() {
                for item in arr {
                    if let Some(s) = item.as_str() {
                        if let Some(dep) = parse_pep508(s, is_dev) {
                            deps.push(dep);
                        }
                    }
                }
            }
        }
    }

    Ok(deps)
}

/// Parse a PEP 508 dependency string like "requests>=2.31.0" or "numpy".
fn parse_pep508(s: &str, is_dev: bool) -> Option<DetectedDep> {
    let s = s.trim();
    if s.is_empty() {
        return None;
    }

    // Split on first version operator
    let ops = [">=", "<=", "~=", "==", "!=", "<", ">"];
    for op in ops {
        if let Some(pos) = s.find(op) {
            let name = s[..pos].trim().to_string();
            let rest = s[pos + op.len()..].trim();
            // Take first version (before comma or semicolon for environment markers)
            let version = rest
                .split([',', ';'])
                .next()
                .unwrap_or("latest")
                .trim()
                .to_string();
            let version = if version.is_empty() { "latest".to_string() } else { version };

            if !name.is_empty() {
                return Some(DetectedDep::new(name, version, Ecosystem::PyPI, is_dev));
            }
            return None;
        }
    }

    // No version specifier — bare name (possibly with extras like "package[extra]")
    let name = s.split('[').next().unwrap_or(s).trim().to_string();
    // Also strip environment markers after ;
    let name = name.split(';').next().unwrap_or(&name).trim().to_string();
    if !name.is_empty() {
        Some(DetectedDep::new(name, "latest".to_string(), Ecosystem::PyPI, is_dev))
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_requirements_pinned() {
        let dir = tempfile::tempdir().unwrap();
        let req = dir.path().join("requirements.txt");
        std::fs::write(&req, "requests==2.31.0\nflask==3.0.0\n").unwrap();

        let deps = parse_requirements_txt(&req).unwrap();
        assert_eq!(deps.len(), 2);

        let requests = deps.iter().find(|d| d.name == "requests").unwrap();
        assert_eq!(requests.version, "2.31.0");
        assert!(!requests.is_dev);
        assert_eq!(requests.source_spec, "pypi:requests@2.31.0");
    }

    #[test]
    fn parse_requirements_unpinned() {
        let dir = tempfile::tempdir().unwrap();
        let req = dir.path().join("requirements.txt");
        std::fs::write(&req, "requests\n").unwrap();

        let deps = parse_requirements_txt(&req).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].version, "latest");
    }

    #[test]
    fn parse_requirements_skips_comments_and_flags() {
        let dir = tempfile::tempdir().unwrap();
        let req = dir.path().join("requirements.txt");
        std::fs::write(&req, "# comment\n-r other.txt\n--index-url https://pypi.org\nrequests==2.0\n").unwrap();

        let deps = parse_requirements_txt(&req).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
    }

    #[test]
    fn parse_requirements_gte() {
        let dir = tempfile::tempdir().unwrap();
        let req = dir.path().join("requirements.txt");
        std::fs::write(&req, "requests>=2.31.0,<3.0\n").unwrap();

        let deps = parse_requirements_txt(&req).unwrap();
        assert_eq!(deps[0].version, "2.31.0");
    }

    #[test]
    fn parse_pyproject_basic() {
        let dir = tempfile::tempdir().unwrap();
        let pyp = dir.path().join("pyproject.toml");
        std::fs::write(
            &pyp,
            r#"
[project]
name = "my-app"
dependencies = [
    "requests>=2.31.0",
    "flask==3.0.0",
]

[project.optional-dependencies]
dev = ["pytest>=7.0.0"]
docs = ["sphinx>=6.0"]
"#,
        )
        .unwrap();

        let deps = parse_pyproject_toml(&pyp).unwrap();
        assert_eq!(deps.len(), 4);

        let requests = deps.iter().find(|d| d.name == "requests").unwrap();
        assert_eq!(requests.version, "2.31.0");
        assert!(!requests.is_dev);

        let pytest = deps.iter().find(|d| d.name == "pytest").unwrap();
        assert!(pytest.is_dev);

        // "docs" extra is NOT dev
        let sphinx = deps.iter().find(|d| d.name == "sphinx").unwrap();
        assert!(!sphinx.is_dev);
    }

    #[test]
    fn parse_pyproject_test_extra_is_dev() {
        let dir = tempfile::tempdir().unwrap();
        let pyp = dir.path().join("pyproject.toml");
        std::fs::write(
            &pyp,
            r#"
[project]
name = "my-app"
dependencies = []

[project.optional-dependencies]
test = ["pytest>=7.0.0"]
testing = ["coverage>=6.0"]
"#,
        )
        .unwrap();

        let deps = parse_pyproject_toml(&pyp).unwrap();
        assert!(deps.iter().all(|d| d.is_dev));
    }

    #[test]
    fn parse_pyproject_no_deps() {
        let dir = tempfile::tempdir().unwrap();
        let pyp = dir.path().join("pyproject.toml");
        std::fs::write(&pyp, "[project]\nname = \"my-app\"\n").unwrap();

        let deps = parse_pyproject_toml(&pyp).unwrap();
        assert!(deps.is_empty());
    }

    #[test]
    fn pep508_bare_name() {
        let dep = parse_pep508("numpy", false).unwrap();
        assert_eq!(dep.name, "numpy");
        assert_eq!(dep.version, "latest");
    }

    #[test]
    fn pep508_with_extras() {
        let dep = parse_pep508("requests[security]>=2.31.0", false).unwrap();
        // Name should strip extras bracket
        assert_eq!(dep.version, "2.31.0");
    }
}
```

- [ ] **Step 2: Run tests to verify they pass**

Run: `cargo test -p ctxfs -- deps::python::tests`
Expected: 9 tests pass

- [ ] **Step 3: Commit**

```bash
git add crates/ctxfs-cli/src/deps/python.rs
git commit -m "feat(cli): implement requirements.txt and pyproject.toml parsers"
```

---

### Task 6: Slug derivation and collision handling

**Files:**
- Create: `crates/ctxfs-cli/src/deps/slug.rs`
- Modify: `crates/ctxfs-cli/src/deps/mod.rs` (add `mod slug; pub use slug::*;`)

- [ ] **Step 1: Write slug module with tests**

Create `crates/ctxfs-cli/src/deps/slug.rs`:

```rust
use super::{DetectedDep, Ecosystem};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Derive a mount directory slug from a source spec string.
/// - `npm:react@19.1.0` -> `react`
/// - `npm:@types/node@20.0.0` -> `types-node`
/// - `github:owner/repo@main` -> `repo-main`
/// - `github:owner/repo` -> `repo`
/// - `crate:serde@1.0` -> `serde`
pub fn source_to_slug(source_spec: &str) -> String {
    // Strip provider prefix
    let rest = source_spec
        .split_once(':')
        .map_or(source_spec, |(_, r)| r);

    // For GitHub sources: owner/repo@ref -> repo-ref
    if source_spec.starts_with("github:") {
        let repo_ref = rest;
        let (owner_repo, git_ref) = repo_ref.split_once('@').unwrap_or((repo_ref, ""));
        let repo = owner_repo.split('/').nth(1).unwrap_or(owner_repo);
        if git_ref.is_empty() {
            return repo.to_string();
        }
        return format!("{repo}-{git_ref}");
    }

    // For registry sources: name@version -> name (slug only uses name)
    let name = rest.split_once('@').map_or(rest, |(n, _)| n);

    // Sanitize scoped npm packages: @types/node -> types-node
    name.trim_start_matches('@').replace('/', "-")
}

/// Compute mount paths for a list of deps, handling collisions.
/// Returns a map from source_spec -> mount path.
pub fn compute_mount_paths(deps: &[DetectedDep], mount_dir: &Path) -> HashMap<String, PathBuf> {
    // First pass: compute raw slugs and detect collisions
    let mut slug_counts: HashMap<String, Vec<&DetectedDep>> = HashMap::new();
    for dep in deps {
        let slug = source_to_slug(&dep.source_spec);
        slug_counts.entry(slug).or_default().push(dep);
    }

    let mut result = HashMap::new();
    for (slug, colliding_deps) in &slug_counts {
        if colliding_deps.len() == 1 {
            // No collision
            result.insert(
                colliding_deps[0].source_spec.clone(),
                mount_dir.join(slug),
            );
        } else {
            // Collision: prefix with ecosystem
            for dep in colliding_deps {
                let prefixed = format!("{}-{}", dep.ecosystem, slug);
                result.insert(dep.source_spec.clone(), mount_dir.join(prefixed));
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn slug_npm_basic() {
        assert_eq!(source_to_slug("npm:react@19.1.0"), "react");
    }

    #[test]
    fn slug_npm_scoped() {
        assert_eq!(source_to_slug("npm:@types/node@20.0.0"), "types-node");
    }

    #[test]
    fn slug_github_with_ref() {
        assert_eq!(source_to_slug("github:owner/repo@main"), "repo-main");
    }

    #[test]
    fn slug_github_with_tag() {
        assert_eq!(source_to_slug("github:owner/repo@v1.0.0"), "repo-v1.0.0");
    }

    #[test]
    fn slug_crate() {
        assert_eq!(source_to_slug("crate:serde@1.0"), "serde");
    }

    #[test]
    fn slug_pypi() {
        assert_eq!(source_to_slug("pypi:requests@2.31.0"), "requests");
    }

    #[test]
    fn no_collision_unique_names() {
        let deps = vec![
            DetectedDep::new("react".into(), "19.1.0".into(), Ecosystem::Npm, false),
            DetectedDep::new("serde".into(), "1.0".into(), Ecosystem::Crate, false),
        ];
        let paths = compute_mount_paths(&deps, Path::new("./deps"));
        assert_eq!(paths["npm:react@19.1.0"], PathBuf::from("./deps/react"));
        assert_eq!(paths["crate:serde@1.0"], PathBuf::from("./deps/serde"));
    }

    #[test]
    fn collision_adds_ecosystem_prefix() {
        let deps = vec![
            DetectedDep::new("requests".into(), "2.31.0".into(), Ecosystem::PyPI, false),
            DetectedDep::new("requests".into(), "0.14.0".into(), Ecosystem::Crate, false),
        ];
        let paths = compute_mount_paths(&deps, Path::new("./deps"));
        assert_eq!(
            paths["pypi:requests@2.31.0"],
            PathBuf::from("./deps/pypi-requests")
        );
        assert_eq!(
            paths["crate:requests@0.14.0"],
            PathBuf::from("./deps/crate-requests")
        );
    }
}
```

- [ ] **Step 2: Register the module in deps/mod.rs**

In `crates/ctxfs-cli/src/deps/mod.rs`, add after the `pub mod mount;` line:

```rust
mod slug;
pub use slug::{compute_mount_paths, source_to_slug};
```

- [ ] **Step 3: Run tests to verify they pass**

Run: `cargo test -p ctxfs -- deps::slug::tests`
Expected: 8 tests pass

- [ ] **Step 4: Commit**

```bash
git add crates/ctxfs-cli/src/deps/slug.rs crates/ctxfs-cli/src/deps/mod.rs
git commit -m "feat(cli): add mount directory slug derivation with collision handling"
```

---

### Task 7: Refactor CLI Commands enum for multi-mount and deps

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs`

- [ ] **Step 1: Update the Mount variant to support multi-mount**

In `crates/ctxfs-cli/src/main.rs`, replace the `Mount` variant (lines 22-33) with:

```rust
    /// Mount a remote source as a local directory
    Mount {
        /// Source spec(s) (e.g., github:owner/repo@ref, npm:react@19.1.0)
        #[arg(required = true)]
        sources: Vec<String>,
        /// Local mount point (for single source only; mutually exclusive with --mount-dir)
        #[arg(long, short = 'p')]
        mount_point: Option<PathBuf>,
        /// Base directory for auto-derived mount points (required for multiple sources)
        #[arg(long, short = 'd')]
        mount_dir: Option<PathBuf>,
        /// Start the daemon's NFS server but skip the kernel mount step
        #[arg(long)]
        server_only: bool,
    },
```

- [ ] **Step 2: Update the Unmount variant to support --all**

Replace the `Unmount` variant (lines 34-38) with:

```rust
    /// Unmount a mounted filesystem
    Unmount {
        /// Mount point or mount ID (required unless --all is used)
        target: Option<String>,
        /// Unmount all active mounts
        #[arg(long)]
        all: bool,
    },
```

- [ ] **Step 3: Add the Deps subcommand group**

Add after the `Setup` variant in the `Commands` enum:

```rust
    /// Dependency detection and batch mounting
    Deps {
        #[command(subcommand)]
        action: DepsAction,
    },
```

- [ ] **Step 4: Define DepsAction enum**

Add after the `CacheAction` enum definition:

```rust
#[derive(Subcommand)]
enum DepsAction {
    /// List detected dependencies from manifest files
    List {
        /// Project directory to scan
        #[arg(default_value = ".")]
        project_dir: PathBuf,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Include dev dependencies
        #[arg(long)]
        include_dev: bool,
    },
    /// Mount detected dependencies
    Mount {
        /// Project directory to scan
        #[arg(default_value = ".")]
        project_dir: PathBuf,
        /// Mount all production dependencies (non-interactive)
        #[arg(long)]
        all: bool,
        /// Mount specific packages by name (comma-separated)
        #[arg(long, value_delimiter = ',')]
        select: Option<Vec<String>>,
        /// Include dev dependencies
        #[arg(long)]
        include_dev: bool,
        /// Base directory for mounts
        #[arg(long, short = 'd', default_value = "./ctxfs-deps")]
        mount_dir: PathBuf,
        /// Start NFS servers but skip kernel mounts
        #[arg(long)]
        server_only: bool,
    },
    /// Unmount all deps previously mounted
    Unmount {
        /// Base directory to scan for mounts
        #[arg(long, short = 'd', default_value = "./ctxfs-deps")]
        mount_dir: PathBuf,
    },
}
```

- [ ] **Step 5: Verify it compiles**

Run: `cargo check -p ctxfs`
Expected: compiles (with warnings about unhandled match arms — we'll handle those in the next tasks)

- [ ] **Step 6: Commit**

```bash
git add crates/ctxfs-cli/src/main.rs
git commit -m "feat(cli): restructure Commands enum for multi-mount, unmount --all, and deps subcommand"
```

---

### Task 8: Batch mount/unmount logic

**Files:**
- Modify: `crates/ctxfs-cli/src/deps/mount.rs`

- [ ] **Step 1: Write the batch mount and unmount module**

Replace the contents of `crates/ctxfs-cli/src/deps/mount.rs`:

```rust
use anyhow::{Context, Result};
use ctxfs_ipc::service::CtxfsServiceClient;
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Result of a single mount attempt.
pub struct MountResult {
    pub source: String,
    pub mount_point: PathBuf,
    pub success: bool,
    pub error: Option<String>,
}

/// Result of a single unmount attempt.
pub struct UnmountResult {
    pub mount_point: String,
    pub success: bool,
    pub error: Option<String>,
}

/// Context with a longer deadline for operations that call external APIs.
fn long_context() -> tarpc::context::Context {
    let mut ctx = tarpc::context::current();
    ctx.deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    ctx
}

/// Mount multiple sources concurrently via the daemon, then run kernel mounts sequentially.
/// Returns (successes, failures).
pub async fn batch_mount(
    client: &CtxfsServiceClient,
    mounts: &HashMap<String, PathBuf>,
    server_only: bool,
) -> Vec<MountResult> {
    // Step 1: Issue all daemon mount RPCs concurrently
    let mut daemon_futures = Vec::new();
    let entries: Vec<_> = mounts.iter().collect();

    for (source, mount_point) in &entries {
        let mp_str = mount_point.to_string_lossy().to_string();

        // Ensure mount point directory exists
        if let Err(e) = std::fs::create_dir_all(mount_point) {
            daemon_futures.push(futures::future::ready(MountResult {
                source: (*source).clone(),
                mount_point: (*mount_point).clone(),
                success: false,
                error: Some(format!("failed to create directory: {e}")),
            }).boxed());
            continue;
        }

        let client = client.clone();
        let source = (*source).clone();
        let mount_point = (*mount_point).clone();
        daemon_futures.push(Box::pin(async move {
            let mp_str = mount_point.to_string_lossy().to_string();
            match client.mount(long_context(), source.clone(), mp_str).await {
                Ok(Ok(info)) => {
                    if server_only {
                        MountResult {
                            source,
                            mount_point,
                            success: true,
                            error: None,
                        }
                    } else {
                        // Step 2 will handle kernel mount
                        MountResult {
                            source,
                            mount_point,
                            success: true,
                            error: None,
                        }
                    }
                }
                Ok(Err(e)) => MountResult {
                    source,
                    mount_point,
                    success: false,
                    error: Some(e),
                },
                Err(e) => MountResult {
                    source,
                    mount_point,
                    success: false,
                    error: Some(e.to_string()),
                },
            }
        }) as std::pin::Pin<Box<dyn std::future::Future<Output = MountResult> + Send>>);
    }

    // Wait for all daemon RPCs
    let results: Vec<MountResult> = futures::future::join_all(daemon_futures).await;

    if server_only {
        return results;
    }

    // Step 2: Run kernel mounts sequentially for successful daemon mounts
    let mut final_results = Vec::new();
    for mut result in results {
        if result.success {
            // Get the mount info to find the NFS port
            let mp_str = result.mount_point.to_string_lossy().to_string();
            if let Err(e) = super::super::run_mount_nfs_for_source(client, &result.source, &mp_str).await {
                // Daemon mount succeeded but kernel mount failed — clean up
                let _ = client
                    .unmount(tarpc::context::current(), mp_str)
                    .await;
                result.success = false;
                result.error = Some(format!("kernel mount failed: {e}"));
            }
        }
        final_results.push(result);
    }

    final_results
}

/// Unmount all mounts whose mount_point is under the given directory.
pub async fn batch_unmount_dir(
    client: &CtxfsServiceClient,
    mount_dir: &Path,
) -> Vec<UnmountResult> {
    let mount_dir_str = mount_dir.to_string_lossy().to_string();

    // Get all active mounts
    let mounts = match client.list(tarpc::context::current()).await {
        Ok(m) => m,
        Err(e) => {
            return vec![UnmountResult {
                mount_point: mount_dir_str,
                success: false,
                error: Some(format!("failed to list mounts: {e}")),
            }];
        }
    };

    // Filter to mounts under our directory
    let matching: Vec<_> = mounts
        .into_iter()
        .filter(|m| m.mount_point.starts_with(&mount_dir_str))
        .collect();

    let mut results = Vec::new();
    for m in matching {
        // Kernel unmount first
        if let Err(e) = super::super::run_umount(&m.mount_point) {
            eprintln!("warning: kernel umount failed for {}: {e}", m.mount_point);
        }

        // Daemon cleanup
        let success = match client
            .unmount(tarpc::context::current(), m.mount_point.clone())
            .await
        {
            Ok(Ok(())) => true,
            Ok(Err(e)) => {
                results.push(UnmountResult {
                    mount_point: m.mount_point,
                    success: false,
                    error: Some(e),
                });
                continue;
            }
            Err(e) => {
                results.push(UnmountResult {
                    mount_point: m.mount_point,
                    success: false,
                    error: Some(e.to_string()),
                });
                continue;
            }
        };

        results.push(UnmountResult {
            mount_point: m.mount_point,
            success,
            error: None,
        });
    }

    results
}

/// Unmount all active mounts tracked by the daemon.
pub async fn batch_unmount_all(client: &CtxfsServiceClient) -> Vec<UnmountResult> {
    let mounts = match client.list(tarpc::context::current()).await {
        Ok(m) => m,
        Err(e) => {
            return vec![UnmountResult {
                mount_point: "(all)".into(),
                success: false,
                error: Some(format!("failed to list mounts: {e}")),
            }];
        }
    };

    let mut results = Vec::new();
    for m in mounts {
        if let Err(e) = super::super::run_umount(&m.mount_point) {
            eprintln!("warning: kernel umount failed for {}: {e}", m.mount_point);
        }

        match client
            .unmount(tarpc::context::current(), m.mount_point.clone())
            .await
        {
            Ok(Ok(())) => results.push(UnmountResult {
                mount_point: m.mount_point,
                success: true,
                error: None,
            }),
            Ok(Err(e)) => results.push(UnmountResult {
                mount_point: m.mount_point,
                success: false,
                error: Some(e),
            }),
            Err(e) => results.push(UnmountResult {
                mount_point: m.mount_point,
                success: false,
                error: Some(e.to_string()),
            }),
        }
    }

    results
}

/// Print a summary of mount results.
pub fn print_mount_summary(results: &[MountResult]) {
    let success_count = results.iter().filter(|r| r.success).count();
    let total = results.len();
    println!("Mounted {success_count}/{total} dependencies:");
    for r in results {
        if r.success {
            println!("  ok  {} -> {}", r.source, r.mount_point.display());
        } else {
            let err = r.error.as_deref().unwrap_or("unknown error");
            println!("  ERR {} -- {}", r.source, err);
        }
    }
}

/// Print a summary of unmount results.
pub fn print_unmount_summary(results: &[UnmountResult]) {
    let success_count = results.iter().filter(|r| r.success).count();
    let total = results.len();
    println!("Unmounted {success_count}/{total}:");
    for r in results {
        if r.success {
            println!("  ok  {}", r.mount_point);
        } else {
            let err = r.error.as_deref().unwrap_or("unknown error");
            println!("  ERR {} -- {}", r.mount_point, err);
        }
    }
}

use futures::FutureExt;
```

Note: This module references `super::super::run_mount_nfs_for_source` and `super::super::run_umount`. The `run_umount` already exists in `main.rs`. We need to add a helper `run_mount_nfs_for_source` in Task 9 when wiring up main.rs. For now, this module will not compile standalone — that's fine, it will compile once Task 9 wires everything together.

- [ ] **Step 2: Commit (will compile after Task 9)**

```bash
git add crates/ctxfs-cli/src/deps/mount.rs
git commit -m "feat(cli): implement batch mount/unmount logic with concurrent daemon RPCs"
```

---

### Task 9: Wire up main.rs command handlers

**Files:**
- Modify: `crates/ctxfs-cli/src/main.rs`

This is the largest task — it connects all the pieces. The match arms for `Mount`, `Unmount`, `Deps` need to be rewritten.

- [ ] **Step 1: Add necessary imports at the top of main.rs**

Add to the imports at the top of `crates/ctxfs-cli/src/main.rs`:

```rust
use std::collections::HashMap;
```

- [ ] **Step 2: Rewrite the Mount handler for multi-mount**

Replace the `Commands::Mount { ... }` match arm (lines 119-178) with:

```rust
        Commands::Mount {
            sources,
            mount_point,
            mount_dir,
            server_only,
        } => {
            // Validate argument combinations
            if mount_point.is_some() && mount_dir.is_some() {
                anyhow::bail!("cannot use both --mount-point and --mount-dir");
            }
            if mount_point.is_some() && sources.len() > 1 {
                anyhow::bail!("use --mount-dir for multiple sources");
            }
            if mount_point.is_none() && mount_dir.is_none() {
                if sources.len() == 1 {
                    anyhow::bail!(
                        "provide either a mount point with -p or use --mount-dir.\n\
                         Example: ctxfs mount {} -p /tmp/mnt\n\
                         Example: ctxfs mount {} -d ./deps",
                        sources[0], sources[0]
                    );
                }
                anyhow::bail!("--mount-dir is required for multiple sources");
            }

            let client = connect(&config).await?;

            if let Some(mp) = mount_point {
                // Single mount (legacy path)
                let mp_str = mp.to_str().context("invalid mount point path")?.to_string();
                std::fs::create_dir_all(&mp).context("failed to create mount point directory")?;

                let info = client
                    .mount(long_context(), sources[0].clone(), mp_str.clone())
                    .await?
                    .map_err(|e| anyhow::anyhow!(e))?;

                if server_only {
                    println!("NFS server ready:");
                    println!("  Source:   {}", info.source);
                    println!("  Commit:   {}", info.commit_sha);
                    println!("  ID:       {}", info.id);
                    println!("  NFS port: {}", info.nfs_port);
                    return Ok(());
                }

                println!(
                    "NFS server listening on 127.0.0.1:{} — mounting kernel side (may prompt for sudo)",
                    info.nfs_port
                );
                if let Err(e) = run_mount_nfs(info.nfs_port, &mp_str) {
                    let _ = client
                        .unmount(tarpc::context::current(), mp_str)
                        .await;
                    return Err(anyhow::anyhow!("kernel mount failed: {e}"));
                }

                println!("Mounted {} at {}", info.source, info.mount_point);
                println!("  Commit:   {}", info.commit_sha);
                println!("  ID:       {}", info.id);
                println!("  NFS port: {}", info.nfs_port);
            } else {
                // Multi-mount path
                let base_dir = mount_dir.unwrap();
                let mut mount_map = HashMap::new();
                for source in &sources {
                    let slug = deps::source_to_slug(source);
                    mount_map.insert(source.clone(), base_dir.join(slug));
                }

                // Handle slug collisions by re-deriving with ecosystem prefix
                let mut slug_counts: HashMap<String, Vec<String>> = HashMap::new();
                for (source, path) in &mount_map {
                    let slug = path.file_name().unwrap().to_string_lossy().to_string();
                    slug_counts.entry(slug).or_default().push(source.clone());
                }
                for (slug, colliders) in &slug_counts {
                    if colliders.len() > 1 {
                        for source in colliders {
                            let prefix = source.split_once(':').map_or("", |(p, _)| p);
                            let new_slug = format!("{prefix}-{slug}");
                            mount_map.insert(source.clone(), base_dir.join(new_slug));
                        }
                    }
                }

                let results = deps::mount::batch_mount(&client, &mount_map, server_only).await;
                deps::mount::print_mount_summary(&results);

                if results.iter().any(|r| !r.success) {
                    std::process::exit(1);
                }
            }
        }
```

- [ ] **Step 3: Rewrite the Unmount handler for --all**

Replace the `Commands::Unmount { target }` match arm (lines 180-194) with:

```rust
        Commands::Unmount { target, all } => {
            if all {
                let client = connect(&config).await?;
                let results = deps::mount::batch_unmount_all(&client).await;
                if results.is_empty() {
                    println!("No active mounts");
                } else {
                    deps::mount::print_unmount_summary(&results);
                    if results.iter().any(|r| !r.success) {
                        std::process::exit(1);
                    }
                }
            } else {
                let target = target.context("provide a target or use --all")?;

                if let Err(e) = run_umount(&target) {
                    eprintln!("warning: kernel umount failed: {e}");
                }

                let client = connect(&config).await?;
                client
                    .unmount(tarpc::context::current(), target.clone())
                    .await?
                    .map_err(|e| anyhow::anyhow!(e))?;

                println!("Unmounted {target}");
            }
        }
```

- [ ] **Step 4: Add the Deps command handler**

Add a new match arm for `Commands::Deps` in the main match block:

```rust
        Commands::Deps { action } => match action {
            DepsAction::List {
                project_dir,
                json,
                include_dev,
            } => {
                let all_deps = deps::detect_all(&project_dir);
                if all_deps.is_empty() {
                    eprintln!(
                        "No supported manifest files found in {}",
                        project_dir.display()
                    );
                    std::process::exit(1);
                }

                let filtered: Vec<_> = if include_dev {
                    all_deps
                } else {
                    all_deps.into_iter().filter(|d| !d.is_dev).collect()
                };

                if json {
                    #[derive(serde::Serialize)]
                    struct DepsOutput {
                        manifests: Vec<String>,
                        dependencies: Vec<deps::DetectedDep>,
                    }
                    // Detect which manifests exist
                    let mut manifests = Vec::new();
                    for name in &["package.json", "Cargo.toml", "requirements.txt", "pyproject.toml"] {
                        if project_dir.join(name).is_file() {
                            manifests.push((*name).to_string());
                        }
                    }
                    let output = DepsOutput {
                        manifests,
                        dependencies: filtered,
                    };
                    println!("{}", serde_json::to_string_pretty(&output)?);
                } else if filtered.is_empty() {
                    println!("No matching dependencies found");
                } else {
                    // Group by ecosystem
                    let mut by_eco: std::collections::BTreeMap<String, Vec<&deps::DetectedDep>> =
                        std::collections::BTreeMap::new();
                    for dep in &filtered {
                        by_eco
                            .entry(dep.ecosystem.to_string())
                            .or_default()
                            .push(dep);
                    }
                    for (eco, eco_deps) in &by_eco {
                        println!("\n{eco}:");
                        for dep in eco_deps {
                            let dev_tag = if dep.is_dev { " [dev]" } else { "" };
                            println!("  {} @{}{}", dep.name, dep.version, dev_tag);
                        }
                    }
                    println!("\n{} dependencies found", filtered.len());
                }
            }

            DepsAction::Mount {
                project_dir,
                all,
                select,
                include_dev,
                mount_dir,
                server_only,
            } => {
                let all_deps = deps::detect_all(&project_dir);
                if all_deps.is_empty() {
                    eprintln!(
                        "No supported manifest files found in {}",
                        project_dir.display()
                    );
                    std::process::exit(1);
                }

                // Filter dev deps based on flag
                let pool: Vec<_> = if include_dev {
                    all_deps
                } else {
                    all_deps.into_iter().filter(|d| !d.is_dev).collect()
                };

                if pool.is_empty() {
                    println!("No matching dependencies found");
                    return Ok(());
                }

                // Determine which deps to mount
                let selected: Vec<deps::DetectedDep> = if let Some(names) = select {
                    // --select mode: match by name (qualified or bare)
                    let mut matched = Vec::new();
                    for name in &names {
                        let matching: Vec<_> = if name.contains(':') {
                            // Qualified: "npm:react"
                            pool.iter()
                                .filter(|d| {
                                    let qualified = format!("{}:{}", d.ecosystem, d.name);
                                    qualified == *name
                                })
                                .cloned()
                                .collect()
                        } else {
                            // Bare name
                            pool.iter()
                                .filter(|d| d.name == *name)
                                .cloned()
                                .collect()
                        };

                        if matching.is_empty() {
                            eprintln!("warning: no dependency found matching '{name}'");
                        } else if matching.len() > 1 {
                            eprintln!(
                                "error: ambiguous name '{name}', matches: {}. Use qualified form (e.g., npm:{name})",
                                matching
                                    .iter()
                                    .map(|d| format!("{}:{}", d.ecosystem, d.name))
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            );
                            std::process::exit(1);
                        } else {
                            matched.push(matching.into_iter().next().unwrap());
                        }
                    }
                    matched
                } else if all {
                    // --all mode
                    pool
                } else {
                    // Interactive mode
                    let is_tty = std::io::IsTerminal::is_terminal(&std::io::stdin());
                    if !is_tty {
                        eprintln!("error: use --all or --select in non-interactive mode");
                        std::process::exit(1);
                    }

                    let labels: Vec<String> = pool.iter().map(|d| d.picker_label()).collect();
                    // Pre-select production deps
                    let defaults: Vec<bool> = pool.iter().map(|d| !d.is_dev).collect();

                    let selections = dialoguer::MultiSelect::new()
                        .with_prompt("Select dependencies to mount")
                        .items(&labels)
                        .defaults(&defaults)
                        .interact()
                        .context("interactive selection failed")?;

                    selections.into_iter().map(|i| pool[i].clone()).collect()
                };

                if selected.is_empty() {
                    println!("No dependencies selected");
                    return Ok(());
                }

                let mount_paths = deps::compute_mount_paths(&selected, &mount_dir);
                let client = connect(&config).await?;

                let results =
                    deps::mount::batch_mount(&client, &mount_paths, server_only).await;
                deps::mount::print_mount_summary(&results);

                if results.iter().any(|r| !r.success) {
                    std::process::exit(1);
                }
            }

            DepsAction::Unmount { mount_dir } => {
                let client = connect(&config).await?;
                let results = deps::mount::batch_unmount_dir(&client, &mount_dir).await;
                if results.is_empty() {
                    println!("No active mounts found under {}", mount_dir.display());
                } else {
                    deps::mount::print_unmount_summary(&results);
                    if results.iter().any(|r| !r.success) {
                        std::process::exit(1);
                    }
                }
            }
        },
```

- [ ] **Step 5: Add the helper function for batch mount kernel-side**

Add after the `run_umount` function at the bottom of `main.rs`:

```rust
/// Look up the NFS port for a source by querying daemon status, then run kernel mount.
async fn run_mount_nfs_for_source(
    client: &CtxfsServiceClient,
    _source: &str,
    mount_point: &str,
) -> Result<()> {
    // The daemon mount RPC already returned, so the NFS server is up.
    // We need to find the port — query the mount list.
    let mounts = client.list(tarpc::context::current()).await?;
    let info = mounts
        .iter()
        .find(|m| m.mount_point == mount_point)
        .context("mount not found after daemon RPC")?;

    run_mount_nfs(info.nfs_port, mount_point)
}
```

- [ ] **Step 6: Make run_umount and run_mount_nfs_for_source accessible from deps::mount**

The `deps::mount` module references `super::super::run_umount` and `super::super::run_mount_nfs_for_source`. Make sure these functions are `pub(crate)`:

Change `fn run_umount(` to `pub(crate) fn run_umount(` and keep `run_mount_nfs_for_source` as `pub(crate) async fn`.

Also change `fn run_mount_nfs(` to `pub(crate) fn run_mount_nfs(` since it's called from the helper.

- [ ] **Step 7: Update deps/mount.rs to use correct paths**

The `super::super::` references in `deps/mount.rs` need to resolve correctly. Since `mount.rs` is at `deps/mount.rs`, `super::super::` points to the `crate` root (main.rs module scope). Verify the references compile.

- [ ] **Step 8: Verify everything compiles**

Run: `cargo check -p ctxfs`
Expected: compiles with no errors

- [ ] **Step 9: Commit**

```bash
git add crates/ctxfs-cli/src/main.rs crates/ctxfs-cli/src/deps/mount.rs
git commit -m "feat(cli): wire up multi-mount, unmount --all, and deps command handlers"
```

---

### Task 10: Unit tests for detect_all integration

**Files:**
- Modify: `crates/ctxfs-cli/src/deps/mod.rs` (add more tests)

- [ ] **Step 1: Add integration tests for detect_all with real fixture files**

Add to the `#[cfg(test)] mod tests` block in `crates/ctxfs-cli/src/deps/mod.rs`:

```rust
    #[test]
    fn detect_all_package_json() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"react": "^19.1.0"}, "devDependencies": {"jest": "~29.0.0"}}"#,
        )
        .unwrap();

        let deps = detect_all(dir.path());
        assert_eq!(deps.len(), 2);
        assert!(deps.iter().any(|d| d.name == "react" && !d.is_dev));
        assert!(deps.iter().any(|d| d.name == "jest" && d.is_dev));
    }

    #[test]
    fn detect_all_cargo_toml() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();

        let deps = detect_all(dir.path());
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "serde");
        assert_eq!(deps[0].ecosystem, Ecosystem::Crate);
    }

    #[test]
    fn detect_all_requirements_txt() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("requirements.txt"),
            "requests==2.31.0\nflask==3.0.0\n",
        )
        .unwrap();

        let deps = detect_all(dir.path());
        assert_eq!(deps.len(), 2);
        assert!(deps.iter().all(|d| d.ecosystem == Ecosystem::PyPI));
    }

    #[test]
    fn detect_all_multiple_manifests() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("package.json"),
            r#"{"dependencies": {"react": "^19.1.0"}}"#,
        )
        .unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"x\"\nversion = \"0.1.0\"\n\n[dependencies]\nserde = \"1.0\"\n",
        )
        .unwrap();

        let deps = detect_all(dir.path());
        assert_eq!(deps.len(), 2);
        assert!(deps.iter().any(|d| d.ecosystem == Ecosystem::Npm));
        assert!(deps.iter().any(|d| d.ecosystem == Ecosystem::Crate));
    }
```

- [ ] **Step 2: Run all tests**

Run: `cargo test -p ctxfs -- deps::tests`
Expected: 9 tests pass (5 original + 4 new)

- [ ] **Step 3: Commit**

```bash
git add crates/ctxfs-cli/src/deps/mod.rs
git commit -m "test(cli): add detect_all integration tests with fixture files"
```

---

### Task 11: Clippy, fmt, and final verification

**Files:**
- All modified files

- [ ] **Step 1: Run cargo fmt**

Run: `cargo fmt --all`
Expected: formatting applied

- [ ] **Step 2: Run clippy**

Run: `cargo clippy --all-targets --tests`
Expected: no errors (warnings from pedantic are OK if existing)

Fix any clippy issues that arise — common ones will be:
- Unused imports (remove them)
- `too_many_arguments` (add `#[allow(clippy::too_many_arguments)]` if needed)
- `too_many_lines` (add `#[allow(clippy::too_many_lines)]` on main function)

- [ ] **Step 3: Run full test suite**

Run: `cargo test`
Expected: all tests pass (including existing tests — no regressions)

- [ ] **Step 4: Verify CLI help output**

Run: `cargo run -p ctxfs -- --help`
Expected: shows Mount, Unmount, List, Status, Daemon, Cache, Setup, Deps subcommands

Run: `cargo run -p ctxfs -- deps --help`
Expected: shows List, Mount, Unmount subcommands

Run: `cargo run -p ctxfs -- deps mount --help`
Expected: shows --all, --select, --include-dev, --mount-dir, --server-only flags

- [ ] **Step 5: Commit any fixes**

```bash
git add -A
git commit -m "style: fmt and clippy fixes for multi-mount and deps feature"
```

---

### Task 12: Update CLAUDE.md documentation

**Files:**
- Modify: `CLAUDE.md`

- [ ] **Step 1: Add deps module info to Architecture section**

After the `ctxfs-cli: clap CLI binary (depends on core, ipc, daemon)` line in CLAUDE.md, no new crate is needed — but document the new CLI module:

The deps detection module lives inside ctxfs-cli (`src/deps/`). No documentation changes are needed beyond verifying the existing CLAUDE.md accurately describes the CLI crate.

- [ ] **Step 2: Verify CLAUDE.md is accurate**

Read `CLAUDE.md` and confirm the architecture section and environment variables are still accurate. No new env vars or crates were added.

- [ ] **Step 3: Commit if any changes made**

```bash
git add CLAUDE.md
git commit -m "docs: update CLAUDE.md for deps feature"
```
