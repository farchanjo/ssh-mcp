// DDD role: ValueObject

package schemas

// #SdkMove names the MCP SDK dependency, the advisory that forces the move and
// the release floors the resolved version must meet.
// DDD role: ValueObject
#SdkMove: {
	// Crate name as Cargo.toml declares it.
	crate: "rmcp"
	// Advisory the move closes.
	advisory: "RUSTSEC-2026-0462"
	// First release that carries the fix.
	fixed_in: "3.9.0"
	// Release the manifest must require at least.
	minimum: "3.11.0"
}

// #ProtocolPosture states the lifecycle the server keeps, its ceiling and the
// answer fixed for each client revision.
// DDD role: ValueObject
#ProtocolPosture: {
	// Highest revision advertised and served with initialize.
	ceiling: "2025-11-25"
	// Lifecycle the server keeps.
	lifecycle: "initialize"
	// Answer for a client that asks for 2026-07-28.
	newer_client: "2025-11-25"
	// Answer for a revision the server does not know.
	unknown_client: "2025-11-25"
	// Revision the embedded daemon client asks for.
	daemon_client: "2025-11-25"
	// HTTP session mode that keeps Mcp-Session-Id working.
	http_sessions: "legacy-session-mode"
}
