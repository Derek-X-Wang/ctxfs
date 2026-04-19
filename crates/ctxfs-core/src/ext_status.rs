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
#[must_use]
pub fn query_fskit_extension_status(bundle_id: &str) -> ExtensionInfo {
    #[cfg(target_os = "macos")]
    {
        use std::process::Command;
        match Command::new("pluginkit")
            .args(["-m", "-p", "com.apple.fskit.fsmodule"])
            .output()
        {
            Ok(output) => {
                let stdout = String::from_utf8_lossy(&output.stdout);
                let line = stdout.lines().find(|l| l.contains(bundle_id));
                let registered = line.is_some();
                let enabled = line.is_some_and(|l| l.trim_start().starts_with('+'));
                let version = line.and_then(parse_version);
                ExtensionInfo {
                    bundle_id: bundle_id.to_string(),
                    registered,
                    enabled,
                    version,
                    platform_supported: true,
                }
            }
            Err(_) => ExtensionInfo {
                bundle_id: bundle_id.to_string(),
                registered: false,
                enabled: false,
                version: None,
                platform_supported: true,
            },
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

#[cfg(target_os = "macos")]
fn parse_version(line: &str) -> Option<String> {
    let start = line.find('(')? + 1;
    let end = line.rfind(')')?;
    if end <= start {
        return None;
    }
    let v = &line[start..end];
    if v.is_empty() || v == "null" || v == "(null)" {
        return None;
    }
    Some(v.to_string())
}
