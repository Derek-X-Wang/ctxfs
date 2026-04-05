//! End-to-end tests that exercise the `ctxfs` binary as a subprocess.
//!
//! # Safety contract
//!
//! Every test runs in a hermetic environment — it never reads or writes
//! anything under `~/.ctxfs/*`. All daemon state (socket, PID file, cache)
//! lives inside a per-test [`TempDir`] that is deleted automatically on drop.
//!
//! The [`TestEnv::start_daemon`] helper returns a [`DaemonGuard`] that, on
//! drop, sends `ctxfs daemon stop` and then `kill -9` as a last resort so
//! panicking tests cannot leak processes.
//!
//! These tests **never** invoke `sudo mount_nfs` — kernel mounts are gated
//! behind a separate `CTXFS_E2E_SUDO=1` opt-in suite.

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

mod common;

use common::TestEnv;
use predicates::prelude::*;

#[test]
fn daemon_status_reports_not_running_when_no_daemon() {
    let env = TestEnv::new();

    env.ctxfs(&["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("not running"));
}

#[test]
fn daemon_guard_cleans_up_on_panic() {
    // Prove that a panicking test does not leak daemon processes. We spawn a
    // daemon, capture its PID, trigger a panic inside `catch_unwind`, then
    // assert the PID is no longer alive.
    let env = TestEnv::new();
    let pid_alive = std::panic::catch_unwind(|| {
        let guard = env.start_daemon();
        let pid = guard.pid();
        // Simulate a test panic — guard::drop runs as the stack unwinds.
        panic!("intentional panic to exercise cleanup (pid was {pid})");
    });
    assert!(pid_alive.is_err(), "expected the inner block to panic");

    // The socket should be cleaned up because DaemonGuard ran its graceful
    // stop sequence during unwind.
    assert!(
        !env.socket_path().exists(),
        "socket should be removed after panic unwind"
    );
    assert!(
        !env.pid_file().exists(),
        "pid file should be removed after panic unwind"
    );
}

#[test]
fn parallel_daemons_dont_interfere() {
    // Two independent TestEnvs must be able to run daemons at the same time.
    // Each gets its own tempdir, socket, pid file, and cache dir.
    let env_a = TestEnv::new();
    let env_b = TestEnv::new();

    assert_ne!(env_a.socket_path(), env_b.socket_path());

    let daemon_a = env_a.start_daemon();
    let daemon_b = env_b.start_daemon();

    // Both daemons should be reachable independently.
    env_a
        .ctxfs(&["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pong"));
    env_b
        .ctxfs(&["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pong"));

    drop(daemon_a);
    drop(daemon_b);
}

#[test]
fn mount_server_only_starts_nfs_and_reports_port() {
    // Skip automatically if we have no network — this test hits GitHub.
    if std::env::var("CTXFS_E2E_SKIP_NETWORK").is_ok() {
        eprintln!("skipping network test");
        return;
    }

    let env = TestEnv::new();
    let _daemon = env.start_daemon();

    let mount_point = env.tempdir_path().join("mnt");
    std::fs::create_dir_all(&mount_point).unwrap();

    // `--server-only` must start the NFS server and print the loopback port,
    // skipping the sudo kernel mount so the test can run non-interactively.
    let output = env
        .ctxfs(&[
            "mount",
            "--server-only",
            "github:octocat/Hello-World@master",
            mount_point.to_str().unwrap(),
        ])
        .output()
        .unwrap();

    assert!(
        output.status.success(),
        "ctxfs mount --server-only failed: stderr={}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    // The CLI should print the NFS port the daemon bound.
    let port: u16 = stdout
        .lines()
        .find_map(|l| l.trim_start().strip_prefix("NFS port:"))
        .and_then(|s| s.trim().parse().ok())
        .unwrap_or_else(|| panic!("could not parse NFS port from stdout:\n{stdout}"));

    assert!(port > 1024, "NFS port should be unprivileged, got {port}");

    // Verify the daemon is actually listening on that port.
    let addr = format!("127.0.0.1:{port}");
    std::net::TcpStream::connect(&addr)
        .unwrap_or_else(|e| panic!("daemon is not listening on {addr}: {e}"));
}

#[test]
fn daemon_lifecycle_start_ping_stop() {
    let env = TestEnv::new();

    assert!(
        !env.socket_path().exists(),
        "socket should not exist before daemon start"
    );

    let daemon = env.start_daemon();

    assert!(
        env.socket_path().exists(),
        "daemon should have created the socket"
    );
    assert!(
        env.pid_file().exists(),
        "daemon should have created the pid file"
    );

    // Verify the daemon responds to ping via the CLI.
    env.ctxfs(&["daemon", "status"])
        .assert()
        .success()
        .stdout(predicate::str::contains("pong"));

    // Explicit teardown to verify cleanup (also covered by Drop).
    drop(daemon);

    // Give the daemon a moment to finish cleanup after SIGTERM.
    std::thread::sleep(std::time::Duration::from_millis(300));

    assert!(
        !env.socket_path().exists(),
        "socket should be removed after daemon stop"
    );
    assert!(
        !env.pid_file().exists(),
        "pid file should be removed after daemon stop"
    );
}
