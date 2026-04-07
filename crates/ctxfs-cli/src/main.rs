mod setup;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ctxfs_core::config::Config;
use ctxfs_ipc::service::CtxfsServiceClient;
use ctxfs_ipc::transport;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "ctxfs",
    about = "ContextFS — AI-native read-only mountable filesystem"
)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand)]
enum Commands {
    /// Mount a remote source as a local directory
    Mount {
        /// Source spec (e.g., github:owner/repo@ref)
        source: String,
        /// Local mount point
        mount_point: PathBuf,
        /// Start the daemon's NFS server for this source but skip the kernel
        /// mount step. Useful for debugging or when you want to run
        /// `mount_nfs` yourself with custom flags.
        #[arg(long)]
        server_only: bool,
    },
    /// Unmount a mounted filesystem
    Unmount {
        /// Mount point or mount ID
        target: String,
    },
    /// List active mounts
    List {
        /// Output as JSON
        #[arg(long)]
        json: bool,
    },
    /// Show status of a specific mount
    Status {
        /// Mount ID
        mount_id: String,
    },
    /// Daemon management
    Daemon {
        #[command(subcommand)]
        action: DaemonAction,
    },
    /// Cache management
    Cache {
        #[command(subcommand)]
        action: CacheAction,
    },
    /// One-time setup for passwordless NFS mounts
    Setup {
        #[command(subcommand)]
        action: SetupAction,
    },
}

#[derive(Subcommand)]
enum SetupAction {
    /// Install sudoers entry for passwordless mount/umount
    Install,
    /// Remove the sudoers entry
    Uninstall,
    /// Check if sudoers is already configured
    Check,
}

#[derive(Subcommand)]
enum DaemonAction {
    /// Start the daemon (foreground)
    Start,
    /// Stop a running daemon
    Stop,
    /// Check daemon status
    Status,
}

#[derive(Subcommand)]
enum CacheAction {
    /// Show cache statistics
    Stats,
    /// Prune cache to free space
    Prune {
        /// Maximum blob cache size (e.g., 500000000 for ~500MB)
        #[arg(long)]
        max_size: Option<u64>,
        /// Clear all cached tree manifests
        #[arg(long)]
        trees: bool,
        /// Clear all cached registry resolutions
        #[arg(long)]
        resolutions: bool,
    },
}

