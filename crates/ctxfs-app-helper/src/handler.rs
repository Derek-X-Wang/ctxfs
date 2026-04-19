use crate::rpc::{Request, Response};
use ctxfs_ipc::service::CtxfsServiceClient;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct HandlerState {
    client: Arc<Mutex<Option<CtxfsServiceClient>>>,
    socket_path: PathBuf,
}

impl HandlerState {
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            client: Arc::new(Mutex::new(None)),
            socket_path,
        }
    }

    async fn client(&self) -> Result<CtxfsServiceClient, String> {
        let mut guard = self.client.lock().await;
        if guard.is_none() {
            let new_client = ctxfs_ipc::transport::connect_client(&self.socket_path)
                .await
                .map_err(|e| format!("daemon connect failed: {e}"))?;
            *guard = Some(new_client);
        }
        Ok(guard.as_ref().unwrap().clone())
    }

    async fn reset_client(&self) {
        let mut guard = self.client.lock().await;
        *guard = None;
    }
}

pub async fn dispatch(state: &HandlerState, req: &Request) -> Response {
    match req.method.as_str() {
        "ping" => Response::ok(req.id, "pong"),

        "list" => match state.client().await {
            Ok(client) => match client.list(tarpc::context::current()).await {
                Ok(infos) => Response::ok(req.id, infos),
                Err(e) => {
                    state.reset_client().await;
                    Response::err(req.id, format!("list rpc failed: {e}"))
                }
            },
            Err(e) => Response::err(req.id, e),
        },

        "unmount" => {
            #[derive(serde::Deserialize)]
            struct UnmountParams {
                target: String,
            }
            let params: UnmountParams = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return Response::err(req.id, format!("invalid params for unmount: {e}"))
                }
            };
            match state.client().await {
                Ok(client) => {
                    match client
                        .unmount(tarpc::context::current(), params.target)
                        .await
                    {
                        Ok(Ok(())) => Response::ok(req.id, serde_json::json!({"ok": true})),
                        Ok(Err(e)) => Response::err(req.id, e),
                        Err(e) => {
                            state.reset_client().await;
                            Response::err(req.id, format!("unmount rpc failed: {e}"))
                        }
                    }
                }
                Err(e) => Response::err(req.id, e),
            }
        }

        "cache_breakdown" => match state.client().await {
            Ok(client) => match client.cache_breakdown(tarpc::context::current()).await {
                Ok(Ok(breakdown)) => Response::ok(req.id, breakdown),
                Ok(Err(e)) => Response::err(req.id, e),
                Err(e) => {
                    state.reset_client().await;
                    Response::err(req.id, format!("cache_breakdown rpc failed: {e}"))
                }
            },
            Err(e) => Response::err(req.id, e),
        },

        "set_cache_limits" => {
            #[derive(serde::Deserialize)]
            struct Params {
                max_bytes: u64,
            }
            let params: Params = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return Response::err(
                        req.id,
                        format!("invalid params for set_cache_limits: {e}"),
                    )
                }
            };
            match state.client().await {
                Ok(client) => {
                    match client
                        .set_cache_limits(tarpc::context::current(), params.max_bytes)
                        .await
                    {
                        Ok(Ok(breakdown)) => Response::ok(req.id, breakdown),
                        Ok(Err(e)) => Response::err(req.id, e),
                        Err(e) => {
                            state.reset_client().await;
                            Response::err(req.id, format!("set_cache_limits rpc failed: {e}"))
                        }
                    }
                }
                Err(e) => Response::err(req.id, e),
            }
        }

        "prune_blobs" => {
            #[derive(serde::Deserialize)]
            struct Params {
                target_bytes: u64,
            }
            let params: Params = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return Response::err(
                        req.id,
                        format!("invalid params for prune_blobs: {e}"),
                    )
                }
            };
            match state.client().await {
                Ok(client) => {
                    match client
                        .prune_blobs(tarpc::context::current(), params.target_bytes)
                        .await
                    {
                        Ok(Ok(bytes_freed)) => {
                            Response::ok(req.id, serde_json::json!({"bytes_freed": bytes_freed}))
                        }
                        Ok(Err(e)) => Response::err(req.id, e),
                        Err(e) => {
                            state.reset_client().await;
                            Response::err(req.id, format!("prune_blobs rpc failed: {e}"))
                        }
                    }
                }
                Err(e) => Response::err(req.id, e),
            }
        }

        "extension_status" => {
            #[derive(serde::Serialize)]
            struct Status {
                bundle_id: String,
                registered: bool,
                enabled: bool,
                version: Option<String>,
                platform_supported: bool,
            }

            let config = ctxfs_core::config::Config::load();
            let bundle_id = config
                .fskit_bundle_id
                .unwrap_or_else(|| "ai.ctxfs.fskitbridge.fskitext".to_string());

            #[cfg(target_os = "macos")]
            {
                match std::process::Command::new("pluginkit")
                    .args(["-m", "-p", "com.apple.fskit.fsmodule"])
                    .output()
                {
                    Ok(output) => {
                        let stdout = String::from_utf8_lossy(&output.stdout);
                        let line = stdout.lines().find(|l| l.contains(&bundle_id));
                        let registered = line.is_some();
                        // pluginkit prefixes enabled extensions with `+`
                        let enabled = line.is_some_and(|l| l.trim_start().starts_with('+'));
                        // Try to parse version from "bundle_id(1.2.3)" format.
                        // pluginkit may emit "bundle_id((null))" when no version is set.
                        let version = line.and_then(|l| {
                            let start = l.find('(')? + 1;
                            let end = l.rfind(')')?;
                            if end > start {
                                let v = l[start..end].trim_matches('(').trim_matches(')');
                                if v != "null" && !v.is_empty() {
                                    return Some(v.to_string());
                                }
                            }
                            None
                        });
                        Response::ok(
                            req.id,
                            Status {
                                bundle_id,
                                registered,
                                enabled,
                                version,
                                platform_supported: true,
                            },
                        )
                    }
                    Err(_) => Response::ok(
                        req.id,
                        Status {
                            bundle_id,
                            registered: false,
                            enabled: false,
                            version: None,
                            platform_supported: true, // still macOS, just pluginkit unavailable
                        },
                    ),
                }
            }

            #[cfg(not(target_os = "macos"))]
            {
                Response::ok(
                    req.id,
                    Status {
                        bundle_id,
                        registered: false,
                        enabled: false,
                        version: None,
                        platform_supported: false,
                    },
                )
            }
        }

        "test_github_token" => {
            #[derive(serde::Deserialize)]
            struct Params {
                token: String,
            }

            #[derive(serde::Serialize)]
            struct TokenResult {
                valid: bool,
                user: Option<String>,
                remaining: Option<u64>,
                reset_at: Option<String>,
            }

            let params: Params = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return Response::err(
                        req.id,
                        format!("invalid params for test_github_token: {e}"),
                    )
                }
            };
            if params.token.is_empty() {
                return Response::err(req.id, "token is empty");
            }

            let client = reqwest::Client::new();
            // Fetch /rate_limit and /user in parallel for a faster UI response.
            let rate_limit_fut = client
                .get("https://api.github.com/rate_limit")
                .header(
                    "Authorization",
                    format!("Bearer {}", params.token),
                )
                .header(
                    "User-Agent",
                    concat!("ctxfs/", env!("CARGO_PKG_VERSION")),
                )
                .header("Accept", "application/vnd.github+json")
                .send();
            let user_fut = client
                .get("https://api.github.com/user")
                .header(
                    "Authorization",
                    format!("Bearer {}", params.token),
                )
                .header(
                    "User-Agent",
                    concat!("ctxfs/", env!("CARGO_PKG_VERSION")),
                )
                .header("Accept", "application/vnd.github+json")
                .send();

            let (rate_res, user_res) = tokio::join!(rate_limit_fut, user_fut);

            let rate_resp = match rate_res {
                Ok(r) => r,
                Err(e) => return Response::err(req.id, format!("request failed: {e}")),
            };
            if !rate_resp.status().is_success() {
                return Response::err(
                    req.id,
                    format!("GitHub returned {}", rate_resp.status()),
                );
            }
            let rate_body: serde_json::Value = match rate_resp.json().await {
                Ok(v) => v,
                Err(e) => {
                    return Response::err(req.id, format!("failed to parse rate_limit: {e}"))
                }
            };

            let remaining = rate_body["resources"]["core"]["remaining"].as_u64();
            let reset_at = rate_body["resources"]["core"]["reset"]
                .as_i64()
                .and_then(|ts| chrono::DateTime::<chrono::Utc>::from_timestamp(ts, 0))
                .map(|dt| dt.to_rfc3339());

            let user = match user_res {
                Ok(r) if r.status().is_success() => match r.json::<serde_json::Value>().await {
                    Ok(body) => body["login"].as_str().map(std::string::ToString::to_string),
                    Err(_) => None,
                },
                _ => None,
            };

            Response::ok(
                req.id,
                TokenResult {
                    valid: true,
                    user,
                    remaining,
                    reset_at,
                },
            )
        }

        "config_read" => {
            let path = ctxfs_core::config::load_config_path();
            let content = std::fs::read_to_string(&path).unwrap_or_default();
            let snapshot_hash = ctxfs_core::config::ConfigSnapshot::read(&path)
                .map(|s| s.hash().to_string())
                .unwrap_or_default();
            Response::ok(
                req.id,
                serde_json::json!({
                    "path": path.to_string_lossy(),
                    "content": content,
                    "snapshot_hash": snapshot_hash,
                }),
            )
        }

        "config_set" => {
            #[derive(serde::Deserialize)]
            struct Params {
                content: String,
                snapshot_hash: String,
            }
            let params: Params = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return Response::err(req.id, format!("invalid params for config_set: {e}"))
                }
            };
            let path = ctxfs_core::config::load_config_path();
            let snapshot = ctxfs_core::config::ConfigSnapshot::from_hash(params.snapshot_hash);
            match snapshot.write_back(&path, &params.content) {
                Ok(()) => Response::ok(req.id, serde_json::json!({"ok": true})),
                Err(ctxfs_core::config::ConfigWriteError::ExternalEdit { expected, actual }) => {
                    Response::err(
                        req.id,
                        format!(
                            "config modified externally (expected hash {expected}, found {actual})"
                        ),
                    )
                }
                Err(e) => Response::err(req.id, format!("write failed: {e}")),
            }
        }

        "config_set_value" => {
            #[derive(serde::Deserialize)]
            struct Params {
                key: String,
                value: serde_json::Value,
            }
            let params: Params = match serde_json::from_value(req.params.clone()) {
                Ok(p) => p,
                Err(e) => {
                    return Response::err(
                        req.id,
                        format!("invalid params for config_set_value: {e}"),
                    )
                }
            };
            let path = ctxfs_core::config::load_config_path();
            // Convert serde_json::Value → toml_edit::Value
            let toml_value = match params.value {
                serde_json::Value::String(s) => toml_edit::Value::from(s),
                serde_json::Value::Bool(b) => toml_edit::Value::from(b),
                serde_json::Value::Number(ref n) if n.is_u64() => {
                    toml_edit::Value::from(n.as_u64().unwrap() as i64)
                }
                serde_json::Value::Number(ref n) if n.is_i64() => {
                    toml_edit::Value::from(n.as_i64().unwrap())
                }
                serde_json::Value::Number(ref n) if n.is_f64() => {
                    toml_edit::Value::from(n.as_f64().unwrap())
                }
                other => {
                    return Response::err(
                        req.id,
                        format!("unsupported value type for {}: {other}", params.key),
                    )
                }
            };
            match ctxfs_core::config::update_config_key(&path, &params.key, toml_value) {
                Ok(()) => {
                    Response::ok(req.id, serde_json::json!({"ok": true, "key": params.key}))
                }
                Err(e) => Response::err(req.id, format!("write failed: {e}")),
            }
        }

        other => Response::err(req.id, format!("unknown method: {other}")),
    }
}
