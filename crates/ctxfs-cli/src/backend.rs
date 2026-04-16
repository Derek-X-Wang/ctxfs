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
    candidates.iter().any(|p| p.as_ref().is_some_and(|p| p.exists()))
}

/// Resolve backend: flag > env > config > auto-detect.
///
/// Priority chain:
/// 1. Explicit `--backend` flag
/// 2. `CTXFS_BACKEND` environment variable
/// 3. Config file default
/// 4. Auto-detect (macOS 26+ with ContextFS.app → FSKit, otherwise NFS)
pub fn detect_backend(flag: Option<Backend>, config_default: Option<Backend>) -> Backend {
    if let Some(b) = flag {
        return b;
    }
    if let Ok(v) = std::env::var("CTXFS_BACKEND") {
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
        let result = detect_backend(Some(Backend::FsKit), Some(Backend::Nfs));
        assert_eq!(result, Backend::FsKit);

        let result = detect_backend(Some(Backend::Nfs), Some(Backend::FsKit));
        assert_eq!(result, Backend::Nfs);
    }

    #[test]
    fn config_default_used_when_no_flag() {
        // When CTXFS_BACKEND is not set and there is no flag, config wins.
        // We temporarily unset the env var to isolate this test.
        let old = std::env::var("CTXFS_BACKEND").ok();
        std::env::remove_var("CTXFS_BACKEND");

        let result = detect_backend(None, Some(Backend::FsKit));
        // Only valid if auto-detect wouldn't choose FsKit anyway (it won't on
        // a dev machine without the app installed), but the config value is
        // what matters here — if auto-detect would also return FsKit we still
        // get the right answer.
        assert_eq!(result, Backend::FsKit);

        let result = detect_backend(None, Some(Backend::Nfs));
        assert_eq!(result, Backend::Nfs);

        // Restore env.
        if let Some(v) = old {
            std::env::set_var("CTXFS_BACKEND", v);
        }
    }

    #[test]
    fn no_flag_no_config_falls_back() {
        let old = std::env::var("CTXFS_BACKEND").ok();
        std::env::remove_var("CTXFS_BACKEND");

        let result = detect_backend(None, None);
        // Must return one of the two valid variants.
        assert!(matches!(result, Backend::Nfs | Backend::FsKit));

        if let Some(v) = old {
            std::env::set_var("CTXFS_BACKEND", v);
        }
    }

    #[test]
    fn env_var_overrides_config() {
        let old = std::env::var("CTXFS_BACKEND").ok();
        std::env::set_var("CTXFS_BACKEND", "nfs");

        let result = detect_backend(None, Some(Backend::FsKit));
        assert_eq!(result, Backend::Nfs);

        // Restore env.
        match old {
            Some(v) => std::env::set_var("CTXFS_BACKEND", v),
            None => std::env::remove_var("CTXFS_BACKEND"),
        }
    }

    #[test]
    fn env_var_invalid_falls_through_to_config() {
        let old = std::env::var("CTXFS_BACKEND").ok();
        std::env::set_var("CTXFS_BACKEND", "invalid_backend_value");

        // Invalid env var is ignored; config default should win.
        let result = detect_backend(None, Some(Backend::FsKit));
        assert_eq!(result, Backend::FsKit);

        match old {
            Some(v) => std::env::set_var("CTXFS_BACKEND", v),
            None => std::env::remove_var("CTXFS_BACKEND"),
        }
    }
}
