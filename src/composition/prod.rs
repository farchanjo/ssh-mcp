//! Production wiring.
//!
//! Pins concrete adapter types and exposes the two transport entry points
//! [`run_http`] and [`run_stdio`]. H3-H9 land the concrete adapters; until
//! then the binaries call [`run_http`] / [`run_stdio`] which delegate to
//! the legacy v3 runtime so `ssh-mcp` keeps working through the migration.
//!
//! Concrete adapter types and a `build_use_cases()` constructor will land
//! once H3-H9 deliver the implementations. The placeholder type aliases
//! below document the intended shape.

use crate::mcp::runtime::{self, RuntimeError};

/// Run the HTTP transport (axum + rmcp `StreamableHttpService`).
///
/// In H2 this delegates to the legacy v3 entry point so the binary keeps
/// serving traffic. Etapas H10-H17 replace the body with the
/// adapter-backed `UseCases` wiring.
///
/// # Errors
///
/// Propagates any error returned by the underlying v3 runtime (listener
/// bind failure, axum graceful-shutdown failure, etc).
pub async fn run_http() -> Result<(), RuntimeError> {
    // TODO(H10-H17): wire UseCases + adapters here once they exist.
    runtime::run_http().await
}

/// Run the stdio transport (rmcp `serve(stdio())`).
///
/// In H2 this delegates to the legacy v3 entry point so the binary keeps
/// serving traffic. Etapas H10-H17 replace the body with the
/// adapter-backed `UseCases` wiring.
///
/// # Errors
///
/// Propagates any error returned by the underlying v3 runtime
/// (transport setup failure, service join failure, etc).
pub async fn run_stdio() -> Result<(), RuntimeError> {
    // TODO(H10-H17): wire UseCases + adapters here once they exist.
    runtime::run_stdio().await
}
