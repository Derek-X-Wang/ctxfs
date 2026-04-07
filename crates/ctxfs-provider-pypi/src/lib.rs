//! `PyPI` registry resolver — resolves `PyPI` package specs to GitHub source repos.

use async_trait::async_trait;
use ctxfs_core::error::CtxfsError;
use ctxfs_provider_common::{
    repo_url::parse_github_url,
    resolver::{RegistryResolver, ResolvedSource},
};
use serde_json::Value;

/// Keys to search in `info.project_urls`, checked case-insensitively in priority order.
const PROJECT_URL_KEYS: &[&str] = &[
    "source code",
    "source",
    "github",
    "repository",
    "code",
    "homepage",
];

/// Resolver that queries the `PyPI` JSON API to map packages to GitHub source repos.
#[derive(Debug)]
pub struct PyPIResolver {
    client: reqwest::Client,
}

impl PyPIResolver {
    #[must_use]
    pub fn new() -> Self {
        Self {
            client: ctxfs_provider_common::http::registry_client(),
        }
    }

    async fn fetch_metadata(&self, name: &str, version: Option<&str>) -> Result<Value, CtxfsError> {
        let url = match version {
            Some(v) => format!("https://pypi.org/pypi/{name}/{v}/json"),
            None => format!("https://pypi.org/pypi/{name}/json"),
        };
        let label = match version {
            Some(v) => format!("pypi:{name}@{v}"),
            None => format!("pypi:{name}"),
        };
        tracing::debug!(name, version, %url, "fetching PyPI registry metadata");
        ctxfs_provider_common::http::fetch_registry_json(&self.client, &url, "PyPI", &label).await
    }
}

impl Default for PyPIResolver {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl RegistryResolver for PyPIResolver {
    async fn resolve(&self, name: &str, version: &str) -> Result<ResolvedSource, CtxfsError> {
        let json = self.fetch_metadata(name, Some(version)).await?;

        let (owner, repo) = extract_repo_url(&json).ok_or_else(|| CtxfsError::NoSourceRepo {
            package: format!("{name}@{version}"),
            registry: "pypi".into(),
        })?;

        Ok(ResolvedSource {
            owner,
            repo,
            git_ref: format!("v{version}"),
            subpath: None,
        })
    }

    async fn resolve_latest(&self, name: &str) -> Result<String, CtxfsError> {
        let json = self.fetch_metadata(name, None).await?;

        json["info"]["version"]
            .as_str()
            .map(String::from)
            .ok_or_else(|| {
                CtxfsError::Provider(format!(
                    "PyPI response for {name} missing 'info.version' field"
                ))
            })
    }
}

/// Extract `(owner, repo)` from `PyPI` metadata by searching `info.project_urls`
/// case-insensitively in priority order, then falling back to `info.home_page`.
pub fn extract_repo_url(json: &Value) -> Option<(String, String)> {
    let info = json.get("info")?;

    // Build a lowercase key map once, then check priority keys in order.
    if let Some(project_urls) = info.get("project_urls").and_then(Value::as_object) {
        let lower_map: std::collections::HashMap<String, &str> = project_urls
            .iter()
            .filter_map(|(k, v)| v.as_str().map(|url| (k.to_lowercase(), url)))
            .collect();

        for &key in PROJECT_URL_KEYS {
            if let Some(&url) = lower_map.get(key) {
                if let Some(result) = parse_github_url(url) {
                    return Some(result);
                }
            }
        }
    }

    // Fallback to home_page.
    if let Some(home_page) = info.get("home_page").and_then(Value::as_str) {
        return parse_github_url(home_page);
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extract_from_project_urls_source_code() {
        let json = serde_json::json!({
            "info": {
                "project_urls": {
                    "Source Code": "https://github.com/psf/requests"
                }
            }
        });
        assert_eq!(
            extract_repo_url(&json),
            Some(("psf".into(), "requests".into()))
        );
    }

    #[test]
    fn extract_from_project_urls_github_key() {
        let json = serde_json::json!({
            "info": {
                "project_urls": {
                    "GitHub": "https://github.com/owner/repo"
                }
            }
        });
        assert_eq!(
            extract_repo_url(&json),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn extract_from_project_urls_case_insensitive() {
        let json = serde_json::json!({
            "info": {
                "project_urls": {
                    "source code": "https://github.com/owner/repo"
                }
            }
        });
        assert_eq!(
            extract_repo_url(&json),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn extract_from_home_page_fallback() {
        let json = serde_json::json!({
            "info": {
                "project_urls": null,
                "home_page": "https://github.com/owner/repo"
            }
        });
        assert_eq!(
            extract_repo_url(&json),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn extract_no_repo_found() {
        let json = serde_json::json!({
            "info": {
                "project_urls": {
                    "Documentation": "https://docs.example.com"
                },
                "home_page": "https://example.com"
            }
        });
        assert_eq!(extract_repo_url(&json), None);
    }

    #[test]
    fn extract_missing_project_urls() {
        let json = serde_json::json!({
            "info": { "home_page": null }
        });
        assert_eq!(extract_repo_url(&json), None);
    }

    #[test]
    fn extract_prefers_source_over_homepage() {
        // "Source" has higher priority than "Homepage" in PROJECT_URL_KEYS
        let json = serde_json::json!({
            "info": {
                "project_urls": {
                    "Homepage": "https://github.com/wrong/repo",
                    "Source": "https://github.com/correct/repo"
                }
            }
        });
        let result = extract_repo_url(&json);
        assert_eq!(result, Some(("correct".into(), "repo".into())));
    }

    #[test]
    fn extract_project_urls_empty_object() {
        let json = serde_json::json!({
            "info": {
                "project_urls": {},
                "home_page": "https://github.com/owner/repo"
            }
        });
        assert_eq!(
            extract_repo_url(&json),
            Some(("owner".into(), "repo".into()))
        );
    }
}
