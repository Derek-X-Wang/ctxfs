/// Registration state of the `FSKit` filesystem extension.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
pub struct ExtensionInfo {
    pub bundle_id: String,
    pub registered: bool,
    pub enabled: bool,
    pub version: Option<String>,
    pub platform_supported: bool,
}

/// Query the `FSKit` extension registration state via `pluginkit`.
///
/// Returns a structured view suitable for both the CLI's `diag` command and
/// the app-helper's `extension_status` RPC.
///
/// On non-macOS hosts `platform_supported` is `false` and all other fields
/// carry their zero/`None` values.
///
/// ## Enablement caveat
///
/// On macOS 26+ the Login Items & Extensions toggle state for `ExtensionKit`
/// `FSKit` modules is not reliably exposed by `pluginkit`. We therefore treat
/// `enabled == registered`: if the extension is discoverable in the system's
/// extension database, we consider it available to `FSKit`. The real
/// enablement check happens when `fskit-rs` attempts to start a session —
/// a mount against a toggled-off extension surfaces the failure there.
#[must_use]
pub fn query_fskit_extension_status(bundle_id: &str) -> ExtensionInfo {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        // `pluginkit -m -i <id> --raw` is what fskit-rs itself uses to locate
        // a registered extension. Exit code is always 0; the signal is
        // whether the bundle identifier shows up in the raw plist dump.
        let (registered, version) = match Command::new("pluginkit")
            .args(["-m", "-i", bundle_id, "--raw"])
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let needle = format!("CFBundleIdentifier = \"{bundle_id}\"");
                if stdout.contains(&needle) {
                    (true, parse_version_from_raw(&stdout))
                } else {
                    (false, None)
                }
            }
            Err(_) => (false, None),
        };

        ExtensionInfo {
            bundle_id: bundle_id.to_string(),
            registered,
            enabled: registered,
            version,
            platform_supported: true,
        }
    }
    #[cfg(not(target_os = "macos"))]
    {
        ExtensionInfo {
            bundle_id: bundle_id.to_string(),
            registered: false,
            enabled: false,
            version: None,
            platform_supported: false,
        }
    }
}

/// Pull `CFBundleShortVersionString = "…";` out of `pluginkit --raw` output.
#[cfg(target_os = "macos")]
fn parse_version_from_raw(raw: &str) -> Option<String> {
    let key = "CFBundleShortVersionString = \"";
    let start = raw.find(key)? + key.len();
    let end = start + raw[start..].find('"')?;
    let v = &raw[start..end];
    if v.is_empty() { None } else { Some(v.to_string()) }
}
