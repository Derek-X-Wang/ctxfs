//! `ctxfs setup` — one-time configuration for passwordless NFS mounts.
//!
//! Creates a sudoers drop-in at `/etc/sudoers.d/ctxfs` that allows the
//! current user to run `mount_nfs` and `umount` without a password prompt.

use anyhow::{Context as _, Result};
use std::path::Path;

// Import atomic_write from ctxfs-core (used in set_default_backend).
use ctxfs_core::config::atomic_write;

const SUDOERS_PATH: &str = "/etc/sudoers.d/ctxfs";

/// Generate the sudoers file content for the current user and platform.
pub fn generate_sudoers(username: &str) -> String {
    use std::fmt::Write;
    let mut out = String::new();
    let _ = writeln!(
        out,
        "# Created by `ctxfs setup` — allows passwordless NFS mount/umount"
    );
    let _ = writeln!(
        out,
        "# for user '{username}' so `ctxfs mount` doesn't prompt for a password."
    );
    let _ = writeln!(out, "# Remove with: sudo rm /etc/sudoers.d/ctxfs");
    let _ = writeln!(out);

    #[cfg(target_os = "macos")]
    {
        let _ = writeln!(out, "{username} ALL=(root) NOPASSWD: /sbin/mount_nfs *");
        let _ = writeln!(out, "{username} ALL=(root) NOPASSWD: /sbin/umount *");
    }

    #[cfg(target_os = "linux")]
    {
        let _ = writeln!(out, "{username} ALL=(root) NOPASSWD: /bin/mount -t nfs *");
        let _ = writeln!(out, "{username} ALL=(root) NOPASSWD: /bin/umount *");
    }

    out
}

/// Check whether passwordless sudo is configured for NFS mounts.
///
/// The sudoers file at `/etc/sudoers.d/ctxfs` is 0440 root:wheel, so
/// non-root users typically can't read it. Instead of guessing from
/// file existence, we test whether `sudo -n mount_nfs` actually works
/// without a password prompt — this directly verifies the capability
/// we care about.
pub fn is_configured(_username: &str) -> bool {
    let path = Path::new(SUDOERS_PATH);
    if !path.exists() {
        return false;
    }

    // The file exists. Verify passwordless sudo actually works by running
    // `sudo -n mount_nfs` with no args — it returns usage (exit 1) but
    // doesn't prompt for a password. If it would prompt, sudo -n fails
    // with exit 1 AND writes "sudo: a password is required" to stderr.
    #[cfg(target_os = "macos")]
    let test_cmd = "mount_nfs";
    #[cfg(target_os = "linux")]
    let test_cmd = "mount";
    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    return false;

    match std::process::Command::new("sudo")
        .args(["-n", test_cmd])
        .output()
    {
        Ok(output) => {
            let stderr = String::from_utf8_lossy(&output.stderr);
            // If stderr contains "password is required", sudo would prompt.
            !stderr.contains("password is required")
        }
        Err(_) => false,
    }
}

/// Install the sudoers entry. Invokes `sudo tee` to write and `sudo visudo -c`
/// to validate. Prints instructions to the user.
pub fn install_sudoers() -> Result<()> {
    let username = whoami::username();

    if is_configured(&username) {
        println!("Already configured: {SUDOERS_PATH} exists for user '{username}'.");
        println!("To reconfigure, run: sudo rm {SUDOERS_PATH} && ctxfs setup");
        return Ok(());
    }

    let content = generate_sudoers(&username);

    println!("This will create {SUDOERS_PATH} with:");
    println!();
    for line in content.lines() {
        println!("  {line}");
    }
    println!();
    println!("You'll be prompted for your sudo password one last time.");
    println!();

    // Write via sudo tee (the standard pattern for writing to privileged paths).
    let mut child = std::process::Command::new("sudo")
        .args(["tee", SUDOERS_PATH])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::null())
        .spawn()
        .context("failed to run sudo tee")?;

    if let Some(ref mut stdin) = child.stdin {
        use std::io::Write;
        stdin
            .write_all(content.as_bytes())
            .context("failed to write sudoers content")?;
    }

    let status = child.wait().context("sudo tee failed")?;
    if !status.success() {
        anyhow::bail!("sudo tee exited with {status}");
    }

    // Set correct permissions (sudoers files must be 0440).
    let chmod = std::process::Command::new("sudo")
        .args(["chmod", "0440", SUDOERS_PATH])
        .status()
        .context("failed to chmod sudoers file")?;
    if !chmod.success() {
        anyhow::bail!("sudo chmod failed");
    }

    // Validate with visudo.
    let check = std::process::Command::new("sudo")
        .args(["visudo", "-c", "-f", SUDOERS_PATH])
        .status()
        .context("visudo validation failed")?;
    if !check.success() {
        // If validation fails, remove the bad file to avoid locking the user out.
        let _ = std::process::Command::new("sudo")
            .args(["rm", "-f", SUDOERS_PATH])
            .status();
        anyhow::bail!(
            "sudoers validation failed — file removed for safety. \
             Please report this as a bug."
        );
    }

    println!();
    println!("Done! `ctxfs mount` will no longer ask for a password.");
    println!("To undo: sudo rm {SUDOERS_PATH}");

    #[cfg(target_os = "macos")]
    {
        println!();
        println!("macOS note: your terminal app also needs Full Disk Access to read");
        println!("NFS-mounted files. Open System Settings to grant it:");
        // Attempt to open the FDA pane directly.
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles")
            .status();
        println!("  System Settings > Privacy & Security > Full Disk Access");
        println!("Add your terminal app, then restart it.");
    }

    Ok(())
}

