use std::net::Ipv4Addr;
use std::path::Path;
use std::process::{Command, Output};

use log::error;
use regex::Regex;
use tokio::net::TcpListener;

use crate::handler::Handler;
use crate::info::Info;
use crate::mounter::Mounter;
use crate::socket::Socket;
use crate::{Filesystem, MountOptions, info, mounter, socket};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Session {
    socket: Socket,
    mounter: Option<Mounter>,
    port: u16,
}

impl Session {
    pub(super) async fn new<FS>(fs: FS, opts: MountOptions) -> Result<Self>
    where
        FS: Filesystem + Send + Sync + Clone + 'static,
    {
        let (server_port, fs_type) = read_config(&opts.fskit_id)?;

        let handler = Handler::new(fs);

        let socket = Socket::start(handler, server_port, opts.auth_token.clone()).await?;

        let mounter = match Mounter::mount(opts, &fs_type) {
            Ok(mount) => mount,
            Err(err) => {
                socket.stop().await;
                return Err(Error::Mounter(err));
            }
        };

        Ok(Self {
            socket,
            mounter: Some(mounter),
            port: server_port,
        })
    }

    /// Returns the local port the listener is bound to.
    pub fn port(&self) -> u16 {
        self.port
    }

    /// Stops the listener. Useful in tests that don't have a mounter.
    pub async fn shutdown(&self) {
        self.socket.stop().await;
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        if let Some(ref mut mounter) = self.mounter {
            let _ = mounter.unmount().inspect_err(|err| error!("{err}"));
        }

        futures::executor::block_on(async {
            self.socket.stop().await;
        });
    }
}

/// Builder for creating a `Session` with optional auth token enforcement.
///
/// Use this when you need fine-grained control over the session configuration,
/// e.g. to require an auth token on every TCP connection or to bind to a
/// random port for testing.
#[derive(Debug)]
pub struct SessionBuilder<FS> {
    fs: FS,
    auth_token: Option<Vec<u8>>,
}

impl<FS> SessionBuilder<FS>
where
    FS: Filesystem + Send + Sync + Clone + 'static,
{
    /// Create a new builder with the given filesystem implementation.
    pub fn new(fs: FS) -> Self {
        Self {
            fs,
            auth_token: None,
        }
    }

    /// Require all TCP connections to authenticate with this token as the
    /// first frame before any VFS requests are dispatched.
    ///
    /// If not set, the listener accepts all connections (backward-compatible
    /// with upstream behavior).
    #[must_use]
    pub fn with_auth_token(mut self, token: Vec<u8>) -> Self {
        self.auth_token = Some(token);
        self
    }

    /// Bind to a random localhost port and start the accept loop.
    ///
    /// Returns a [`Session`] with [`Session::port`] reporting the actual port.
    /// This is useful for integration tests that need a live listener without
    /// a registered FSKit extension.
    pub async fn bind_random(self) -> Result<Session> {
        let listener = TcpListener::bind((Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|e| Error::Socket(socket::Error::Io(e)))?;
        let port = listener
            .local_addr()
            .map_err(|e| Error::Socket(socket::Error::Io(e)))?
            .port();

        let handler = Handler::new(self.fs);
        let socket = Socket::start_with_listener(handler, listener, self.auth_token).await?;

        Ok(Session {
            socket,
            mounter: None,
            port,
        })
    }
}

fn read_config(fskit_id: &str) -> Result<(u16, String)> {
    // Get the output of the 'pluginkit' command
    // pluginkit -m -i <fskit_id> --raw
    let output = Command::new("pluginkit")
        .args(["-m", "-i", fskit_id, "--raw"])
        .output()?;
    if !output.status.success() {
        error!(
            "failed to query pluginkit for {fskit_id}: {}",
            describe_failure(&output)
        );
        return Err(Error::ExtensionNotRegistered);
    }

    let stdout = String::from_utf8_lossy(&output.stdout);

    // Find the full path to appex
    let reg = Regex::new(r#"(?m)^\s*path = "([^"]+)";"#).unwrap();
    let Some(line) = reg.captures_iter(&stdout).last() else {
        error!("pluginkit did not return a registered path for {fskit_id}");
        return Err(Error::ExtensionNotRegistered);
    };

    // Get configuration
    let info = Info::new(Path::new(&line[1]))?;
    Ok((info.server_port()?, info.fs_type()?))
}

pub(super) fn describe_failure(output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    if stderr.is_empty() {
        output.status.to_string()
    } else {
        stderr
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("file system extension not registered")]
    ExtensionNotRegistered,

    #[error(transparent)]
    Info(#[from] info::Error),

    #[error(transparent)]
    Socket(#[from] socket::Error),

    #[error(transparent)]
    Mounter(#[from] mounter::Error),
}
