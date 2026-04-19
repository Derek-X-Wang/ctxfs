//! Integration tests for `--json` flag output on `list`, `cache stats`, and `diag`.
//!
//! These tests exercise the CLI binary directly via `assert_cmd`. When no daemon
//! is running (the common case in CI), error paths must not mix human-readable
//! text on stdout — stdout stays either empty or valid JSON. Errors go to stderr.

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

use assert_cmd::Command;
use serde_json::Value;

/// Helper: build a `ctxfs` command with an isolated environment so tests
/// don't accidentally read from `~/.ctxfs/` and interfere with each other.
fn ctxfs() -> Command {
    let mut cmd = Command::cargo_bin("ctxfs").unwrap();
    // Point at a non-existent socket so daemon RPCs fail cleanly rather than
    // connecting to a real daemon that might be running on the dev machine.
    cmd.env(
        "CTXFS_SOCKET",
        "/tmp/ctxfs-test-nonexistent-socket.sock",
    );
    cmd
}

#[test]
fn diag_json_emits_valid_json_object() {
    let output = ctxfs()
        .args(["diag", "--json"])
        .output()
        .expect("exec");

    // diag should always exit 0 — it degrades gracefully on every check.
    assert!(
        output.status.success(),
        "diag --json should exit 0; stderr: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let stdout = String::from_utf8_lossy(&output.stdout);
    let parsed: Value = serde_json::from_str(stdout.trim())
        .unwrap_or_else(|e| panic!("output must be valid JSON: {e}\nstdout was:\n{stdout}"));

    assert!(parsed.is_object(), "diag JSON must be an object");
    assert!(parsed.get("product").is_some(), "missing 'product' key");
    assert!(parsed.get("version").is_some(), "missing 'version' key");
    assert!(
        parsed.get("bundle_id").is_some(),
        "missing 'bundle_id' key"
    );
    assert!(
        parsed.get("backend").is_some(),
        "missing 'backend' key"
    );
    assert!(
        parsed.get("config_loaded").is_some(),
        "missing 'config_loaded' key"
    );
    assert!(
        parsed.get("daemon_running").is_some(),
        "missing 'daemon_running' key"
    );
    // macOS version OR a null is fine; just must exist
    assert!(
        parsed.get("macos_version").is_some(),
        "missing 'macos_version' key"
    );
}

#[test]
fn list_json_emits_valid_json_array_or_errors_cleanly() {
    let output = ctxfs()
        .args(["list", "--json"])
        .output()
        .expect("exec");

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: Value =
            serde_json::from_str(stdout.trim()).expect("success output must be valid JSON");
        assert!(parsed.is_array(), "list JSON must be an array");
    } else {
        // Daemon not running → failure is acceptable, BUT stdout must not
        // contain mixed human-readable text. It must be empty or valid JSON.
        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if !trimmed.is_empty() {
            serde_json::from_str::<Value>(trimmed)
                .expect("if stdout is non-empty it must still be valid JSON");
        }
    }
}

#[test]
fn cache_stats_json_emits_valid_json_object_or_errors_cleanly() {
    let output = ctxfs()
        .args(["cache", "stats", "--json"])
        .output()
        .expect("exec");

    if output.status.success() {
        let stdout = String::from_utf8_lossy(&output.stdout);
        let parsed: Value = serde_json::from_str(stdout.trim()).expect("valid JSON");
        assert!(parsed.is_object(), "cache stats JSON must be an object");
    } else {
        // Daemon not running → stdout must be empty or valid JSON (not mixed text).
        let stdout = String::from_utf8_lossy(&output.stdout);
        let trimmed = stdout.trim();
        if !trimmed.is_empty() {
            serde_json::from_str::<Value>(trimmed)
                .expect("if stdout is non-empty it must still be valid JSON");
        }
    }
}
