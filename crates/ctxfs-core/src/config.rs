use serde::{Deserialize, Serialize};
use std::path::PathBuf;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Config {
    pub socket_path: PathBuf,
    pub pid_file: PathBuf,
    pub cache_dir: PathBuf,
    pub cache_max_bytes: u64,
    pub log_level: String,
    pub github_token: Option<String>,
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
        }
    }
}

impl Config {
    pub fn from_env() -> Self {
        let mut config = Self::default();

        if let Ok(v) = std::env::var("CTXFS_SOCKET") {
            config.socket_path = PathBuf::from(v);
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
        config.github_token = std::env::var("GITHUB_TOKEN").ok();

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
    fn config_serde_roundtrip() {
        let config = Config::default();
        let config2 = config.serde_roundtrip().unwrap();
        assert_eq!(config.log_level, config2.log_level);
        assert_eq!(config.cache_max_bytes, config2.cache_max_bytes);
        assert_eq!(config.socket_path, config2.socket_path);
    }
}
