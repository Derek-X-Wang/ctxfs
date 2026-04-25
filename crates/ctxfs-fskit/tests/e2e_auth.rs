//! End-to-end auth handshake tests for the ctxfs-daemon seam.
//!
//! These tests exercise `AuthToken::generate()` → `SessionBuilder::with_auth_token()`
//! → TCP handshake, proving the daemon-side token flows through to the fskit-rs
//! enforcement layer without being silently dropped.

use std::ffi::OsStr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use async_trait::async_trait;
use ctxfs_fskit::auth::AuthToken;
use fskit_rs::protocol::{request, response, AuthenticateRequest, Request, Response};
use fskit_rs::session::SessionBuilder;
use fskit_rs::{
    AccessMask, DirectoryEntries, Error, Filesystem, Item, ItemAttributes, ItemType, OpenMode,
    PathConfOperations, PreallocateFlag, ResourceIdentifier, Result, SetXattrPolicy, StatFsResult,
    SupportedCapabilities, SyncFlags, TaskOptions, VolumeBehavior, VolumeIdentifier, Xattrs,
};
use prost::Message as _;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

// ── Inline mock filesystem ──────────────────────────────────────────────────

/// Minimal stub that increments a counter on any non-auth dispatch.
/// Every method returns ENOSYS; the counter lets tests confirm the auth
/// gate blocks all handler calls before authentication.
#[derive(Clone)]
struct MockFs {
    non_auth_calls: Arc<Mutex<u32>>,
}

impl MockFs {
    fn new(counter: Arc<Mutex<u32>>) -> Self {
        Self {
            non_auth_calls: counter,
        }
    }

    fn record(&self) -> Error {
        *self.non_auth_calls.lock().unwrap() += 1;
        Error::Posix(libc::ENOSYS)
    }
}

