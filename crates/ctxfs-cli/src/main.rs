mod backend;
mod deps;
mod setup;
mod symlink;

use std::collections::HashMap;
use std::io::IsTerminal;
use std::path::PathBuf;

use anyhow::{Context, Result};
use clap::{Parser, Subcommand};
use ctxfs_core::config::Config;
use ctxfs_core::Backend;
use ctxfs_ipc::service::CtxfsServiceClient;
use ctxfs_ipc::transport;

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
        /// Source spec(s) (e.g., github:owner/repo@ref)
        #[arg(required = true)]
        sources: Vec<String>,
        /// Local mount point (single source only; mutually exclusive with --mount-dir)
        #[arg(long, short = 'p')]
        mount_point: Option<PathBuf>,
        /// Base directory for auto-derived mount points
        #[arg(long, short = 'd')]
        mount_dir: Option<PathBuf>,
        /// Start the daemon's NFS server for this source but skip the kernel
        /// mount step. Useful for debugging or when you want to run
        /// `mount_nfs` yourself with custom flags.
        #[arg(long)]
        server_only: bool,
        /// Backend to use for mounting (nfs or fskit); overrides env and config
        #[arg(long, value_parser = clap::value_parser!(Backend))]
        backend: Option<Backend>,
    },
    /// Unmount a mounted filesystem
    Unmount {
        /// Mount point or mount ID (required unless --all)
        target: Option<String>,
        /// Unmount all active mounts
        #[arg(long)]
        all: bool,
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
    /// Dependency detection and batch mounting
    Deps {
        #[command(subcommand)]
        action: DepsAction,
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
    /// Install FSKit extension for macOS 26+ (no sudo, no FDA).
    InstallFskit,
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

#[derive(Subcommand)]
enum DepsAction {
    /// List detected dependencies
    List {
        /// Project directory to scan
        #[arg(default_value = ".")]
        project_dir: PathBuf,
        /// Output as JSON
        #[arg(long)]
        json: bool,
        /// Include dev dependencies
        #[arg(long)]
        include_dev: bool,
    },
    /// Mount detected dependencies
    Mount {
        /// Project directory to scan
        #[arg(default_value = ".")]
        project_dir: PathBuf,
        /// Mount all detected dependencies (non-interactive)
        #[arg(long)]
        all: bool,
        /// Select specific dependencies by name (comma-separated)
        #[arg(long, value_delimiter = ',')]
        select: Option<Vec<String>>,
        /// Include dev dependencies
        #[arg(long)]
        include_dev: bool,
        /// Base directory for auto-derived mount points
        #[arg(long, short = 'd', default_value = "./ctxfs-deps")]
        mount_dir: PathBuf,
        /// Start NFS servers but skip kernel mounts
        #[arg(long)]
        server_only: bool,
    },
    /// Unmount deps from mount directory
    Unmount {
        /// Base directory where deps were mounted
        #[arg(long, short = 'd', default_value = "./ctxfs-deps")]
        mount_dir: PathBuf,
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
            sources,
            mount_point,
            mount_dir,
            server_only,
            backend: backend_flag,
        } => {
            handle_mount(&config, sources, mount_point, mount_dir, server_only, backend_flag).await?;
        }

        Commands::Unmount { target, all } => {
            handle_unmount(&config, target, all).await?;
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

                // On macOS 26+, prompt to also set up the FSKit extension.
                #[cfg(target_os = "macos")]
                if setup::is_macos_26_or_later() {
                    println!();
                    println!("FSKit backend available (macOS 26+):");
                    println!("  - No sudo required for mounts");
                    println!("  - No Full Disk Access required");
                    println!("  - Faster, more reliable than NFS");
                    println!();
                    let install_fskit = dialoguer::Confirm::new()
                        .with_prompt("Also install the FSKit extension now?")
                        .default(true)
                        .interact()
                        .unwrap_or(false);
                    if install_fskit {
                        if let Err(e) = setup::install_fskit() {
                            eprintln!("FSKit install failed: {e}");
                        }
                    } else {
                        println!("Skipped. You can run `ctxfs setup install-fskit` later.");
                    }
                }
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
                #[cfg(target_os = "macos")]
                {
                    println!();
                    println!("macOS note: your terminal app also needs Full Disk Access to read");
                    println!(
                        "NFS-mounted files. If `ls` on a mount returns 'Operation not permitted',"
                    );
                    println!("grant Full Disk Access to your terminal in:");
                    println!("  System Settings > Privacy & Security > Full Disk Access");
                    println!();
                    println!("To open this pane now, run:");
                    println!("  open \"x-apple.systempreferences:com.apple.settings.PrivacySecurity.extension?Privacy_AllFiles\"");
                }
                setup::check_fskit_status();
            }
            SetupAction::InstallFskit => {
                setup::install_fskit().map_err(|e| anyhow::anyhow!(e))?;
            }
        },

        Commands::Deps { action } => {
            handle_deps(&config, action).await?;
        }
    }

    Ok(())
}

