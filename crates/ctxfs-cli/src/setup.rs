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
    let _ = writeln!(out, "# Created by `ctxfs setup` — allows passwordless NFS mount/umount");
    let _ = writeln!(out, "# for user '{username}' so `ctxfs mount` doesn't prompt for a password.");
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

/// Check whether the sudoers entry already exists and contains the current user.
pub fn is_configured(username: &str) -> bool {
    let path = Path::new(SUDOERS_PATH);
    if !path.exists() {
        return false;
    }
    match std::fs::read_to_string(path) {
        Ok(content) => content.contains(username) && content.contains("NOPASSWD"),
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
    fn is_configured_returns_false_for_nonexistent_path() {
        // /etc/sudoers.d/ctxfs almost certainly doesn't exist in test env
        // (and if it does, it won't contain "nonexistent_user_12345")
        assert!(!is_configured("nonexistent_user_12345"));
    }
}
