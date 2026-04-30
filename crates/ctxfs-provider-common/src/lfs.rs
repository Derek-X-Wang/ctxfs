//! Git LFS pointer detection.
//!
//! Pure-bytes helpers used by GitHub fetch paths to detect LFS pointer
//! files. Phase 4 surfaces the count + sample paths in `ctxfs status`;
//! Phase 5 will smudge to real bytes via the LFS smudge endpoint.
//!
//! Manual parser instead of `regex` to keep the no-new-deps promise.
//! The pointer format is rigid: exactly three newline-terminated lines.

/// Parsed LFS pointer fields. The pointer-content bytes themselves stay in
/// the cache verbatim (M5 surfaces only; smudge is Phase 5).
#[derive(Debug, Clone, Eq, PartialEq)]
pub struct LfsPointerInfo {
    /// The `oid sha256:<hex>` value — the SHA-256 of the actual object.
    pub oid_sha256: String,
    /// Object size in bytes as declared in the pointer.
    pub size: u64,
}

const LFS_VERSION_LINE: &str = "version https://git-lfs.github.com/spec/v1";
const LFS_OID_PREFIX: &str = "oid sha256:";
const LFS_SIZE_PREFIX: &str = "size ";
const LFS_POINTER_MAX_BYTES: usize = 1024;

/// Detect a Git LFS pointer file. The pointer format is well-defined:
/// three lines (`version`, `oid sha256:<64-hex>`, `size <decimal>`), each
/// newline-terminated, with no trailing content. Returns `Some(info)` only
/// when the entire input matches; `None` otherwise.
///
/// The 1024-byte cap fast-paths non-LFS reads — pointers are ≤ ~200 bytes.
/// False-positive rate against real source trees is essentially zero.
#[must_use]
pub fn detect_lfs_pointer(bytes: &[u8]) -> Option<LfsPointerInfo> {
    if bytes.is_empty() || bytes.len() > LFS_POINTER_MAX_BYTES {
        return None;
    }
    let s = std::str::from_utf8(bytes).ok()?;

    // Split into exactly four pieces: three content lines + the empty
    // remainder after the trailing newline. Anything else is rejected.
    let mut iter = s.split('\n');
    let v_line = iter.next()?;
    let o_line = iter.next()?;
    let s_line = iter.next()?;
    let trailer = iter.next()?;
    if iter.next().is_some() {
        return None; // extra newline / content after size line
    }
    if !trailer.is_empty() {
        return None; // bytes after the size line's terminator
    }

    if v_line != LFS_VERSION_LINE {
        return None;
    }
    let oid = o_line.strip_prefix(LFS_OID_PREFIX)?;
    if oid.len() != 64 || !oid.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    let size_str = s_line.strip_prefix(LFS_SIZE_PREFIX)?;
    let size: u64 = size_str.parse().ok()?;

    Some(LfsPointerInfo {
        oid_sha256: oid.to_string(),
        size,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pointer(oid: &str, size: u64) -> Vec<u8> {
        format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize {size}\n")
            .into_bytes()
    }

    #[test]
    fn detects_canonical_pointer() {
        let oid = "a".repeat(64);
        let info = detect_lfs_pointer(&pointer(&oid, 12345)).expect("matches");
        assert_eq!(info.oid_sha256, oid);
        assert_eq!(info.size, 12345);
    }

    #[test]
    fn rejects_non_pointer_text() {
        let bytes = b"hello world\n";
        assert!(detect_lfs_pointer(bytes).is_none());
    }

    #[test]
    fn rejects_truncated_pointer_missing_final_newline() {
        let bytes = b"version https://git-lfs.github.com/spec/v1\noid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize 1";
        assert!(detect_lfs_pointer(bytes).is_none());
    }

    #[test]
    fn rejects_oversized_input() {
        let bytes = vec![b'a'; 2048];
        assert!(detect_lfs_pointer(&bytes).is_none());
    }

    #[test]
    fn rejects_empty_input() {
        assert!(detect_lfs_pointer(&[]).is_none());
    }

    #[test]
    fn rejects_extra_trailing_content() {
        let oid = "f".repeat(64);
        let mut bytes = pointer(&oid, 10);
        bytes.extend_from_slice(b"trailing junk");
        assert!(detect_lfs_pointer(&bytes).is_none());
    }

    #[test]
    fn rejects_wrong_version_url() {
        let bytes = b"version https://git-lfs.gitlab.com/spec/v1\noid sha256:aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa\nsize 1\n";
        assert!(detect_lfs_pointer(bytes).is_none());
    }

    #[test]
    fn rejects_non_hex_oid() {
        let bytes = b"version https://git-lfs.github.com/spec/v1\noid sha256:zzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzzz\nsize 1\n";
        assert!(detect_lfs_pointer(bytes).is_none());
    }

    #[test]
    fn rejects_non_decimal_size() {
        let oid = "a".repeat(64);
        let bytes =
            format!("version https://git-lfs.github.com/spec/v1\noid sha256:{oid}\nsize one\n");
        assert!(detect_lfs_pointer(bytes.as_bytes()).is_none());
    }

    #[test]
    fn rejects_crlf_line_endings() {
        // Windows-format pointer with \r\n line endings: parser rejects
        // because the trailing \r on the version line breaks the exact
        // string match with LFS_VERSION_LINE.
        let oid = "a".repeat(64);
        let bytes =
            format!("version https://git-lfs.github.com/spec/v1\r\noid sha256:{oid}\r\nsize 1\r\n");
        assert!(detect_lfs_pointer(bytes.as_bytes()).is_none());
    }
}
