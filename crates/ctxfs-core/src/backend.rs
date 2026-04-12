use std::fmt;
use std::str::FromStr;

use serde::{Deserialize, Serialize};

use crate::error::CtxfsError;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Nfs,
    FsKit,
}

impl fmt::Display for Backend {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Backend::Nfs => write!(f, "nfs"),
            Backend::FsKit => write!(f, "fskit"),
        }
    }
}

impl FromStr for Backend {
    type Err = CtxfsError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s {
            "nfs" => Ok(Backend::Nfs),
            "fskit" => Ok(Backend::FsKit),
            other => Err(CtxfsError::InvalidSource(format!(
                "unsupported backend '{other}', expected 'nfs' or 'fskit'"
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_nfs() {
        assert_eq!(Backend::Nfs.to_string(), "nfs");
    }

    #[test]
    fn display_fskit() {
        assert_eq!(Backend::FsKit.to_string(), "fskit");
    }

    #[test]
    fn from_str_nfs() {
        let b: Backend = "nfs".parse().unwrap();
        assert_eq!(b, Backend::Nfs);
    }

    #[test]
    fn from_str_fskit() {
        let b: Backend = "fskit".parse().unwrap();
        assert_eq!(b, Backend::FsKit);
    }

    #[test]
    fn from_str_invalid() {
        let err = "fuse".parse::<Backend>().unwrap_err();
        assert!(err.to_string().contains("fuse"));
        assert!(matches!(err, CtxfsError::InvalidSource(_)));
    }

    #[test]
    fn from_str_empty_invalid() {
        let err = "".parse::<Backend>().unwrap_err();
        assert!(matches!(err, CtxfsError::InvalidSource(_)));
    }

    #[test]
    fn serde_roundtrip_nfs() {
        let b = Backend::Nfs;
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, "\"nfs\"");
        let b2: Backend = serde_json::from_str(&json).unwrap();
        assert_eq!(b, b2);
    }

    #[test]
    fn serde_roundtrip_fskit() {
        let b = Backend::FsKit;
        let json = serde_json::to_string(&b).unwrap();
        assert_eq!(json, "\"fskit\"");
        let b2: Backend = serde_json::from_str(&json).unwrap();
        assert_eq!(b, b2);
    }

    #[test]
    fn display_roundtrip_via_fromstr() {
        for backend in [Backend::Nfs, Backend::FsKit] {
            let s = backend.to_string();
            let parsed: Backend = s.parse().unwrap();
            assert_eq!(backend, parsed);
        }
    }
}