#[tokio::main]
#[allow(clippy::too_many_lines)] // CLI dispatch is naturally long
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info")),
        )
        .init();

    let cli = Cli::parse();
    let config = Config::from_env();

    match cli.command {
        Commands::Mount {
            source,
            mount_point,
            server_only,
        } => {
            let client = connect(&config).await?;
            let mount_point_str = mount_point
                .to_str()
                .context("invalid mount point path")?
                .to_string();

            std::fs::create_dir_all(&mount_point)
                .context("failed to create mount point directory")?;

            // Step 1: ask the daemon to spawn an NFS server for this source.
            let info = client
                .mount(long_context(), source.clone(), mount_point_str.clone())
                .await?
                .map_err(|e| anyhow::anyhow!(e))?;

            if server_only {
                println!("NFS server ready:");
                println!("  Source:   {}", info.source);
                println!("  Commit:   {}", info.commit_sha);
                println!("  ID:       {}", info.id);
                println!("  NFS port: {}", info.nfs_port);
                println!();
                println!("To mount manually, run:");
                #[cfg(target_os = "macos")]
                println!(
                    "  sudo mount_nfs -o nolocks,vers=3,tcp,port={p},mountport={p},soft \\",
                    p = info.nfs_port
                );
                #[cfg(target_os = "linux")]
                println!(
                    "  sudo mount -t nfs -o nolock,vers=3,tcp,port={p},mountport={p},soft \\",
                    p = info.nfs_port
                );
                println!("    127.0.0.1:/ {}", info.mount_point);
                return Ok(());
            }

            // Step 2: run the OS mount command (needs sudo on macOS).
            println!(
                "NFS server listening on 127.0.0.1:{} — mounting kernel side (may prompt for sudo)",
                info.nfs_port
            );
            if let Err(e) = run_mount_nfs(info.nfs_port, &mount_point_str) {
                // If the kernel mount fails, ask the daemon to stop the NFS server.
                let _ = client
                    .unmount(tarpc::context::current(), mount_point_str.clone())
                    .await;
                return Err(anyhow::anyhow!("kernel mount failed: {e}"));
            }

            println!("Mounted {} at {}", info.source, info.mount_point);
            println!("  Commit:   {}", info.commit_sha);
            println!("  ID:       {}", info.id);
            println!("  NFS port: {}", info.nfs_port);
        }

        Commands::Unmount { target } => {
            // Step 1: unmount the kernel mount first (may prompt for sudo).
            if let Err(e) = run_umount(&target) {
                eprintln!("warning: kernel umount failed: {e}");
            }

            // Step 2: ask the daemon to stop the NFS server for this mount.
            let client = connect(&config).await?;
            client
                .unmount(tarpc::context::current(), target.clone())
                .await?
                .map_err(|e| anyhow::anyhow!(e))?;

            println!("Unmounted {target}");
        }

        Commands::List { json } => {
            let client = connect(&config).await?;
            let mounts = client.list(tarpc::context::current()).await?;

            if json {
                println!("{}", serde_json::to_string_pretty(&mounts)?);
            } else if mounts.is_empty() {
                println!("No active mounts");
            } else {
                println!(
                    "{:<30} {:<40} {:<10} {:<12}",
                    "SOURCE", "MOUNT POINT", "STATUS", "COMMIT"
                );
                for m in &mounts {
                    println!(
                        "{:<30} {:<40} {:<10} {:<12}",
                        m.source,
                        m.mount_point,
                        m.status.to_string(),
                        &m.commit_sha[..12.min(m.commit_sha.len())]
                    );
                }
            }
        }

        Commands::Status { mount_id } => {
            let client = connect(&config).await?;
            let info = client
                .status(tarpc::context::current(), mount_id)
                .await?
                .map_err(|e| anyhow::anyhow!(e))?;

            println!("Mount: {}", info.id);
            println!("  Source:      {}", info.source);
            println!("  Mount point: {}", info.mount_point);
            println!("  Commit:      {}", info.commit_sha);
            println!("  Status:      {}", info.status);
            println!("  Mounted at:  {}", info.mounted_at);
        }

        Commands::Daemon { action } => match action {
            DaemonAction::Start => {
                println!("Starting ctxfs daemon...");
                let daemon = ctxfs_daemon::Daemon::new(config)?;
                daemon.run().await?;
            }
            DaemonAction::Stop => {
                let client = connect(&config).await?;
                // Unmount everything, then the daemon will notice and exit
                let mounts = client.list(tarpc::context::current()).await?;
                for m in &mounts {
                    let _ = client
                        .unmount(tarpc::context::current(), m.id.clone())
                        .await;
                }
                // Send a signal to the PID
                if config.pid_file.exists() {
                    if let Ok(pid_str) = std::fs::read_to_string(&config.pid_file) {
                        if let Ok(pid) = pid_str.trim().parse::<i32>() {
                            // SAFETY: sending SIGTERM to a known PID
                            #[allow(unsafe_code)]
                            let _ = unsafe { libc::kill(pid, libc::SIGTERM) };
                            println!("Sent SIGTERM to daemon (PID {pid})");
                            return Ok(());
                        }
                    }
                }
                println!("Could not find daemon PID");
            }
            DaemonAction::Status => match connect(&config).await {
                Ok(client) => match client.ping(tarpc::context::current()).await {
                    Ok(resp) => println!("Daemon is running ({resp})"),
                    Err(e) => println!("Daemon unreachable: {e}"),
                },
                Err(_) => println!("Daemon is not running"),
            },
        },

        Commands::Cache { action } => match action {
            CacheAction::Stats => {
                let client = connect(&config).await?;
                let stats = client
                    .cache_stats(tarpc::context::current())
                    .await?
                    .map_err(|e| anyhow::anyhow!(e))?;

                println!("Cache statistics:");
                println!(
                    "  Blobs:        {} entries, {} bytes",
                    stats.entry_count, stats.total_bytes
                );
                println!(
                    "  Trees:        {} entries, {} bytes",
                    stats.tree_count, stats.tree_bytes
                );
                println!("  Resolutions:  {} entries", stats.resolution_count);
            }
            CacheAction::Prune {
                max_size,
                trees: _,
                resolutions: _,
            } => {
                let client = connect(&config).await?;
                let stats = client
                    .cache_prune(tarpc::context::current(), max_size)
                    .await?
                    .map_err(|e| anyhow::anyhow!(e))?;

                println!("Cache pruned:");
                println!("  Freed:       {} bytes", stats.freed_bytes);
                println!(
                    "  Blobs:       {} entries, {} bytes",
                    stats.entry_count, stats.total_bytes
                );
                println!(
                    "  Trees:       {} entries, {} bytes",
                    stats.tree_count, stats.tree_bytes
                );
                println!("  Resolutions: {} entries", stats.resolution_count);
            }
        },

        Commands::Setup { action } => match action {
            SetupAction::Install => {
                setup::install_sudoers()?;
            }
            SetupAction::Uninstall => {
                setup::uninstall_sudoers()?;
            }
            SetupAction::Check => {
                let username = whoami::username();
                if setup::is_configured(&username) {
                    println!("Configured: /etc/sudoers.d/ctxfs exists for user '{username}'.");
                    println!("mount/umount will not prompt for a password.");
                } else {
                    println!("Not configured. Run `ctxfs setup install` for passwordless mounts.");
                }
            }
        },
    }

    Ok(())
}

