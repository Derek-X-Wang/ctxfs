use serde::{Deserialize, Serialize};

/// Incoming JSON-RPC request (one per line on stdin).
///
/// `params` is unused by the current scaffold; Tasks 7-10 add methods that consume it.
#[allow(dead_code)]
#[derive(Debug, Deserialize)]
pub struct Request {
    pub id: u64,
    pub method: String,
    #[serde(default)]
    pub params: serde_json::Value,
}

#[derive(Debug, Serialize)]
pub struct Response {
    pub id: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub result: Option<serde_json::Value>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

impl Response {
    pub fn ok(id: u64, result: impl Serialize) -> Self {
        Self {
            id,
            result: Some(serde_json::to_value(result).unwrap_or(serde_json::Value::Null)),
            error: None,
        }
    }

    pub fn err(id: u64, error: impl Into<String>) -> Self {
        Self {
            id,
            result: None,
            error: Some(error.into()),
        }
    }
}
