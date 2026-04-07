//! crates.io registry resolver — resolves Rust crate specs to GitHub source repos.

use async_trait::async_trait;
use ctxfs_core::error::CtxfsError;
use ctxfs_provider_common::{
    repo_url::parse_github_url,
    resolver::{RegistryResolver, ResolvedSource},
};
use serde_json::Value;

/// Resolver that queries the crates.io API to map crates to GitHub source repos.
#[derive(Debug)]
pub struct CrateResolver {
    client: reqwest::Client,
}

impl CrateResolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: ctxfs_provider_common::http::registry_client(),
        }
    }

    async fn fetch_crate_metadata(&self, name: &str) -> Result<Value, CtxfsError> {
        let url = format!("https://crates.io/api/v1/crates/{name}");

        tracing::debug!(name, %url, "fetching crates.io metadata");

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| CtxfsError::Provider(format!("crates.io request failed: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CtxfsError::NotFound(format!("crate:{name}")));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(CtxfsError::RateLimited {
                retry_after_secs: 60,
            });
        }
        if !status.is_success() {
            return Err(CtxfsError::Provider(format!("crates.io returned {status}")));
        }

        resp.json::<Value>()
            .await
            .map_err(|e| CtxfsError::Provider(format!("failed to parse crates.io response: {e}")))
    }
}

impl Default for CrateResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RegistryResolver for CrateResolver {
    async fn resolve(&self, name: &str, version: &str) -> Result<ResolvedSource, CtxfsError> {
        let json = self.fetch_crate_metadata(name).await?;

        let (owner, repo) = extract_repo_url(&json).ok_or_else(|| CtxfsError::NoSourceRepo {
            package: format!("{name}@{version}"),
            registry: "crates.io".into(),
        })?;

        Ok(ResolvedSource {
            owner,
            repo,
            git_ref: format!("v{version}"),
            subpath: None,
        })
    }

    async fn resolve_latest(&self, name: &str) -> Result<String, CtxfsError> {
        let json = self.fetch_crate_metadata(name).await?;

        extract_latest_version(&json).ok_or_else(|| {
            CtxfsError::Provider(format!(
                "crates.io response for {name} missing version fields"
            ))
        })
    }
}

/// Extract `(owner, repo)` from the `crate.repository` field in crates.io metadata.
fn extract_repo_url(json: &Value) -> Option<(String, String)> {
    let repo_url = json.get("crate")?.get("repository")?.as_str()?;
    parse_github_url(repo_url)
}

/// Extract the latest version from crates.io metadata.
///
/// Prefers `crate.max_stable_version`; falls back to `crate.max_version`
/// for pre-release-only crates.
fn extract_latest_version(json: &Value) -> Option<String> {
    let krate = json.get("crate")?;

    // Try max_stable_version first — skip if null or empty string
    if let Some(stable) = krate.get("max_stable_version").and_then(Value::as_str) {
        if !stable.is_empty() {
            return Some(stable.to_string());
        }
    }

    krate
        .get("max_version")
        .and_then(Value::as_str)
        .map(String::from)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_repo_from_crate_metadata() {
        let json = serde_json::json!({
            "crate": {
                "repository": "https://github.com/dtolnay/itoa"
            }
        });
        assert_eq!(
            extract_repo_url(&json),
            Some(("dtolnay".into(), "itoa".into()))
        );
    }

    #[test]
    fn extract_repo_with_git_suffix() {
        let json = serde_json::json!({
            "crate": {
                "repository": "https://github.com/serde-rs/serde.git"
            }
        });
        assert_eq!(
            extract_repo_url(&json),
            Some(("serde-rs".into(), "serde".into()))
        );
    }

    #[test]
    fn extract_repo_missing() {
        let json = serde_json::json!({
            "crate": { "name": "some-crate" }
        });
        assert_eq!(extract_repo_url(&json), None);
    }

    #[test]
    fn extract_repo_non_github() {
        let json = serde_json::json!({
            "crate": {
                "repository": "https://gitlab.com/owner/repo"
            }
        });
        assert_eq!(extract_repo_url(&json), None);
    }

    #[test]
    fn extract_latest_stable() {
        let json = serde_json::json!({
            "crate": {
                "max_stable_version": "1.0.11",
                "max_version": "2.0.0-alpha"
            }
        });
        assert_eq!(extract_latest_version(&json), Some("1.0.11".into()));
    }

    #[test]
    fn extract_latest_prerelease_only() {
        let json = serde_json::json!({
            "crate": {
                "max_stable_version": null,
                "max_version": "0.1.0-beta.1"
            }
        });
        assert_eq!(extract_latest_version(&json), Some("0.1.0-beta.1".into()));
    }

    #[test]
    fn extract_latest_empty_stable() {
        let json = serde_json::json!({
            "crate": {
                "max_stable_version": "",
                "max_version": "0.1.0-beta.1"
            }
        });
        assert_eq!(extract_latest_version(&json), Some("0.1.0-beta.1".into()));
    }
}
