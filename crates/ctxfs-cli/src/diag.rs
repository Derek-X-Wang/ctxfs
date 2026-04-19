use ctxfs_core::config::Config;
use serde::Serialize;
use std::path::PathBuf;

/// Structured diagnostics report. Serialized to JSON with `--json`; rendered
/// as human-readable text otherwise. All fields that appear in text output are
/// present here — JSON is a superset, not a subset.
#[derive(Debug, Serialize)]
pub struct DiagReport {
    pub product: &'static str,
    pub version: &'static str,
    pub bundle_id: Option<String>,
    pub backend: String,
    pub backend_source: String,
    pub config_path: PathBuf,
    pub config_loaded: bool,
    pub daemon_running: bool,
    pub daemon_pid: Option<i32>,
    pub extension_registered: bool,
    pub extension_enabled: bool,
    pub extension_bundle_id: Option<String>,
    /// macOS product version string (e.g., "15.3.1"). `None` on non-macOS.
    pub macos_version: Option<String>,
    /// macOS product name (e.g., "macOS"). `None` on non-macOS.
    pub macos_name: Option<String>,
    /// Number of active mounts. `None` if daemon is not running or RPC failed.
    pub mount_count: Option<usize>,
}

/// Print runtime diagnostic information for support.
///
/// Every check degrades gracefully: a failed daemon connection, a missing
/// `pluginkit`, or a non-macOS host all produce informative fallback text
/// instead of errors.
pub async fn handle_diag(config: &Config, json: bool) {
    let report = collect_report(config).await;
    if json {
        match serde_json::to_string_pretty(&report) {
            Ok(s) => println!("{s}"),
            Err(e) => eprintln!("failed to serialize diag report: {e}"),
        }
    } else {
        print_human_readable(&report);
    }
}

/// Collect all diagnostic data into a `DiagReport`.
async fn collect_report(config: &Config) -> DiagReport {
    let backend = crate::backend::detect_backend(None, config.default_backend);
    let backend_source = if std::env::var("CTXFS_BACKEND").is_ok() {
        "env"
    } else if config.default_backend.is_some() {
        "config"
    } else {
        "auto-detected"
    };

    let config_path = ctxfs_core::config::load_config_path();
    let config_loaded = config_path.exists();

    let (daemon_running, daemon_pid) = check_daemon(config).await;

    let (extension_registered, extension_enabled, extension_bundle_id) =
        query_extension_status(config);

    let (macos_version, macos_name) = query_macos_version();

    let mount_count = if daemon_running {
        query_mount_count(config).await
    } else {
        None
    };

    DiagReport {
        product: "ContextFS",
        version: env!("CARGO_PKG_VERSION"),
        bundle_id: config.fskit_bundle_id.clone(),
        backend: backend.to_string(),
        backend_source: backend_source.to_string(),
        config_path,
        config_loaded,
        daemon_running,
        daemon_pid,
        extension_registered,
        extension_enabled,
        extension_bundle_id,
        macos_version,
        macos_name,
        mount_count,
    }
}

/// Render a `DiagReport` as human-readable text (the original format).
fn print_human_readable(r: &DiagReport) {
    println!("ContextFS Diagnostics");
    println!("  Product:    {}", r.product);
    println!("  Version:    {}", r.version);
    println!(
        "  Bundle ID:  {}",
        r.bundle_id.as_deref().unwrap_or("not set")
    );
    println!("  Backend:    {} ({})", r.backend, r.backend_source);

    if r.config_loaded {
        println!("  Config:     {} (loaded)", r.config_path.display());
    } else {
        println!(
            "  Config:     {} (not found — using defaults + env)",
            r.config_path.display()
        );
    }

    if r.daemon_running {
        if let Some(pid) = r.daemon_pid {
            println!("  Daemon:     running (PID {pid})");
        } else {
            println!("  Daemon:     running");
        }
    } else {
        println!("  Daemon:     not running");
    }

    // Extension (macOS only)
    if let Some(bundle_id) = &r.extension_bundle_id {
        if r.extension_registered {
            println!("  Extension:  {bundle_id} (enabled)");
        } else {
            println!("  Extension:  not registered");
        }
    } else if cfg!(target_os = "macos") {
        if r.extension_registered {
            println!("  Extension:  registered");
        } else {
            println!("  Extension:  not registered");
        }
    } else {
        println!("  Extension:  not checked (not macOS)");
    }

    // macOS version (macOS only)
    match (&r.macos_version, &r.macos_name) {
        (Some(v), Some(n)) => println!("  macOS:      {v} ({n})"),
        (Some(v), None) => println!("  macOS:      {v}"),
        _ => {
            #[cfg(target_os = "macos")]
            println!("  macOS:      unknown");
        }
    }

    match r.mount_count {
        Some(n) => println!("  Mounts:     {n} active"),
        None if r.daemon_running => println!("  Mounts:     unknown (RPC error)"),
        None => println!("  Mounts:     unknown (daemon not running)"),
    }
}

// ---------------------------------------------------------------------------
// Data-gathering helpers
// ---------------------------------------------------------------------------

/// Try to connect and ping the daemon. Returns (is_running, pid_if_known).
async fn check_daemon(config: &Config) -> (bool, Option<i32>) {
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

/// Query FSKit extension registration status.
/// Returns `(registered, enabled, bundle_id_if_checked)`.
fn query_extension_status(config: &Config) -> (bool, bool, Option<String>) {
    let bundle_id = config
        .fskit_bundle_id
        .as_deref()
        .unwrap_or("ai.ctxfs.companion.fskitext");
    let info = ctxfs_core::query_fskit_extension_status(bundle_id);
    if info.platform_supported {
        (info.registered, info.enabled, Some(info.bundle_id))
    } else {
        (false, false, None)
    }
}

/// Query macOS product version and name via `sw_vers`. Returns `(version, name)`.
fn query_macos_version() -> (Option<String>, Option<String>) {
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

        (version, name)
    }
    #[cfg(not(target_os = "macos"))]
    {
        (None, None)
    }
}

/// Query the daemon for the active mount count via the list RPC.
async fn query_mount_count(config: &Config) -> Option<usize> {
    let client = ctxfs_ipc::transport::connect_client(&config.socket_path)
        .await
        .ok()?;
    let mounts = client.list(tarpc::context::current()).await.ok()?;
    Some(mounts.len())
}
