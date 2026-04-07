use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::CtxfsError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    GitHub,
    Npm,
    PyPI,
    Crate,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderType::GitHub => write!(f, "github"),
            ProviderType::Npm => write!(f, "npm"),
            ProviderType::PyPI => write!(f, "pypi"),
            ProviderType::Crate => write!(f, "crate"),
        }
    }
}

impl std::str::FromStr for ProviderType {
    type Err = CtxfsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github" => Ok(ProviderType::GitHub),
            "npm" => Ok(ProviderType::Npm),
            "pypi" => Ok(ProviderType::PyPI),
            "crate" => Ok(ProviderType::Crate),
            other => Err(CtxfsError::InvalidSource(format!(
                "unsupported provider '{other}', expected 'github', 'npm', 'pypi', or 'crate'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpec {
    pub provider_type: ProviderType,
    pub name: String,
    pub version: String,
    pub subpath: Option<String>,
}

impl SourceSpec {
    /// Parse a source string.
    ///
    /// GitHub: `github:owner/repo@ref` or `github:owner/repo@ref:subpath`
    /// Registries: `npm:react@19.1.0`, `npm:@babel/core@7.24.0`,
    ///             `pypi:requests@2.31.0`, `crate:serde@1.0.0`
    pub fn parse(s: &str) -> Result<Self, CtxfsError> {
        let (provider_str, rest) = s.split_once(':').ok_or_else(|| {
            CtxfsError::InvalidSource(format!("missing provider prefix in '{s}'"))
        })?;

        let provider_type: ProviderType = provider_str.parse()?;

        match provider_type {
            ProviderType::GitHub => Self::parse_github(provider_type, rest, s),
            ProviderType::Npm | ProviderType::PyPI | ProviderType::Crate => {
                Self::parse_registry(provider_type, rest, s)
            }
        }
    }

    fn parse_github(
        provider_type: ProviderType,
        rest: &str,
        original: &str,
    ) -> Result<Self, CtxfsError> {
        // Split off optional subpath (after second ':')
        let (repo_ref, subpath) = match rest.split_once(':') {
            Some((rr, sp)) => (rr, Some(sp.to_string())),
            None => (rest, None),
        };

        let (owner_repo, git_ref) = repo_ref
            .split_once('@')
            .ok_or_else(|| CtxfsError::InvalidSource(format!("missing @ref in '{original}'")))?;

        let (owner, repo) = owner_repo.split_once('/').ok_or_else(|| {
            CtxfsError::InvalidSource(format!("missing owner/repo in '{original}'"))
        })?;

        if owner.is_empty() || repo.is_empty() || git_ref.is_empty() {
            return Err(CtxfsError::InvalidSource(format!(
                "empty owner, repo, or ref in '{original}'"
            )));
        }

        Ok(Self {
            provider_type,
            name: format!("{owner}/{repo}"),
            version: git_ref.to_string(),
            subpath,
        })
    }

    fn parse_registry(
        provider_type: ProviderType,
        rest: &str,
        original: &str,
    ) -> Result<Self, CtxfsError> {
        // Split off optional subpath (after second ':' but only if not part of scoped name)
        // For registries, subpath is after `:` following the version
        // e.g., npm:@babel/core@7.24.0:dist/index.js
        // We split on last '@' first to get name+version, then check for subpath in version part.

        // Split on last '@' to handle scoped packages like @babel/core@7.24.0
        let at_pos = rest.rfind('@').ok_or_else(|| {
            CtxfsError::InvalidSource(format!("missing @version in '{original}'"))
        })?;

        let name = &rest[..at_pos];
        let version_and_subpath = &rest[at_pos + 1..];

        if name.is_empty() {
            return Err(CtxfsError::InvalidSource(format!(
                "empty package name in '{original}'"
            )));
        }

        // Split version from optional subpath
        let (version, subpath) = match version_and_subpath.split_once(':') {
            Some((v, sp)) => (v, Some(sp.to_string())),
            None => (version_and_subpath, None),
        };

        if version.is_empty() {
            return Err(CtxfsError::InvalidSource(format!(
                "empty version in '{original}'"
            )));
        }

        Ok(Self {
            provider_type,
            name: name.to_string(),
            version: version.to_string(),
            subpath,
        })
    }

    /// A stable identifier for this source (used as mount id prefix).
    /// Sanitizes special characters: `/` → `_`, `@` → `_at_`.
    pub fn id(&self) -> String {
        let name_sanitized = self.name.replace('@', "_at_").replace('/', "_");
        format!("{}_{}_{}", self.provider_type, name_sanitized, self.version)
    }
}

impl fmt::Display for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}@{}", self.provider_type, self.name, self.version)?;
        if let Some(ref sp) = self.subpath {
            write!(f, ":{sp}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- Existing tests, updated for new field names ---

    #[test]
    fn parse_basic() {
        let s = SourceSpec::parse("github:octocat/Hello-World@master").unwrap();
        assert_eq!(s.provider_type, ProviderType::GitHub);
        assert_eq!(s.name, "octocat/Hello-World");
        assert_eq!(s.version, "master");
        assert_eq!(s.subpath, None);
    }

    #[test]
    fn parse_with_subpath() {
        let s = SourceSpec::parse("github:octocat/Hello-World@main:src/lib").unwrap();
        assert_eq!(s.subpath, Some("src/lib".to_string()));
    }

    #[test]
    fn parse_errors() {
        assert!(SourceSpec::parse("octocat/Hello-World").is_err());
        assert!(SourceSpec::parse("github:octocat/Hello-World").is_err());
        assert!(SourceSpec::parse("gitlab:octocat/Hello-World@main").is_err());
        assert!(SourceSpec::parse("github:/Hello-World@main").is_err());
    }

    #[test]
    fn display_roundtrip() {
        let s = SourceSpec::parse("github:octocat/Hello-World@master").unwrap();
        assert_eq!(s.to_string(), "github:octocat/Hello-World@master");
    }

    #[test]
    fn display_with_subpath() {
        let s = SourceSpec::parse("github:owner/repo@v1.0:src/main").unwrap();
        assert_eq!(s.to_string(), "github:owner/repo@v1.0:src/main");
    }

    #[test]
    fn parse_sha_ref() {
        let s = SourceSpec::parse("github:owner/repo@abc123def456").unwrap();
        assert_eq!(s.version, "abc123def456");
    }

    #[test]
    fn parse_tag_ref() {
        let s = SourceSpec::parse("github:owner/repo@v2.1.0").unwrap();
        assert_eq!(s.version, "v2.1.0");
    }

    #[test]
    fn parse_empty_repo_fails() {
        assert!(SourceSpec::parse("github:owner/@main").is_err());
    }

    #[test]
    fn parse_empty_owner_fails() {
        assert!(SourceSpec::parse("github:/repo@main").is_err());
    }

    #[test]
    fn parse_empty_ref_fails() {
        assert!(SourceSpec::parse("github:owner/repo@").is_err());
    }

    #[test]
    fn parse_no_slash_fails() {
        assert!(SourceSpec::parse("github:ownerrepo@main").is_err());
    }

    #[test]
    fn id_is_stable() {
        let s1 = SourceSpec::parse("github:octocat/Hello-World@main").unwrap();
        let s2 = SourceSpec::parse("github:octocat/Hello-World@main").unwrap();
        assert_eq!(s1.id(), s2.id());
        assert!(s1.id().contains("octocat"));
        assert!(s1.id().contains("Hello-World"));
    }

    #[test]
    fn serde_roundtrip() {
        let s = SourceSpec::parse("github:owner/repo@main:src/lib").unwrap();
        let json = serde_json::to_string(&s).unwrap();
        let s2: SourceSpec = serde_json::from_str(&json).unwrap();
        assert_eq!(s, s2);
    }

    #[test]
    fn error_messages_include_input() {
        let err = SourceSpec::parse("bad").unwrap_err();
        assert!(err.to_string().contains("bad"));
    }

    // --- New tests for npm/PyPI/crate parsing ---

    #[test]
    fn parse_npm_basic() {
        let s = SourceSpec::parse("npm:react@19.1.0").unwrap();
        assert_eq!(s.provider_type, ProviderType::Npm);
        assert_eq!(s.name, "react");
        assert_eq!(s.version, "19.1.0");
        assert_eq!(s.subpath, None);
    }

    #[test]
    fn parse_npm_scoped() {
        let s = SourceSpec::parse("npm:@babel/core@7.24.0").unwrap();
        assert_eq!(s.provider_type, ProviderType::Npm);
        assert_eq!(s.name, "@babel/core");
        assert_eq!(s.version, "7.24.0");
        assert_eq!(s.subpath, None);
    }

    #[test]
    fn parse_pypi() {
        let s = SourceSpec::parse("pypi:requests@2.31.0").unwrap();
        assert_eq!(s.provider_type, ProviderType::PyPI);
        assert_eq!(s.name, "requests");
        assert_eq!(s.version, "2.31.0");
    }

    #[test]
    fn parse_crate() {
        let s = SourceSpec::parse("crate:serde@1.0.0").unwrap();
        assert_eq!(s.provider_type, ProviderType::Crate);
        assert_eq!(s.name, "serde");
        assert_eq!(s.version, "1.0.0");
    }

    #[test]
    fn parse_npm_no_version_fails() {
        assert!(SourceSpec::parse("npm:react").is_err());
    }

    #[test]
    fn parse_npm_empty_version_fails() {
        assert!(SourceSpec::parse("npm:react@").is_err());
    }

    #[test]
    fn id_sanitizes_special_chars() {
        let s = SourceSpec::parse("npm:@babel/core@7.24.0").unwrap();
        let id = s.id();
        assert!(!id.contains('/'), "id should not contain /: {id}");
        assert!(!id.contains('@'), "id should not contain @: {id}");
    }

    #[test]
    fn display_npm() {
        let s = SourceSpec::parse("npm:react@19.1.0").unwrap();
        assert_eq!(s.to_string(), "npm:react@19.1.0");
    }

    #[test]
    fn display_npm_scoped() {
        let s = SourceSpec::parse("npm:@babel/core@7.24.0").unwrap();
        assert_eq!(s.to_string(), "npm:@babel/core@7.24.0");
    }

    #[test]
    fn id_github_format() {
        let s = SourceSpec::parse("github:octocat/Hello-World@main").unwrap();
        assert_eq!(s.id(), "github_octocat_Hello-World_main");
    }

    #[test]
    fn id_npm_format() {
        let s = SourceSpec::parse("npm:react@19.1.0").unwrap();
        assert_eq!(s.id(), "npm_react_19.1.0");
    }

    #[test]
    fn id_npm_scoped_format() {
        let s = SourceSpec::parse("npm:@babel/core@7.24.0").unwrap();
        assert_eq!(s.id(), "npm__at_babel_core_7.24.0");
    }

    // --- NoSourceRepo error test ---

    #[test]
    fn no_source_repo_error() {
        let e = CtxfsError::NoSourceRepo {
            package: "requests".into(),
            registry: "pypi".into(),
        };
        let msg = e.to_string();
        assert!(msg.contains("requests"));
        assert!(msg.contains("pypi"));
        assert!(msg.contains("no source repository found"));
    }
}
