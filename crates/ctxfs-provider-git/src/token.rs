/// Structured result from validating a GitHub Personal Access Token.
#[derive(Debug, serde::Serialize, serde::Deserialize)]
pub struct TokenInfo {
    pub valid: bool,
    pub user: Option<String>,
    pub remaining: Option<u64>,
    pub reset_at: Option<String>,
}

/// Validate a GitHub Personal Access Token by calling `/rate_limit` and `/user`
/// in parallel.  Returns structured info for UI display.
///
/// # Errors
///
/// Returns `Err` with a human-readable message when the token is empty, the
/// network request fails, or GitHub returns a non-2xx status for `/rate_limit`.
pub async fn validate_github_token(token: &str) -> Result<TokenInfo, String> {
    if token.is_empty() {
        return Err("token is empty".to_string());
    }
    let auth =
        ctxfs_provider_common::http::bearer_header(token).ok_or("invalid token encoding")?;
    let client = reqwest::Client::new();
    let rate_limit_fut = client
        .get("https://api.github.com/rate_limit")
        .header(reqwest::header::AUTHORIZATION, auth.clone())
        .header("User-Agent", concat!("ctxfs/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send();
    let user_fut = client
        .get("https://api.github.com/user")
        .header(reqwest::header::AUTHORIZATION, auth)
        .header("User-Agent", concat!("ctxfs/", env!("CARGO_PKG_VERSION")))
        .header("Accept", "application/vnd.github+json")
        .send();

    let (rate_res, user_res) = tokio::join!(rate_limit_fut, user_fut);
    let rate_resp = rate_res.map_err(|e| format!("request failed: {e}"))?;
    if !rate_resp.status().is_success() {
        return Err(format!("GitHub returned {}", rate_resp.status()));
    }
    let rate_body: serde_json::Value = rate_resp
        .json()
        .await
        .map_err(|e| format!("failed to parse rate_limit: {e}"))?;

    let remaining = rate_body["resources"]["core"]["remaining"].as_u64();
    let reset_at = rate_body["resources"]["core"]["reset"]
        .as_i64()
        .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0))
        .map(|dt| dt.to_rfc3339());
    let user = match user_res {
        Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
            Ok(body) => body["login"].as_str().map(std::string::ToString::to_string),
            Err(_) => None,
        },
        _ => None,
    };
    Ok(TokenInfo {
        valid: true,
        user,
        remaining,
        reset_at,
    })
}
