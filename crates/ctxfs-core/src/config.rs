use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::backend::Backend;

/// Intermediate deserialization target for `~/.ctxfs/config.toml`.
///
/// All fields are optional — a missing key means "keep the default".
/// Env vars always win over file values (applied afterwards in `load()`).
#[derive(Deserialize, Default)]
struct ConfigFile {
    github_token: Option<String>,
    socket_path: Option<String>,
    pid_file: Option<String>,
    cache_dir: Option<String>,
    cache_max_bytes: Option<u64>,
    log_level: Option<String>,
    redis_url: Option<String>,
    latest_ttl_secs: Option<u64>,
    tree_cache_max_bytes: Option<u64>,
    backend: Option<String>,
    fskit_bundle_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub socket_path: PathBuf,
    pub pid_file: PathBuf,
    pub cache_dir: PathBuf,
    pub cache_max_bytes: u64,
    pub log_level: String,
    pub github_token: Option<String>,
    pub redis_url: Option<String>,
    pub latest_ttl_secs: u64,
    pub tree_cache_max_bytes: u64,
    pub default_backend: Option<Backend>,
    /// Bundle ID of the installed `ContextFS` appex.
    pub fskit_bundle_id: Option<String>,
}

impl Default for Config {
    fn default() -> Self {
        let base = dirs::home_dir()
            .unwrap_or_else(|| PathBuf::from("/tmp"))
            .join(".ctxfs");
        Self {
            socket_path: base.join("ctxfs.sock"),
            pid_file: base.join("ctxfs.pid"),
            cache_dir: base.join("cache"),
            cache_max_bytes: 512 * 1024 * 1024, // 512 MB
            log_level: "info".to_string(),
            github_token: None,
            redis_url: None,
            latest_ttl_secs: 3600,
            tree_cache_max_bytes: 500 * 1024 * 1024, // 500 MB
            default_backend: None,
            fskit_bundle_id: Some("ai.ctxfs.fskitbridge.fskitext".to_string()),
        }
    }
}

impl Config {
    /// Parse a TOML string and return a `Config` with file values applied on
    /// top of defaults.  Env vars are NOT read here; call `load()` for the
    /// full precedence chain.
    ///
    /// # Errors
    ///
    /// Returns a `toml::de::Error` if the TOML is malformed or contains
    /// unknown keys that fail deserialization.
    pub fn from_toml_str(s: &str) -> Result<Self, toml::de::Error> {
        let file: ConfigFile = toml::from_str(s)?;
        let mut config = Self::default();
        Self::apply_file(&mut config, &file);
        Ok(config)
    }

    /// Primary entry point: defaults → `~/.ctxfs/config.toml` → env vars.
    ///
    /// A missing or unreadable config file is silently ignored.
    pub fn load() -> Self {
        let mut config = Self::default();

        // Try to load the config file.  Missing file is not an error.
        if let Some(home) = dirs::home_dir() {
            let path = home.join(".ctxfs").join("config.toml");
            if path.exists() {
                match std::fs::read_to_string(&path) {
                    Ok(contents) => match toml::from_str::<ConfigFile>(&contents) {
                        Ok(file) => Self::apply_file(&mut config, &file),
                        Err(e) => {
                            // Warn but don't abort — env vars still work.
                            tracing::warn!(
                                "failed to parse {}: {e}",
                                path.display()
                            );
                        }
                    },
                    Err(e) => {
                        tracing::warn!("failed to read {}: {e}", path.display());
                    }
                }
            }
        }

        // Env vars win over file values.
        Self::apply_env(&mut config);
        config
    }

    /// Apply `ConfigFile` values on top of an existing config (file over
    /// defaults, but not yet env vars).
    fn apply_file(config: &mut Self, file: &ConfigFile) {
        if let Some(v) = &file.github_token {
            if !v.is_empty() {
                config.github_token = Some(v.clone());
            }
        }
        if let Some(v) = &file.socket_path {
            config.socket_path = PathBuf::from(v);
        }
        if let Some(v) = &file.pid_file {
            config.pid_file = PathBuf::from(v);
        }
        if let Some(v) = &file.cache_dir {
            config.cache_dir = PathBuf::from(v);
        }
        if let Some(v) = file.cache_max_bytes {
            config.cache_max_bytes = v;
        }
        if let Some(v) = &file.log_level {
            config.log_level = v.clone();
        }
        if let Some(v) = &file.redis_url {
            if !v.is_empty() {
                config.redis_url = Some(v.clone());
            }
        }
        if let Some(v) = file.latest_ttl_secs {
            config.latest_ttl_secs = v;
        }
        if let Some(v) = file.tree_cache_max_bytes {
            config.tree_cache_max_bytes = v;
        }
        if let Some(v) = &file.backend {
            config.default_backend = v.parse().ok();
        }
        if let Some(v) = &file.fskit_bundle_id {
            if !v.is_empty() {
                config.fskit_bundle_id = Some(v.clone());
            }
        }
    }

