//! NDJSON daemon entry point — `ssh-mcp-tail`.
//!
//! v5 Phase 4: thin shell over `ssh_mcp::composition::embed` that hand-rolls
//! a tokio multi-thread runtime (mirrors `ssh-mcp-stdio` so the strict
//! Clippy baseline keeps applying). All real wiring lives in
//! [`ssh_mcp::embed`] / [`ssh_mcp::composition::embed`]; this file only
//! parses `clap` arguments and hands control to the right subcommand.

#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use std::error::Error;

use clap::Parser as _;
use tokio::runtime::Builder;

use ssh_mcp::embed::cli::{Cli, run_subcommand};

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let cli = Cli::parse();
    let runtime = Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run_subcommand(cli))
}