// ---------------------------------------------------------------------------
// Mount handler
// ---------------------------------------------------------------------------

async fn handle_mount(
    config: &Config,
    sources: Vec<String>,
    mount_point: Option<PathBuf>,
    mount_dir: Option<PathBuf>,
    server_only: bool,
    backend_flag: Option<Backend>,
) -> Result<()> {
    let selected_backend = backend::detect_backend(backend_flag, None);

    // Validation: mount_point and mount_dir are mutually exclusive.
    if mount_point.is_some() && mount_dir.is_some() {
        anyhow::bail!("--mount-point and --mount-dir are mutually exclusive");
    }

    // mount_point requires exactly one source.
    if mount_point.is_some() && sources.len() > 1 {
        anyhow::bail!("--mount-point can only be used with a single source");
    }

    // At least one of the two is required.
    if mount_point.is_none() && mount_dir.is_none() {
        anyhow::bail!("either --mount-point or --mount-dir is required");
    }

    // For FSKit mounts, ensure /Volumes/ctxfs/ exists before asking the daemon.
    // The daemon errors out cleanly if it's missing, but we can do better UX
    // by creating it with a single sudo prompt instead of sending the user to
    // `ctxfs setup install-fskit`.
    if selected_backend == Backend::FsKit {
        ensure_volumes_ctxfs_exists()?;
    }

    let client = connect(config).await?;

    if let Some(mp) = mount_point {
        // Single-source mount (original behaviour).
        let source = &sources[0];
        let mp_str = mp.to_str().context("invalid mount point path")?.to_string();

        // NFS mounts AT the -p path, so it must pre-exist as a directory.
        // FSKit places a symlink at -p, so the path must NOT pre-exist.
        if selected_backend == Backend::FsKit {
            if let Some(parent) = mp.parent() {
                if !parent.as_os_str().is_empty() {
                    std::fs::create_dir_all(parent)
                        .context("failed to create mount point parent directory")?;
                }
            }
            // One stat handles regular files, directories, and dangling symlinks.
            if std::fs::symlink_metadata(&mp).is_ok() {
                handle_existing_fskit_mount_point(&mp)?;
            }
        } else {
            std::fs::create_dir_all(&mp).context("failed to create mount point directory")?;
        }

        let info = client
            .mount(
                long_context(),
                source.clone(),
                mp_str.clone(),
                selected_backend,
            )
            .await?
            .map_err(|e| anyhow::anyhow!(e))?;

        if server_only {
            print_server_only_info(&info);
            return Ok(());
        }

        if info.backend == Backend::FsKit {
            if let Some(volume_path) = info.volume_path.as_deref() {
                let user_path = std::path::Path::new(&mp_str);
                let volume = std::path::Path::new(volume_path);

                if user_path == volume {
                    println!("Mounted FSKit volume at {}", volume.display());
                } else {
                    match symlink::create_symlink(user_path, volume) {
                        Ok(created_at) => {
                            println!("Mounted FSKit volume at {}", volume.display());
                            println!("Linked from: {}", created_at.display());
                        }
                        Err(e) => {
                            eprintln!(
                                "warning: mounted at {} but failed to create symlink {}: {e}",
                                volume.display(),
                                user_path.display()
                            );
                        }
                    }
                }
            } else {
                println!("Mounted FSKit volume (no volume_path reported)");
            }
            println!("  Source:   {}", info.source);
            println!("  Commit:   {}", info.commit_sha);
            println!("  ID:       {}", info.id);
        } else {
            let port = info
                .nfs_port
                .ok_or_else(|| anyhow::anyhow!("mount did not return an NFS port"))?;
            println!(
                "NFS server listening on 127.0.0.1:{port} — mounting kernel side (may prompt for sudo)"
            );
            if let Err(e) = run_mount_nfs(port, &mp_str) {
                let _ = client
                    .unmount(tarpc::context::current(), mp_str.clone())
                    .await;
                return Err(anyhow::anyhow!("kernel mount failed: {e}"));
            }

            println!("Mounted {} at {}", info.source, info.mount_point);
            println!("  Commit:   {}", info.commit_sha);
            println!("  ID:       {}", info.id);
            println!("  NFS port: {port}");
        }
    } else if let Some(base_dir) = mount_dir {
        // Multi-source mount with slug-derived paths.
        let mounts: HashMap<String, PathBuf> = sources
            .iter()
            .map(|src| {
                let slug = deps::source_to_slug(src);
                (src.clone(), base_dir.join(slug))
            })
            .collect();

        let results = deps::mount::batch_mount(&client, &mounts, server_only).await;
        deps::mount::print_mount_summary(&results);

        let failures = results.iter().filter(|r| !r.success).count();
        if failures > 0 {
            anyhow::bail!("{failures} mount(s) failed");
        }
    }

    Ok(())
}

