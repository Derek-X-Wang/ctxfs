mod cargo_deps;
pub mod mount;
mod npm;
mod python;
mod slug;
pub use slug::{compute_mount_paths, source_to_slug};

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
        format!(
            "{} {} @{}{}",
            self.ecosystem, self.name, self.version, dev_tag
        )
    }
}

/// Detect all dependencies from manifest files in the given directory.
pub fn detect_all(project_dir: &Path) -> Vec<DetectedDep> {
    let mut deps = Vec::new();

    let pkg_json = project_dir.join("package.json");
    if pkg_json.is_file() {
        match npm::parse_package_json(&pkg_json) {
            Ok(mut d) => deps.append(&mut d),
            Err(e) => tracing::warn!("failed to parse {}: {e}", pkg_json.display()),
        }
    }

    let cargo_toml = project_dir.join("Cargo.toml");
    if cargo_toml.is_file() {
        match cargo_deps::parse_cargo_toml(&cargo_toml) {
            Ok(mut d) => deps.append(&mut d),
            Err(e) => tracing::warn!("failed to parse {}: {e}", cargo_toml.display()),
        }
    }

    let requirements_txt = project_dir.join("requirements.txt");
    if requirements_txt.is_file() {
        match python::parse_requirements_txt(&requirements_txt) {
            Ok(mut d) => deps.append(&mut d),
            Err(e) => tracing::warn!("failed to parse {}: {e}", requirements_txt.display()),
        }
    }

    let pyproject_toml = project_dir.join("pyproject.toml");
    if pyproject_toml.is_file() {
        match python::parse_pyproject_toml(&pyproject_toml) {
            Ok(mut d) => deps.append(&mut d),
            Err(e) => tracing::warn!("failed to parse {}: {e}", pyproject_toml.display()),
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
}
