use crate::rpc::{Request, Response};
use ctxfs_ipc::service::CtxfsServiceClient;
use std::path::PathBuf;
use std::sync::Arc;
use tokio::sync::Mutex;

pub struct HandlerState {
    client: Arc<Mutex<Option<CtxfsServiceClient>>>,
    socket_path: PathBuf,
    /// Cached FSKit bundle ID — resolved once at init from Config, so
    /// extension_status doesn't call Config::load() on every request.
    pub bundle_id: String,
}

impl HandlerState {
    pub fn new(socket_path: PathBuf) -> Self {
        let config = ctxfs_core::config::Config::load();
        let bundle_id = config
            .fskit_bundle_id
            .unwrap_or_else(|| "ai.ctxfs.companion.fskitext".to_string());
        Self {
            client: Arc::new(Mutex::new(None)),
            socket_path,
            bundle_id,
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

/// Call a daemon RPC via the persistent client; on transport error, reset the
/// client so the next request reconnects.
async fn dispatch_rpc<T, F, Fut>(
    state: &HandlerState,
    req_id: u64,
    rpc_name: &str,
    f: F,
) -> Response
where
    F: FnOnce(CtxfsServiceClient) -> Fut,
    Fut: std::future::Future<Output = Result<Result<T, String>, tarpc::client::RpcError>>,
    T: serde::Serialize,
{
    match state.client().await {
        Ok(client) => match f(client).await {
            Ok(Ok(value)) => Response::ok(req_id, value),
            Ok(Err(e)) => Response::err(req_id, e),
            Err(e) => {
                state.reset_client().await;
                Response::err(req_id, format!("{rpc_name} rpc failed: {e}"))
            }
        },
        Err(e) => Response::err(req_id, e),
    }
}

pub async fn dispatch(state: &HandlerState, req: &Request) -> Response {
    match req.method.as_str() {
        "ping" => Response::ok(req.id, "pong"),

        "list" => {
            dispatch_rpc(state, req.id, "list", |client| async move {
                client
                    .list(tarpc::context::current())
                    .await
                    .map(Ok)
            })
            .await
        }

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
            dispatch_rpc(state, req.id, "unmount", |client| async move {
                client
                    .unmount(tarpc::context::current(), params.target)
                    .await
                    .map(|r| r.map(|()| serde_json::json!({"ok": true})))
            })
            .await
        }

        "cache_breakdown" => {
            dispatch_rpc(state, req.id, "cache_breakdown", |client| async move {
                client.cache_breakdown(tarpc::context::current()).await
            })
            .await
        }

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
            dispatch_rpc(state, req.id, "set_cache_limits", |client| async move {
                client
                    .set_cache_limits(tarpc::context::current(), params.max_bytes)
                    .await
            })
            .await
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
            dispatch_rpc(state, req.id, "prune_blobs", |client| async move {
                client
                    .prune_blobs(tarpc::context::current(), params.target_bytes)
                    .await
                    .map(|r| r.map(|bytes_freed| serde_json::json!({"bytes_freed": bytes_freed})))
            })
            .await
        }

        "extension_status" => {
            // bundle_id is pre-cached in HandlerState — no Config::load() per-request.
            let bundle_id = state.bundle_id.clone();

            // Run the blocking pluginkit subprocess on the blocking thread pool
            // so it doesn't stall the async executor.
            let info = tokio::task::spawn_blocking(move || {
                ctxfs_core::query_fskit_extension_status(&bundle_id)
            })
            .await
            .unwrap_or_else(|_| ctxfs_core::ExtensionInfo {
                bundle_id: state.bundle_id.clone(),
                registered: false,
                enabled: false,
                version: None,
                platform_supported: cfg!(target_os = "macos"),
            });

            Response::ok(req.id, info)
        }

        "test_github_token" => {
            #[derive(serde::Deserialize)]
            struct Params {
                token: String,
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
            match ctxfs_provider_git::validate_github_token(&params.token).await {
                Ok(info) => Response::ok(req.id, info),
                Err(e) => Response::err(req.id, e),
            }
        }

        "config_read" => {
            use sha2::{Digest, Sha256};
            let path = ctxfs_core::config::load_config_path();
            let (content, snapshot_hash) = match std::fs::read(&path) {
                Ok(bytes) => (
                    String::from_utf8_lossy(&bytes).to_string(),
                    hex::encode(Sha256::digest(&bytes)),
                ),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    (String::new(), String::new())
                }
                Err(e) => return Response::err(req.id, format!("config_read failed: {e}")),
            };
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
