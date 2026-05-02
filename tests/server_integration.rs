//! Integration smoke tests for `McpSshServer`.
//!
//! These tests construct a real `McpSshServer` and exercise the
//! `ServerHandler::get_info` / tool listing surface end-to-end. They do NOT
//! make any network calls — only the in-process rmcp ServerInfo and tool
//! router are inspected.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::missing_panics_doc,
    reason = "tests use unwrap on values constructed in the test itself"
)]

use rmcp::ServerHandler;
use rmcp::model::ProtocolVersion;
use ssh_mcp::mcp::server::McpSshServer;

#[test]
fn server_constructs_and_advertises_subscribe_capability() {
    let server = McpSshServer::new();
    let info = server.get_info();
    let subscribe = info
        .capabilities
        .resources
        .as_ref()
        .and_then(|r| r.subscribe)
        .unwrap_or(false);
    assert!(subscribe, "resources.subscribe must be advertised as true");
}

#[test]
fn server_advertises_resources_list_changed() {
    let info = McpSshServer::new().get_info();
    let list_changed = info
        .capabilities
        .resources
        .as_ref()
        .and_then(|r| r.list_changed)
        .unwrap_or(false);
    assert!(
        list_changed,
        "resources.list_changed must be advertised as true"
    );
}

#[test]
fn server_advertises_tools_list_changed() {
    let info = McpSshServer::new().get_info();
    let list_changed = info
        .capabilities
        .tools
        .as_ref()
        .and_then(|t| t.list_changed)
        .unwrap_or(false);
    assert!(
        list_changed,
        "tools.list_changed must be advertised as true"
    );
}

#[test]
fn server_protocol_version_is_2025_06_18() {
    let info = McpSshServer::new().get_info();
    assert_eq!(info.protocol_version, ProtocolVersion::V_2025_06_18);
}

#[test]
fn server_implementation_name_is_ssh_mcp() {
    let info = McpSshServer::new().get_info();
    assert_eq!(info.server_info.name, "ssh-mcp");
}

#[test]
fn server_implementation_version_matches_cargo_pkg_version() {
    let info = McpSshServer::new().get_info();
    assert_eq!(info.server_info.version, env!("CARGO_PKG_VERSION"));
}

#[test]
fn server_instructions_mention_all_five_resource_schemes() {
    let info = McpSshServer::new().get_info();
    let instructions = info
        .instructions
        .as_deref()
        .expect("instructions must be populated");
    for scheme in [
        "shell://",
        "command://",
        "transfer://",
        "session://",
        "forward://",
    ] {
        assert!(
            instructions.contains(scheme),
            "instructions must mention `{scheme}`: {instructions}"
        );
    }
}

#[test]
fn server_instructions_mention_subscribe_recommendation() {
    let info = McpSshServer::new().get_info();
    let instructions = info
        .instructions
        .as_deref()
        .expect("instructions must be populated");
    assert!(
        instructions.contains("subscribe"),
        "instructions must guide clients toward `subscribe`: {instructions}"
    );
}

#[test]
fn server_default_constructs_same_capabilities() {
    let info_new = McpSshServer::new().get_info();
    let info_default = McpSshServer::default().get_info();
    assert_eq!(info_new.protocol_version, info_default.protocol_version);
    assert_eq!(info_new.server_info.name, info_default.server_info.name);
    assert_eq!(
        info_new.server_info.version,
        info_default.server_info.version
    );
}

#[test]
fn server_clone_preserves_get_info() {
    let original = McpSshServer::new();
    let cloned = original.clone();
    let original_info = original.get_info();
    let cloned_info = cloned.get_info();
    assert_eq!(original_info.server_info.name, cloned_info.server_info.name);
    assert_eq!(
        original_info.server_info.version,
        cloned_info.server_info.version
    );
}

#[test]
fn list_resources_impl_returns_typed_payload_with_no_pagination() {
    // The free function exposes the same logic the rmcp router calls. We
    // exercise it without a peer to verify it doesn't panic on an empty /
    // arbitrary storage state and reports `next_cursor = None` (no pagination).
    let result = ssh_mcp::mcp::resources::list_resources_impl();
    assert!(result.next_cursor.is_none());
}

#[test]
fn server_construction_does_not_panic_repeatedly() {
    // Build several servers in succession to make sure each construction
    // path is independent. The tool router is rebuilt every time.
    for _ in 0..5_usize {
        let _ = McpSshServer::new();
    }
}
