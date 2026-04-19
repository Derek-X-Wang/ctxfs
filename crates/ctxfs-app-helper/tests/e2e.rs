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
