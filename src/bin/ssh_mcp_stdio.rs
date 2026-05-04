//! Stdio transport entry point for ssh-mcp.
//!
//! Thin shell that defers to [`ssh_mcp::composition::prod::run_stdio`]. The
//! v4 composition root in turn delegates to the legacy v3 runtime in H2
//! (this etapa) and progressively replaces it with adapter-backed wiring as
//! etapas H3-H17 land.

#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use std::error::Error;

use tokio::runtime::Builder;

use ssh_mcp::composition::prod::run_stdio;

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let runtime = Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run_stdio())
}
