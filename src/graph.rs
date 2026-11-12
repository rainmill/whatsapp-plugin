//! The Graph API, behind a trait so the plugin's own tests answer for Meta.

use std::sync::{Arc, RwLock};

use laterite_core::serde_json::{self, Value};
use laterite_core::strata::async_trait;

/// The Graph API version used until the credential names another.
pub const DEFAULT_VERSION: &str = "v23.0";

/// How a call is made.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// A read.
    Get,
    /// A write.
    Post,
    /// A removal.
    Delete,
}

/// One call: `path` is everything after the version, query string included.
#[derive(Clone, Debug)]
pub struct Request {
    /// How the call is made.
    pub method: Method,
    /// The Graph API version, such as `v23.0`.
    pub version: String,
    /// The path after the version, starting with the node's id.
    pub path: String,
    /// The access token.
    pub token: String,
    /// The JSON body of a write.
    pub body: Option<Value>,
}

/// A call that did not succeed.
#[derive(Clone, Debug, thiserror::Error)]
#[error("{message}")]
pub struct GraphError {
    /// The HTTP status, when Meta answered at all.
    pub status: Option<u16>,
    /// Meta's error code, when the answer carried one.
    pub code: Option<i64>,
    /// Meta's message, or what went wrong reaching it.
    pub message: String,
}

impl GraphError {
    /// Whether another try may succeed: Meta was unreachable, answered with a
    /// server error, or asked the caller to slow down.
    pub fn is_temporary(&self) -> bool {
        match self.status {
            None => true,
            Some(status) => status >= 500 || status == 429,
        }
    }
}

/// Something that answers Graph API calls.
#[async_trait]
pub trait Graph: Send + Sync {
    /// Makes one call and returns the JSON it answered with.
    async fn call(&self, request: Request) -> Result<Value, GraphError>;
}

/// The Graph API over HTTPS.
pub struct Http;

#[async_trait]
impl Graph for Http {
    async fn call(&self, request: Request) -> Result<Value, GraphError> {
        let unreachable = |e: reqwest::Error| GraphError {
            status: None,
            code: None,
            message: format!("Meta could not be reached: {e}"),
        };
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()
            .map_err(unreachable)?;
        let url = format!(
            "https://graph.facebook.com/{}/{}",
            request.version, request.path
        );
        let call = match request.method {
            Method::Get => client.get(url),
            Method::Post => client.post(url),
            Method::Delete => client.delete(url),
        }
        .bearer_auth(&request.token);
        let call = match &request.body {
            Some(body) => call.json(body),
            None => call,
        };
        let response = call.send().await.map_err(unreachable)?;
        let status = response.status();
        let text = response.text().await.map_err(unreachable)?;
        let json: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        if status.is_success() {
            return Ok(json);
        }
        Err(refusal(status.as_u16(), &json, &text))
    }
}

/// Reads Meta's error out of a refused call's body, keeping the raw body when
/// it has no `error` object.
pub(crate) fn refusal(status: u16, json: &Value, raw: &str) -> GraphError {
    let error = json.get("error");
    let detail = error
        .and_then(|e| e.pointer("/error_data/details"))
        .and_then(Value::as_str);
    let message = error
        .and_then(|e| e.get("message"))
        .and_then(Value::as_str)
        .map(|m| match detail {
            Some(detail) if detail != m => format!("{m} {detail}"),
            _ => m.to_string(),
        })
        .unwrap_or_else(|| {
            let raw: String = raw.chars().take(300).collect();
            format!("Meta answered {status}. {raw}")
        });
    GraphError {
        status: Some(status),
        code: error.and_then(|e| e.get("code")).and_then(Value::as_i64),
        message,
    }
}

static GRAPH: RwLock<Option<Arc<dyn Graph>>> = RwLock::new(None);

/// What answers Graph API calls: [`Http`] unless [`use_graph`] set another.
pub fn graph() -> Arc<dyn Graph> {
    GRAPH
        .read()
        .ok()
        .and_then(|g| g.clone())
        .unwrap_or_else(|| Arc::new(Http))
}

/// Answers every later Graph API call with `graph`: a test's stand-in for Meta.
pub fn use_graph(graph: Arc<dyn Graph>) {
    if let Ok(mut slot) = GRAPH.write() {
        *slot = Some(graph);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_refusal_carries_metas_code_and_words() {
        let json: Value = serde_json::from_str(
            r#"{"error":{"message":"Invalid parameter","code":100,"error_data":{"details":"Name is taken."}}}"#,
        )
        .unwrap();
        let error = refusal(400, &json, "");
        assert_eq!(error.code, Some(100));
        assert_eq!(error.message, "Invalid parameter Name is taken.");
        assert!(!error.is_temporary());
        let error = refusal(503, &Value::Null, "upstream down");
        assert_eq!(error.message, "Meta answered 503. upstream down");
        assert!(error.is_temporary());
    }
}
