use super::{DetectedDep, Ecosystem};
use anyhow::{Context, Result};
use std::fs;
use std::path::Path;

const DEV_EXTRA_NAMES: &[&str] = &["dev", "test", "testing"];

/// Version operators used in PEP 508 dependency specifiers, ordered longest-first
/// so that multi-char operators like ">=" are matched before ">".
const VERSION_OPS: &[&str] = &[">=", "<=", "~=", "==", "!=", "<", ">"];

/// Parse a PEP 508 dependency string and return `(name, version)`.
///
/// Examples:
/// - `"requests>=2.31.0"` -> `("requests", "2.31.0")`
/// - `"numpy"` -> `("numpy", "latest")`
/// - `"requests[security]>=2.31.0"` -> `("requests", "2.31.0")`
/// - `"Django>=3.2,<4.0"` -> `("Django", "3.2")`
fn parse_pep508(spec: &str) -> (String, String) {
    let spec = spec.trim();

    // Find the first version operator.
    let op_pos = VERSION_OPS
        .iter()
        .filter_map(|op| spec.find(op).map(|pos| (pos, *op)))
        .min_by_key(|&(pos, _)| pos);

    if let Some((op_pos, op)) = op_pos {
        let raw_name = &spec[..op_pos];
        let version_part = &spec[op_pos + op.len()..];

        // Strip [extras] from name and trim.
        let name = strip_extras(raw_name).trim().to_string();

        // Take version up to the first comma or semicolon (environment markers).
        let version = version_part
            .split([',', ';'])
            .next()
            .unwrap_or(version_part)
            .trim()
            .to_string();

        let version = if version.is_empty() {
            "latest".to_string()
        } else {
            version
        };

        (name, version)
    } else {
        // Bare name, possibly with [extras] or ; markers.
        let without_markers = spec.split(';').next().unwrap_or(spec);
        let name = strip_extras(without_markers).trim().to_string();
        (name, "latest".to_string())
    }
}

/// Remove `[extras]` bracket section from a package name like `"requests[security]"`.
fn strip_extras(name: &str) -> &str {
    if let Some(bracket) = name.find('[') {
        &name[..bracket]
    } else {
        name
    }
}

/// Parse a `requirements.txt` file and return the detected dependencies.
///
/// Lines that are blank, start with `#`, or are flags (`-r`, `-e`, `--index-url`, etc.)
/// are ignored. All dependencies are treated as production (`is_dev = false`).
pub fn parse_requirements_txt(path: &Path) -> Result<Vec<DetectedDep>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let mut deps = Vec::new();

    for line in content.lines() {
        let line = line.trim();

        // Skip blank lines and comments.
        if line.is_empty() || line.starts_with('#') {
            continue;
        }

        // Skip option flags: -r, -e, --index-url, --extra-index-url, etc.
        if line.starts_with('-') {
            continue;
        }

        // Strip inline comments.
        let line = line.split('#').next().unwrap_or(line).trim();
        if line.is_empty() {
            continue;
        }

        let (name, version) = parse_pep508(line);
        if name.is_empty() {
            continue;
        }

        deps.push(DetectedDep::new(name, version, Ecosystem::PyPI, false));
    }

    Ok(deps)
}

