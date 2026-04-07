//! Integration test: resolve real npm packages to GitHub repos.
//! Gated behind `CTXFS_E2E_NETWORK=1` to avoid hitting APIs in offline CI.

use ctxfs_provider_common::resolver::RegistryResolver;
use ctxfs_provider_npm::NpmResolver;

fn skip_without_network() -> bool {
    if std::env::var("CTXFS_E2E_NETWORK").is_err() {
        eprintln!("skipping network test (set CTXFS_E2E_NETWORK=1 to enable)");
        true
    } else {
        false
    }
}

#[test]
fn resolve_lodash() {
    if skip_without_network() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resolver = NpmResolver::new();

    let result = rt.block_on(resolver.resolve("lodash", "4.17.21")).unwrap();
    assert_eq!(result.owner, "lodash");
    assert_eq!(result.repo, "lodash");
    assert!(!result.git_ref.is_empty(), "git_ref should be set");
    assert!(result.subpath.is_none(), "lodash has no monorepo subpath");
}

#[test]
fn resolve_latest_lodash() {
    if skip_without_network() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resolver = NpmResolver::new();

    let version = rt.block_on(resolver.resolve_latest("lodash")).unwrap();
    assert!(!version.is_empty());
    // Should be a semver-like string
    assert!(
        version.contains('.'),
        "version should contain dots: {version}"
    );
}