/// Ensure `/Volumes/ctxfs/` exists and is writable by the current user.
///
/// `/Volumes/` is root-owned, so we invoke sudo once to create the directory
/// and chown it. Subsequent FSKit mounts create per-volume subdirs without sudo.
fn ensure_volumes_ctxfs_exists() -> Result<()> {
    let parent = std::path::Path::new("/Volumes/ctxfs");
    if parent.exists() {
        return Ok(());
    }

    println!("/Volumes/ctxfs/ does not exist yet — creating it (requires sudo)...");

    let username = std::env::var("USER").unwrap_or_else(|_| whoami::username());

    // Single sudo invocation so the user sees one prompt.
    let shell_cmd =
        format!("mkdir -p /Volumes/ctxfs && chown {username}:staff /Volumes/ctxfs");
    let status = std::process::Command::new("sudo")
        .args(["sh", "-c", &shell_cmd])
        .status()
        .context("failed to invoke sudo for /Volumes/ctxfs setup")?;

    if !status.success() {
        anyhow::bail!(
            "sudo failed to create /Volumes/ctxfs (exit status {status}). \
             Run manually: sudo mkdir -p /Volumes/ctxfs && sudo chown {username}:staff /Volumes/ctxfs"
        );
    }

    println!("Created /Volumes/ctxfs/ (owned by {username}:staff)");
    Ok(())
}

