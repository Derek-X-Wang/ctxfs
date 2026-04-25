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
    Pat {
        token_id_prefix: String,
    },
    /// GitHub App installation token (placeholder for future).
    GithubApp {
        installation_id: u64,
    },
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
    Active {
        until: SystemTime,
    },
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
        if let Some(v) = headers
            .get("x-ratelimit-limit")
            .and_then(|s| s.parse().ok())
        {
            self.limit = Some(v);
        }
        if let Some(v) = headers
            .get("x-ratelimit-remaining")
            .and_then(|s| s.parse().ok())
        {
            self.remaining = Some(v);
        }
        if let Some(secs) = headers
            .get("x-ratelimit-reset")
            .and_then(|s| s.parse::<u64>().ok())
        {
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

/// Verdict returned by [`ThrottleClassifier::classify`].
#[derive(Debug, Clone, Eq, PartialEq)]
pub enum RateLimitVerdict {
    Ok {
        resource: ResourceClass,
    },
    PrimaryExhausted {
        reset_at: SystemTime,
        resource: ResourceClass,
    },
    SecondaryThrottle {
        retry_after: Duration,
        resource: ResourceClass,
    },
    Other {
        status: u16,
    },
}

/// Classifies an HTTP response into a rate-limit verdict.
///
/// Order of checks (matters):
/// 1. `Retry-After` header present + status 429/403 → `SecondaryThrottle`
///    (secondary throttles can fire while `x-ratelimit-remaining` is still
///    nonzero, so this comes before the remaining-zero check).
/// 2. `x-ratelimit-remaining == 0` and status not 2xx → `PrimaryExhausted`.
/// 3. Status 2xx → `Ok`.
/// 4. Anything else → `Other`.
#[derive(Debug)]
pub struct ThrottleClassifier;

impl ThrottleClassifier {
    #[must_use]
    pub fn classify(
        status: u16,
        headers: &std::collections::HashMap<String, String>,
    ) -> RateLimitVerdict {
        let resource = headers
            .get("x-ratelimit-resource")
            .map(|s| ResourceClass::parse(s))
            .unwrap_or_else(|| ResourceClass::Other("unknown".to_string()));

        // 1. Secondary throttle: 429 or 403 with Retry-After.
        if (status == 429 || status == 403) && headers.contains_key("retry-after") {
            if let Some(secs) = headers
                .get("retry-after")
                .and_then(|s| s.parse::<u64>().ok())
            {
                return RateLimitVerdict::SecondaryThrottle {
                    retry_after: Duration::from_secs(secs),
                    resource,
                };
            }
        }

        // 2. Primary exhausted.
        if let Some(remaining) = headers
            .get("x-ratelimit-remaining")
            .and_then(|s| s.parse::<u64>().ok())
        {
            if remaining == 0 && !(200..300).contains(&status) {
                if let Some(reset_secs) = headers
                    .get("x-ratelimit-reset")
                    .and_then(|s| s.parse::<u64>().ok())
                {
                    return RateLimitVerdict::PrimaryExhausted {
                        reset_at: SystemTime::UNIX_EPOCH + Duration::from_secs(reset_secs),
                        resource,
                    };
                }
            }
        }

        // 3. OK.
        if (200..300).contains(&status) {
            return RateLimitVerdict::Ok { resource };
        }

        // 4. Other.
        RateLimitVerdict::Other { status }
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
        assert_eq!(
            ResourceClass::parse("code_search"),
            ResourceClass::CodeSearch
        );
    }

    #[test]
    fn resource_class_unknown_falls_back_to_other() {
        assert!(matches!(
            ResourceClass::parse("audit_log"),
            ResourceClass::Other(_)
        ));
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

    use std::collections::HashMap;

    fn hdr(pairs: &[(&str, &str)]) -> HashMap<String, String> {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
            .collect()
    }

    #[test]
    fn classifier_returns_ok_for_200() {
        let h = hdr(&[
            ("x-ratelimit-resource", "core"),
            ("x-ratelimit-remaining", "100"),
        ]);
        let v = ThrottleClassifier::classify(200, &h);
        assert!(matches!(
            v,
            RateLimitVerdict::Ok {
                resource: ResourceClass::Core
            }
        ));
    }

    #[test]
    fn classifier_primary_exhausted_when_remaining_zero() {
        let h = hdr(&[
            ("x-ratelimit-resource", "core"),
            ("x-ratelimit-remaining", "0"),
            ("x-ratelimit-reset", "1700000000"),
        ]);
        let v = ThrottleClassifier::classify(403, &h);
        match v {
            RateLimitVerdict::PrimaryExhausted { reset_at, resource } => {
                assert_eq!(resource, ResourceClass::Core);
                assert_eq!(
                    reset_at,
                    SystemTime::UNIX_EPOCH + Duration::from_secs(1700000000)
                );
            }
            other => panic!("expected PrimaryExhausted, got {other:?}"),
        }
    }

    #[test]
    fn classifier_secondary_throttle_429_with_retry_after_and_remaining_nonzero() {
        let h = hdr(&[
            ("x-ratelimit-resource", "core"),
            ("x-ratelimit-remaining", "4500"),
            ("retry-after", "60"),
        ]);
        let v = ThrottleClassifier::classify(429, &h);
        match v {
            RateLimitVerdict::SecondaryThrottle {
                retry_after,
                resource,
            } => {
                assert_eq!(retry_after, Duration::from_secs(60));
                assert_eq!(resource, ResourceClass::Core);
            }
            other => panic!("expected SecondaryThrottle, got {other:?}"),
        }
    }

    #[test]
    fn classifier_secondary_throttle_403_with_retry_after_is_secondary() {
        // GitHub's secondary limits sometimes return 403 (not 429) with retry-after.
        let h = hdr(&[("x-ratelimit-remaining", "4500"), ("retry-after", "30")]);
        let v = ThrottleClassifier::classify(403, &h);
        assert!(matches!(v, RateLimitVerdict::SecondaryThrottle { .. }));
    }

    #[test]
    fn classifier_other_for_500() {
        let v = ThrottleClassifier::classify(500, &HashMap::new());
        assert!(matches!(v, RateLimitVerdict::Other { status: 500 }));
    }

    #[test]
    fn classifier_other_when_remaining_zero_but_no_reset() {
        let h = hdr(&[
            ("x-ratelimit-remaining", "0"),
            // no x-ratelimit-reset
        ]);
        let v = ThrottleClassifier::classify(403, &h);
        assert!(matches!(v, RateLimitVerdict::Other { status: 403 }));
    }
}
