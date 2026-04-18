#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use std::sync::Arc;

use dashmap::DashSet;
use poem_mcpserver::McpServer;
use poem_mcpserver::protocol::rpc::{BatchRequest, Request, RequestId};
use serde_json::Value;
use ssh_mcp::mcp::commands::McpSSHCommands;
use tokio::io::{AsyncBufReadExt, BufReader};
use tokio::sync::Mutex;
use tracing_subscriber::EnvFilter;

/// Shared state carried across the stdio event loop and each spawned
/// request task. Every field is cheap to clone (just an `Arc` bump).
#[derive(Clone)]
struct StdioContext {
    server: Arc<Mutex<McpServer<McpSSHCommands>>>,
    cancelled: Arc<DashSet<String>>,
    stdout: Arc<Mutex<()>>,
}

impl StdioContext {
    fn new(server: McpServer<McpSSHCommands>) -> Self {
        Self {
            server: Arc::new(Mutex::new(server)),
            cancelled: Arc::new(DashSet::new()),
            stdout: Arc::new(Mutex::new(())),
        }
    }
}

/// Canonical string key for a JSON-RPC id so we can index both the typed
/// `RequestId` produced by poem-mcpserver and the raw `serde_json::Value`
/// extracted from cancellation notifications.
fn request_id_key(id: &Value) -> String {
    match id {
        Value::String(s) => format!("s:{s}"),
        Value::Number(n) => format!("n:{n}"),
        other => format!("v:{other}"),
    }
}

fn typed_id_key(id: &RequestId) -> String {
    match id {
        RequestId::String(s) => format!("s:{s}"),
        RequestId::Int(n) => format!("n:{n}"),
    }
}

fn build_fallback_response(id: Value, method: &str) -> Value {
    match method {
        "resources/templates/list" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "resourceTemplates": [] }
        }),
        "resources/read" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32602, "message": "Resource not found" }
        }),
        "prompts/get" => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": { "code": -32602, "message": "Prompt not found" }
        }),
        _ => serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": -32601,
                "message": format!("Method not found: {method}")
            }
        }),
    }
}

/// poem-mcpserver 0.3.1 deserialises `notifications/cancelled` via a
/// `Cancelled { request_id, .. }` variant, but MCP clients send `requestId`
/// (camelCase). The mismatch makes the built-in parser reject the
/// notification; the fallback branch then replies with a `{id:null,error:…}`
/// object that is not a valid JSON-RPC response and crashes the client's
/// transport. We intercept the cancel at the raw-JSON layer to both honour
/// it (record the id) and to never reply — notifications are fire-and-forget.
fn intercept_cancel(raw: &Value, cancelled: &DashSet<String>) -> bool {
    if raw.get("method").and_then(Value::as_str) != Some("notifications/cancelled") {
        return false;
    }
    let Some(params) = raw.get("params") else {
        return true;
    };
    let Some(req_id) = params.get("requestId").or_else(|| params.get("request_id")) else {
        return true;
    };
    let key = request_id_key(req_id);
    tracing::debug!(request_id = %key, "cancellation requested");
    cancelled.insert(key);
    true
}

/// Serialise once then emit the response, dropping it if the id has been
/// cancelled in the meantime. The check-and-write happens under the same
/// stdout guard so we never write a stale response after a concurrent
/// cancel is recorded.
async fn emit_response(
    id_key: Option<&str>,
    text: &str,
    cancelled: &DashSet<String>,
    stdout: &Mutex<()>,
) {
    let guard = stdout.lock().await;
    if let Some(key) = id_key
        && cancelled.remove(key).is_some()
    {
        tracing::debug!(id = key, "dropping response for cancelled request");
        return;
    }
    println!("{text}");
    drop(guard);
}

async fn emit_value(id_key: Option<&str>, value: &Value, ctx: &StdioContext) {
    match serde_json::to_string(value) {
        Ok(text) => emit_response(id_key, &text, &ctx.cancelled, &ctx.stdout).await,
        Err(err) => tracing::error!(error = %err, "failed to serialize response"),
    }
}

async fn process_single_request(request: Request, ctx: StdioContext) {
    if request.jsonrpc != "2.0" {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": {
                "code": -32600,
                "message": "invalid JSON-RPC version, expected `2.0`"
            }
        });
        emit_value(None, &payload, &ctx).await;
        return;
    }
    let id_key = request.id.as_ref().map(typed_id_key);
    let response = ctx.server.lock().await.handle_request(request).await;
    let Some(resp) = response else {
        return;
    };
    match serde_json::to_string(&resp) {
        Ok(text) => {
            emit_response(id_key.as_deref(), &text, &ctx.cancelled, &ctx.stdout).await;
        }
        Err(err) => tracing::error!(error = %err, "failed to serialize response"),
    }
}