/// Resolve a pre-existing `-p` path when doing an FSKit mount.
///
/// FSKit places a symlink at `-p`, so the path must not pre-exist. Auto-clear
/// only the two safe cases: stale ctxfs symlinks and empty directories.
/// Anything else is user data — error with guidance.
fn handle_existing_fskit_mount_point(path: &std::path::Path) -> Result<()> {
    let meta = std::fs::symlink_metadata(path)
        .with_context(|| format!("failed to stat {}", path.display()))?;

    if meta.is_symlink() {
        // safe_remove_symlink only touches symlinks into /Volumes/ctxfs/.
        if symlink::safe_remove_symlink(path)
            .with_context(|| format!("failed to remove symlink {}", path.display()))?
        {
            return Ok(());
        }
        anyhow::bail!(
            "{} is a symlink that does not point into {}. \
             Remove it manually if you want to reuse this path.",
            path.display(),
            symlink::CTXFS_VOLUMES_PREFIX
        );
    }

    if meta.is_dir() {
        // Let the kernel tell us if it's non-empty — one syscall, no TOCTOU.
        match std::fs::remove_dir(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::DirectoryNotEmpty => anyhow::bail!(
                "{} is a non-empty directory. Remove it manually if you want \
                 to use this path for an FSKit mount.",
                path.display()
            ),
            Err(e) => Err(e)
                .with_context(|| format!("failed to remove {}", path.display())),
        }
    } else {
        anyhow::bail!(
            "{} exists and is not a directory or ctxfs symlink. \
             Remove it manually.",
            path.display()
        )
    }
}

fn print_server_only_info(info: &ctxfs_ipc::service::MountInfo) {
    println!("NFS server ready:");
    println!("  Source:   {}", info.source);
    println!("  Commit:   {}", info.commit_sha);
    println!("  ID:       {}", info.id);
    println!("  NFS port: {}", info.nfs_port.unwrap_or(0));
    println!();
    println!("To mount manually, run:");
    #[cfg(target_os = "macos")]
    println!(
        "  sudo mount_nfs -o nolocks,vers=3,tcp,port={p},mountport={p},soft \\",
        p = info.nfs_port.unwrap_or(0)
    );
    #[cfg(target_os = "linux")]
    println!(
        "  sudo mount -t nfs -o nolock,vers=3,tcp,port={p},mountport={p},soft \\",
        p = info.nfs_port.unwrap_or(0)
    );
    println!("    127.0.0.1:/ {}", info.mount_point);
}

// ---------------------------------------------------------------------------
// Unmount handler
// ---------------------------------------------------------------------------

async fn handle_unmount(config: &Config, target: Option<String>, all: bool) -> Result<()> {
    if all {
        let client = connect(config).await?;
        let results = deps::mount::batch_unmount_all(&client).await;
        if results.is_empty() {
            println!("No active mounts to unmount");
        } else {
            deps::mount::print_unmount_summary(&results);
        }
        return Ok(());
    }

    let target = target.context("target is required unless --all is specified")?;

    let target_path = std::path::Path::new(&target);
    let is_ctxfs_link = symlink::is_ctxfs_symlink(target_path);

    // Resolve symlink to the underlying volume path for the daemon.
    let daemon_target = if is_ctxfs_link {
        symlink::resolve_ctxfs_path(target_path)
            .to_string_lossy()
            .into_owned()
    } else {
        target.clone()
    };

    // FSKit's kernel teardown is owned by the daemon (via fskit-rs's Session
    // drop). Running `umount` here would race with it. NFS is the opposite:
    // the CLI did the kernel mount, so the CLI must do the kernel umount.
    let is_fskit = daemon_target.starts_with(symlink::CTXFS_VOLUMES_PREFIX);
    if !is_fskit {
        if let Err(e) = run_umount(&daemon_target) {
            eprintln!("warning: kernel umount failed: {e}");
        }
    }

    // fskit-rs calls `hdiutil detach` on session drop, which can block >10s
    // when multiple volumes share a virtual device — use the longer deadline.
    let client = connect(config).await?;
    let rpc_result = client
        .unmount(long_context(), daemon_target.clone())
        .await;

    // Clean up the symlink even if the RPC timed out — the daemon may have
    // finished internally after we gave up waiting.
    if is_ctxfs_link {
        let _ = symlink::safe_remove_symlink(target_path);
    }

    match rpc_result {
        Ok(Ok(())) => {
            println!("Unmounted {target}");
            Ok(())
        }
        Ok(Err(e)) => Err(anyhow::anyhow!(e)),
        Err(e) => {
            // fskit-rs's hdiutil detach can outlast our RPC deadline; if the
            // volume is gone, treat the timeout as success.
            if is_fskit && !volume_still_mounted(&daemon_target) {
                eprintln!("note: unmount RPC timed out but volume is already torn down");
                println!("Unmounted {target}");
                Ok(())
            } else {
                Err(anyhow::Error::from(e))
            }
        }
    }
}

