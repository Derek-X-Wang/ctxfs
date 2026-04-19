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

        other => Response::err(req.id, format!("unknown method: {other}")),
    }
}