#[async_trait]
impl Filesystem for MockFs {
    async fn get_resource_identifier(&mut self) -> Result<ResourceIdentifier> {
        Err(self.record())
    }
    async fn get_volume_identifier(&mut self) -> Result<VolumeIdentifier> {
        Err(self.record())
    }
    async fn get_volume_behavior(&mut self) -> Result<VolumeBehavior> {
        Err(self.record())
    }
    async fn get_path_conf_operations(&mut self) -> Result<PathConfOperations> {
        Err(self.record())
    }
    async fn get_volume_capabilities(&mut self) -> Result<SupportedCapabilities> {
        Err(self.record())
    }
    async fn get_volume_statistics(&mut self) -> Result<StatFsResult> {
        Err(self.record())
    }
    async fn mount(&mut self, _options: TaskOptions) -> Result<()> {
        Err(self.record())
    }
    async fn unmount(&mut self) -> Result<()> {
        Err(self.record())
    }
    async fn synchronize(&mut self, _flags: SyncFlags) -> Result<()> {
        Err(self.record())
    }
    async fn get_attributes(&mut self, _item_id: u64) -> Result<ItemAttributes> {
        Err(self.record())
    }
    async fn set_attributes(
        &mut self,
        _item_id: u64,
        _attributes: ItemAttributes,
    ) -> Result<ItemAttributes> {
        Err(self.record())
    }
    async fn lookup_item(&mut self, _name: &OsStr, _directory_id: u64) -> Result<Item> {
        Err(self.record())
    }
    async fn reclaim_item(&mut self, _item_id: u64) -> Result<()> {
        Err(self.record())
    }
    async fn read_symbolic_link(&mut self, _item_id: u64) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn create_item(
        &mut self,
        _name: &OsStr,
        _type: ItemType,
        _directory_id: u64,
        _attributes: ItemAttributes,
    ) -> Result<Item> {
        Err(self.record())
    }
    async fn create_symbolic_link(
        &mut self,
        _name: &OsStr,
        _directory_id: u64,
        _new_attributes: ItemAttributes,
        _contents: Vec<u8>,
    ) -> Result<Item> {
        Err(self.record())
    }
    async fn create_link(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn remove_item(
        &mut self,
        _item_id: u64,
        _name: &OsStr,
        _directory_id: u64,
    ) -> Result<()> {
        Err(self.record())
    }
    async fn rename_item(
        &mut self,
        _item_id: u64,
        _source_directory_id: u64,
        _source_name: &OsStr,
        _destination_name: &OsStr,
        _destination_directory_id: u64,
        _over_item_id: Option<u64>,
    ) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn enumerate_directory(
        &mut self,
        _directory_id: u64,
        _cookie: u64,
        _verifier: u64,
    ) -> Result<DirectoryEntries> {
        Err(self.record())
    }
    async fn activate(&mut self, _options: TaskOptions) -> Result<Item> {
        Err(self.record())
    }
    async fn deactivate(&mut self) -> Result<()> {
        Err(self.record())
    }
    async fn get_supported_xattr_names(&mut self, _item_id: u64) -> Result<Xattrs> {
        Err(self.record())
    }
    async fn get_xattr(&mut self, _name: &OsStr, _item_id: u64) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn set_xattr(
        &mut self,
        _name: &OsStr,
        _value: Option<Vec<u8>>,
        _item_id: u64,
        _policy: SetXattrPolicy,
    ) -> Result<()> {
        Err(self.record())
    }
    async fn get_xattrs(&mut self, _item_id: u64) -> Result<Xattrs> {
        Err(self.record())
    }
    async fn open_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        Err(self.record())
    }
    async fn close_item(&mut self, _item_id: u64, _modes: Vec<OpenMode>) -> Result<()> {
        Err(self.record())
    }
    async fn read(&mut self, _item_id: u64, _offset: i64, _length: i64) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn write(&mut self, _contents: Vec<u8>, _item_id: u64, _offset: i64) -> Result<i64> {
        Err(self.record())
    }
    async fn check_access(&mut self, _item_id: u64, _access: Vec<AccessMask>) -> Result<bool> {
        Err(self.record())
    }
    async fn set_volume_name(&mut self, _name: Vec<u8>) -> Result<Vec<u8>> {
        Err(self.record())
    }
    async fn preallocate_space(
        &mut self,
        _item_id: u64,
        _offset: i64,
        _length: i64,
        _flags: Vec<PreallocateFlag>,
    ) -> Result<i64> {
        Err(self.record())
    }
    async fn deactivate_item(&mut self, _item_id: u64) -> Result<()> {
        Err(self.record())
    }
}

// ── Helpers ─────────────────────────────────────────────────────────────────

fn encode_request(req: &Request) -> Vec<u8> {
    let mut buf = Vec::new();
    req.encode_length_delimited(&mut buf).unwrap();
    buf
}

fn decode_response(buf: &[u8]) -> Response {
    Response::decode_length_delimited(&mut &buf[..]).unwrap()
}

// ── Tests ────────────────────────────────────────────────────────────────────

/// A daemon-generated `AuthToken` must be accepted when the client sends the
/// identical raw bytes as the first frame.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_generated_token_accepts_matching_client() {
    let token = AuthToken::generate();
    let raw = token.bytes_vec();

    let calls = Arc::new(Mutex::new(0u32));
    let fs = MockFs::new(calls.clone());
    let session = SessionBuilder::new(fs)
        .with_auth_token(raw.clone())
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest {
            token: raw,
        })),
    };
    stream.write_all(&encode_request(&auth)).await.unwrap();

    let mut resp_buf = vec![0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut resp_buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0, "expected a response from the server");

    let resp = decode_response(&resp_buf[..n]);
    assert!(
        matches!(resp.content, Some(response::Content::Success(_))),
        "expected Success response, got: {:?}",
        resp.content
    );

    session.shutdown().await;
    assert_eq!(
        *calls.lock().unwrap(),
        0,
        "auth handshake must not dispatch to the filesystem handler"
    );
}