/// Remove the sudoers entry.
pub fn uninstall_sudoers() -> Result<()> {
    if !Path::new(SUDOERS_PATH).exists() {
        println!("Nothing to remove: {SUDOERS_PATH} does not exist.");
        return Ok(());
    }

    let status = std::process::Command::new("sudo")
        .args(["rm", "-f", SUDOERS_PATH])
        .status()
        .context("failed to remove sudoers file")?;

    if !status.success() {
        anyhow::bail!("sudo rm failed");
    }

    println!("Removed {SUDOERS_PATH}. `ctxfs mount` will prompt for sudo again.");
    Ok(())
}

// ---------------------------------------------------------------------------
// FSKit setup helpers (macOS 26+)
// ---------------------------------------------------------------------------

/// Returns true if running on macOS 26 (Tahoe) or later.
///
/// Shells out to `sw_vers -productVersion` and checks the major version number.
/// Returns false on any error or on non-macOS platforms.
pub fn is_macos_26_or_later() -> bool {
    #[cfg(not(target_os = "macos"))]
    return false;

    #[cfg(target_os = "macos")]
    {
        let output = match std::process::Command::new("sw_vers")
            .arg("-productVersion")
            .output()
        {
            Ok(o) => o,
            Err(_) => return false,
        };
        let version = String::from_utf8_lossy(&output.stdout);
        let major: u32 = version
            .trim()
            .split('.')
            .next()
            .and_then(|s| s.parse().ok())
            .unwrap_or(0);
        major >= 26
    }
}

/// Find the ContextFS.app bundle.
///
/// Search order:
/// 1. `CTXFS_FSKIT_APP_PATH` environment variable
/// 2. Next to the current binary
/// 3. `~/Applications/ContextFS.app`
/// 4. `/Applications/ContextFS.app`
pub fn find_fskit_app() -> Option<std::path::PathBuf> {
    // 1. Environment override.
    if let Ok(env_path) = std::env::var("CTXFS_FSKIT_APP_PATH") {
        let p = std::path::PathBuf::from(env_path);
        if p.exists() {
            return Some(p);
        }
    }

    // 2. Next to the current binary.
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let candidate = dir.join("ContextFS.app");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 3. ~/Applications/ContextFS.app
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join("Applications").join("ContextFS.app");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 4. /Applications/ContextFS.app
    let system_path = std::path::PathBuf::from("/Applications/ContextFS.app");
    if system_path.exists() {
        return Some(system_path);
    }

    None
}

