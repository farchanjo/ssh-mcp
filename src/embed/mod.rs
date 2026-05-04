//! v5 Phase 4 — NDJSON daemon embedded transport.
//!
//! `ssh-mcp-tail` is the v5.0 binary that embeds the rmcp `ServerHandler`
//! and an rmcp client inside the same process. The two halves talk to
//! each other through a `tokio::io::duplex` pair carrying real JSON-RPC
//! framing, so every code path the production HTTP / stdio binaries
//! exercise is exercised here too. Stdin reads NDJSON commands; stdout
//! emits NDJSON events; stderr is reserved for `RUST_LOG` tracing.
//!
//! Module map:
//!
//! - [`parser`] — NDJSON line reader → typed [`parser::Op`] enum.
//! - [`formatter`] — typed [`formatter::Event`] → NDJSON line on stdout.
//! - [`duplex_transport`] — wires the in-process MCP transport.
//! - [`event_mux`] — drains the rmcp client's notification stream into
//!   the formatter.
//! - [`dispatcher`] — translates each [`parser::Op`] into the matching
//!   `tools/call` / `resources/subscribe` request.
//! - [`signals`] — SIGTERM / SIGINT / SIGHUP graceful shutdown sequence.
//! - [`cli`] — `clap` subcommand definitions (`run` / `shell` / `daemon`).
//!
//! Design rationale: [ADR 0008](../docs/adr/0008-ndjson-daemon-protocol.md).
//! Wire reference: `docs/INSTRUCTIONS_DAEMON.md`.

pub mod cli;
pub mod dispatcher;
pub mod duplex_transport;
pub mod event_mux;
pub mod formatter;
pub mod parser;
pub mod signals;
