#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use poem_mcpserver::McpServer;
use ssh_mcp::mcp::McpSSHCommands;

#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error>> {
    poem_mcpserver::stdio::stdio(McpServer::new().tools(McpSSHCommands {})).await?;
    Ok(())
}
