#![allow(unused_results)]

use assert_cmd::prelude::*;
use std::io::{BufRead, BufReader, Write};
use std::process::{Command, Stdio};

#[test]
fn list_errors_when_daemon_down() {
    // Spawn helper with CTXFS_SOCKET pointing to a path that definitely won't
    // be a live daemon socket. Expect the response to have an "error" field.
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("nonexistent.sock");

    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .env("CTXFS_SOCKET", &socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":1,"method":"list"}}"#).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(
        response.contains(r#""error""#),
        "expected error, got: {response}"
    );
    assert!(response.contains(r#""id":1"#));

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn unmount_errors_when_params_missing() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // Missing params.target
    writeln!(
        stdin,
        r#"{{"id":1,"method":"unmount","params":{{}}}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(
        response.contains(r#""error""#),
        "expected error for missing params: {response}"
    );

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn helper_responds_to_ping() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn helper");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":1,"method":"ping"}}"#).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(
        response.contains(r#""result":"pong""#),
        "unexpected response: {response}"
    );
    assert!(
        response.contains(r#""id":1"#),
        "response must echo id"
    );

    // Second request on same process — proves persistent loop.
    writeln!(stdin, r#"{{"id":2,"method":"ping"}}"#).unwrap();
    stdin.flush().unwrap();
    let mut response2 = String::new();
    reader.read_line(&mut response2).unwrap();
    assert!(
        response2.contains(r#""id":2"#),
        "second request failed: {response2}"
    );

    // Close stdin — helper should exit gracefully.
    drop(stdin);
    let status = child.wait().unwrap();
    assert!(status.success(), "helper should exit 0 on stdin close");
}

#[test]
fn helper_unknown_method_returns_error_response() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn helper");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":42,"method":"nonexistent"}}"#).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(response.contains(r#""id":42"#));
    assert!(response.contains(r#""error""#));
    assert!(response.contains("unknown method"));

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn set_cache_limits_errors_when_params_missing() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":1,"method":"set_cache_limits","params":{{}}}}"#).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(
        response.contains(r#""error""#),
        "expected param validation error: {response}"
    );
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn prune_blobs_errors_when_params_missing() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":1,"method":"prune_blobs","params":{{}}}}"#).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(
        response.contains(r#""error""#),
        "expected param validation error: {response}"
    );
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn extension_status_returns_structured_response() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":1,"method":"extension_status"}}"#).unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    // Must have result (not error) — this method shouldn't fail even on non-macOS
    assert!(
        response.contains(r#""result""#),
        "expected result, got: {response}"
    );
    let parsed: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    let result = &parsed["result"];
    // Schema check
    assert!(result["bundle_id"].is_string());
    assert!(result["registered"].is_boolean());
    assert!(result["enabled"].is_boolean());
    assert!(result["platform_supported"].is_boolean());

    #[cfg(target_os = "macos")]
    assert_eq!(result["platform_supported"], true);

    #[cfg(not(target_os = "macos"))]
    assert_eq!(result["platform_supported"], false);

    drop(stdin);
    let _ = child.wait();
}

#[test]
fn cache_breakdown_errors_when_daemon_down() {
    let tmp = tempfile::tempdir().unwrap();
    let socket = tmp.path().join("nonexistent.sock");
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .env("CTXFS_SOCKET", &socket)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(stdin, r#"{{"id":7,"method":"cache_breakdown"}}"#).unwrap();
    stdin.flush().unwrap();
    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(response.contains(r#""error""#));
    assert!(response.contains(r#""id":7"#));
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn test_github_token_empty_returns_error() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(
        stdin,
        r#"{{"id":1,"method":"test_github_token","params":{{"token":""}}}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(
        response.contains(r#""error""#),
        "empty token should error: {response}"
    );
    assert!(response.contains("empty"), "error should mention 'empty': {response}");
    drop(stdin);
    let _ = child.wait();
}

#[test]
fn test_github_token_missing_params_returns_error() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    writeln!(
        stdin,
        r#"{{"id":1,"method":"test_github_token","params":{{}}}}"#
    )
    .unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    assert!(
        response.contains(r#""error""#),
        "missing token param should error: {response}"
    );
    drop(stdin);
    let _ = child.wait();
}

/// Full-lifecycle test. Requires a running ctxfs daemon.
/// Run with: cargo test -p ctxfs-app-helper -- --ignored helper_persistent_session
#[test]
#[ignore]
fn helper_persistent_session_across_multiple_requests() {
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn helper");

    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    // 1. Five pings — proves the dispatch loop is persistent.
    for id in 1..=5 {
        writeln!(stdin, r#"{{"id":{id},"method":"ping"}}"#).unwrap();
        stdin.flush().unwrap();
        let mut line = String::new();
        reader.read_line(&mut line).unwrap();
        assert!(
            line.contains(&format!(r#""id":{id}"#)),
            "ping {id} failed: {line}"
        );
        assert!(line.contains(r#""result":"pong""#));
    }

    // 2. cache_breakdown — requires daemon.
    writeln!(stdin, r#"{{"id":10,"method":"cache_breakdown"}}"#).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(line.trim())
        .unwrap_or_else(|e| panic!("invalid JSON for cache_breakdown: {e}\nline: {line}"));
    assert_eq!(parsed["id"], 10);
    assert!(
        parsed.get("error").is_none(),
        "cache_breakdown errored — is daemon running? {line}"
    );
    let result = &parsed["result"];
    assert!(result["blob_bytes"].is_u64(), "missing blob_bytes: {result}");
    assert!(result["blob_count"].is_u64());
    assert!(result["tree_bytes"].is_u64());
    assert!(result["tree_count"].is_u64());
    assert!(result["max_bytes"].is_u64());

    // 3. list — returns an array even if empty.
    writeln!(stdin, r#"{{"id":20,"method":"list"}}"#).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["id"], 20);
    assert!(parsed.get("error").is_none(), "list errored: {line}");
    assert!(
        parsed["result"].is_array(),
        "list result must be array: {parsed}"
    );

    // 4. extension_status — no daemon required, must always work.
    writeln!(stdin, r#"{{"id":30,"method":"extension_status"}}"#).unwrap();
    stdin.flush().unwrap();
    let mut line = String::new();
    reader.read_line(&mut line).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(line.trim()).unwrap();
    assert_eq!(parsed["id"], 30);
    let result = &parsed["result"];
    assert!(result["bundle_id"].is_string());
    assert!(result["registered"].is_boolean());
    assert!(result["enabled"].is_boolean());
    assert!(result["platform_supported"].is_boolean());

    // 5. Close stdin — helper exits cleanly.
    drop(stdin);
    let status = child.wait().expect("wait");
    assert!(status.success(), "helper exited non-zero: {status:?}");
}

// Live network test — gated on env var to avoid flaky CI.
#[test]
#[ignore]
fn test_github_token_validates_real_token() {
    let token = std::env::var("GITHUB_TOKEN").expect("set GITHUB_TOKEN to run");
    let mut child = Command::cargo_bin("ctxfs-app-helper")
        .unwrap()
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn");
    let mut stdin = child.stdin.take().unwrap();
    let stdout = child.stdout.take().unwrap();
    let mut reader = BufReader::new(stdout);

    let req =
        serde_json::json!({"id": 1, "method": "test_github_token", "params": {"token": token}});
    writeln!(stdin, "{req}").unwrap();
    stdin.flush().unwrap();

    let mut response = String::new();
    reader.read_line(&mut response).unwrap();
    let parsed: serde_json::Value = serde_json::from_str(response.trim()).unwrap();
    assert_eq!(parsed["result"]["valid"], true);
    assert!(parsed["result"]["remaining"].is_u64());
    drop(stdin);
    let _ = child.wait();
}
