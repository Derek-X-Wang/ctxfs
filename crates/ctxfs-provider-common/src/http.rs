//! Shared HTTP client and helpers for registry API calls.

use ctxfs_core::error::CtxfsError;
use reqwest::header::{HeaderMap, HeaderValue, AUTHORIZATION};
use serde_json::Value;

const USER_AGENT: &str = "ctxfs/0.1";

/// Default retry-after seconds when a 429 response has no Retry-After header.
pub const DEFAULT_RETRY_AFTER_SECS: u64 = 60;

/// Build an `Authorization: Bearer <token>` header value.
///
/// Returns `None` if `token` is empty or contains characters that are
/// not valid in an HTTP header value (non-ASCII or control bytes).
/// The latter is defensive — real GitHub tokens are always printable ASCII.
#[must_use]
pub fn bearer_header(token: &str) -> Option<HeaderValue> {
    if token.is_empty() {
        return None;
    }
    format!("Bearer {token}").parse().ok()
}

/// Build an `Authorization: Bearer <token>` header value from a known-valid
/// token, inserting it into `headers` under [`AUTHORIZATION`].
///
/// No-op when `token` is `None`.  Silently skips the insert if the token
/// string produces an invalid `HeaderValue` (shouldn't happen in practice
/// since GitHub tokens are printable ASCII).
pub fn insert_bearer_header(headers: &mut HeaderMap, token: Option<&str>) {
    let Some(token) = token else { return };
    if let Some(hv) = bearer_header(token) {
        let _ = headers.insert(AUTHORIZATION, hv);
    }
}

/// Build a reqwest client with standard headers for registry API calls.
pub fn registry_client() -> reqwest::Client {
    let headers = HeaderMap::new();
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .build()
        .expect("failed to build HTTP client")
}

/// Extract HTTP response headers into a `HashMap<lowercase_key, value>`.
///
/// Skips headers whose values aren't valid UTF-8. Used by both the registry
/// fetch path here and the GitHub provider's rate-limit check to avoid
/// duplicating the lowercase-and-collect dance.
#[must_use]
pub fn response_headers_map(resp: &reqwest::Response) -> std::collections::HashMap<String, String> {
    resp.headers()
        .iter()
        .filter_map(|(k, v)| {
            v.to_str()
                .ok()
                .map(|s| (k.as_str().to_lowercase(), s.to_string()))
        })
        .collect()
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
    let headers = response_headers_map(&resp);

    tracing::debug!(
        target: "ctxfs.provider.fetch",
        registry = registry_name,
        url = url,
        status = status.as_u16(),
        ratelimit_remaining = headers.get("x-ratelimit-remaining").map_or("?", String::as_str),
        "registry fetch completed"
    );

    let verdict = crate::rate_limit::ThrottleClassifier::classify(status.as_u16(), &headers);
    if let crate::rate_limit::RateLimitVerdict::SecondaryThrottle {
        retry_after,
        ref resource,
    } = verdict
    {
        let resource_str = format!("{resource:?}");
        tracing::warn!(
            target: "ctxfs.provider.throttle",
            registry = registry_name,
            resource = resource_str.as_str(),
            retry_after_secs = retry_after.as_secs(),
            "secondary throttle detected"
        );
    }

    if status == reqwest::StatusCode::NOT_FOUND {
        return Err(CtxfsError::NotFound(not_found_label.to_string()));
    }
    if status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        let retry_after = headers
            .get("retry-after")
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
