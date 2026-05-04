//! NDJSON daemon entry point — `ssh-mcp-tail`.
//!
//! v5 Phase 4: thin shell over [`ssh_mcp::composition::embed`] that
//! hand-rolls a tokio multi-thread runtime (mirrors `ssh-mcp-stdio` so
//! the strict Clippy baseline keeps applying — the `#[tokio::main]`
//! macro is incompatible with our `forbid`-level Layer A lints). All
//! real wiring lives in [`ssh_mcp::embed`] /
//! [`ssh_mcp::composition::embed`]; this file only parses argv and
//! hands control to the right subcommand.

#![deny(warnings)]
#![deny(clippy::unwrap_used)]

use std::error::Error;
use std::io::{Write as _, stderr, stdout};

use tokio::runtime::Builder;

use ssh_mcp::embed::cli::{Cli, CliParseError, HELP_TEXT, run_subcommand};

fn main() -> Result<(), Box<dyn Error + Send + Sync>> {
    let cli = match Cli::parse_env() {
        Ok(cli) => cli,
        Err(CliParseError::HelpRequested) => {
            let handle = stdout();
            let mut out = handle.lock();
            out.write_all(HELP_TEXT.as_bytes())?;
            return Ok(());
        }
        Err(CliParseError::VersionRequested) => {
            let handle = stdout();
            let mut out = handle.lock();
            out.write_all(format!("ssh-mcp-tail {}\n", env!("CARGO_PKG_VERSION")).as_bytes())?;
            return Ok(());
        }
        Err(err) => {
            let handle = stderr();
            let mut err_out = handle.lock();
            err_out.write_all(format!("error: {err}\n\n").as_bytes())?;
            err_out.write_all(HELP_TEXT.as_bytes())?;
            return Err(Box::new(err));
        }
    };
    let runtime = Builder::new_multi_thread().enable_all().build()?;
    runtime.block_on(run_subcommand(cli))
}
