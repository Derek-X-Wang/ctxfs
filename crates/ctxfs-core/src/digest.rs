use serde::{Deserialize, Serialize};
use sha2::{Digest as Sha2Digest, Sha256};
use std::fmt;
use std::path::PathBuf;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum HashAlgorithm {
    Sha256,
    Sha1,
}

impl fmt::Display for HashAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            HashAlgorithm::Sha256 => write!(f, "sha256"),
            HashAlgorithm::Sha1 => write!(f, "sha1"),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct Digest {
    pub algorithm: HashAlgorithm,
    pub hex: String,
}

impl Digest {
    pub fn sha256(data: &[u8]) -> Self {
        let mut hasher = Sha256::new();
        hasher.update(data);
        let result = hasher.finalize();
        Self {
            algorithm: HashAlgorithm::Sha256,
            hex: hex::encode(result),
        }
    }

    /// Construct a Digest from an existing SHA-256 hex string (e.g. from Git).
    pub fn from_sha256_hex(hex_str: impl Into<String>) -> Self {
        Self {
            algorithm: HashAlgorithm::Sha256,
            hex: hex_str.into(),
        }
    }

    /// Construct a Digest from an existing SHA-1 hex string. The 40-char
    /// hexes returned by the GitHub Trees API are Git blob SHA-1s; this
    /// constructor labels them correctly so future readers don't conclude
    /// the cache stores SHA-256 content.
    pub fn from_sha1_hex(hex_str: impl Into<String>) -> Self {
        Self {
            algorithm: HashAlgorithm::Sha1,
            hex: hex_str.into(),
        }
    }

    /// Return a fan-out path: `sha256/ab/cdef0123...`
    pub fn to_path(&self) -> PathBuf {
        let mut p = PathBuf::new();
        p.push(self.algorithm.to_string());
        p.push(&self.hex[..2]);
        p.push(&self.hex[2..]);
        p
    }
}

impl fmt::Display for Digest {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:{}", self.algorithm, self.hex)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_hello_world() {
        let d = Digest::sha256(b"hello world");
        assert_eq!(d.algorithm, HashAlgorithm::Sha256);
        assert_eq!(
            d.hex,
            "b94d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn to_path_fan_out() {
        let d = Digest::sha256(b"hello world");
        let path = d.to_path();
        assert_eq!(
            path.to_str().unwrap(),
            "sha256/b9/4d27b9934d3e08a52e52d7da7dabfac484efe37a5380ee9088f7ace2efcde9"
        );
    }

    #[test]
    fn display() {
        let d = Digest::sha256(b"test");
        assert!(d.to_string().starts_with("sha256:"));
    }

    #[test]
    fn sha256_empty_input() {
        let d = Digest::sha256(b"");
        assert_eq!(
            d.hex,
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[test]
    fn from_sha256_hex() {
        let d = Digest::from_sha256_hex("abcdef0123456789");
        assert_eq!(d.algorithm, HashAlgorithm::Sha256);
        assert_eq!(d.hex, "abcdef0123456789");
    }

    #[test]
    fn equality_and_hash() {
        use std::collections::HashSet;
        let d1 = Digest::sha256(b"same");
        let d2 = Digest::sha256(b"same");
        let d3 = Digest::sha256(b"different");
        assert_eq!(d1, d2);
        assert_ne!(d1, d3);

        let mut set = HashSet::new();
        let _ = set.insert(d1.clone());
        assert!(set.contains(&d2));
        assert!(!set.contains(&d3));
    }

    #[test]
    fn serde_roundtrip() {
        let d = Digest::sha256(b"serde test");
        let json = serde_json::to_string(&d).unwrap();
        let d2: Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(d, d2);
    }

    #[test]
    fn hash_algorithm_display() {
        assert_eq!(HashAlgorithm::Sha256.to_string(), "sha256");
    }

    #[test]
    fn different_inputs_different_digests() {
        let d1 = Digest::sha256(b"input1");
        let d2 = Digest::sha256(b"input2");
        assert_ne!(d1.hex, d2.hex);
    }

    #[test]
    fn from_sha1_hex_roundtrip() {
        let git_blob_sha1 = "356a192b7913b04c54574d18c28d46e6395428ab";
        let d = Digest::from_sha1_hex(git_blob_sha1);
        assert_eq!(d.algorithm, HashAlgorithm::Sha1);
        assert_eq!(d.hex, git_blob_sha1);
    }

    #[test]
    fn sha1_to_path_uses_sha1_subdir() {
        let d = Digest::from_sha1_hex("356a192b7913b04c54574d18c28d46e6395428ab");
        assert_eq!(
            d.to_path().to_str().unwrap(),
            "sha1/35/6a192b7913b04c54574d18c28d46e6395428ab"
        );
    }

    #[test]
    fn hash_algorithm_sha1_display() {
        assert_eq!(HashAlgorithm::Sha1.to_string(), "sha1");
    }

    #[test]
    fn sha1_serde_roundtrip() {
        let d = Digest::from_sha1_hex("aabbccdd00112233445566778899aabbccddeeff");
        let json = serde_json::to_string(&d).unwrap();
        let d2: Digest = serde_json::from_str(&json).unwrap();
        assert_eq!(d2, d);
        assert_eq!(d2.algorithm, HashAlgorithm::Sha1);
    }
}