/// Returns true if `path` appears in the kernel mount table.
fn volume_still_mounted(path: &str) -> bool {
    match std::process::Command::new("mount").output() {
        Ok(out) if out.status.success() => {
            let stdout = String::from_utf8_lossy(&out.stdout);
            stdout
                .lines()
                .any(|line| line.contains(&format!(" on {path} ")))
        }
        _ => false, // can't tell; assume it's gone
    }
}

// ---------------------------------------------------------------------------
// Deps handler
// ---------------------------------------------------------------------------

#[allow(clippy::too_many_lines)]
async fn handle_deps(config: &Config, action: DepsAction) -> Result<()> {
    match action {
        DepsAction::List {
            project_dir,
            json,
            include_dev,
        } => {
            let result = deps::detect_all(&project_dir);
            let filtered = filter_dev(result.deps, include_dev);

            if json {
                #[derive(serde::Serialize)]
                struct JsonOutput {
                    manifests: Vec<String>,
                    dependencies: Vec<deps::DetectedDep>,
                }

                let output = JsonOutput {
                    manifests: result.manifests,
                    dependencies: filtered,
                };
                println!("{}", serde_json::to_string_pretty(&output)?);
            } else if filtered.is_empty() {
                anyhow::bail!("no dependencies detected in {}", project_dir.display());
            } else {
                // Group by ecosystem.
                let mut by_eco: HashMap<String, Vec<&deps::DetectedDep>> = HashMap::new();
                for dep in &filtered {
                    by_eco
                        .entry(dep.ecosystem.to_string())
                        .or_default()
                        .push(dep);
                }
                for (eco, eco_deps) in &by_eco {
                    println!("[{eco}]");
                    for dep in eco_deps {
                        let dev_tag = if dep.is_dev { " [dev]" } else { "" };
                        println!("  {} @{}{}", dep.name, dep.version, dev_tag);
                    }
                }
                println!();
                println!("{} dependencies detected", filtered.len());
            }
        }

        DepsAction::Mount {
            project_dir,
            all,
            select,
            include_dev,
            mount_dir,
            server_only,
        } => {
            let result = deps::detect_all(&project_dir);
            let filtered = filter_dev(result.deps, include_dev);

            if filtered.is_empty() {
                anyhow::bail!("no dependencies detected in {}", project_dir.display());
            }

            let selected = if all {
                filtered
            } else if let Some(names) = select {
                select_by_name(&filtered, &names)?
            } else {
                interactive_select(&filtered)?
            };

            if selected.is_empty() {
                println!("No dependencies selected");
                return Ok(());
            }

            let mounts = deps::compute_mount_paths(&selected, &mount_dir);
            let client = connect(config).await?;
            let results = deps::mount::batch_mount(&client, &mounts, server_only).await;
            deps::mount::print_mount_summary(&results);

            let failures = results.iter().filter(|r| !r.success).count();
            if failures > 0 {
                anyhow::bail!("{failures} mount(s) failed");
            }
        }

        DepsAction::Unmount { mount_dir } => {
            let client = connect(config).await?;
            let results = deps::mount::batch_unmount_dir(&client, &mount_dir).await;
            if results.is_empty() {
                println!("No active mounts under {} to unmount", mount_dir.display());
            } else {
                deps::mount::print_unmount_summary(&results);
            }
        }
    }

    Ok(())
}

/// Filter out dev dependencies unless `include_dev` is set.
fn filter_dev(deps: Vec<deps::DetectedDep>, include_dev: bool) -> Vec<deps::DetectedDep> {
    if include_dev {
        deps
    } else {
        deps.into_iter().filter(|d| !d.is_dev).collect()
    }
}

