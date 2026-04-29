//! Shared test infrastructure for replay tests.
//!
//! Provides a minimal hand-rolled HTTP/1.1 mock server (no TLS — replay tests
//! pass `http://127.0.0.1:PORT` as the `api_host` so the provider's scheme-
//! aware `api_url` generates plain-HTTP URLs), Git-blob SHA-1 helpers, and a
//! tarball builder for constructing test fixtures.

#![allow(clippy::unwrap_used, clippy::expect_used, unused_results, dead_code)]

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

// ─── Mock Route ──────────────────────────────────────────────────────────────

/// A single route in the mock HTTP server.
pub struct MockRoute {
    /// HTTP method to match (e.g. `"GET"`).
    pub method: &'static str,
    /// Path prefix to match (the request path must start with this).
    pub path_prefix: String,
    /// HTTP status code to return.
    pub status: u16,
    /// Extra response headers as `(name, value)` pairs.
    pub headers: Vec<(&'static str, String)>,
    /// Response body bytes.
    pub body: Vec<u8>,
    /// If `Some`, incremented each time this route is matched.
    pub hit_count: Option<Arc<AtomicU64>>,
    /// Optional delay in milliseconds before sending the response.
    /// Used by the singleflight test to ensure both callers are in-flight.
    pub delay_ms: Option<u64>,
}

// ─── Mock Server ─────────────────────────────────────────────────────────────

/// A minimal in-process HTTP/1.1 server for replay tests.
///
/// Constructed via [`MockServer::spawn`]. Dropped when the test ends; the
/// background task exits on the next `accept()` error after the listener is
/// closed.
pub struct MockServer {
    /// `http://127.0.0.1:PORT` — use as the `api_host` for `GitHubProvider`.
    pub host: String,
    pub port: u16,
    _handle: tokio::task::JoinHandle<()>,
}

impl MockServer {
    /// Bind a free port and start serving `routes` in the background.
    pub async fn spawn(routes: Vec<MockRoute>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let host = format!("http://127.0.0.1:{port}");

        let routes = Arc::new(routes);
        let handle = tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    break;
                };
                let routes = Arc::clone(&routes);
                tokio::spawn(handle_connection(stream, routes));
            }
        });

        Self {
            host,
            port,
            _handle: handle,
        }
    }
}

async fn handle_connection(mut stream: tokio::net::TcpStream, routes: Arc<Vec<MockRoute>>) {
    // Read the request headers (up to 16 KiB; sufficient for our test cases).
    let mut buf = vec![0u8; 16 * 1024];
    let n = stream.read(&mut buf).await.unwrap_or(0);
    if n == 0 {
        return;
    }

    let request_text = String::from_utf8_lossy(&buf[..n]);
    let first_line = request_text.lines().next().unwrap_or("");
    let parts: Vec<&str> = first_line.split_whitespace().collect();
    if parts.len() < 2 {
        return;
    }
    let method = parts[0];
    // Strip query string for prefix matching (the tree route uses `?recursive=1`).
    let full_path = parts[1];
    let path = full_path.split('?').next().unwrap_or(full_path);

    // Find the first matching route.
    if let Some(route) = routes
        .iter()
        .find(|r| r.method == method && path.starts_with(r.path_prefix.as_str()))
    {
        if let Some(ref counter) = route.hit_count {
            counter.fetch_add(1, Ordering::Relaxed);
        }
        if let Some(ms) = route.delay_ms {
            tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
        }
        let response = build_http_response(route.status, &route.headers, &route.body);
        let _ = stream.write_all(&response).await;
    } else {
        // Return 404 for unregistered routes so tests fail clearly.
        let body = format!("no route for {method} {path}").into_bytes();
        let response = build_http_response(404, &[], &body);
        let _ = stream.write_all(&response).await;
    }
}

fn build_http_response(status: u16, headers: &[(&str, String)], body: &[u8]) -> Vec<u8> {
    let status_text = match status {
        200 => "OK",
        302 => "Found",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    };
    let mut head = format!(
        "HTTP/1.1 {status} {status_text}\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    for (name, value) in headers {
        head.push_str(&format!("{name}: {value}\r\n"));
    }
    head.push_str("\r\n");
    let mut out = head.into_bytes();
    out.extend_from_slice(body);
    out
}

// ─── Git blob SHA-1 ──────────────────────────────────────────────────────────

/// Compute the Git blob SHA-1 for `content`.
///
/// The Git format is: `sha1("blob <size>\0" || content)`.
pub fn git_blob_sha1(content: &[u8]) -> String {
    use sha1::Digest as Sha1Digest;
    let mut hasher = sha1::Sha1::new();
    let header = format!("blob {}\0", content.len());
    hasher.update(header.as_bytes());
    hasher.update(content);
    hex::encode(hasher.finalize())
}

// ─── Tarball builder ─────────────────────────────────────────────────────────

/// Build a gzip-compressed tar archive from a list of `(path, content)` pairs.
///
/// Paths are stored raw in the tar header name field (no OS normalisation and no
/// `tar` crate validation), so callers can include `..` segments to test
/// path-traversal rejection in the provider's `validate_tar_entry_path`.
pub fn build_tarball(files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut tar_data = Vec::new();
    for (path, content) in files {
        write_raw_tar_entry(&mut tar_data, path.as_bytes(), content);
    }
    // Two 512-byte zero blocks terminate the archive.
    tar_data.extend(std::iter::repeat_n(0u8, 1024));

    // Gzip-compress the tar data.
    let mut gz_buf = Vec::new();
    {
        use std::io::Write;
        let mut gz = flate2::write::GzEncoder::new(&mut gz_buf, flate2::Compression::fast());
        gz.write_all(&tar_data).unwrap();
        gz.finish().unwrap();
    }
    gz_buf
}

