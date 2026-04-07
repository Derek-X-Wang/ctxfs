//! Shared HTTP client for registry API calls.

use reqwest::header::HeaderMap;

const USER_AGENT: &str = "ctxfs/0.1";

/// Build a reqwest client with standard headers for registry API calls.
pub fn registry_client() -> reqwest::Client {
    let headers = HeaderMap::new();
    reqwest::Client::builder()
        .user_agent(USER_AGENT)
        .default_headers(headers)
        .build()
        .expect("failed to build HTTP client")
}
