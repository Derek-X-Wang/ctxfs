use anyhow::{bail, Context, Result};
use clap::{Parser, Subcommand};
use ctxfs_core::config::Config;
use ctxfs_ipc::service::CtxfsServiceClient;
use ctxfs_ipc::transport;
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "ctxfs", about = "ContextFS — AI-native read-only mountable filesystem")]
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
        /// Maximum cache size (e.g., 500000000 for ~500MB)
        #[arg(long)]
        max_size: Option<u64>,
    },
}

#[tokio::main]
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
        } => {
            let mut client = connect(&config).await?;
            let mount_point_str = mount_point
                .to_str()
                .context("invalid mount point path")?
                .to_string();

            let info = client
                .mount(tarpc::context::current(), source, mount_point_str)
                .await?
                .map_err(|e| anyhow::anyhow!(e))?;

            println!("Mounted {} at {}", info.source, info.mount_point);
            println!("  Commit: {}", info.commit_sha);
            println!("  ID:     {}", info.id);
        }

        Commands::Unmount { target } => {
            let mut client = connect(&config).await?;
            client
                .unmount(tarpc::context::current(), target.clone())
                .await?
                .map_err(|e| anyhow::anyhow!(e))?;

            println!("Unmounted {target}");
        }

        Commands::List { json } => {
            let mut client = connect(&config).await?;
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
            let mut client = connect(&config).await?;
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
                let mut client = connect(&config).await?;
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
                            unsafe {
                                libc::kill(pid, libc::SIGTERM);
                            }
                            println!("Sent SIGTERM to daemon (PID {pid})");
                            return Ok(());
                        }
                    }
                }
                println!("Could not find daemon PID");
            }
            DaemonAction::Status => {
                match connect(&config).await {
                    Ok(mut client) => {
                        match client.ping(tarpc::context::current()).await {
                            Ok(resp) => println!("Daemon is running ({resp})"),
                            Err(e) => println!("Daemon unreachable: {e}"),
                        }
                    }
                    Err(_) => println!("Daemon is not running"),
                }
            }
        },

        Commands::Cache { action } => match action {
            CacheAction::Stats => {
                let mut client = connect(&config).await?;
                let stats = client
                    .cache_stats(tarpc::context::current())
                    .await?
                    .map_err(|e| anyhow::anyhow!(e))?;

                println!("Cache statistics:");
                println!("  Entries:     {}", stats.entry_count);
                println!("  Total size:  {} bytes", stats.total_bytes);
            }
            CacheAction::Prune { max_size } => {
                let mut client = connect(&config).await?;
                let stats = client
                    .cache_prune(tarpc::context::current(), max_size)
                    .await?
                    .map_err(|e| anyhow::anyhow!(e))?;

                println!("Cache pruned:");
                println!("  Freed:       {} bytes", stats.freed_bytes);
                println!("  Entries:     {}", stats.entry_count);
                println!("  Total size:  {} bytes", stats.total_bytes);
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
