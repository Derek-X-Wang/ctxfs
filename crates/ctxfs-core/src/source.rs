use serde::{Deserialize, Serialize};
use std::fmt;

use crate::error::CtxfsError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderType {
    GitHub,
}

impl fmt::Display for ProviderType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ProviderType::GitHub => write!(f, "github"),
        }
    }
}

impl std::str::FromStr for ProviderType {
    type Err = CtxfsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "github" => Ok(ProviderType::GitHub),
            other => Err(CtxfsError::InvalidSource(format!(
                "unsupported provider '{other}', expected 'github'"
            ))),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceSpec {
    pub provider_type: ProviderType,
    pub owner: String,
    pub repo: String,
    pub git_ref: String,
    pub subpath: Option<String>,
}

impl SourceSpec {
    /// Parse a source string like `github:owner/repo@ref` or `github:owner/repo@ref:subpath`
    pub fn parse(s: &str) -> Result<Self, CtxfsError> {
        let (provider_str, rest) = s.split_once(':').ok_or_else(|| {
            CtxfsError::InvalidSource(format!("missing provider prefix in '{s}'"))
        })?;

        let provider_type: ProviderType = provider_str.parse()?;

        // Split off optional subpath (after second ':')
        let (repo_ref, subpath) = match rest.split_once(':') {
            Some((rr, sp)) => (rr, Some(sp.to_string())),
            None => (rest, None),
        };

        let (owner_repo, git_ref) = repo_ref
            .split_once('@')
            .ok_or_else(|| CtxfsError::InvalidSource(format!("missing @ref in '{s}'")))?;

        let (owner, repo) = owner_repo
            .split_once('/')
            .ok_or_else(|| CtxfsError::InvalidSource(format!("missing owner/repo in '{s}'")))?;

        if owner.is_empty() || repo.is_empty() || git_ref.is_empty() {
            return Err(CtxfsError::InvalidSource(format!(
                "empty owner, repo, or ref in '{s}'"
            )));
        }

        Ok(Self {
            provider_type,
            owner: owner.to_string(),
            repo: repo.to_string(),
            git_ref: git_ref.to_string(),
            subpath,
        })
    }

    /// A stable identifier for this source (used as mount id prefix).
    pub fn id(&self) -> String {
        format!("{}_{}_{}", self.owner, self.repo, self.git_ref)
    }
}

impl fmt::Display for SourceSpec {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{}:{}/{}@{}",
            self.provider_type, self.owner, self.repo, self.git_ref
        )?;
        if let Some(ref sp) = self.subpath {
            write!(f, ":{sp}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_basic() {
        let s = SourceSpec::parse("github:octocat/Hello-World@master").unwrap();
        assert_eq!(s.provider_type, ProviderType::GitHub);
        assert_eq!(s.owner, "octocat");
        assert_eq!(s.repo, "Hello-World");
        assert_eq!(s.git_ref, "master");
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
        assert_eq!(s.git_ref, "abc123def456");
    }

    #[test]
    fn parse_tag_ref() {
        let s = SourceSpec::parse("github:owner/repo@v2.1.0").unwrap();
        assert_eq!(s.git_ref, "v2.1.0");
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
}