async fn connect(config: &Config) -> Result<CtxfsServiceClient> {
    transport::connect_client(&config.socket_path)
        .await
        .context(format!(
            "failed to connect to daemon at {}. Start with: ctxfs daemon start",
            config.socket_path.display()
        ))
}

/// Context with a longer deadline for operations that call external APIs.
fn long_context() -> tarpc::context::Context {
    let mut ctx = tarpc::context::current();
    ctx.deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    ctx
}

/// Invoke the OS-native NFS mount command against the daemon's loopback NFS
/// server. Requires sudo on macOS and Linux (kernel restriction).
fn run_mount_nfs(port: u16, mount_point: &str) -> Result<()> {
    let opts = format!(
        "nolocks,vers=3,tcp,port={port},mountport={port},soft,actimeo=120,rsize=131072,wsize=131072"
    );

    #[cfg(target_os = "macos")]
    let status = std::process::Command::new("sudo")
        .args(["mount_nfs", "-o", opts.as_str(), "127.0.0.1:/", mount_point])
        .status()
        .context("failed to invoke sudo mount_nfs")?;

    #[cfg(target_os = "linux")]
    let status = std::process::Command::new("sudo")
        .args([
            "mount",
            "-t",
            "nfs",
            "-o",
            opts.as_str(),
            "127.0.0.1:/",
            mount_point,
        ])
        .status()
        .context("failed to invoke sudo mount")?;

    #[cfg(not(any(target_os = "macos", target_os = "linux")))]
    anyhow::bail!("NFS mount not supported on this platform");

    if !status.success() {
        anyhow::bail!("mount command exited with {status}");
    }
    Ok(())
}

/// Invoke the OS-native `umount` against a mount point. Requires sudo.
fn run_umount(mount_point: &str) -> Result<()> {
    let status = std::process::Command::new("sudo")
        .args(["umount", mount_point])
        .status()
        .context("failed to invoke sudo umount")?;

    if !status.success() {
        anyhow::bail!("umount exited with {status}");
    }
    Ok(())
}
