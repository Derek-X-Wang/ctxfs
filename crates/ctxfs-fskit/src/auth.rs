use std::fmt;

/// Per-mount 256-bit authentication token.
#[derive(Clone)]
pub struct AuthToken {
    bytes: [u8; 32],
}

impl fmt::Debug for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AuthToken")
            .field("bytes", &"[redacted]")
            .finish()
    }
}

impl fmt::Display for AuthToken {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.to_hex())
    }
}

impl AuthToken {
    /// Generate a new random 256-bit token.
    pub fn generate() -> Self {
        use rand::RngCore as _;
        let mut bytes = [0u8; 32];
        rand::thread_rng().fill_bytes(&mut bytes);
        Self { bytes }
    }

    /// Deserialize from a hex string (e.g., from mounts.json).
    pub fn from_hex(s: &str) -> Result<Self, hex::FromHexError> {
        let decoded = hex::decode(s)?;
        let bytes: [u8; 32] = decoded
            .try_into()
            .map_err(|_| hex::FromHexError::InvalidStringLength)?;
        Ok(Self { bytes })
    }

    /// Serialize to a hex string (e.g., for mounts.json).
    pub fn to_hex(&self) -> String {
        hex::encode(self.bytes)
    }

    /// Constant-time comparison to validate a candidate byte slice.
    pub fn validate(&self, candidate: &[u8]) -> bool {
        if candidate.len() != 32 {
            return false;
        }
        // Constant-time comparison to avoid timing attacks.
        let mut diff: u8 = 0;
        for (a, b) in self.bytes.iter().zip(candidate.iter()) {
            diff |= a ^ b;
        }
        diff == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_produces_unique_tokens() {
        let t1 = AuthToken::generate();
        let t2 = AuthToken::generate();
        assert_ne!(t1.bytes, t2.bytes);
    }

    #[test]
    fn hex_roundtrip() {
        let token = AuthToken::generate();
        let hex = token.to_hex();
        let recovered = AuthToken::from_hex(&hex).expect("valid hex");
        assert_eq!(token.bytes, recovered.bytes);
    }

    #[test]
    fn validate_correct_token() {
        let token = AuthToken::generate();
        assert!(token.validate(&token.bytes));
    }

    #[test]
    fn validate_wrong_token() {
        let token = AuthToken::generate();
        let mut wrong = token.bytes;
        wrong[0] ^= 0xFF;
        assert!(!token.validate(&wrong));
    }

    #[test]
    fn validate_wrong_length() {
        let token = AuthToken::generate();
        assert!(!token.validate(&token.bytes[..16]));
    }

    #[test]
    fn from_hex_invalid() {
        let result = AuthToken::from_hex("not_valid_hex!!");
        assert!(result.is_err());
    }
}
