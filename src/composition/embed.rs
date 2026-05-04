//! Phase 4 placeholder — composition wiring for the embedded daemon.
//!
//! The full implementation will land alongside the rest of the
//! `ssh-mcp-tail` binary in Phase 4. Phase 3 ships only the function
//! signatures so [`crate::embed::cli`] (which the user-authored clap
//! tree references) compiles end-to-end against the lib build.

use std::error::Error;

use crate::embed::cli::{DaemonArgs, RunArgs, ShellArgs};

/// Phase 4 placeholder. Run the multi-session NDJSON daemon loop.
///
/// # Errors
///
/// Always returns an error reporting that the daemon is not yet wired;
/// binaries propagate this so operators see a clear Phase-4-pending
/// message rather than a silent no-op.
pub async fn run_daemon(_args: DaemonArgs) -> Result<(), Box<dyn Error + Send + Sync>> {
    Err("ssh-mcp-tail daemon is a Phase 4 deliverable; not yet wired".into())
}

/// Phase 4 placeholder. Run the one-shot connect + exec wrapper.
///
/// # Errors
///
/// Always returns an error reporting that the daemon is not yet wired.
pub async fn run_one_shot(_args: RunArgs) -> Result<(), Box<dyn Error + Send + Sync>> {
    Err("ssh-mcp-tail run is a Phase 4 deliverable; not yet wired".into())
}

/// Phase 4 placeholder. Run the interactive PTY wrapper.
///
/// # Errors
///
/// Always returns an error reporting that the daemon is not yet wired.
pub async fn run_shell(_args: ShellArgs) -> Result<(), Box<dyn Error + Send + Sync>> {
    Err("ssh-mcp-tail shell is a Phase 4 deliverable; not yet wired".into())
}