/// Select dependencies by name from --select flag.
///
/// Accepts bare names ("react") or qualified names ("npm:react") to resolve
/// ambiguity when the same package name appears in multiple ecosystems.
fn select_by_name(deps: &[deps::DetectedDep], names: &[String]) -> Result<Vec<deps::DetectedDep>> {
    let mut selected = Vec::new();
    for name in names {
        if let Some((eco_prefix, bare_name)) = name.split_once(':') {
            // Qualified name: match both ecosystem and dep name.
            let matches: Vec<_> = deps
                .iter()
                .filter(|d| {
                    d.ecosystem.to_string().eq_ignore_ascii_case(eco_prefix) && d.name == bare_name
                })
                .collect();
            match matches.len() {
                0 => anyhow::bail!("no dependency matching '{name}' found"),
                1 => selected.push(matches[0].clone()),
                _ => {
                    // Shouldn't happen with qualified names, but handle gracefully.
                    anyhow::bail!("multiple dependencies matching '{name}' — this is unexpected");
                }
            }
        } else {
            // Bare name: match by dep name only.
            let matches: Vec<_> = deps.iter().filter(|d| d.name == *name).collect();
            match matches.len() {
                0 => anyhow::bail!("no dependency named '{name}' found"),
                1 => selected.push(matches[0].clone()),
                _ => {
                    let ecosystems: Vec<_> =
                        matches.iter().map(|d| d.ecosystem.to_string()).collect();
                    anyhow::bail!(
                        "ambiguous name '{name}' — found in: {}. Qualify with ecosystem prefix (e.g., {}:{name}).",
                        ecosystems.join(", "),
                        ecosystems[0]
                    );
                }
            }
        }
    }
    Ok(selected)
}

/// Interactive multi-select picker using dialoguer.
fn interactive_select(deps: &[deps::DetectedDep]) -> Result<Vec<deps::DetectedDep>> {
    if !std::io::stdin().is_terminal() {
        anyhow::bail!("non-interactive terminal — use --all or --select to choose dependencies");
    }

    let labels: Vec<String> = deps.iter().map(deps::DetectedDep::picker_label).collect();
    // Pre-select production (non-dev) dependencies.
    let defaults: Vec<bool> = deps.iter().map(|d| !d.is_dev).collect();

    let selections = dialoguer::MultiSelect::new()
        .with_prompt("Select dependencies to mount")
        .items(&labels)
        .defaults(&defaults)
        .interact()
        .context("interactive selection cancelled")?;

    Ok(selections.into_iter().map(|i| deps[i].clone()).collect())
}

// ---------------------------------------------------------------------------
// Shared helpers
// ---------------------------------------------------------------------------

async fn connect(config: &Config) -> Result<CtxfsServiceClient> {
    transport::connect_client(&config.socket_path)
        .await
        .context(format!(
            "failed to connect to daemon at {}. Start with: ctxfs daemon start",
            config.socket_path.display()
        ))
}

/// Context with a longer deadline for operations that call external APIs.
pub(crate) fn long_context() -> tarpc::context::Context {
    let mut ctx = tarpc::context::current();
    ctx.deadline = std::time::Instant::now() + std::time::Duration::from_secs(60);
    ctx
}

/// Invoke the OS-native NFS mount command against the daemon's loopback NFS
/// server. Requires sudo on macOS and Linux (kernel restriction).
pub(crate) fn run_mount_nfs(port: u16, mount_point: &str) -> Result<()> {
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
pub(crate) fn run_umount(mount_point: &str) -> Result<()> {
    let status = std::process::Command::new("sudo")
        .args(["umount", mount_point])
        .status()
        .context("failed to invoke sudo umount")?;

    if !status.success() {
        anyhow::bail!("umount exited with {status}");
    }
    Ok(())
}
