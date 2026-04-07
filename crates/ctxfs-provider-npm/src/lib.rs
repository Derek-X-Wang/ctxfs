//! npm registry resolver — resolves npm package specs to GitHub source repos.

use async_trait::async_trait;
use ctxfs_core::error::CtxfsError;
use ctxfs_provider_common::{
    repo_url::parse_github_url,
    resolver::{RegistryResolver, ResolvedSource},
};
use serde_json::Value;

/// Resolver that queries the npm registry to map packages to GitHub source repos.
#[derive(Debug)]
pub struct NpmResolver {
    client: reqwest::Client,
}

impl NpmResolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: ctxfs_provider_common::http::registry_client(),
        }
    }

    async fn fetch_version_metadata(&self, name: &str, version: &str) -> Result<Value, CtxfsError> {
        let encoded = encode_package_name(name);
        let url = format!("https://registry.npmjs.org/{encoded}/{version}");

        tracing::debug!(name, version, %url, "fetching npm registry metadata");

        let resp = self
            .client
            .get(&url)
            .send()
            .await
            .map_err(|e| CtxfsError::Provider(format!("npm registry request failed: {e}")))?;

        let status = resp.status();
        if status == reqwest::StatusCode::NOT_FOUND {
            return Err(CtxfsError::NotFound(format!("npm:{name}@{version}")));
        }
        if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
            return Err(CtxfsError::RateLimited {
                retry_after_secs: 60,
            });
        }
        if !status.is_success() {
            return Err(CtxfsError::Provider(format!(
                "npm registry returned {status}"
            )));
        }

        resp.json::<Value>()
            .await
            .map_err(|e| CtxfsError::Provider(format!("failed to parse npm response: {e}")))
    }
}

impl Default for NpmResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RegistryResolver for NpmResolver {
    async fn resolve(&self, name: &str, version: &str) -> Result<ResolvedSource, CtxfsError> {
        let json = self.fetch_version_metadata(name, version).await?;

        let (owner, repo, directory) =
            extract_repo_info(&json).ok_or_else(|| CtxfsError::NoSourceRepo {
                package: format!("{name}@{version}"),
                registry: "npm".into(),
            })?;

        let git_ref = extract_git_head(&json).unwrap_or_else(|| format!("v{version}"));

        Ok(ResolvedSource {
            owner,
            repo,
            git_ref,
            subpath: directory,
        })
    }

    async fn resolve_latest(&self, name: &str) -> Result<String, CtxfsError> {
        let json = self.fetch_version_metadata(name, "latest").await?;

        json["version"].as_str().map(String::from).ok_or_else(|| {
            CtxfsError::Provider(format!(
                "npm registry response for {name}/latest missing 'version' field"
            ))
        })
    }
}

/// Extract (owner, repo, directory) from npm `repository` field.
///
/// The `repository` field can be:
/// - A plain string URL: `"https://github.com/lodash/lodash.git"`
/// - An object: `{ "type": "git", "url": "...", "directory": "packages/foo" }`
fn extract_repo_info(json: &Value) -> Option<(String, String, Option<String>)> {
    let repo_field = json.get("repository")?;

    let (url, directory) = if let Some(url_str) = repo_field.as_str() {
        (url_str.to_string(), None)
    } else {
        let url_str = repo_field.get("url")?.as_str()?;
        let dir = repo_field
            .get("directory")
            .and_then(Value::as_str)
            .map(String::from);
        (url_str.to_string(), dir)
    };

    let (owner, repo) = parse_github_url(&url)?;
    Some((owner, repo, directory))
}

/// Extract `dist.gitHead` from npm version metadata.
fn extract_git_head(json: &Value) -> Option<String> {
    json.get("dist")?.get("gitHead")?.as_str().map(String::from)
}

/// Encode a package name for use in npm registry URLs.
/// Scoped packages like `@babel/core` become `@babel%2Fcore`.
fn encode_package_name(name: &str) -> String {
    if let Some(rest) = name.strip_prefix('@') {
        // Scoped package: encode the `/` between scope and name
        format!("@{}", rest.replacen('/', "%2F", 1))
    } else {
        name.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_repo_from_url_string() {
        let json = serde_json::json!({
            "repository": "https://github.com/lodash/lodash.git"
        });
        let result = extract_repo_info(&json);
        assert_eq!(result, Some(("lodash".into(), "lodash".into(), None)));
    }

    #[test]
    fn extract_repo_from_object_with_directory() {
        let json = serde_json::json!({
            "repository": {
                "type": "git",
                "url": "https://github.com/facebook/react.git",
                "directory": "packages/react-dom"
            }
        });
        let result = extract_repo_info(&json);
        assert_eq!(
            result,
            Some((
                "facebook".into(),
                "react".into(),
                Some("packages/react-dom".into())
            ))
        );
    }

    #[test]
    fn extract_repo_github_shorthand() {
        let json = serde_json::json!({
            "repository": {
                "type": "git",
                "url": "github:facebook/react"
            }
        });
        let result = extract_repo_info(&json);
        assert_eq!(result, Some(("facebook".into(), "react".into(), None)));
    }

    #[test]
    fn extract_repo_missing() {
        let json = serde_json::json!({ "name": "pkg", "version": "1.0" });
        assert_eq!(extract_repo_info(&json), None);
    }

    #[test]
    fn extract_repo_non_github() {
        let json = serde_json::json!({
            "repository": "https://gitlab.com/owner/repo"
        });
        assert_eq!(extract_repo_info(&json), None);
    }

    #[test]
    fn extract_githead_present() {
        let json = serde_json::json!({ "dist": { "gitHead": "abc123" } });
        assert_eq!(extract_git_head(&json), Some("abc123".into()));
    }

    #[test]
    fn extract_githead_absent() {
        let json = serde_json::json!({ "dist": { "tarball": "https://..." } });
        assert_eq!(extract_git_head(&json), None);
    }

    #[test]
    fn encode_scoped_package() {
        assert_eq!(encode_package_name("@babel/core"), "@babel%2Fcore");
    }

    #[test]
    fn encode_unscoped_package() {
        assert_eq!(encode_package_name("lodash"), "lodash");
    }
}
