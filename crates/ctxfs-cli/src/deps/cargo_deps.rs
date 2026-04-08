use super::{DetectedDep, Ecosystem};
use anyhow::{Context, Result};
use std::path::Path;
use toml::Value;

pub fn parse_cargo_toml(path: &Path) -> Result<Vec<DetectedDep>> {
    let contents = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;
    let doc: Value = contents
        .parse()
        .with_context(|| format!("failed to parse TOML in {}", path.display()))?;

    let mut deps = Vec::new();

    if let Some(table) = doc.get("dependencies").and_then(Value::as_table) {
        parse_dep_table(table, false, &mut deps);
    }
    if let Some(table) = doc.get("dev-dependencies").and_then(Value::as_table) {
        parse_dep_table(table, true, &mut deps);
    }

    Ok(deps)
}

fn parse_dep_table(
    table: &toml::map::Map<String, Value>,
    is_dev: bool,
    out: &mut Vec<DetectedDep>,
) {
    for (name, value) in table {
        if let Some(version) = extract_version(value) {
            out.push(DetectedDep::new(
                name.clone(),
                version,
                Ecosystem::Crate,
                is_dev,
            ));
        }
    }
}

fn extract_version(value: &Value) -> Option<String> {
    match value {
        // serde = "1.0"
        Value::String(v) => Some(v.clone()),
        // serde = { version = "1.0", features = [...] }
        Value::Table(t) => {
            // Skip path and git deps that have no version key
            if t.contains_key("path") || t.contains_key("git") {
                return None;
            }
            t.get("version")?.as_str().map(String::from)
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_toml(content: &str) -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("Cargo.toml");
        let mut f = std::fs::File::create(&path).unwrap();
        f.write_all(content.as_bytes()).unwrap();
        (dir, path)
    }

    #[test]
    fn parse_string_versions() {
        let (_dir, path) = write_toml(
            r#"
[package]
name = "my-crate"
version = "0.1.0"

[dependencies]
serde = "1.0"
anyhow = "1"

[dev-dependencies]
tempfile = "3"
"#,
        );

        let deps = parse_cargo_toml(&path).unwrap();

        // 2 prod + 1 dev
        assert_eq!(deps.len(), 3);

        let serde = deps.iter().find(|d| d.name == "serde").unwrap();
        assert_eq!(serde.version, "1.0");
        assert!(!serde.is_dev);
        assert_eq!(serde.source_spec, "crate:serde@1.0");

        let anyhow = deps.iter().find(|d| d.name == "anyhow").unwrap();
        assert_eq!(anyhow.version, "1");
        assert!(!anyhow.is_dev);

        let tempfile = deps.iter().find(|d| d.name == "tempfile").unwrap();
        assert_eq!(tempfile.version, "3");
        assert!(tempfile.is_dev);
    }

    #[test]
    fn parse_table_versions() {
        let (_dir, path) = write_toml(
            r#"
[dependencies]
serde = { version = "1.0", features = ["derive"] }
"#,
        );

        let deps = parse_cargo_toml(&path).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "serde");
        assert_eq!(deps[0].version, "1.0");
    }

    #[test]
    fn skip_path_dependencies() {
        let (_dir, path) = write_toml(
            r#"
[dependencies]
my-local = { path = "../my-local" }
serde = "1.0"
"#,
        );

        let deps = parse_cargo_toml(&path).unwrap();
        assert_eq!(deps.len(), 1);
        assert!(deps.iter().all(|d| d.name != "my-local"));
    }

    #[test]
    fn skip_git_dependencies() {
        let (_dir, path) = write_toml(
            r#"
[dependencies]
my-git = { git = "https://github.com/example/repo" }
anyhow = "1"
"#,
        );

        let deps = parse_cargo_toml(&path).unwrap();
        assert_eq!(deps.len(), 1);
        assert!(deps.iter().all(|d| d.name != "my-git"));
    }

    #[test]
    fn no_deps_section() {
        let (_dir, path) = write_toml(
            r#"
[package]
name = "my-crate"
version = "0.1.0"
"#,
        );

        let deps = parse_cargo_toml(&path).unwrap();
        assert!(deps.is_empty());
    }
}
