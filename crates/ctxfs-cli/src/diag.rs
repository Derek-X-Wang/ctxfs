use ctxfs_core::config::Config;

/// Print runtime diagnostic information for support.
///
/// Every check degrades gracefully: a failed daemon connection, a missing
/// `pluginkit`, or a non-macOS host all produce informative fallback text
/// instead of errors.
pub async fn handle_diag(config: &Config) {
    println!("ContextFS Diagnostics");
    println!("  Product:    ContextFS");
    println!("  Version:    {}", env!("CARGO_PKG_VERSION"));
    println!(
        "  Bundle ID:  {}",
        config
            .fskit_bundle_id
            .as_deref()
            .unwrap_or("not set")
    );

    // Backend
    let backend = crate::backend::detect_backend(None, config.default_backend);
    let backend_source = if std::env::var("CTXFS_BACKEND").is_ok() {
        "env"
    } else if config.default_backend.is_some() {
        "config"
    } else {
        "auto-detected"
    };
    println!("  Backend:    {backend} ({backend_source})");

    // Config file
    let config_path = dirs::home_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("/tmp"))
        .join(".ctxfs")
        .join("config.toml");
    if config_path.exists() {
        println!("  Config:     {} (loaded)", config_path.display());
    } else {
        println!(
            "  Config:     {} (not found — using defaults + env)",
            config_path.display()
        );
    }

    // Daemon status — try to ping
    let (daemon_running, daemon_pid) = check_daemon(config).await;
    if daemon_running {
        if let Some(pid) = daemon_pid {
            println!("  Daemon:     running (PID {pid})");
        } else {
            println!("  Daemon:     running");
        }
    } else {
        println!("  Daemon:     not running");
    }

    // Extension status (macOS only)
    check_extension(config);

    // macOS version (macOS only)
    check_macos_version();

    // Mount count — try list RPC
    if daemon_running {
        let mount_count = query_mount_count(config).await;
        match mount_count {
            Some(n) => println!("  Mounts:     {n} active"),
            None => println!("  Mounts:     unknown (RPC error)"),
        }
    } else {
        println!("  Mounts:     unknown (daemon not running)");
    }
}

/// Try to connect and ping the daemon. Returns (is_running, pid_if_known).
async fn check_daemon(config: &Config) -> (bool, Option<i32>) {
    // Read PID from pid file first (best-effort).
    let pid = config
        .pid_file
        .exists()
        .then(|| std::fs::read_to_string(&config.pid_file).ok())
        .flatten()
        .and_then(|s| s.trim().parse::<i32>().ok());

    match ctxfs_ipc::transport::connect_client(&config.socket_path).await {
        Ok(client) => match client.ping(tarpc::context::current()).await {
            Ok(_) => (true, pid),
            Err(_) => (false, None),
        },
        Err(_) => (false, None),
    }
}

/// Run `pluginkit` to check FSKit extension registration (macOS only).
fn check_extension(config: &Config) {
    #[cfg(target_os = "macos")]
    {
        match std::process::Command::new("pluginkit")
            .args(["-m", "-p", "com.apple.fskit.fsmodule"])
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let bundle_id = config
                    .fskit_bundle_id
                    .as_deref()
                    .unwrap_or("ai.ctxfs.fskitbridge.fskitext");
                if stdout.contains("fskitbridge") {
                    println!("  Extension:  {bundle_id} (enabled)");
                } else {
                    println!("  Extension:  not registered");
                }
            }
            Err(_) => {
                println!("  Extension:  not checked (pluginkit unavailable)");
            }
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = config; // suppress unused warning
        println!("  Extension:  not checked (not macOS)");
    }
}

/// Run `sw_vers` to get the macOS version (macOS only).
fn check_macos_version() {
    #[cfg(target_os = "macos")]
    {
        let version = std::process::Command::new("sw_vers")
            .args(["-productVersion"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());

        let name = std::process::Command::new("sw_vers")
            .args(["-productName"])
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string());

        match (version, name) {
            (Some(v), Some(n)) => println!("  macOS:      {v} ({n})"),
            (Some(v), None) => println!("  macOS:      {v}"),
            _ => println!("  macOS:      unknown"),
        }
    }
    #[cfg(not(target_os = "macos"))]
    {}
}

/// Query the daemon for the active mount count via the list RPC.
async fn query_mount_count(config: &Config) -> Option<usize> {
    let client = ctxfs_ipc::transport::connect_client(&config.socket_path)
        .await
        .ok()?;
    let mounts = client.list(tarpc::context::current()).await.ok()?;
    Some(mounts.len())
}
