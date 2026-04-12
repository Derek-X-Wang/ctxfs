//! `ctxfs setup` — one-time configuration for passwordless NFS mounts.
//!
//! Creates a sudoers drop-in at `/etc/sudoers.d/ctxfs` that allows the
//! current user to run `mount_nfs` and `umount` without a password prompt.

use anyhow::{Context, Result};
use std::path::Path;

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

/// Find the CtxfsFS.app bundle.
///
/// Search order:
/// 1. `CTXFS_FSKIT_APP_PATH` environment variable
/// 2. Next to the current binary
/// 3. `~/Applications/CtxfsFS.app`
/// 4. `/Applications/CtxfsFS.app`
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
            let candidate = dir.join("CtxfsFS.app");
            if candidate.exists() {
                return Some(candidate);
            }
        }
    }

    // 3. ~/Applications/CtxfsFS.app
    if let Some(home) = dirs::home_dir() {
        let candidate = home.join("Applications").join("CtxfsFS.app");
        if candidate.exists() {
            return Some(candidate);
        }
    }

    // 4. /Applications/CtxfsFS.app
    let system_path = std::path::PathBuf::from("/Applications/CtxfsFS.app");
    if system_path.exists() {
        return Some(system_path);
    }

    None
}

/// Install the FSKit extension for macOS 26+.
///
/// 1. Locates `CtxfsFS.app` via `find_fskit_app`.
/// 2. Copies it to `~/Applications/CtxfsFS.app` (skips if already there).
/// 3. Creates `/Volumes/ctxfs/` as the standard mount-point root (requires sudo once).
/// 4. Prints instructions to enable the System Extension in System Settings and opens
///    the relevant pane on macOS.
pub fn install_fskit() -> Result<(), String> {
    // Step 1: find the app bundle.
    let app_src = find_fskit_app()
        .ok_or_else(|| {
            "CtxfsFS.app not found. Make sure it is next to the ctxfs binary, in \
             ~/Applications, or /Applications, or set CTXFS_FSKIT_APP_PATH."
                .to_string()
        })?;

    // Step 2: copy to ~/Applications if not already there.
    let home = dirs::home_dir()
        .ok_or_else(|| "could not determine home directory".to_string())?;
    let dest = home.join("Applications").join("CtxfsFS.app");

    if dest != app_src {
        if dest.exists() {
            println!("CtxfsFS.app already installed at {}.", dest.display());
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
        println!("CtxfsFS.app is already at the target location.");
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
