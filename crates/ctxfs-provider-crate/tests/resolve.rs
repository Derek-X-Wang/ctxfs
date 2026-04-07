//! Integration test: resolve real crates.io packages to GitHub repos.
//! Gated behind `CTXFS_E2E_NETWORK=1` to avoid hitting APIs in offline CI.

use ctxfs_provider_common::resolver::RegistryResolver;
use ctxfs_provider_crate::CrateResolver;

fn skip_without_network() -> bool {
    if std::env::var("CTXFS_E2E_NETWORK").is_err() {
        eprintln!("skipping network test (set CTXFS_E2E_NETWORK=1 to enable)");
        true
    } else {
        false
    }
}

#[test]
fn resolve_itoa() {
    if skip_without_network() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resolver = CrateResolver::new();

    let result = rt.block_on(resolver.resolve("itoa", "1.0.11")).unwrap();
    assert_eq!(result.owner, "dtolnay");
    assert_eq!(result.repo, "itoa");
    assert!(!result.git_ref.is_empty());
}

#[test]
fn resolve_latest_itoa() {
    if skip_without_network() {
        return;
    }
    let rt = tokio::runtime::Runtime::new().unwrap();
    let resolver = CrateResolver::new();

    let version = rt.block_on(resolver.resolve_latest("itoa")).unwrap();
    assert!(!version.is_empty());
}
