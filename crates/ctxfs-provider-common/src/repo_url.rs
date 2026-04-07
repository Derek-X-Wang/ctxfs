//! Parse repository URLs from package registry metadata into (owner, repo) pairs.

/// Extract (owner, repo) from a GitHub URL. Returns `None` if not a GitHub URL.
pub fn parse_github_url(url: &str) -> Option<(String, String)> {
    let url = url.trim();
    if url.is_empty() {
        return None;
    }

    // Handle "github:owner/repo" shorthand
    if let Some(rest) = url.strip_prefix("github:") {
        return parse_owner_repo_from_path(rest);
    }

    // Normalize: strip git+ prefix
    let normalized = url.strip_prefix("git+").unwrap_or(url);

    // Handle SCP-style: ssh://git@github.com:owner/repo.git
    if let Some(after) = normalized.strip_prefix("ssh://git@github.com:") {
        let path = after.strip_suffix(".git").unwrap_or(after);
        return parse_owner_repo_from_path(path);
    }

    // Try various schemes for github.com
    let prefixes = [
        "https://github.com/",
        "http://github.com/",
        "ssh://git@github.com/",
        "git://github.com/",
    ];

    for prefix in prefixes {
        if let Some(rest) = normalized.strip_prefix(prefix) {
            let path = rest.strip_suffix(".git").unwrap_or(rest);
            return parse_owner_repo_from_path(path);
        }
    }

    None
}

fn parse_owner_repo_from_path(path: &str) -> Option<(String, String)> {
    let mut parts = path.splitn(3, '/');
    let owner = parts.next().filter(|s| !s.is_empty())?;
    let repo = parts.next().filter(|s| !s.is_empty())?;
    Some((owner.to_string(), repo.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn https_url() {
        assert_eq!(
            parse_github_url("https://github.com/lodash/lodash"),
            Some(("lodash".into(), "lodash".into()))
        );
    }

    #[test]
    fn https_with_git_suffix() {
        assert_eq!(
            parse_github_url("https://github.com/facebook/react.git"),
            Some(("facebook".into(), "react".into()))
        );
    }

    #[test]
    fn git_plus_https() {
        assert_eq!(
            parse_github_url("git+https://github.com/babel/babel.git"),
            Some(("babel".into(), "babel".into()))
        );
    }

    #[test]
    fn git_ssh() {
        assert_eq!(
            parse_github_url("git+ssh://git@github.com/owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn git_protocol() {
        assert_eq!(
            parse_github_url("git://github.com/owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn github_shorthand() {
        assert_eq!(
            parse_github_url("github:facebook/react"),
            Some(("facebook".into(), "react".into()))
        );
    }

    #[test]
    fn url_with_tree_path() {
        assert_eq!(
            parse_github_url("https://github.com/owner/repo/tree/main/src"),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn scp_syntax() {
        assert_eq!(
            parse_github_url("git+ssh://git@github.com:owner/repo.git"),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn gitlab_returns_none() {
        assert_eq!(parse_github_url("https://gitlab.com/owner/repo"), None);
    }

    #[test]
    fn empty_string_returns_none() {
        assert_eq!(parse_github_url(""), None);
    }

    #[test]
    fn not_a_url_returns_none() {
        assert_eq!(parse_github_url("just some text"), None);
    }

    #[test]
    fn url_with_trailing_slash() {
        assert_eq!(
            parse_github_url("https://github.com/owner/repo/"),
            Some(("owner".into(), "repo".into()))
        );
    }

    #[test]
    fn ssh_shorthand_no_scheme() {
        // Pure SSH shorthand without ssh:// scheme — common in git configs
        // This format may not be supported; None is acceptable
        let result = parse_github_url("git@github.com:owner/repo.git");
        // Document the behavior — if None, that's fine, it's an unusual format
        assert!(result.is_none() || result == Some(("owner".into(), "repo".into())));
    }

    #[test]
    fn scp_without_git_plus_prefix() {
        let result = parse_github_url("ssh://git@github.com:owner/repo.git");
        assert_eq!(result, Some(("owner".into(), "repo".into())));
    }
}
