//! Infrastructure / inbound adapters for the v4 hexagonal stack.
//!
//! The [`mcp`] submodule wires the rmcp transport (HTTP + stdio) to the
//! use case container produced by [`crate::composition::UseCases`]. Future
//! transports (REST gateway, gRPC, etc.) would land alongside it.

pub mod mcp;