/// When the client sends a *different* token than the one the session was
/// configured with, the server must respond with EACCES and close the
/// connection.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn daemon_generated_token_rejects_mismatched_client() {
    let server_token = AuthToken::generate();
    let client_token = AuthToken::generate();
    // Paranoia: two independently generated 256-bit tokens must not collide.
    assert_ne!(
        server_token.bytes_vec(),
        client_token.bytes_vec(),
        "test invariant: generated tokens must differ"
    );

    let calls = Arc::new(Mutex::new(0u32));
    let fs = MockFs::new(calls.clone());
    let session = SessionBuilder::new(fs)
        .with_auth_token(server_token.bytes_vec())
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();
    let bad_auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest {
            token: client_token.bytes_vec(),
        })),
    };
    stream.write_all(&encode_request(&bad_auth)).await.unwrap();

    // Server must respond with posix_error = EACCES then close.
    let mut resp_buf = vec![0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut resp_buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0, "expected an error response from the server");

    let resp = decode_response(&resp_buf[..n]);
    assert!(
        matches!(resp.content, Some(response::Content::PosixError(e)) if e == libc::EACCES),
        "expected EACCES, got: {:?}",
        resp.content
    );

    // Connection must be closed after the rejection.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let follow_up = Request {
        id: 2,
        content: None,
    };
    let _ = stream.write_all(&encode_request(&follow_up)).await;
    let n2 = stream.read(&mut resp_buf).await.unwrap_or(0);
    assert_eq!(n2, 0, "server must have closed the connection after EACCES");

    session.shutdown().await;
    assert_eq!(
        *calls.lock().unwrap(),
        0,
        "rejected client must not reach the filesystem handler"
    );
}

/// After a successful authentication, sending a second `AuthenticateRequest`
/// must be rejected with EPROTO and the connection must be closed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn replay_authenticate_after_success_is_rejected() {
    let token = AuthToken::generate();
    let raw = token.bytes_vec();

    let calls = Arc::new(Mutex::new(0u32));
    let fs = MockFs::new(calls.clone());
    let session = SessionBuilder::new(fs)
        .with_auth_token(raw.clone())
        .bind_random()
        .await
        .unwrap();
    let port = session.port();

    let mut stream = TcpStream::connect(("127.0.0.1", port)).await.unwrap();

    // First auth — must succeed.
    let first_auth = Request {
        id: 1,
        content: Some(request::Content::Authenticate(AuthenticateRequest {
            token: raw.clone(),
        })),
    };
    stream
        .write_all(&encode_request(&first_auth))
        .await
        .unwrap();

    let mut resp_buf = vec![0u8; 256];
    let n = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut resp_buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n > 0, "expected Success response for first auth");
    let first_resp = decode_response(&resp_buf[..n]);
    assert!(
        matches!(first_resp.content, Some(response::Content::Success(_))),
        "first auth must succeed, got: {:?}",
        first_resp.content
    );

    // Replay the same Authenticate frame — must be rejected with EPROTO.
    let replay_auth = Request {
        id: 2,
        content: Some(request::Content::Authenticate(AuthenticateRequest {
            token: raw,
        })),
    };
    stream
        .write_all(&encode_request(&replay_auth))
        .await
        .unwrap();

    let n2 = tokio::time::timeout(Duration::from_secs(2), stream.read(&mut resp_buf))
        .await
        .unwrap()
        .unwrap();
    assert!(n2 > 0, "expected an error response for replayed auth");
    let replay_resp = decode_response(&resp_buf[..n2]);
    assert!(
        matches!(replay_resp.content, Some(response::Content::PosixError(e)) if e == libc::EPROTO),
        "expected EPROTO for replay, got: {:?}",
        replay_resp.content
    );

    // Connection must be closed after the EPROTO response.
    tokio::time::sleep(Duration::from_millis(100)).await;
    let follow_up = Request {
        id: 3,
        content: None,
    };
    let _ = stream.write_all(&encode_request(&follow_up)).await;
    let n3 = stream.read(&mut resp_buf).await.unwrap_or(0);
    assert_eq!(n3, 0, "server must have closed the connection after EPROTO");

    session.shutdown().await;
}