/// Install the FSKit extension for macOS 26+.
///
/// 1. Locates `ContextFS.app` via `find_fskit_app`.
/// 2. Copies it to `~/Applications/ContextFS.app` (skips if already there).
/// 3. Creates `/Volumes/ctxfs/` as the standard mount-point root (requires sudo once).
/// 4. Prints instructions to enable the System Extension in System Settings and opens
///    the relevant pane on macOS.
pub fn install_fskit() -> Result<(), String> {
    // Step 1: find the app bundle.
    let app_src = find_fskit_app()
        .ok_or_else(|| {
            "ContextFS.app not found. Make sure it is next to the ctxfs binary, in \
             ~/Applications, or /Applications, or set CTXFS_FSKIT_APP_PATH."
                .to_string()
        })?;

    // Step 2: copy to ~/Applications if not already there.
    let home = dirs::home_dir()
        .ok_or_else(|| "could not determine home directory".to_string())?;
    let dest = home.join("Applications").join("ContextFS.app");

    if dest != app_src {
        if dest.exists() {
            println!("ContextFS.app already installed at {}.", dest.display());
        } else {
            println!("Copying {} to {}...", app_src.display(), dest.display());
            std::fs::create_dir_all(home.join("Applications"))
                .map_err(|e| format!("failed to create ~/Applications: {e}"))?;

            // Use `cp -R` for a recursive bundle copy.
            let status = std::process::Command::new("cp")
                .args(["-R", &app_src.to_string_lossy(), &dest.to_string_lossy()])
                .status()
                .map_err(|e| format!("failed to run cp: {e}"))?;
            if !status.success() {
                return Err(format!("cp exited with {status}"));
            }
            println!("Copied.");
        }
    } else {
        println!("ContextFS.app is already at the target location.");
    }

    // Step 3: create /Volumes/ctxfs/ as mount-point root.
    let volumes_dir = std::path::Path::new("/Volumes/ctxfs");
    if volumes_dir.exists() {
        println!("/Volumes/ctxfs already exists.");
    } else {
        println!("Creating /Volumes/ctxfs (requires sudo)...");
        let status = std::process::Command::new("sudo")
            .args(["mkdir", "-p", "/Volumes/ctxfs"])
            .status()
            .map_err(|e| format!("failed to run sudo mkdir: {e}"))?;
        if !status.success() {
            return Err(format!("sudo mkdir /Volumes/ctxfs exited with {status}"));
        }
        println!("Created /Volumes/ctxfs.");
    }

    // Step 4: print instructions and open System Settings.
    println!();
    println!("FSKit extension installed. To activate it:");
    println!();
    println!("  1. Open System Settings → General → Login Items & Extensions");
    println!("     (or Privacy & Security → Extensions on older macOS 26 betas)");
    println!("  2. Click the ContextFS entry under File System Extensions");
    println!("  3. Toggle it ON and authenticate when prompted.");
    println!();
    println!("Once enabled, `ctxfs mount` will use the FSKit backend — no sudo,");
    println!("no Full Disk Access required.");
    println!();

    #[cfg(target_os = "macos")]
    {
        // Open the Login Items & Extensions pane.
        let _ = std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.LoginItems-Settings.extension")
            .status();
    }

    Ok(())
}

