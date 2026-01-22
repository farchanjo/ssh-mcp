#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use dotenv::dotenv;
use poem::{EndpointExt, Route, Server, listener::TcpListener, middleware::Tracing};
use poem_mcpserver::{McpServer, streamable_http};
use tracing::info;

mod mcp;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    dotenv().ok();

    // Initialize logging with proper tracing default
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::from_default_env()
                .add_directive("info".parse().expect("valid directive")),
        )
        .init();

    // Setup MCP server
    let mcp_port: u16 = std::env::var("MCP_PORT")
        .ok()
        .and_then(|p| p.parse().ok())
        .unwrap_or(8000);
    let mcp_addr = format!("0.0.0.0:{}", mcp_port);
    info!("Starting MCP server on {}", mcp_addr);

    // Setup the poem-mcpserver endpoint with SSH commands
    let app = Route::new()
        .at(
            "/",
            streamable_http::endpoint(|_| McpServer::new().tools(mcp::McpSSHCommands {})),
        )
        .with(Tracing);

    info!("MCP Server with SSH client support is ready");
    info!("Use the ssh_connect command to establish SSH connections");
    info!("Use the ssh_forward command to set up port forwarding");

    // Run the MCP server
    Server::new(TcpListener::bind(mcp_addr))
        .name("SSH MCP Server")
        .run(app)
        .await?;

    Ok(())
}
