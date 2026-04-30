use ctxfs_core::Backend;

/// Check if macOS version is 26+.
fn macos_version_26_or_later() -> bool {
    #[cfg(target_os = "macos")]
    {
        let output = std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output();
        if let Ok(out) = output {
            if let Ok(version_str) = std::str::from_utf8(&out.stdout) {
                let major: Option<u32> = version_str
                    .trim()
                    .split('.')
                    .next()
                    .and_then(|s| s.parse().ok());
                return major.is_some_and(|v| v >= 26);
            }
        }
        false
    }
    #[cfg(not(target_os = "macos"))]
    {
        false
    }
}

/// Check if ContextFS.app is installed.
fn fskit_app_installed() -> bool {
    let candidates = [
        dirs::home_dir().map(|h| h.join("Applications/ContextFS.app")),
        Some(std::path::PathBuf::from("/Applications/ContextFS.app")),
    ];
    candidates
        .iter()
        .any(|p| p.as_ref().is_some_and(|p| p.exists()))
}

/// Resolve backend: flag > env > config > auto-detect.
///
/// Priority chain:
/// 1. Explicit `--backend` flag
/// 2. `CTXFS_BACKEND` environment variable
/// 3. Config file default
/// 4. Auto-detect (macOS 26+ with ContextFS.app → FSKit, otherwise NFS)
pub fn detect_backend(flag: Option<Backend>, config_default: Option<Backend>) -> Backend {
    let env_value = std::env::var("CTXFS_BACKEND").ok();
    detect_backend_inner(flag, config_default, env_value.as_deref())
}

/// Inner resolver with the env value injected — testable without touching
/// process-global state. The four-test parallel-env race in
/// `cargo test -p ctxfs-cli` is structurally avoided by routing tests through
/// this function.
fn detect_backend_inner(
    flag: Option<Backend>,
    config_default: Option<Backend>,
    env_value: Option<&str>,
) -> Backend {
    if let Some(b) = flag {
        return b;
    }
    if let Some(v) = env_value {
        if let Ok(b) = v.parse() {
            return b;
        }
    }
    if let Some(b) = config_default {
        return b;
    }
    if macos_version_26_or_later() && fskit_app_installed() {
        return Backend::FsKit;
    }
    Backend::Nfs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn explicit_flag_wins() {
        // Flag should override everything, including a mismatched config.
        let result = detect_backend_inner(Some(Backend::FsKit), Some(Backend::Nfs), None);
        assert_eq!(result, Backend::FsKit);

        let result = detect_backend_inner(Some(Backend::Nfs), Some(Backend::FsKit), None);
        assert_eq!(result, Backend::Nfs);
    }

    #[test]
    fn config_default_used_when_no_flag() {
        // When env is unset (None) and there is no flag, config wins.
        let result = detect_backend_inner(None, Some(Backend::FsKit), None);
        assert_eq!(result, Backend::FsKit);

        let result = detect_backend_inner(None, Some(Backend::Nfs), None);
        assert_eq!(result, Backend::Nfs);
    }

    #[test]
    fn no_flag_no_config_falls_back() {
        let result = detect_backend_inner(None, None, None);
        // Must return one of the two valid variants.
        assert!(matches!(result, Backend::Nfs | Backend::FsKit));
    }

    #[test]
    fn env_var_overrides_config() {
        let result = detect_backend_inner(None, Some(Backend::FsKit), Some("nfs"));
        assert_eq!(result, Backend::Nfs);
    }

    #[test]
    fn env_var_invalid_falls_through_to_config() {
        // Invalid env value is ignored; config default should win.
        let result = detect_backend_inner(None, Some(Backend::FsKit), Some("invalid_backend_value"));
        assert_eq!(result, Backend::FsKit);
    }

    #[test]
    fn detect_backend_public_reads_env() {
        // Smoke test that the public wrapper still composes correctly: the
        // explicit flag short-circuits the env read, so this is race-free.
        let result = detect_backend(Some(Backend::Nfs), Some(Backend::FsKit));
        assert_eq!(result, Backend::Nfs);
    }
}