/// Print FSKit backend status for `ctxfs setup check`.
pub fn check_fskit_status() {
    #[cfg(target_os = "macos")]
    {
        println!();
        println!("FSKit backend (macOS 26+):");

        let macos_ok = is_macos_26_or_later();
        if macos_ok {
            println!("  macOS version:  26+ (FSKit supported)");
        } else {
            println!("  macOS version:  <26 (FSKit not available on this version)");
        }

        match find_fskit_app() {
            Some(p) => println!("  App installed:  yes ({})", p.display()),
            None => println!("  App installed:  no — run `ctxfs setup install-fskit` to install"),
        }

        let volumes_dir = std::path::Path::new("/Volumes/ctxfs");
        if volumes_dir.exists() {
            println!("  Mount dir:      /Volumes/ctxfs (exists)");
        } else {
            println!("  Mount dir:      /Volumes/ctxfs (missing — run `ctxfs setup install-fskit`)");
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        println!();
        println!("FSKit backend: not applicable (macOS only)");
    }
}

// ---------------------------------------------------------------------------
// Default backend configuration
// ---------------------------------------------------------------------------

/// Persist a default backend choice in `~/.ctxfs/config.toml`.
///
/// The function uses a line-by-line strategy to preserve other config
/// settings and comments:
///
/// - If a `backend = "..."` line (or a commented-out `# backend = ...` line)
///   is present, it is replaced with the canonical `backend = "<value>"` form.
/// - If no such line exists, the setting is appended to the end.
/// - If the file does not exist, a minimal file containing only the backend
///   line is created (including the parent directory).
pub fn set_default_backend(backend: &str, config_path: &std::path::Path) -> anyhow::Result<()> {
    if !matches!(backend, "nfs" | "fskit") {
        anyhow::bail!("invalid backend: {backend:?}. Use 'nfs' or 'fskit'");
    }

    let new_line = format!("backend = \"{backend}\"");

    if !config_path.exists() {
        atomic_write(config_path, format!("{new_line}\n").as_bytes())
            .with_context(|| format!("failed to write {}", config_path.display()))?;
        return Ok(());
    }

    let contents = std::fs::read_to_string(config_path)
        .with_context(|| format!("failed to read {}", config_path.display()))?;

    // Regex-free line rewrite: match `backend = "..."` or `# backend = ...`
    let mut found = false;
    let mut output_lines: Vec<String> = contents
        .lines()
        .map(|line| {
            let trimmed = line.trim();
            // Match an active `backend = ...` line (possibly with leading spaces).
            let is_active = trimmed.starts_with("backend") && {
                let rest = trimmed.trim_start_matches("backend").trim_start();
                rest.starts_with('=')
            };
            // Match a commented-out `# backend = ...` line.
            let is_commented = {
                let stripped = trimmed.trim_start_matches('#').trim_start();
                stripped.starts_with("backend") && {
                    let rest = stripped.trim_start_matches("backend").trim_start();
                    rest.starts_with('=')
                }
            };
            if is_active || is_commented {
                found = true;
                new_line.clone()
            } else {
                line.to_string()
            }
        })
        .collect();

    if !found {
        output_lines.push(new_line);
    }

    // Re-join, preserving trailing newline if the original had one.
    let mut result = output_lines.join("\n");
    if contents.ends_with('\n') || !contents.is_empty() {
        result.push('\n');
    }

    atomic_write(config_path, result.as_bytes())
        .with_context(|| format!("failed to write {}", config_path.display()))?;

    Ok(())
}

/// CLI entry-point for `ctxfs setup default-backend <backend>`.
pub fn handle_default_backend(backend: &str) -> anyhow::Result<()> {
    let config_path = dirs::home_dir()
        .context("could not determine home directory")?
        .join(".ctxfs")
        .join("config.toml");

    set_default_backend(backend, &config_path)?;

    println!("Default backend set to: {backend}");
    println!("Config file: {}", config_path.display());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ctxfs_core::config::{ConfigSnapshot, ConfigWriteError};

    #[test]
    fn generate_sudoers_contains_username() {
        let content = generate_sudoers("testuser");
        assert!(content.contains("testuser ALL=(root) NOPASSWD:"));
    }

    #[test]
    fn generate_sudoers_has_mount_and_umount() {
        let content = generate_sudoers("alice");

        #[cfg(target_os = "macos")]
        {
            assert!(content.contains("/sbin/mount_nfs"), "missing mount_nfs");
            assert!(content.contains("/sbin/umount"), "missing umount");
        }

        #[cfg(target_os = "linux")]
        {
            assert!(content.contains("/bin/mount -t nfs"), "missing mount nfs");
            assert!(content.contains("/bin/umount"), "missing umount");
        }
    }

    #[test]
    fn generate_sudoers_is_idempotent() {
        let a = generate_sudoers("bob");
        let b = generate_sudoers("bob");
        assert_eq!(a, b);
    }

    #[test]
    fn generate_sudoers_different_users_differ() {
        let a = generate_sudoers("alice");
        let b = generate_sudoers("bob");
        assert_ne!(a, b);
    }

    #[test]
    fn is_configured_depends_on_sudoers_file() {
        // is_configured checks whether /etc/sudoers.d/ctxfs exists AND
        // whether passwordless sudo for mount_nfs works. In most test
        // environments the file won't exist, so this returns false. On
        // developer machines where `ctxfs setup install` has been run,
        // it correctly returns true. Either way, it shouldn't panic.
        let _ = is_configured("testuser");
    }

    #[test]
    fn is_macos_26_or_later_does_not_panic() {
        // Just verify the function runs without panicking regardless of platform.
        let _ = is_macos_26_or_later();
    }

    #[test]
    fn find_fskit_app_returns_option() {
        // Without CTXFS_FSKIT_APP_PATH set (and no real app bundle present in CI),
        // this returns None. What matters is it doesn't panic.
        // If CTXFS_FSKIT_APP_PATH is set to a valid path in the environment, it
        // returns Some. Either outcome is acceptable.
        let _ = find_fskit_app();
    }

    #[test]
    fn check_fskit_status_does_not_panic() {
        // Should print to stdout without panicking regardless of platform / state.
        check_fskit_status();
    }

    // -----------------------------------------------------------------------
    // set_default_backend tests
    // -----------------------------------------------------------------------

    /// Helper: create a temp dir and return a config path inside it.
    fn temp_config() -> (tempfile::TempDir, std::path::PathBuf) {
        let dir = tempfile::TempDir::new().expect("tempdir");
        let path = dir.path().join("config.toml");
        (dir, path)
    }

    #[test]
    fn default_backend_creates_file_when_absent() {
        let (_dir, path) = temp_config();
        set_default_backend("fskit", &path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("backend = \"fskit\""));
    }

    #[test]
    fn default_backend_replaces_existing_value() {
        let (_dir, path) = temp_config();
        std::fs::write(&path, "log_level = \"debug\"\nbackend = \"nfs\"\n").unwrap();
        set_default_backend("fskit", &path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("backend = \"fskit\""));
        assert!(!contents.contains("backend = \"nfs\""));
        // Other settings are preserved.
        assert!(contents.contains("log_level = \"debug\""));
    }

    #[test]
    fn default_backend_uncomments_commented_line() {
        let (_dir, path) = temp_config();
        std::fs::write(&path, "# backend = \"auto\"  # nfs | fskit | auto\n").unwrap();
        set_default_backend("nfs", &path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("backend = \"nfs\""));
        assert!(!contents.contains("# backend"));
    }

    #[test]
    fn default_backend_appends_when_line_absent() {
        let (_dir, path) = temp_config();
        std::fs::write(&path, "log_level = \"info\"\ncache_max_bytes = 1024\n").unwrap();
        set_default_backend("nfs", &path).unwrap();
        let contents = std::fs::read_to_string(&path).unwrap();
        assert!(contents.contains("backend = \"nfs\""));
        // Other settings are preserved.
        assert!(contents.contains("log_level = \"info\""));
        assert!(contents.contains("cache_max_bytes = 1024"));
    }

    #[test]
    fn default_backend_rejects_invalid_value() {
        let (_dir, path) = temp_config();
        let err = set_default_backend("auto", &path).unwrap_err();
        assert!(err.to_string().contains("invalid backend"));
    }

    // -----------------------------------------------------------------------
    // atomic_write tests
    // -----------------------------------------------------------------------

    #[test]
    fn atomic_write_creates_new_file() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        atomic_write(&path, b"github_token = \"x\"\n").unwrap();
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "github_token = \"x\"\n"
        );
    }

    #[test]
    fn atomic_write_overwrites_existing() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "old").unwrap();
        atomic_write(&path, b"new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn atomic_write_leaves_no_temp_file_on_success() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        atomic_write(&path, b"content").unwrap();
        // Only config.toml should exist
        let mut entries: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        entries.sort();
        assert_eq!(entries, vec!["config.toml"]);
    }

    #[test]
    fn snapshot_detects_external_edit() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "original").unwrap();

        let snap = ConfigSnapshot::read(&path).unwrap();

        // Simulate external editor saving different content
        std::fs::write(&path, "external edit").unwrap();

        let result = snap.write_back(&path, "gui edit");
        assert!(
            matches!(result, Err(ConfigWriteError::ExternalEdit { .. })),
            "expected ExternalEdit error, got {result:?}"
        );
        // File should still have external content
        assert_eq!(
            std::fs::read_to_string(&path).unwrap(),
            "external edit"
        );
    }

    #[test]
    fn snapshot_allows_write_when_unchanged() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        std::fs::write(&path, "original").unwrap();

        let snap = ConfigSnapshot::read(&path).unwrap();

        snap.write_back(&path, "new").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new");
    }

    #[test]
    fn snapshot_read_missing_file_produces_empty_hash() {
        let tmp = tempfile::tempdir().unwrap();
        let path = tmp.path().join("config.toml");
        // File doesn't exist yet
        let snap = ConfigSnapshot::read(&path).unwrap();
        // write_back should succeed when the file is still missing
        snap.write_back(&path, "new content").unwrap();
        assert_eq!(std::fs::read_to_string(&path).unwrap(), "new content");
    }
}
