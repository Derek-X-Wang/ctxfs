use std::fs;
use std::path::Path;
use std::process::Command;

pub(super) type Result<T> = std::result::Result<T, Error>;

pub(super) fn run(path: &Path, force: bool) -> Result<()> {
    if !path.exists() {
        return Err(Error::AppNotFound);
    }

    let apps = Path::new("/Applications");
    let app = apps.join(path.file_name().unwrap());

    // 1. rm -rf /Applications/<app>
    if app.exists() {
        if !force {
            return Err(Error::AppInstalled);
        }
        fs::remove_dir_all(&app)?;
    }

    // 2. cp -r <path_to_app> /Applications
    run_cmd(
        "cp",
        &["-r", path.to_str().unwrap(), apps.to_str().unwrap()],
    )?;

    // 3. xattr -dr com.apple.quarantine /Applications/<app>
    run_cmd(
        "xattr",
        &["-dr", "com.apple.quarantine", app.to_str().unwrap()],
    )?;

    // 4. open -a /Applications/<app> --args -s
    run_cmd("open", &["-a", app.to_str().unwrap(), "--args", "-s"])?;

    Ok(())
}

fn run_cmd(cmd: &'static str, args: &[&str]) -> Result<()> {
    let status = Command::new(cmd).args(args).status()?;
    if status.success() {
        Ok(())
    } else {
        Err(Error::CommandFailed {
            command: format!("{cmd} {}", args.join(" ")),
            status: status.to_string(),
        })
    }
}

#[derive(thiserror::Error, Debug)]
pub enum Error {
    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("host application not found")]
    AppNotFound,

    #[error("host application is already installed")]
    AppInstalled,

    #[error("command `{command}` failed: {status}")]
    CommandFailed { command: String, status: String },
}
