use std::collections::HashMap;
use std::path::{Path, PathBuf};

use super::DetectedDep;

/// Derive a short, filesystem-safe slug from a source spec string.
///
/// Examples:
/// - `"npm:react@19.1.0"` → `"react"`
/// - `"npm:@types/node@20.0.0"` → `"types-node"`
/// - `"github:owner/repo@main"` → `"repo-main"`
/// - `"github:owner/repo"` → `"repo"`
/// - `"crate:serde@1.0"` → `"serde"`
/// - `"pypi:requests@2.31.0"` → `"requests"`
pub fn source_to_slug(source_spec: &str) -> String {
    // Strip provider prefix (everything up to and including the first ':').
    let rest = match source_spec.split_once(':') {
        Some((_, r)) => r,
        None => source_spec,
    };

    let provider = source_spec
        .split_once(':')
        .map(|(p, _)| p)
        .unwrap_or("");

    if provider == "github" {
        // rest = "owner/repo@ref" or "owner/repo"
        let repo_and_ref = rest.split_once('/').map(|(_, r)| r).unwrap_or(rest);
        return match repo_and_ref.split_once('@') {
            Some((repo, git_ref)) => format!("{repo}-{git_ref}"),
            None => repo_and_ref.to_owned(),
        };
    }

    // Registry sources: rest = "name@version" or "@scope/name@version"
    let name = match rest.split_once('@') {
        // Scoped npm packages start with '@', so splitting on '@' gives ("", "scope/name@version").
        // Handle that by checking if the part before '@' is empty.
        Some(("", scoped)) => {
            // scoped = "scope/name@version" or "scope/name"
            match scoped.split_once('@') {
                Some((scoped_name, _)) => scoped_name,
                None => scoped,
            }
        }
        Some((n, _)) => n,
        None => rest,
    };

    // Sanitize: trim leading '@', replace '/' with '-'.
    name.trim_start_matches('@').replace('/', "-")
}

/// Compute mount paths for a list of detected dependencies.
///
/// Each dep's `source_spec` is mapped to `mount_dir/<slug>`. When two or more
/// deps from *different* ecosystems share the same raw slug, the paths are
/// disambiguated by prepending the ecosystem name: `mount_dir/<ecosystem>-<slug>`.
pub fn compute_mount_paths(deps: &[DetectedDep], mount_dir: &Path) -> HashMap<String, PathBuf> {
    // Build (source_spec -> raw_slug) and detect collisions.
    let slugs: Vec<(&DetectedDep, String)> = deps
        .iter()
        .map(|dep| (dep, source_to_slug(&dep.source_spec)))
        .collect();

    // Count how many distinct source_specs share each raw slug.
    let mut slug_count: HashMap<String, usize> = HashMap::new();
    for (_, slug) in &slugs {
        *slug_count.entry(slug.clone()).or_insert(0) += 1;
    }

    slugs
        .into_iter()
        .map(|(dep, slug)| {
            let collision = slug_count.get(&slug).copied().unwrap_or(0) > 1;
            let dir_name = if collision {
                format!("{}-{}", dep.ecosystem, slug)
            } else {
                slug
            };
            (dep.source_spec.clone(), mount_dir.join(dir_name))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::deps::{DetectedDep, Ecosystem};
    use std::path::PathBuf;

    #[test]
    fn slug_npm_basic() {
        assert_eq!(source_to_slug("npm:react@19.1.0"), "react");
    }

    #[test]
    fn slug_npm_scoped() {
        assert_eq!(source_to_slug("npm:@types/node@20.0.0"), "types-node");
    }

    #[test]
    fn slug_github_with_ref() {
        assert_eq!(source_to_slug("github:owner/repo@main"), "repo-main");
    }

    #[test]
    fn slug_github_with_tag() {
        assert_eq!(source_to_slug("github:owner/repo@v1.0.0"), "repo-v1.0.0");
    }

    #[test]
    fn slug_crate() {
        assert_eq!(source_to_slug("crate:serde@1.0"), "serde");
    }

    #[test]
    fn slug_pypi() {
        assert_eq!(source_to_slug("pypi:requests@2.31.0"), "requests");
    }

    #[test]
    fn no_collision_unique_names() {
        let deps = vec![
            DetectedDep::new("react".into(), "19.1.0".into(), Ecosystem::Npm, false),
            DetectedDep::new("serde".into(), "1.0.0".into(), Ecosystem::Crate, false),
        ];
        let mount_dir = PathBuf::from("/mnt");
        let paths = compute_mount_paths(&deps, &mount_dir);

        assert_eq!(paths[&deps[0].source_spec], PathBuf::from("/mnt/react"));
        assert_eq!(paths[&deps[1].source_spec], PathBuf::from("/mnt/serde"));
    }

    #[test]
    fn collision_adds_ecosystem_prefix() {
        let deps = vec![
            DetectedDep::new("requests".into(), "2.31.0".into(), Ecosystem::PyPI, false),
            DetectedDep::new("requests".into(), "0.1.0".into(), Ecosystem::Crate, false),
        ];
        let mount_dir = PathBuf::from("/mnt");
        let paths = compute_mount_paths(&deps, &mount_dir);

        assert_eq!(
            paths[&deps[0].source_spec],
            PathBuf::from("/mnt/pypi-requests")
        );
        assert_eq!(
            paths[&deps[1].source_spec],
            PathBuf::from("/mnt/crate-requests")
        );
    }
}