/// Write one POSIX ustar tar entry (header + content) into `out`.
///
/// Path bytes are written verbatim into the 100-byte name field without any
/// validation, allowing `..` segments to survive for path-traversal tests.
fn write_raw_tar_entry(out: &mut Vec<u8>, path: &[u8], content: &[u8]) {
    let mut hdr = [0u8; 512];

    // name field: bytes 0–99 (null-padded).
    let name_len = path.len().min(99);
    hdr[0..name_len].copy_from_slice(&path[..name_len]);

    // mode: "0000644\0"
    hdr[100..108].copy_from_slice(b"0000644\0");
    // uid / gid: zeros
    hdr[108..116].copy_from_slice(b"0000000\0");
    hdr[116..124].copy_from_slice(b"0000000\0");

    // size: 11 octal digits + null (12 bytes total).
    let size = content.len();
    let size_oct = format!("{size:011o}\0");
    hdr[124..136].copy_from_slice(size_oct.as_bytes());

    // mtime: zeros
    hdr[136..148].copy_from_slice(b"00000000000\0");

    // checksum placeholder: 8 spaces (for calculation).
    hdr[148..156].copy_from_slice(b"        ");

    // typeflag: '0' = regular file.
    hdr[156] = b'0';

    // ustar magic + version (POSIX).
    hdr[257..263].copy_from_slice(b"ustar\0");
    hdr[263..265].copy_from_slice(b"00");

    // Compute checksum over header with spaces in the checksum field.
    let cksum: u32 = hdr.iter().map(|b| u32::from(*b)).sum();
    // Store: 6 octal digits, null, space — exactly 8 bytes.
    let cksum_str = format!("{cksum:06o}\0 ");
    hdr[148..156].copy_from_slice(cksum_str.as_bytes());

    out.extend_from_slice(&hdr);

    // Content followed by padding to next 512-byte boundary.
    out.extend_from_slice(content);
    let padding = (512 - (content.len() % 512)) % 512;
    out.extend(std::iter::repeat_n(0u8, padding));
}

/// Build a tarball that also includes the top-level wrapper directory entry
/// (as codeload tarballs do: `owner-repo-sha/`).
pub fn build_codeload_tarball(wrapper: &str, files: &[(String, Vec<u8>)]) -> Vec<u8> {
    let mut buf = Vec::new();
    {
        let gz = flate2::write::GzEncoder::new(&mut buf, flate2::Compression::fast());
        let mut archive = tar::Builder::new(gz);

        // Add wrapper directory entry.
        let mut dir_header = tar::Header::new_gnu();
        dir_header.set_size(0);
        dir_header.set_mode(0o755);
        dir_header.set_mtime(0);
        dir_header.set_entry_type(tar::EntryType::Directory);
        dir_header.set_cksum();
        archive
            .append_data(&mut dir_header, format!("{wrapper}/"), &[][..])
            .unwrap();

        // Add file entries.
        for (path, content) in files {
            let mut header = tar::Header::new_gnu();
            header.set_size(content.len() as u64);
            header.set_mode(0o644);
            header.set_mtime(0);
            header.set_entry_type(tar::EntryType::Regular);
            header.set_cksum();
            archive
                .append_data(&mut header, path.as_str(), content.as_slice())
                .unwrap();
        }
        let gz = archive.into_inner().unwrap();
        gz.finish().unwrap();
    }
    buf
}

// ─── Provider factory ─────────────────────────────────────────────────────────

/// Construct a `GitHubProvider` pointed at a local mock server.
///
/// Both `api_host` and `codeload_host` override point to the same mock server
/// so a single in-process listener can handle all routes.
pub fn make_provider(
    server: &MockServer,
    cache: Arc<ctxfs_cache::BlobCache>,
    observability: Arc<ctxfs_provider_common::observability::Observability>,
    tarball_singleflight: Arc<ctxfs_provider_common::fetcher::TarballSingleflightMap>,
) -> ctxfs_provider_git::GitHubProvider {
    ctxfs_provider_git::GitHubProvider::new_with_codeload_host(
        None,
        server.host.clone(),
        Some(server.host.clone()), // codeload override → same server
        cache,
        None,
        None,
        observability,
        tarball_singleflight,
    )
}

/// Build a JSON commit response body: `{"sha": "<sha>"}`.
pub fn commit_json(sha: &str) -> Vec<u8> {
    serde_json::json!({"sha": sha}).to_string().into_bytes()
}

/// Build a JSON tree response body.
pub fn tree_json(sha: &str, entries: &[serde_json::Value], truncated: bool) -> Vec<u8> {
    serde_json::json!({
        "sha": sha,
        "tree": entries,
        "truncated": truncated,
    })
    .to_string()
    .into_bytes()
}

/// Serialize a single tree entry as JSON for use in `tree_json`.
pub fn blob_entry(path: &str, sha: &str, size: u64) -> serde_json::Value {
    serde_json::json!({
        "path": path,
        "mode": "100644",
        "type": "blob",
        "sha": sha,
        "size": size,
    })
}
