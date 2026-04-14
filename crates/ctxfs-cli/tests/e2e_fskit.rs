//! End-to-end FSKit test. Gated on `CTXFS_E2E_FSKIT=1` and a populated
//! `CTXFS_FSKIT_BUNDLE_ID` env var.
//!
//! Proves: `ctxfs mount --backend fskit` succeeds, the volume appears
//! under /Volumes/ctxfs/ and reads work, `ctxfs unmount` cleans up.

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results)]

mod common;

use common::TestEnv;
use predicates::prelude::*;

fn fskit_env_ready() -> bool {
    std::env::var("CTXFS_E2E_FSKIT").ok().as_deref() == Some("1")
        && std::env::var("CTXFS_FSKIT_BUNDLE_ID")
            .map(|s| !s.is_empty())
            .unwrap_or(false)
        && std::path::Path::new("/Volumes/ctxfs").exists()
}

#[test]
fn fskit_mount_and_read_cycle() {
    if !fskit_env_ready() {
        eprintln!(
            "skipping FSKit e2e test: set CTXFS_E2E_FSKIT=1, \
             CTXFS_FSKIT_BUNDLE_ID, and ensure /Volumes/ctxfs/ exists"
        );
        return;
    }

    let env = TestEnv::new();
    let _daemon = env.start_daemon();

    let mount_point = env.tempdir_path().join("test-mnt");

    // Mount via FSKit
    env.ctxfs(&[
        "mount",
        "github:octocat/Hello-World@master",
        "-p",
        mount_point.to_str().unwrap(),
        "--backend",
        "fskit",
    ])
    .assert()
    .success()
    .stdout(predicate::str::contains("FSKit volume"));

    // Verify the mount is readable via the symlink
    let readme = mount_point.join("README");
    let content = std::fs::read_to_string(&readme).expect("read README");
    assert!(!content.is_empty());

    // Unmount
    env.ctxfs(&["unmount", mount_point.to_str().unwrap()])
        .assert()
        .success()
        .stdout(predicate::str::contains("Unmounted"));

    // Symlink should be gone
    assert!(!mount_point.exists(), "symlink should be removed");
}
