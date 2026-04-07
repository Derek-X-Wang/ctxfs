//! Shared HTTP client and helpers for registry API calls.

use ctxfs_core::error::CtxfsError;
use reqwest::header::HeaderMap;
use serde_json::Value;

const USER_AGENT: &str = "ctxfs/0.1";

/// Default retry-after seconds when a 429 response has no Retry-After header.
pub const DEFAULT_RETRY_AFTER_SECS: u64 = 60;

/// Build a reqwest client with standard headers for registry API calls.
pub fn registry_client() -> reqwest::Client {
    let headers = HeaderMap::new();
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .build()
        .expect("failed to build HTTP client")
}

/// Fetch JSON from a registry URL with standard error handling.
///
/// - 404 → `CtxfsError::NotFound(not_found_label)`
/// - 429 → `CtxfsError::RateLimited` (parses Retry-After header if present)
/// - Other non-success → `CtxfsError::Provider`
pub async fn fetch_registry_json(
    client: &reqwest::Client,
    url: &str,
    registry_name: &str,
    not_found_label: &str,
) -> Result<Value, CtxfsError> {
    let resp = client
        .get(url)
        .send()
        .await
        .map_err(|e| CtxfsError::Provider(format!("{registry_name} request failed: {e}")))?;

    let status = resp.status();
    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(CtxfsError::NotFound(not_found_label.to_string()));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = resp
            .headers()
            .get("retry-after")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.parse::<u64>().ok())
            .unwrap_or(DEFAULT_RETRY_AFTER_SECS);
        return Err(CtxfsError::RateLimited {
            retry_after_secs: retry_after,
        });
    }
    if !status.is_success() {
        return Err(CtxfsError::Provider(format!(
            "{registry_name} returned {status}"
        )));
    }

    resp.json::<Value>()
        .await
        .map_err(|e| CtxfsError::Provider(format!("failed to parse {registry_name} response: {e}")))
}
