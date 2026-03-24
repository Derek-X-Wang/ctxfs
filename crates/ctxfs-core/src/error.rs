use thiserror::Error;

#[derive(Error, Debug)]
pub enum CtxfsError {
    #[error("provider error: {0}")]
    Provider(String),

    #[error("cache error: {0}")]
    Cache(String),

    #[error("manifest error: {0}")]
    Manifest(String),

    #[error("IPC error: {0}")]
    Ipc(String),

    #[error("not found: {0}")]
    NotFound(String),

    #[error("invalid source: {0}")]
    InvalidSource(String),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("rate limited: retry after {retry_after_secs}s")]
    RateLimited { retry_after_secs: u64 },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn error_display_messages() {
        let e = CtxfsError::Provider("connection timeout".into());
        assert_eq!(e.to_string(), "provider error: connection timeout");

        let e = CtxfsError::Cache("disk full".into());
        assert_eq!(e.to_string(), "cache error: disk full");

        let e = CtxfsError::NotFound("blob abc".into());
        assert_eq!(e.to_string(), "not found: blob abc");

        let e = CtxfsError::InvalidSource("bad format".into());
        assert_eq!(e.to_string(), "invalid source: bad format");

        let e = CtxfsError::RateLimited {
            retry_after_secs: 30,
        };
        assert_eq!(e.to_string(), "rate limited: retry after 30s");

        let e = CtxfsError::Manifest("corrupt".into());
        assert_eq!(e.to_string(), "manifest error: corrupt");

        let e = CtxfsError::Ipc("disconnected".into());
        assert_eq!(e.to_string(), "IPC error: disconnected");
    }

    #[test]
    fn io_error_conversion() {
        let io_err = std::io::Error::new(std::io::ErrorKind::NotFound, "file missing");
        let ctxfs_err = CtxfsError::from(io_err);
        assert!(matches!(ctxfs_err, CtxfsError::Io(_)));
        assert!(ctxfs_err.to_string().contains("file missing"));
    }
}
