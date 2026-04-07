//! Integration test: resolve real `PyPI` packages to GitHub repos.
//! Gated behind `CTXFS_E2E_NETWORK=1` to avoid hitting APIs in offline CI.

use ctxfs_provider_common::resolver::RegistryResolver;
use ctxfs_provider_pypi::PyPIResolver;

fn skip_without_network() -> bool {
    if std::env::var("CTXFS_E2E_NETWORK").is_err() {
        eprintln!("skipping network test (set CTXFS_E2E_NETWORK=1 to enable)");
        true
    } else {
        false
    }
}

#[test]
fn resolve_six() {
    if skip_without_network() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resolver = PyPIResolver::new();

    let result = rt.block_on(resolver.resolve("six", "1.16.0")).unwrap();
    // six's repo is github.com/benjaminp/six
    assert_eq!(result.owner, "benjaminp");
    assert_eq!(result.repo, "six");
    assert!(!result.git_ref.is_empty());
}

#[test]
fn resolve_latest_six() {
    if skip_without_network() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resolver = PyPIResolver::new();

    let version = rt.block_on(resolver.resolve_latest("six")).unwrap();
    assert!(!version.is_empty());
}
