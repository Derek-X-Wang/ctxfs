//! Auth handshake enforcement tests — unit and integration.

// ── Unit tests for verify_token_ct ─────────────────────────────────────────

use fskit_rs::auth::verify_token_ct;

#[test]
fn constant_time_compare_accepts_equal() {
    let token = [0x42u8; 32];
    assert!(verify_token_ct(&token, &token));
}

#[test]
fn constant_time_compare_rejects_different() {
    let a = [0x42u8; 32];
    let mut b = a;
    b[31] ^= 1;
    assert!(!verify_token_ct(&a, &b));
}

#[test]
fn constant_time_compare_rejects_wrong_length() {
    let a = [0x42u8; 32];
    let short = [0x42u8; 16];
    assert!(!verify_token_ct(&a, &short));
}

// ── Integration tests for TCP handshake behavior ────────────────────────────

use fskit_rs::protocol::{AuthenticateRequest, GetVolumeIdentifier, Request, request};
use fskit_rs::session::SessionBuilder;
use prost::Message as _;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

mod mock_fs;
use mock_fs::MockFs;

fn encode_length_delimited(req: &Request) -> Vec<u8> {
    let mut buf = Vec::new();
    req.encode_length_delimited(&mut buf).unwrap();
    buf
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn valid_token_is_accepted() {
    let token = vec![0xABu8; 32];
    let calls = Arc::new(Mutex::new(0u32));
    let fs = MockFs::new(calls.clone());
    let session = SessionBuilder::new(fs)
        .with_auth_token(token.clone())
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest {
            token: token.clone(),
        })),
    };
    stream
        .write_all(&encode_length_delimited(&auth))
        .await
        .unwrap();

    // Read back the response — should be Success.
    let mut resp_buf = vec![0u8; 64];
    let n = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut resp_buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0, "expected a response from server");

    session.shutdown().await;
    assert_eq!(
        *calls.lock().unwrap(),
        0,
        "auth flow must not dispatch to handler"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn invalid_token_is_rejected_and_closes_connection() {
    let token = vec![0xABu8; 32];
    let wrong = vec![0xCDu8; 32];
    let calls = Arc::new(Mutex::new(0u32));
    let fs = MockFs::new(calls.clone());
    let session = SessionBuilder::new(fs)
        .with_auth_token(token)
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let bad_auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest {
            token: wrong,
        })),
    };
    stream
        .write_all(&encode_length_delimited(&bad_auth))
        .await
        .unwrap();

    // Server must respond with posix_error = EACCES, then close.
    let mut resp_buf = vec![0u8; 256];
    let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut resp_buf))
        .await
        .unwrap();

    // After the bad auth, connection should be closed.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let next = Request {
        id: 2,
        content: None,
    };
    let _ = stream.write_all(&encode_length_delimited(&next)).await;
    let n = stream.read(&mut resp_buf).await.unwrap_or(0);
    assert_eq!(n, 0, "server must have closed the connection");

    session.shutdown().await;
    assert_eq!(*calls.lock().unwrap(), 0);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn non_auth_first_frame_is_rejected() {
    let token = vec![0xABu8; 32];
    let calls = Arc::new(Mutex::new(0u32));
    let fs = MockFs::new(calls.clone());
    let session = SessionBuilder::new(fs)
        .with_auth_token(token)
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let premature = Request {
        id: 1,
        content: Some(request::Content::GetVolumeIdentifier(
            GetVolumeIdentifier {},
        )),
    };
    stream
        .write_all(&encode_length_delimited(&premature))
        .await
        .unwrap();

    let mut resp_buf = vec![0u8; 256];
    let _ = tokio::time::timeout(Duration::from_secs(1), stream.read(&mut resp_buf))
        .await
        .unwrap();

    tokio::time::sleep(Duration::from_millis(100)).await;
    let n = stream.read(&mut resp_buf).await.unwrap_or(0);
    assert_eq!(
        n, 0,
        "server must have closed the connection after non-auth first frame"
    );

    session.shutdown().await;
    assert_eq!(
        *calls.lock().unwrap(),
        0,
        "handler must not see a pre-auth request"
    );
}
