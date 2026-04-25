//! Rate-limit budget tracking and HTTP-response throttle classification.
//!
//! Budgets are keyed by `(AuthIdentity, ResourceClass)` because GitHub's
//! `x-ratelimit-resource` header carries finer-grained quotas than the
//! source dimension. Two sources sharing the same PAT share the same
//! budget; one source with multiple resource classes (core, search,
//! graphql) holds independent budgets per class.

use std::time::{Duration, SystemTime};

/// Identifies the credential under which a request is made. Two requests
/// with the same `AuthIdentity` share a GitHub-side rate-limit budget.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub struct AuthIdentity {
    pub host: String,
    pub kind: AuthKind,
}

#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum AuthKind {
    Anonymous,
    /// Personal-access token. Stored as the **token id prefix** (first 8
    /// chars), not the secret itself, so this struct is safe to log.
    Pat { token_id_prefix: String },
    /// GitHub App installation token (placeholder for future).
    GithubApp { installation_id: u64 },
}

impl AuthIdentity {
    #[must_use]
    pub fn anonymous(host: impl Into<String>) -> Self {
        Self {
            host: host.into(),
            kind: AuthKind::Anonymous,
        }
    }

    #[must_use]
    pub fn pat(host: impl Into<String>, token: &str) -> Self {
        let prefix: String = token.chars().take(8).collect();
        Self {
            host: host.into(),
            kind: AuthKind::Pat {
                token_id_prefix: prefix,
            },
        }
    }
}

/// Snapshot of a rate-limit budget at a point in time.
#[derive(Debug, Clone)]
pub struct RateLimitGauge {
    pub limit: Option<u64>,
    pub remaining: Option<u64>,
    pub reset_at: Option<SystemTime>,
    pub secondary_throttle_state: ThrottleState,
}

#[derive(Debug, Clone, Eq, PartialEq)]
pub enum ThrottleState {
    None,
    /// Secondary throttle active until the wrapped instant.
    Active { until: SystemTime },
}

impl RateLimitGauge {
    #[must_use]
    pub fn unknown() -> Self {
        Self {
            limit: None,
            remaining: None,
            reset_at: None,
            secondary_throttle_state: ThrottleState::None,
        }
    }

    /// Update the gauge from a map of HTTP response headers (lowercased keys).
    /// Missing headers leave the corresponding field unchanged.
    pub fn update_from_headers(&mut self, headers: &std::collections::HashMap<String, String>) {
        if let Some(v) = headers.get("x-ratelimit-limit").and_then(|s| s.parse().ok()) {
            self.limit = Some(v);
        }
        if let Some(v) = headers.get("x-ratelimit-remaining").and_then(|s| s.parse().ok()) {
            self.remaining = Some(v);
        }
        if let Some(secs) = headers.get("x-ratelimit-reset").and_then(|s| s.parse::<u64>().ok()) {
            self.reset_at = Some(SystemTime::UNIX_EPOCH + Duration::from_secs(secs));
        }
    }

    /// Set the secondary-throttle state to active for the given duration from now.
    pub fn set_secondary_throttle(&mut self, retry_after: Duration) {
        self.secondary_throttle_state = ThrottleState::Active {
            until: SystemTime::now() + retry_after,
        };
    }

    /// Clear the secondary-throttle state if its `until` is in the past.
    pub fn clear_expired_throttle(&mut self) {
        if let ThrottleState::Active { until } = self.secondary_throttle_state {
            if SystemTime::now() >= until {
                self.secondary_throttle_state = ThrottleState::None;
            }
        }
    }
}

/// GitHub `x-ratelimit-resource` header values. Each resource class has
/// its own per-`AuthIdentity` budget.
#[derive(Debug, Clone, Eq, PartialEq, Hash)]
pub enum ResourceClass {
    Core,
    Search,
    CodeSearch,
    Graphql,
    Other(String),
}

impl ResourceClass {
    #[must_use]
    pub fn parse(s: &str) -> Self {
        match s {
            "core" => Self::Core,
            "search" => Self::Search,
            "code_search" => Self::CodeSearch,
            "graphql" => Self::Graphql,
            other => Self::Other(other.to_string()),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auth_identity_anonymous_distinct_from_pat() {
        let anon = AuthIdentity::anonymous("api.github.com");
        let pat = AuthIdentity::pat("api.github.com", "ghp_token123");
        assert_ne!(anon, pat);
    }

    #[test]
    fn resource_class_parses_known_values() {
        assert_eq!(ResourceClass::parse("core"), ResourceClass::Core);
        assert_eq!(ResourceClass::parse("search"), ResourceClass::Search);
        assert_eq!(ResourceClass::parse("graphql"), ResourceClass::Graphql);
        assert_eq!(ResourceClass::parse("code_search"), ResourceClass::CodeSearch);
    }

    #[test]
    fn resource_class_unknown_falls_back_to_other() {
        assert!(matches!(ResourceClass::parse("audit_log"), ResourceClass::Other(_)));
    }

    #[test]
    fn gauge_default_is_unlimited_unknown() {
        let g = RateLimitGauge::unknown();
        assert!(g.remaining.is_none());
        assert!(g.limit.is_none());
        assert!(g.reset_at.is_none());
        assert!(matches!(g.secondary_throttle_state, ThrottleState::None));
    }

    #[test]
    fn gauge_update_from_headers_parses_standard_response() {
        let mut headers = std::collections::HashMap::new();
        let _ = headers.insert("x-ratelimit-limit".to_string(), "5000".to_string());
        let _ = headers.insert("x-ratelimit-remaining".to_string(), "4999".to_string());
        let _ = headers.insert("x-ratelimit-reset".to_string(), "1700000000".to_string());

        let mut g = RateLimitGauge::unknown();
        g.update_from_headers(&headers);

        assert_eq!(g.limit, Some(5000));
        assert_eq!(g.remaining, Some(4999));
        assert_eq!(
            g.reset_at,
            Some(SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000))
        );
    }

    #[test]
    fn gauge_update_ignores_missing_headers() {
        let headers = std::collections::HashMap::new();
        let mut g = RateLimitGauge::unknown();
        g.update_from_headers(&headers);
        assert!(g.remaining.is_none());
    }
}
