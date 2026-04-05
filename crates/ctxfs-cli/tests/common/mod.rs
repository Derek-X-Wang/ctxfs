//! Shared test helpers for the CLI e2e suite.
//!
//! Guarantees (see e2e.rs for contract details):
//!  - Each [`TestEnv`] owns a [`TempDir`] that is removed on drop.
//!  - Each [`DaemonGuard`] kills the spawned daemon on drop, even on panic.
//!  - Environment variables are passed per-command, not set globally, so
//!    parallel tests never see each other's state.

#![allow(unused_results, dead_code)]

use assert_cmd::Command as AssertCommand;
use std::path::PathBuf;
use std::process::{Child, Command};
use tempfile::TempDir;

/// Hermetic CLI test environment.
///
/// All daemon state (socket, PID file, cache) is scoped to `tempdir`, which
/// is deleted automatically on drop. Tests never touch `~/.ctxfs/*`.
pub struct TestEnv {
    tempdir: TempDir,
}

impl TestEnv {
    pub fn new() -> Self {
        Self {
            tempdir: tempfile::Builder::new()
                .prefix("ctxfs-e2e-")
                .tempdir()
                .expect("failed to create tempdir"),
        }
    }

    pub fn tempdir_path(&self) -> &std::path::Path {
        self.tempdir.path()
    }

    pub fn socket_path(&self) -> PathBuf {
        self.tempdir.path().join("ctxfs.sock")
    }

    pub fn pid_file(&self) -> PathBuf {
        self.tempdir.path().join("ctxfs.pid")
    }

    pub fn cache_dir(&self) -> PathBuf {
        self.tempdir.path().join("cache")
    }

    /// Environment variables every `ctxfs` invocation in this test should use
    /// to stay isolated from the real user config.
    fn env_vars(&self) -> Vec<(&'static str, String)> {
        vec![
            (
                "CTXFS_SOCKET",
                self.socket_path().to_string_lossy().into_owned(),
            ),
            (
                "CTXFS_PID_FILE",
                self.pid_file().to_string_lossy().into_owned(),
            ),
            (
                "CTXFS_CACHE_DIR",
                self.cache_dir().to_string_lossy().into_owned(),
            ),
            // Quieter logs in tests; override with CTXFS_TEST_LOG=debug if needed.
            (
                "CTXFS_LOG_LEVEL",
                std::env::var("CTXFS_TEST_LOG").unwrap_or_else(|_| "error".into()),
            ),
            // Pass through the developer's token if set — needed for rate limits.
            // Tests that don't hit the network are unaffected.
            (
                "GITHUB_TOKEN",
                std::env::var("GITHUB_TOKEN").unwrap_or_default(),
            ),
        ]
    }

    /// Build an `assert_cmd::Command` for the `ctxfs` binary with isolated env.
    pub fn ctxfs(&self, args: &[&str]) -> AssertCommand {
        let mut cmd = AssertCommand::cargo_bin("ctxfs").expect("ctxfs binary not found");
        cmd.args(args);
        for (k, v) in self.env_vars() {
            cmd.env(k, v);
        }
        cmd
    }
}

impl std::fmt::Debug for TestEnv {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TestEnv")
            .field("tempdir", &self.tempdir.path())
            .finish()
    }
}

/// RAII guard that spawns a `ctxfs daemon start` subprocess and kills it on drop.
///
/// The destructor tries a graceful `ctxfs daemon stop` first, then falls back
/// to `child.kill()` so panicking tests cannot leak daemons.
pub struct DaemonGuard {
    child: Child,
    socket_path: PathBuf,
    env_vars: Vec<(&'static str, String)>,
}

impl DaemonGuard {
    pub fn pid(&self) -> u32 {
        self.child.id()
    }

    /// Wait up to `timeout` for the daemon's unix socket to appear.
    pub fn wait_until_ready(&self, timeout: std::time::Duration) -> std::io::Result<()> {
        let start = std::time::Instant::now();
        while start.elapsed() < timeout {
            if self.socket_path.exists() {
                return Ok(());
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("daemon did not bind {} in time", self.socket_path.display()),
        ))
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        // Best-effort graceful stop via CLI — ignore errors, we'll kill next.
        let mut stop = Command::new(env!("CARGO_BIN_EXE_ctxfs"));
        stop.arg("daemon").arg("stop");
        for (k, v) in &self.env_vars {
            stop.env(k, v);
        }
        let _ = stop.output();

        // Give the daemon up to 500ms to exit on its own.
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
        while std::time::Instant::now() < deadline {
            if let Ok(Some(_)) = self.child.try_wait() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(20));
        }

        // Hard kill as a last resort so we never leak a process.
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl std::fmt::Debug for DaemonGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("DaemonGuard")
            .field("pid", &self.child.id())
            .field("socket_path", &self.socket_path)
            .finish_non_exhaustive()
    }
}

impl TestEnv {
    /// Spawn `ctxfs daemon start` in the background and wait for it to bind
    /// its socket. Returns a guard that tears everything down on drop.
    #[allow(dead_code)] // will be used by later tests
    pub fn start_daemon(&self) -> DaemonGuard {
        let env_vars = self.env_vars();

        let mut cmd = Command::new(env!("CARGO_BIN_EXE_ctxfs"));
        cmd.arg("daemon").arg("start");
        for (k, v) in &env_vars {
            cmd.env(k, v);
        }
        // Redirect stdio to /dev/null so daemon logs don't pollute test output.
        cmd.stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null());

        let child = cmd.spawn().expect("failed to spawn ctxfs daemon");

        let guard = DaemonGuard {
            child,
            socket_path: self.socket_path(),
            env_vars,
        };
        guard
            .wait_until_ready(std::time::Duration::from_secs(5))
            .expect("daemon failed to start");
        guard
    }
}
