#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use poem_mcpserver::McpServer;
use ssh_mcp::mcp::McpSSHCommands;
use tracing_subscriber::EnvFilter;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Initialize tracing with RUST_LOG env filter (logs go to stderr)
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    poem_mcpserver::stdio::stdio(McpServer::new().tools(McpSSHCommands {})).await?;

    Ok(())
}
