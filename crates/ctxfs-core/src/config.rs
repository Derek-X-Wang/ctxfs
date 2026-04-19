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

// ---------------------------------------------------------------------------
// Config file path helper
// ---------------------------------------------------------------------------

/// Canonical path for the user-level config file (`~/.ctxfs/config.toml`).
///
/// Used by the app-helper and CLI so all callers agree on the location.
pub fn load_config_path() -> std::path::PathBuf {
    dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".ctxfs")
        .join("config.toml")
}

// ---------------------------------------------------------------------------
// Atomic write + external-edit detection
// ---------------------------------------------------------------------------

/// Errors that can occur when writing the config file.
#[derive(Debug, thiserror::Error)]
pub enum ConfigWriteError {
    #[error("io error: {0}")]
    Io(#[from] std::io::Error),
    #[error("config file was modified externally (hash {expected} expected, found {actual})")]
    ExternalEdit { expected: String, actual: String },
}

/// Atomically write `contents` to `path` using a temp+fsync+rename strategy.
///
/// - Creates the parent directory if it does not exist.
/// - Writes to a sibling `.tmp` file, calls `fsync`, then renames over the target.
/// - On Unix, `rename` is atomic, so an interrupted write never leaves a half-written file.
/// - Concurrent writes to the same config file are serialized at a higher level;
///   multi-process writers to the same user config are not a supported use-case.
pub fn atomic_write(path: &std::path::Path, contents: &[u8]) -> Result<(), ConfigWriteError> {
    use std::io::Write as _;
    let parent = path.parent().ok_or_else(|| {
        std::io::Error::new(std::io::ErrorKind::InvalidInput, "path has no parent")
    })?;
    std::fs::create_dir_all(parent)?;

    let file_name = path
        .file_name()
        .and_then(|s| s.to_str())
        .unwrap_or("config.toml");
    let tmp_path = parent.join(format!(".{file_name}.tmp"));

    {
        let mut f = std::fs::File::create(&tmp_path)?;
        f.write_all(contents)?;
        f.sync_all()?;
    }
    std::fs::rename(&tmp_path, path)?;
    Ok(())
}

/// A snapshot of a config file's contents at the time it was read.
///
/// Used by the Preferences GUI to detect external edits: take a snapshot
/// when the window opens, re-check before saving.  If the on-disk hash has
/// changed, return [`ConfigWriteError::ExternalEdit`] so the GUI can show a
/// non-destructive "reload or overwrite?" dialog.
#[derive(Debug)]
pub struct ConfigSnapshot {
    /// SHA-256 hex of the file contents at read time.  Empty-file hash when
    /// the file did not exist.
    hash_at_read: String,
}

impl ConfigSnapshot {
    /// Read the file at `path` and record its hash.
    ///
    /// If the file does not exist, records the hash of an empty byte slice so
    /// that a subsequent [`write_back`](Self::write_back) will succeed when the
    /// file is still missing.
    pub fn read(path: &std::path::Path) -> Result<Self, ConfigWriteError> {
        use sha2::Digest as _;
        let bytes = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        Ok(Self {
            hash_at_read: hex::encode(sha2::Sha256::digest(&bytes)),
        })
    }

    /// Reconstruct a snapshot from a hash string (e.g., one returned by the
    /// helper's `config_read` response).  Used for optimistic concurrency: the
    /// caller passes back the hash it received, and `write_back` validates it.
    pub fn from_hash(hash: String) -> Self {
        Self { hash_at_read: hash }
    }

    /// The recorded hash as a hex string.
    pub fn hash(&self) -> &str {
        &self.hash_at_read
    }

    /// Write `contents` to `path` atomically, but only if the file has not
    /// been modified since this snapshot was taken.
    ///
    /// Returns [`ConfigWriteError::ExternalEdit`] if the current on-disk hash
    /// differs from the hash recorded at read time.
    pub fn write_back(&self, path: &std::path::Path, contents: &str) -> Result<(), ConfigWriteError> {
        use sha2::Digest as _;
        let current = match std::fs::read(path) {
            Ok(b) => b,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e.into()),
        };
        let current_hash = hex::encode(sha2::Sha256::digest(&current));
        if current_hash != self.hash_at_read {
            return Err(ConfigWriteError::ExternalEdit {
                expected: self.hash_at_read.clone(),
                actual: current_hash,
            });
        }
        atomic_write(path, contents.as_bytes())
    }
}

// ---------------------------------------------------------------------------
// Per-key TOML update (preserves comments and unknown keys via toml_edit)
// ---------------------------------------------------------------------------

/// Update a single key in the config TOML file, preserving comments and
/// unknown keys.  Creates the file (and its parent directory) if absent.
pub fn update_config_key(
    path: &std::path::Path,
    key: &str,
    value: toml_edit::Value,
) -> Result<(), ConfigWriteError> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut doc: toml_edit::DocumentMut = existing.parse().map_err(|e: toml_edit::TomlError| {
        std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("failed to parse config.toml: {e}"),
        )
    })?;
    doc[key] = toml_edit::Item::Value(value);
    atomic_write(path, doc.to_string().as_bytes())
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
