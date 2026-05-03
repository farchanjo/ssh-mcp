//! H18 smoke integration test for the v4 hexagonal composition.
//!
//! Verifies that `composition::prod::build_server` constructs without
//! panic and exposes the full MCP tool surface (18 with `port_forward`,
//! 17 without) plus the resource subscribe capability. No SSH I/O —
//! heavier scenarios live in `scripts/test_*.py`.

use rmcp::ServerHandler;
use ssh_mcp::composition::prod::build_server;

/// Tools always present (17). `ssh_forward` is added only when the
/// `port_forward` Cargo feature is enabled.
const CORE_TOOLS: [&str; 17] = [
    "ssh_cancel_command",
    "ssh_connect",
    "ssh_disconnect",
    "ssh_disconnect_agent",
    "ssh_download",
    "ssh_execute",
    "ssh_get_command_output",
    "ssh_get_transfer_progress",
    "ssh_list_commands",
    "ssh_list_sessions",
    "ssh_shell_close",
    "ssh_shell_open",
    "ssh_shell_read",
    "ssh_shell_send_key",
    "ssh_shell_wait_for",
    "ssh_shell_write",
    "ssh_upload",
];

#[test]
fn prod_build_server_advertises_capabilities() {
    let info = build_server().get_info();
    assert_eq!(info.server_info.name, "ssh-mcp");
    assert!(
        info.capabilities.tools.is_some(),
        "tools capability missing"
    );
    assert!(
        info.capabilities.resources.is_some(),
        "resources capability missing"
    );
}

#[test]
fn prod_server_exposes_full_tool_surface() {
    let server = build_server();
    for name in CORE_TOOLS {
        let tool = server.get_tool(name);
        assert!(tool.is_some(), "tool `{name}` missing");
        let desc = tool
            .as_ref()
            .and_then(|t| t.description.as_deref())
            .unwrap_or("");
        assert!(!desc.is_empty(), "tool `{name}` missing description");
    }
    #[cfg(feature = "port_forward")]
    assert!(
        server.get_tool("ssh_forward").is_some(),
        "ssh_forward missing under port_forward"
    );
    #[cfg(not(feature = "port_forward"))]
    assert!(
        server.get_tool("ssh_forward").is_none(),
        "ssh_forward leaked without port_forward"
    );
}
