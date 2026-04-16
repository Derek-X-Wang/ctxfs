//! Constant-time token comparison for the bridge authentication handshake.
//!
//! This lives inside fskit-rs (rather than in ctxfs-fskit) because the socket
//! accept loop needs it directly and we want zero layering between the bytes
//! coming off the wire and the compare.

/// Constant-time compare of a received token against an expected token.
///
/// Returns `true` only if the slices are the same length AND every byte
/// matches. Length check happens first; byte-compare is constant-time over
/// the common prefix.
#[must_use]
pub fn verify_token_ct(expected: &[u8], candidate: &[u8]) -> bool {
    if expected.len() != candidate.len() {
        return false;
    }
    let mut diff: u8 = 0;
    for (a, b) in expected.iter().zip(candidate.iter()) {
        diff |= a ^ b;
    }
    diff == 0
}
