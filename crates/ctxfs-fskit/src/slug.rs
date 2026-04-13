//! Volume slug derivation.

use ctxfs_core::source::{ProviderType, SourceSpec};

/// Produce a volume slug from a `SourceSpec`.
///
/// Two projects mounting the same source deliberately produce the same slug,
/// so the `FSKit` volume is shared (with multiple symlinks pointing at it).
///
/// Examples:
/// - `npm:react@19.1.0` → `npm-react-19.1.0`
/// - `npm:@scope/pkg@1.0.0` → `npm-scope-pkg-1.0.0`
/// - `github:rust-lang/rust@master` → `github-rust-lang-rust-master`
pub fn volume_slug(source: &SourceSpec) -> String {
    let provider_prefix = match source.provider_type {
        ProviderType::GitHub => "github",
        ProviderType::Npm => "npm",
        ProviderType::PyPI => "pypi",
        ProviderType::Crate => "crate",
    };

    let name_flat = source
        .name
        .trim_start_matches('@')
        .replace('/', "-")
        .to_lowercase();

    let version_flat = source.version.replace('/', "-");

    format!("{provider_prefix}-{name_flat}-{version_flat}")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(provider: ProviderType, name: &str, version: &str) -> SourceSpec {
        SourceSpec {
            provider_type: provider,
            name: name.into(),
            version: version.into(),
            subpath: None,
        }
    }

    #[test]
    fn npm_simple() {
        assert_eq!(
            volume_slug(&spec(ProviderType::Npm, "react", "19.1.0")),
            "npm-react-19.1.0"
        );
    }

    #[test]
    fn npm_scoped() {
        assert_eq!(
            volume_slug(&spec(ProviderType::Npm, "@scope/pkg", "1.0.0")),
            "npm-scope-pkg-1.0.0"
        );
    }

    #[test]
    fn github_owner_repo() {
        assert_eq!(
            volume_slug(&spec(ProviderType::GitHub, "rust-lang/rust", "master")),
            "github-rust-lang-rust-master"
        );
    }

    #[test]
    fn pypi_package() {
        assert_eq!(
            volume_slug(&spec(ProviderType::PyPI, "requests", "2.31.0")),
            "pypi-requests-2.31.0"
        );
    }

    #[test]
    fn crate_package() {
        assert_eq!(
            volume_slug(&spec(ProviderType::Crate, "serde", "1.0.219")),
            "crate-serde-1.0.219"
        );
    }

    #[test]
    fn uppercase_normalized() {
        assert_eq!(
            volume_slug(&spec(ProviderType::GitHub, "Facebook/React", "v19")),
            "github-facebook-react-v19"
        );
    }
}
