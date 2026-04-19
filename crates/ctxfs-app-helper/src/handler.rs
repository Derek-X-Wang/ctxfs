use crate::rpc::{Request, Response};

// `async` is needed once Tasks 7-10 add await points for tarpc calls.
#[allow(clippy::unused_async)]
pub async fn dispatch(req: &Request) -> Response {
    match req.method.as_str() {
        "ping" => Response::ok(req.id, "pong"),
        other => Response::err(req.id, format!("unknown method: {other}")),
    }
}
