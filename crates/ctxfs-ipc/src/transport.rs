use anyhow::{Context, Result};
use std::path::Path;
use tarpc::serde_transport::unix;
use tokio_serde::formats::Json;

/// Create a tarpc client connected to the daemon via UDS.
pub async fn connect_client(
    socket_path: &Path,
) -> Result<crate::service::CtxfsServiceClient> {
    let transport = unix::connect(socket_path, Json::default)
        .await
        .with_context(|| format!("failed to connect to {}", socket_path.display()))?;

    let client =
        crate::service::CtxfsServiceClient::new(tarpc::client::Config::default(), transport)
            .spawn();

    Ok(client)
}

/// Listen on a Unix domain socket and yield incoming transports.
pub async fn listen(
    socket_path: &Path,
) -> Result<
    impl futures::Stream<
        Item = std::io::Result<
            tarpc::serde_transport::Transport<
                tokio::net::UnixStream,
                tarpc::ClientMessage<crate::service::CtxfsServiceRequest>,
                tarpc::Response<crate::service::CtxfsServiceResponse>,
                Json<
                    tarpc::ClientMessage<crate::service::CtxfsServiceRequest>,
                    tarpc::Response<crate::service::CtxfsServiceResponse>,
                >,
            >,
        >,
    >,
> {
    // Remove stale socket unconditionally (avoids TOCTOU with exists+remove)
    if let Err(e) = std::fs::remove_file(socket_path) {
        if e.kind() != std::io::ErrorKind::NotFound {
            return Err(e)
                .with_context(|| format!("failed to remove stale socket {}", socket_path.display()));
        }
    }
    if let Some(parent) = socket_path.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let incoming = unix::listen(socket_path, Json::default)
        .await
        .with_context(|| format!("failed to listen on {}", socket_path.display()))?;

    Ok(incoming)
}