/// Parse a `pyproject.toml` file and return the detected dependencies.
///
/// Reads `[project.dependencies]` as production deps and
/// `[project.optional-dependencies]` as optional deps, marking extras named
/// `dev`, `test`, or `testing` as `is_dev = true`.
pub fn parse_pyproject_toml(path: &Path) -> Result<Vec<DetectedDep>> {
    let content = fs::read_to_string(path)
        .with_context(|| format!("failed to read {}", path.display()))?;

    let doc: toml::Value = content
        .parse()
        .with_context(|| format!("failed to parse TOML in {}", path.display()))?;

    let mut deps = Vec::new();

    // [project.dependencies] -> production deps.
    if let Some(project_deps) = doc
        .get("project")
        .and_then(|p| p.get("dependencies"))
        .and_then(toml::Value::as_array)
    {
        for item in project_deps {
            if let Some(spec) = item.as_str() {
                let (name, version) = parse_pep508(spec);
                if !name.is_empty() {
                    deps.push(DetectedDep::new(name, version, Ecosystem::PyPI, false));
                }
            }
        }
    }

    // [project.optional-dependencies] -> optional deps, some may be dev.
    if let Some(optional) = doc
        .get("project")
        .and_then(|p| p.get("optional-dependencies"))
        .and_then(toml::Value::as_table)
    {
        for (extra_name, extra_deps) in optional {
            let is_dev = DEV_EXTRA_NAMES
                .iter()
                .any(|&n| extra_name.eq_ignore_ascii_case(n));

            if let Some(items) = extra_deps.as_array() {
                for item in items {
                    if let Some(spec) = item.as_str() {
                        let (name, version) = parse_pep508(spec);
                        if !name.is_empty() {
                            deps.push(DetectedDep::new(
                                name,
                                version,
                                Ecosystem::PyPI,
                                is_dev,
                            ));
                        }
                    }
                }
            }
        }
    }

    Ok(deps)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write as _;
    use tempfile::NamedTempFile;

    fn write_temp(content: &str) -> NamedTempFile {
        let mut f = NamedTempFile::new().unwrap();
        f.write_all(content.as_bytes()).unwrap();
        f
    }

    // ---- PEP 508 helper tests ----

    #[test]
    fn pep508_bare_name() {
        let (name, version) = parse_pep508("numpy");
        assert_eq!(name, "numpy");
        assert_eq!(version, "latest");
    }

    #[test]
    fn pep508_with_extras() {
        let (name, version) = parse_pep508("requests[security]>=2.31.0");
        assert_eq!(name, "requests");
        assert_eq!(version, "2.31.0");
    }

    // ---- requirements.txt tests ----

    #[test]
    fn parse_requirements_pinned() {
        let f = write_temp("requests==2.31.0\nflask==3.0.0\n");
        let deps = parse_requirements_txt(f.path()).unwrap();
        assert_eq!(deps.len(), 2);
        let req = deps.iter().find(|d| d.name == "requests").unwrap();
        assert_eq!(req.version, "2.31.0");
        let flask = deps.iter().find(|d| d.name == "flask").unwrap();
        assert_eq!(flask.version, "3.0.0");
    }

    #[test]
    fn parse_requirements_unpinned() {
        let f = write_temp("requests\n");
        let deps = parse_requirements_txt(f.path()).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].name, "requests");
        assert_eq!(deps[0].version, "latest");
    }

    #[test]
    fn parse_requirements_skips_comments_and_flags() {
        let content = "# this is a comment\n\
                        requests==2.31.0\n\
                        -r other-requirements.txt\n\
                        --index-url https://pypi.org/simple\n\
                        flask==3.0.0\n";
        let f = write_temp(content);
        let deps = parse_requirements_txt(f.path()).unwrap();
        assert_eq!(deps.len(), 2);
        assert!(deps.iter().any(|d| d.name == "requests"));
        assert!(deps.iter().any(|d| d.name == "flask"));
    }

    #[test]
    fn parse_requirements_gte() {
        let f = write_temp("requests>=2.31.0,<3.0\n");
        let deps = parse_requirements_txt(f.path()).unwrap();
        assert_eq!(deps.len(), 1);
        assert_eq!(deps[0].version, "2.31.0");
    }

    // ---- pyproject.toml tests ----

    #[test]
    fn parse_pyproject_basic() {
        let content = r#"
[project]
name = "myapp"
dependencies = [
    "requests>=2.31.0",
    "flask==3.0.0",
]

[project.optional-dependencies]
dev = ["pytest>=7.0", "black"]
docs = ["sphinx>=5.0"]
"#;
        let f = write_temp(content);
        let deps = parse_pyproject_toml(f.path()).unwrap();

        let req = deps.iter().find(|d| d.name == "requests").unwrap();
        assert_eq!(req.version, "2.31.0");
        assert!(!req.is_dev);

        let flask = deps.iter().find(|d| d.name == "flask").unwrap();
        assert_eq!(flask.version, "3.0.0");
        assert!(!flask.is_dev);

        let pytest = deps.iter().find(|d| d.name == "pytest").unwrap();
        assert!(pytest.is_dev);

        let black = deps.iter().find(|d| d.name == "black").unwrap();
        assert!(black.is_dev);

        let sphinx = deps.iter().find(|d| d.name == "sphinx").unwrap();
        assert!(!sphinx.is_dev, "docs extra should not be is_dev");
    }

    #[test]
    fn parse_pyproject_test_extra_is_dev() {
        let content = r#"
[project]
name = "myapp"
dependencies = []

[project.optional-dependencies]
test = ["pytest>=7.0"]
testing = ["coverage>=6.0"]
"#;
        let f = write_temp(content);
        let deps = parse_pyproject_toml(f.path()).unwrap();

        let pytest = deps.iter().find(|d| d.name == "pytest").unwrap();
        assert!(pytest.is_dev, "test extra should be is_dev");

        let coverage = deps.iter().find(|d| d.name == "coverage").unwrap();
        assert!(coverage.is_dev, "testing extra should be is_dev");
    }

    #[test]
    fn parse_pyproject_no_deps() {
        let content = r#"
[project]
name = "minimal"
version = "0.1.0"
"#;
        let f = write_temp(content);
        let deps = parse_pyproject_toml(f.path()).unwrap();
        assert!(deps.is_empty());
    }
}
