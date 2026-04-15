use std::path::Path;
use std::process::{Command, Output};

use log::error;
use regex::Regex;

use crate::handler::Handler;
use crate::info::Info;
use crate::mounter::Mounter;
use crate::socket::Socket;
use crate::{Filesystem, MountOptions, info, mounter, socket};

pub type Result<T> = std::result::Result<T, Error>;

#[derive(Debug)]
pub struct Session {
    socket: Socket,
    mounter: Mounter,
}

impl Session {
    pub(super) async fn new<FS>(fs: FS, opts: MountOptions) -> Result<Self>
    where
        FS: Filesystem + Send + Sync + Clone + 'static,
    {
        let (server_port, fs_type) = read_config(&opts.fskit_id)?;

        let handler = Handler::new(fs);

        let socket = Socket::start(handler, server_port).await?;

        let mounter = match Mounter::mount(opts, &fs_type) {
            Ok(mount) => mount,
            Err(err) => {
                socket.stop().await;
                return Err(Error::Mounter(err));
            }
        };

        Ok(Self { socket, mounter })
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.mounter.unmount().inspect_err(|err| error!("{err}"));

        futures::executor::block_on(async {
            self.socket.stop().await;
        });
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