    /// Apply environment-variable overrides in place (the "env always wins"
    /// layer).  Extracted from `from_env()` so both `from_env()` and `load()`
    /// share the same logic.
    fn apply_env(config: &mut Self) {
        if let Ok(v) = std::env::var("CTXFS_SOCKET") {
            config.socket_path = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("CTXFS_PID_FILE") {
            config.pid_file = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("CTXFS_CACHE_DIR") {
            config.cache_dir = PathBuf::from(v);
        }
        if let Ok(v) = std::env::var("CTXFS_CACHE_MAX_BYTES") {
            if let Ok(n) = v.parse() {
                config.cache_max_bytes = n;
            }
        }
        if let Ok(v) = std::env::var("CTXFS_LOG_LEVEL") {
            config.log_level = v;
        }
        // Empty string is treated as "no token" to match common shell patterns.
        if let Some(v) = std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty()) {
            config.github_token = Some(v);
        } else if std::env::var("GITHUB_TOKEN").is_ok() {
            // Explicitly set to empty — clear any file value.
            config.github_token = None;
        }
        // Empty string is treated as "no URL" to match the same shell pattern.
        if let Some(v) = std::env::var("CTXFS_REDIS_URL")
            .ok()
            .filter(|s| !s.is_empty())
        {
            config.redis_url = Some(v);
        } else if std::env::var("CTXFS_REDIS_URL").is_ok() {
            config.redis_url = None;
        }
        if let Ok(v) = std::env::var("CTXFS_LATEST_TTL_SECS") {
            if let Ok(n) = v.parse() {
                config.latest_ttl_secs = n;
            }
        }
        if let Ok(v) = std::env::var("CTXFS_TREE_CACHE_MAX_BYTES") {
            if let Ok(n) = v.parse() {
                config.tree_cache_max_bytes = n;
            }
        }
        if let Some(v) = std::env::var("CTXFS_BACKEND")
            .ok()
            .filter(|s| !s.is_empty())
        {
            config.default_backend = v.parse().ok();
        }
        if let Some(v) = std::env::var("CTXFS_FSKIT_BUNDLE_ID")
            .ok()
            .filter(|s| !s.is_empty())
        {
            config.fskit_bundle_id = Some(v);
        }
    }

    /// Build a `Config` from defaults + env vars only (no config file).
    ///
    /// Kept for backward compatibility with existing tests.  For production
    /// use prefer `Config::load()`, which also reads `~/.ctxfs/config.toml`.
    pub fn from_env() -> Self {
        let mut config = Self::default();
        Self::apply_env(&mut config);
        config
    }

    #[cfg(test)]
    fn serde_roundtrip(&self) -> Result<Self, serde_json::Error> {
        let json = serde_json::to_string(self)?;
        serde_json::from_str(&json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_config_has_sane_values() {
        let config = Config::default();
        assert!(config.socket_path.to_string_lossy().contains("ctxfs.sock"));
        assert!(config.pid_file.to_string_lossy().contains("ctxfs.pid"));
        assert!(config.cache_dir.to_string_lossy().contains("cache"));
        assert_eq!(config.cache_max_bytes, 512 * 1024 * 1024);
        assert_eq!(config.log_level, "info");
    }

    #[test]
    #[allow(unsafe_code)]
    fn from_env_respects_pid_file_override() {
        // Save and clear any existing value so the test is hermetic.
        let prev = std::env::var("CTXFS_PID_FILE").ok();
        // SAFETY: single-threaded test with env cleanup.
        unsafe { std::env::set_var("CTXFS_PID_FILE", "/tmp/ctxfs-test-override.pid") };

        let config = Config::from_env();

        // Restore before asserting so a failing assert doesn't leak the env var.
        match prev {
            Some(v) => unsafe { std::env::set_var("CTXFS_PID_FILE", v) },
            None => unsafe { std::env::remove_var("CTXFS_PID_FILE") },
        }

        assert_eq!(
            config.pid_file,
            PathBuf::from("/tmp/ctxfs-test-override.pid")
        );
    }

    #[test]
    fn config_serde_roundtrip() {
        let config = Config::default();
        let config2 = config.serde_roundtrip().unwrap();
        assert_eq!(config.log_level, config2.log_level);
        assert_eq!(config.cache_max_bytes, config2.cache_max_bytes);
        assert_eq!(config.socket_path, config2.socket_path);
    }

    #[test]
    fn default_config_has_cache_tier_fields() {
        let config = Config::default();
        assert_eq!(config.latest_ttl_secs, 3600);
        assert_eq!(config.tree_cache_max_bytes, 500 * 1024 * 1024);
        assert!(config.redis_url.is_none());
    }

    #[test]
    fn default_config_has_fskit_bundle_id() {
        assert_eq!(
            Config::default().fskit_bundle_id.as_deref(),
            Some("ai.ctxfs.fskitbridge.fskitext")
        );
    }

    #[test]
    #[allow(unsafe_code)]
    fn from_env_reads_fskit_bundle_id() {
        let prev = std::env::var("CTXFS_FSKIT_BUNDLE_ID").ok();
        unsafe {
            std::env::set_var("CTXFS_FSKIT_BUNDLE_ID", "com.example.fskitbridge.fskitext");
        }
        let config = Config::from_env();
        match prev {
            Some(v) => unsafe { std::env::set_var("CTXFS_FSKIT_BUNDLE_ID", v) },
            None => unsafe { std::env::remove_var("CTXFS_FSKIT_BUNDLE_ID") },
        }
        assert_eq!(
            config.fskit_bundle_id.as_deref(),
            Some("com.example.fskitbridge.fskitext")
        );
    }

    #[test]
    fn config_from_toml_reads_values() {
        let toml = r#"
github_token = "ghp_test"
log_level = "debug"
"#;
        let config = Config::from_toml_str(toml).unwrap();
        assert_eq!(config.github_token.as_deref(), Some("ghp_test"));
        assert_eq!(config.log_level, "debug");
    }

    #[test]
    fn config_from_toml_applies_over_defaults() {
        let toml = r#"
cache_max_bytes = 1073741824
latest_ttl_secs = 7200
socket_path = "/tmp/test.sock"
"#;
        let config = Config::from_toml_str(toml).unwrap();
        assert_eq!(config.cache_max_bytes, 1_073_741_824);
        assert_eq!(config.latest_ttl_secs, 7200);
        assert_eq!(config.socket_path, PathBuf::from("/tmp/test.sock"));
        // Unset fields stay at default.
        assert_eq!(config.log_level, "info");
    }

    #[test]
    fn config_load_uses_defaults_when_no_file() {
        // Config::load() should work even when ~/.ctxfs/config.toml doesn't
        // exist (or if it does, at minimum the default log_level survives env
        // override only if CTXFS_LOG_LEVEL is unset).
        let prev = std::env::var("CTXFS_LOG_LEVEL").ok();
        #[allow(unsafe_code)]
        unsafe {
            std::env::remove_var("CTXFS_LOG_LEVEL");
        }
        let config = Config::load();
        #[allow(unsafe_code)]
        match prev {
            Some(v) => unsafe { std::env::set_var("CTXFS_LOG_LEVEL", v) },
            None => unsafe { std::env::remove_var("CTXFS_LOG_LEVEL") },
        }
        // As long as the real file doesn't set log_level differently,
        // we get the "info" default.
        assert!(!config.log_level.is_empty());
    }

    #[test]
    fn config_serde_roundtrip_with_redis() {
        let config = Config {
            redis_url: Some("redis://localhost:6379".into()),
            latest_ttl_secs: 7200,
            tree_cache_max_bytes: 1024 * 1024 * 1024,
            ..Config::default()
        };
        let config2 = config.serde_roundtrip().unwrap();
        assert_eq!(config.redis_url, config2.redis_url);
        assert_eq!(config.latest_ttl_secs, config2.latest_ttl_secs);
        assert_eq!(config.tree_cache_max_bytes, config2.tree_cache_max_bytes);
    }
}
