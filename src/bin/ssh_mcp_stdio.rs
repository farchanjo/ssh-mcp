#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use poem_mcpserver::McpServer;
use ssh_mcp::mcp::McpSSHCommands;
use tokio::io::{AsyncBufReadExt, BufReader};
use tracing_subscriber::EnvFilter;

/// Run the MCP server over stdio, handling unknown methods (e.g.
/// `resources/templates/list`) that `poem-mcpserver` does not recognize.
/// Without this, unrecognized requests are silently dropped, causing
/// the client to wait until it hits a timeout.
async fn stdio_with_unknown_method_handling(
    mut server: McpServer<McpSSHCommands>,
) -> std::io::Result<()> {
    let mut input = BufReader::new(tokio::io::stdin()).lines();

    tracing::info!("stdio server started");

    while let Some(line) = input.next_line().await? {
        tracing::debug!(request = &line, "received request");

        // Try the normal parse path first.
        match serde_json::from_str::<poem_mcpserver::protocol::rpc::BatchRequest>(&line) {
            Ok(batch_request) => {
                for request in batch_request.into_iter() {
                    if request.jsonrpc != "2.0" {
                        let resp = serde_json::json!({
                            "jsonrpc": "2.0",
                            "id": null,
                            "error": { "code": -32600, "message": "invalid JSON-RPC version, expected `2.0`" }
                        });
                        println!("{resp}");
                        continue;
                    }
                    if let Some(resp) = server.handle_request(request).await {
                        let out =
                            serde_json::to_string(&resp).expect("serialize response");
                        println!("{out}");
                    }
                }
            }
            Err(_) => {
                // Parse failed — poem-mcpserver does not recognize the method.
                // Extract `id` and `method` from the raw JSON so we can reply.
                if let Ok(raw) = serde_json::from_str::<serde_json::Value>(&line) {
                    let id = raw.get("id").cloned().unwrap_or(serde_json::Value::Null);
                    let method = raw
                        .get("method")
                        .and_then(|m| m.as_str())
                        .unwrap_or("unknown");

                    let resp = match method {
                        // MCP spec: return empty lists for capabilities the
                        // server declares but has no concrete entries for.
                        "resources/templates/list" => {
                            tracing::debug!("resources/templates/list -> empty");
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "result": { "resourceTemplates": [] }
                            })
                        }
                        "resources/read" => {
                            tracing::debug!("resources/read -> not found");
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {
                                    "code": -32602,
                                    "message": "Resource not found"
                                }
                            })
                        }
                        "prompts/get" => {
                            tracing::debug!("prompts/get -> not found");
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {
                                    "code": -32602,
                                    "message": "Prompt not found"
                                }
                            })
                        }
                        _ => {
                            tracing::warn!(method, "unknown JSON-RPC method");
                            serde_json::json!({
                                "jsonrpc": "2.0",
                                "id": id,
                                "error": {
                                    "code": -32601,
                                    "message": format!("Method not found: {method}")
                                }
                            })
                        }
                    };
                    println!("{resp}");
                } else {
                    tracing::error!("failed to parse request as JSON");
                    let resp = serde_json::json!({
                        "jsonrpc": "2.0",
                        "id": null,
                        "error": { "code": -32700, "message": "Parse error" }
                    });
                    println!("{resp}");
                }
            }
        }
    }

    Ok(())
}

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing with RUST_LOG env filter (logs go to stderr)
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let server = McpServer::new().tools(McpSSHCommands {});
    stdio_with_unknown_method_handling(server).await?;

    Ok(())
}
