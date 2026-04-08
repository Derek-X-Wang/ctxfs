use super::{DetectedDep, Ecosystem};
use anyhow::{Context, Result};
use std::path::Path;

/// Strip semver range operators from a version string.
///
/// - `^x.y.z` → `x.y.z`
/// - `~x.y.z` → `x.y.z`
/// - `>=x.y.z` → `x.y.z`
/// - `*` → `latest`
/// - complex ranges (spaces or `||`) → `latest`
pub fn strip_version_range(version: &str) -> String {
    let v = version.trim();

    // Complex ranges: contains "||" or internal spaces (after stripping leading operators)
    if v.contains("||") || v.contains(' ') {
        return "latest".to_string();
    }

    // Wildcard
    if v == "*" {
        return "latest".to_string();
    }

    // Strip leading range operators: >=, <=, ~, ^, >, <
    let stripped = v.trim_start_matches(['^', '~', '>', '<', '=']);
    stripped.to_string()
}

/// Parse a `package.json` file and return all dependencies as [`DetectedDep`] entries.
///
/// `dependencies` → `is_dev = false`
/// `devDependencies` → `is_dev = true`
pub fn parse_package_json(path: &Path) -> Result<Vec<DetectedDep>> {
    let content = std::fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let value: serde_json::Value = serde_json::from_str(&content)
        .with_context(|| format!("failed to parse JSON in {}", path.display()))?;

    let mut deps = Vec::new();

    for (is_dev, key) in [(false, "dependencies"), (true, "devDependencies")] {
        if let Some(obj) = value.get(key).and_then(|v| v.as_object()) {
            for (name, ver_val) in obj {
                let raw_version = ver_val.as_str().unwrap_or("latest");
                let version = strip_version_range(raw_version);
                deps.push(DetectedDep::new(
                    name.clone(),
                    version,
                    Ecosystem::Npm,
                    is_dev,
                ));
            }
        }
    }

    Ok(deps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;

    fn write_temp_json(content: &str) -> tempfile::NamedTempFile {
        let mut f = tempfile::NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    // --- strip_version_range ---

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

    // --- parse_package_json ---

    #[test]
    fn parse_basic_package_json() {
        let json = r#"{
            "name": "my-app",
            "dependencies": {
                "react": "^19.1.0",
                "lodash": "~4.17.0"
            },
            "devDependencies": {
                "jest": "^29.0.0"
            }
        }"#;

        let f = write_temp_json(json);
        let deps = parse_package_json(f.path()).unwrap();

        let react = deps.iter().find(|d| d.name == "react").unwrap();
        assert_eq!(react.version, "19.1.0");
        assert!(!react.is_dev);
        assert_eq!(react.source_spec, "npm:react@19.1.0");

        let lodash = deps.iter().find(|d| d.name == "lodash").unwrap();
        assert_eq!(lodash.version, "4.17.0");
        assert!(!lodash.is_dev);

        let jest = deps.iter().find(|d| d.name == "jest").unwrap();
        assert_eq!(jest.version, "29.0.0");
        assert!(jest.is_dev);
        assert_eq!(jest.source_spec, "npm:jest@29.0.0");
    }

    #[test]
    fn parse_scoped_packages() {
        let json = r#"{
            "name": "my-app",
            "dependencies": {
                "@babel/core": "^7.22.0"
            },
            "devDependencies": {
                "@types/node": "^20.0.0"
            }
        }"#;

        let f = write_temp_json(json);
        let deps = parse_package_json(f.path()).unwrap();

        let babel = deps.iter().find(|d| d.name == "@babel/core").unwrap();
        assert_eq!(babel.version, "7.22.0");
        assert!(!babel.is_dev);
        assert_eq!(babel.source_spec, "npm:@babel/core@7.22.0");

        let types_node = deps.iter().find(|d| d.name == "@types/node").unwrap();
        assert_eq!(types_node.version, "20.0.0");
        assert!(types_node.is_dev);
        assert_eq!(types_node.source_spec, "npm:@types/node@20.0.0");
    }

    #[test]
    fn parse_empty_deps() {
        let json = r#"{ "name": "my-app" }"#;
        let f = write_temp_json(json);
        let deps = parse_package_json(f.path()).unwrap();
        assert!(deps.is_empty());
    }
}
