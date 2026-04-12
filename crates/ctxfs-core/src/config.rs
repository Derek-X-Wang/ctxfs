use serde::{Deserialize, Serialize};
use std::path::PathBuf;

use crate::backend::Backend;

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
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut config = Self::default();

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
        config.github_token = std::env::var("GITHUB_TOKEN").ok().filter(|s| !s.is_empty());
        // Empty string is treated as "no URL" to match the same shell pattern.
        config.redis_url = std::env::var("CTXFS_REDIS_URL")
            .ok()
            .filter(|s| !s.is_empty());
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
        config.default_backend = std::env::var("CTXFS_BACKEND")
            .ok()
            .filter(|s| !s.is_empty())
            .and_then(|s| s.parse().ok());

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