async fn handle_fallback_line(raw: &Value, ctx: &StdioContext) {
    let method = raw.get("method").and_then(Value::as_str).unwrap_or("");
    // Notifications have no id and MUST NOT be replied to.
    if raw.get("id").is_none_or(Value::is_null) {
        tracing::warn!(method, "ignoring unknown notification (no reply)");
        return;
    }
    let id = raw.get("id").cloned().unwrap_or(Value::Null);
    let payload = build_fallback_response(id, method);
    emit_value(None, &payload, ctx).await;
}

async fn dispatch_line(line: String, ctx: &StdioContext) {
    let Ok(raw) = serde_json::from_str::<Value>(&line) else {
        let payload = serde_json::json!({
            "jsonrpc": "2.0",
            "id": Value::Null,
            "error": { "code": -32700, "message": "Parse error" }
        });
        emit_value(None, &payload, ctx).await;
        return;
    };

    if intercept_cancel(&raw, &ctx.cancelled) {
        return;
    }

    match serde_json::from_str::<BatchRequest>(&line) {
        Ok(batch) => {
            // Spawn so a long-running `ssh_get_command_output(wait=true)` does
            // not block the main loop from reading further stdin — including
            // follow-up `notifications/cancelled`.
            let ctx_spawn = ctx.clone();
            tokio::spawn(async move {
                for request in batch {
                    process_single_request(request, ctx_spawn.clone()).await;
                }
            });
        }
        Err(_) => handle_fallback_line(&raw, ctx).await,
    }
}

async fn stdio_with_cancellation(server: McpServer<McpSSHCommands>) -> std::io::Result<()> {
    let ctx = StdioContext::new(server);
    let mut input = BufReader::new(tokio::io::stdin()).lines();

    tracing::info!("stdio server started");

    while let Some(line) = input.next_line().await? {
        tracing::debug!(request = &line, "received request");
        dispatch_line(line, &ctx).await;
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let server = McpServer::new().tools(McpSSHCommands {});
    stdio_with_cancellation(server).await?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{build_fallback_response, intercept_cancel, request_id_key, typed_id_key};
    use dashmap::DashSet;
    use poem_mcpserver::protocol::rpc::RequestId;
    use serde_json::{Value, json};

    #[test]
    fn request_id_key_normalises_string_and_number() {
        assert_eq!(request_id_key(&json!("abc")), "s:abc");
        assert_eq!(request_id_key(&json!(42)), "n:42");
        assert_eq!(request_id_key(&Value::Null), "v:null");
    }

    #[test]
    fn typed_id_key_matches_raw_key() {
        assert_eq!(typed_id_key(&RequestId::String("abc".into())), "s:abc");
        assert_eq!(typed_id_key(&RequestId::Int(42)), "n:42");
    }

    #[test]
    fn intercept_cancel_records_camel_case_request_id() {
        let set = DashSet::new();
        let raw = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "requestId": 136, "reason": "user cancelled" }
        });
        assert!(intercept_cancel(&raw, &set));
        assert!(set.contains("n:136"));
    }

    #[test]
    fn intercept_cancel_records_snake_case_request_id() {
        let set = DashSet::new();
        let raw = json!({
            "jsonrpc": "2.0",
            "method": "notifications/cancelled",
            "params": { "request_id": "abc" }
        });
        assert!(intercept_cancel(&raw, &set));
        assert!(set.contains("s:abc"));
    }

    #[test]
    fn intercept_cancel_ignores_non_cancel_methods() {
        let set = DashSet::new();
        let raw = json!({
            "jsonrpc": "2.0",
            "method": "notifications/initialized"
        });
        assert!(!intercept_cancel(&raw, &set));
        assert!(set.is_empty());
    }

    #[test]
    fn intercept_cancel_without_params_still_consumes() {
        let set = DashSet::new();
        let raw = json!({ "jsonrpc": "2.0", "method": "notifications/cancelled" });
        assert!(intercept_cancel(&raw, &set));
        assert!(set.is_empty());
    }

    #[test]
    fn fallback_response_for_templates_returns_empty_list() {
        let resp = build_fallback_response(json!(1), "resources/templates/list");
        assert_eq!(resp["result"]["resourceTemplates"], json!([]));
        assert!(resp["error"].is_null());
    }

    #[test]
    fn fallback_response_for_unknown_method_is_method_not_found() {
        let resp = build_fallback_response(json!(7), "foo/bar");
        assert_eq!(resp["error"]["code"], -32601);
        assert_eq!(resp["error"]["message"], "Method not found: foo/bar");
    }

    #[test]
    fn fallback_response_for_resources_read_is_not_found() {
        let resp = build_fallback_response(json!(1), "resources/read");
        assert_eq!(resp["error"]["code"], -32602);
        assert_eq!(resp["error"]["message"], "Resource not found");
    }

    #[test]
    fn fallback_response_for_prompts_get_is_not_found() {
        let resp = build_fallback_response(json!(1), "prompts/get");
        assert_eq!(resp["error"]["code"], -32602);
        assert_eq!(resp["error"]["message"], "Prompt not found");
    }
}
