//! Argv parser for `ssh-mcp-tail`.
//!
//! The CLI exposes three subcommands per ADR 0008:
//!
//! - `daemon` — primary deliverable: NDJSON command/event loop.
//! - `run` — convenience wrapper that synthesises a single
//!   `connect` + `exec` + `subscribe` flow.
//! - `shell` — convenience wrapper that synthesises an interactive
//!   `connect` + `shell_open` flow.
//!
//! `run` and `shell` both build their internal NDJSON op stream and
//! reuse the same dispatcher / event mux as `daemon`. The wrappers
//! exist so operators can pipe a single command without writing
//! NDJSON by hand.
//!
//! We hand-roll the argv parser instead of pulling `clap`'s derive
//! macros — the strict `forbid` Clippy baseline rejects the
//! `#[allow(clippy::restriction)]` insertions clap produces. The
//! parser surface is small (3 subcommands, ~7 flags) so the cost of
//! avoiding clap is one ~150 LOC module that keeps the lint gate
//! happy.

use std::env::args as env_args;
use std::error::Error;
use std::path::PathBuf;

use crate::composition::embed::{run_daemon, run_one_shot, run_shell};

/// Top-level CLI surface for `ssh-mcp-tail`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Cli {
    /// Subcommand selected on the argv.
    pub cmd: Subcmd,
}

/// One of the three documented subcommands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Subcmd {
    /// Multi-session NDJSON command/event loop.
    Daemon(DaemonArgs),
    /// One-shot connect + exec + drain.
    Run(RunArgs),
    /// Interactive PTY shell.
    Shell(ShellArgs),
}

/// Parameters for the `daemon` subcommand. Every field is optional;
/// each maps to an env-var override (see `docs/DAEMON.md`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DaemonArgs {
    /// Optional explicit `SSH_NDJSON_LINE_MAX` override.
    pub line_max: Option<usize>,
    /// Optional explicit `SSH_HEARTBEAT_INTERVAL_S` override.
    pub heartbeat_secs: Option<u64>,
    /// Optional explicit `SSH_DAEMON_STATS_INTERVAL_S` override.
    pub stats_secs: Option<u64>,
}

/// Parameters for the `run` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunArgs {
    /// Remote host (DNS or IP).
    pub host: String,
    /// Remote user.
    pub user: String,
    /// Optional path to the private key.
    pub key: Option<PathBuf>,
    /// Optional remote port.
    pub port: Option<u16>,
    /// Auto-disconnect after the command completes.
    pub auto_disconnect: bool,
    /// Trailing positional args = the remote command.
    pub command: Vec<String>,
}

/// Parameters for the `shell` subcommand.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellArgs {
    /// Remote host.
    pub host: String,
    /// Remote user.
    pub user: String,
    /// Optional path to the private key.
    pub key: Option<PathBuf>,
    /// Optional remote port.
    pub port: Option<u16>,
    /// PTY width.
    pub cols: u16,
    /// PTY height.
    pub rows: u16,
}

impl Cli {
    /// Parse the process argv (`std::env::args_os()`) and exit
    /// with `--help` text when the parse fails. Mirrors the clap
    /// surface so the binary entry point stays simple.
    ///
    /// # Errors
    /// Returns [`CliParseError`] on any parse failure. The binary
    /// converts the error into a stderr message and exits with a
    /// non-zero status.
    pub fn parse_argv<I, S>(argv: I) -> Result<Self, CliParseError>
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        let mut iter = argv.into_iter().map(Into::into);
        // Skip program name.
        let _ = iter.next();
        let sub = iter.next().ok_or(CliParseError::MissingSubcommand)?;
        let rest: Vec<String> = iter.collect();
        match sub.as_str() {
            "daemon" => Ok(Self {
                cmd: Subcmd::Daemon(parse_daemon_args(&rest)?),
            }),
            "run" => Ok(Self {
                cmd: Subcmd::Run(parse_run_args(&rest)?),
            }),
            "shell" => Ok(Self {
                cmd: Subcmd::Shell(parse_shell_args(&rest)?),
            }),
            "--help" | "-h" => Err(CliParseError::HelpRequested),
            "--version" | "-V" => Err(CliParseError::VersionRequested),
            other => Err(CliParseError::UnknownSubcommand(other.to_string())),
        }
    }

    /// Convenience wrapper around [`Self::parse_argv`] that pulls argv
    /// from `std::env::args_os()`.
    ///
    /// # Errors
    /// Same as [`Self::parse_argv`].
    pub fn parse_env() -> Result<Self, CliParseError> {
        let argv: Vec<String> = env_args().collect();
        Self::parse_argv(argv)
    }
}

fn parse_daemon_args(args: &[String]) -> Result<DaemonArgs, CliParseError> {
    let mut out = DaemonArgs::default();
    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--line-max" => {
                let v = next_value(args, &mut i, "--line-max")?;
                out.line_max = Some(parse_usize(&v)?);
            }
            "--heartbeat-secs" => {
                let v = next_value(args, &mut i, "--heartbeat-secs")?;
                out.heartbeat_secs = Some(parse_u64(&v)?);
            }
            "--stats-secs" => {
                let v = next_value(args, &mut i, "--stats-secs")?;
                out.stats_secs = Some(parse_u64(&v)?);
            }
            other => return Err(CliParseError::UnknownFlag(other.to_string())),
        }
    }
    Ok(out)
}

fn parse_run_args(args: &[String]) -> Result<RunArgs, CliParseError> {
    let mut acc = RunArgsAccum::new();
    let mut i = 0_usize;
    while i < args.len() {
        if acc.seen_separator {
            acc.command.push(args[i].clone());
            i += 1;
            continue;
        }
        consume_run_flag(args, &mut i, &mut acc)?;
    }
    acc.into_run_args()
}

struct RunArgsAccum {
    host: Option<String>,
    user: Option<String>,
    key: Option<PathBuf>,
    port: Option<u16>,
    auto_disconnect: bool,
    command: Vec<String>,
    seen_separator: bool,
}

impl RunArgsAccum {
    const fn new() -> Self {
        Self {
            host: None,
            user: None,
            key: None,
            port: None,
            auto_disconnect: true,
            command: Vec::new(),
            seen_separator: false,
        }
    }

    fn into_run_args(self) -> Result<RunArgs, CliParseError> {
        Ok(RunArgs {
            host: self.host.ok_or(CliParseError::MissingFlag("--host"))?,
            user: self.user.ok_or(CliParseError::MissingFlag("--user"))?,
            key: self.key,
            port: self.port,
            auto_disconnect: self.auto_disconnect,
            command: self.command,
        })
    }
}

fn consume_run_flag(
    args: &[String],
    i: &mut usize,
    acc: &mut RunArgsAccum,
) -> Result<(), CliParseError> {
    match args[*i].as_str() {
        "--host" => acc.host = Some(next_value(args, i, "--host")?),
        "--user" => acc.user = Some(next_value(args, i, "--user")?),
        "--key" => acc.key = Some(PathBuf::from(next_value(args, i, "--key")?)),
        "--port" => acc.port = Some(parse_u16(&next_value(args, i, "--port")?)?),
        "--no-auto-disconnect" => {
            acc.auto_disconnect = false;
            *i += 1;
        }
        "--" => {
            acc.seen_separator = true;
            *i += 1;
        }
        other => return Err(CliParseError::UnknownFlag(other.to_string())),
    }
    Ok(())
}

fn parse_shell_args(args: &[String]) -> Result<ShellArgs, CliParseError> {
    let mut host = None;
    let mut user = None;
    let mut key = None;
    let mut port = None;
    let mut cols = 80_u16;
    let mut rows = 24_u16;
    let mut i = 0_usize;
    while i < args.len() {
        match args[i].as_str() {
            "--host" => host = Some(next_value(args, &mut i, "--host")?),
            "--user" => user = Some(next_value(args, &mut i, "--user")?),
            "--key" => key = Some(PathBuf::from(next_value(args, &mut i, "--key")?)),
            "--port" => port = Some(parse_u16(&next_value(args, &mut i, "--port")?)?),
            "--cols" => cols = parse_u16(&next_value(args, &mut i, "--cols")?)?,
            "--rows" => rows = parse_u16(&next_value(args, &mut i, "--rows")?)?,
            other => return Err(CliParseError::UnknownFlag(other.to_string())),
        }
    }
    Ok(ShellArgs {
        host: host.ok_or(CliParseError::MissingFlag("--host"))?,
        user: user.ok_or(CliParseError::MissingFlag("--user"))?,
        key,
        port,
        cols,
        rows,
    })
}

fn next_value(args: &[String], i: &mut usize, flag: &'static str) -> Result<String, CliParseError> {
    let value = args
        .get(*i + 1)
        .ok_or(CliParseError::MissingFlagValue(flag))?
        .clone();
    *i += 2;
    Ok(value)
}

fn parse_usize(value: &str) -> Result<usize, CliParseError> {
    value
        .parse::<usize>()
        .map_err(|err| CliParseError::ParseIntFailed(value.to_string(), err.to_string()))
}

fn parse_u64(value: &str) -> Result<u64, CliParseError> {
    value
        .parse::<u64>()
        .map_err(|err| CliParseError::ParseIntFailed(value.to_string(), err.to_string()))
}

fn parse_u16(value: &str) -> Result<u16, CliParseError> {
    value
        .parse::<u16>()
        .map_err(|err| CliParseError::ParseIntFailed(value.to_string(), err.to_string()))
}

/// Errors surfaced by the argv parser. Each maps to a clear stderr
/// message the binary entry can render before exiting with a non-
/// zero status.
#[derive(Debug, thiserror::Error)]
pub enum CliParseError {
    /// `ssh-mcp-tail` was invoked without any subcommand.
    #[error("no subcommand provided (expected `daemon`, `run`, or `shell`)")]
    MissingSubcommand,
    /// User passed a subcommand we don't recognise.
    #[error("unknown subcommand: {0}")]
    UnknownSubcommand(String),
    /// User passed a flag we don't recognise.
    #[error("unknown flag: {0}")]
    UnknownFlag(String),
    /// A required flag is missing.
    #[error("missing required flag {0}")]
    MissingFlag(&'static str),
    /// A flag was provided without a value.
    #[error("flag {0} requires a value")]
    MissingFlagValue(&'static str),
    /// Value attached to a numeric flag could not be parsed.
    #[error("could not parse {0} as integer: {1}")]
    ParseIntFailed(String, String),
    /// User asked for `--help`. The binary entry prints the help text
    /// and exits with status 0.
    #[error("help requested")]
    HelpRequested,
    /// User asked for `--version`. The binary entry prints the version
    /// and exits with status 0.
    #[error("version requested")]
    VersionRequested,
}

/// Static `--help` text for the binary entry to render.
pub const HELP_TEXT: &str = concat!(
    "ssh-mcp-tail ",
    env!("CARGO_PKG_VERSION"),
    "\nNDJSON daemon transport for ssh-mcp.\n",
    "\nUsage:\n",
    "  ssh-mcp-tail daemon [--line-max N] [--heartbeat-secs N] [--stats-secs N]\n",
    "  ssh-mcp-tail run --host H --user U [--key K] [--port P] [--no-auto-disconnect] -- <cmd>...\n",
    "  ssh-mcp-tail shell --host H --user U [--key K] [--port P] [--cols 80] [--rows 24]\n",
    "\nDocumentation: docs/DAEMON.md (ADR 0008).\n",
);

/// Dispatch the parsed CLI tree onto the matching composition root
/// helper.
///
/// # Errors
/// Surfaces any error reported by the embed wiring or the subcommand
/// dispatch loop (transport setup failure, dispatcher failure, ...).
pub async fn run_subcommand(cli: Cli) -> Result<(), Box<dyn Error + Send + Sync>> {
    match cli.cmd {
        Subcmd::Daemon(args) => run_daemon(args).await,
        Subcmd::Run(args) => run_one_shot(args).await,
        Subcmd::Shell(args) => run_shell(args).await,
    }
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test-only assertions are deliberately direct"
)]
mod tests {
    use super::*;

    fn argv(args: &[&str]) -> Vec<String> {
        std::iter::once("ssh-mcp-tail")
            .chain(args.iter().copied())
            .map(String::from)
            .collect()
    }

    #[test]
    fn parse_daemon_no_flags() {
        let cli = Cli::parse_argv(argv(&["daemon"])).unwrap();
        assert_eq!(cli.cmd, Subcmd::Daemon(DaemonArgs::default()));
    }

    #[test]
    fn parse_daemon_with_overrides() {
        let cli = Cli::parse_argv(argv(&[
            "daemon",
            "--line-max",
            "32768",
            "--heartbeat-secs",
            "5",
            "--stats-secs",
            "10",
        ]))
        .unwrap();
        match cli.cmd {
            Subcmd::Daemon(args) => {
                assert_eq!(args.line_max, Some(32_768));
                assert_eq!(args.heartbeat_secs, Some(5));
                assert_eq!(args.stats_secs, Some(10));
            }
            _ => panic!("expected daemon"),
        }
    }

    #[test]
    fn parse_run_basic() {
        let cli =
            Cli::parse_argv(argv(&["run", "--host", "h", "--user", "u", "--", "uptime"])).unwrap();
        match cli.cmd {
            Subcmd::Run(args) => {
                assert_eq!(args.host, "h");
                assert_eq!(args.user, "u");
                assert_eq!(args.command, vec!["uptime".to_string()]);
                assert!(args.auto_disconnect);
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn parse_run_no_auto_disconnect() {
        let cli = Cli::parse_argv(argv(&[
            "run",
            "--host",
            "h",
            "--user",
            "u",
            "--no-auto-disconnect",
            "--",
            "ls",
        ]))
        .unwrap();
        match cli.cmd {
            Subcmd::Run(args) => assert!(!args.auto_disconnect),
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn parse_run_with_key_and_port() {
        let cli = Cli::parse_argv(argv(&[
            "run", "--host", "h", "--user", "u", "--key", "/tmp/k", "--port", "2222", "--",
            "uptime",
        ]))
        .unwrap();
        match cli.cmd {
            Subcmd::Run(args) => {
                assert_eq!(args.key.unwrap(), PathBuf::from("/tmp/k"));
                assert_eq!(args.port, Some(2222));
            }
            _ => panic!("expected run"),
        }
    }

    #[test]
    fn parse_shell_with_dimensions() {
        let cli = Cli::parse_argv(argv(&[
            "shell", "--host", "h", "--user", "u", "--cols", "120", "--rows", "30",
        ]))
        .unwrap();
        match cli.cmd {
            Subcmd::Shell(args) => {
                assert_eq!(args.cols, 120);
                assert_eq!(args.rows, 30);
            }
            _ => panic!("expected shell"),
        }
    }

    #[test]
    fn parse_shell_default_dimensions() {
        let cli = Cli::parse_argv(argv(&["shell", "--host", "h", "--user", "u"])).unwrap();
        match cli.cmd {
            Subcmd::Shell(args) => {
                assert_eq!(args.cols, 80);
                assert_eq!(args.rows, 24);
            }
            _ => panic!("expected shell"),
        }
    }

    #[test]
    fn rejects_missing_subcommand() {
        let result = Cli::parse_argv(argv(&[]));
        assert!(matches!(result, Err(CliParseError::MissingSubcommand)));
    }

    #[test]
    fn rejects_unknown_subcommand() {
        let result = Cli::parse_argv(argv(&["bogus"]));
        assert!(matches!(result, Err(CliParseError::UnknownSubcommand(_))));
    }

    #[test]
    fn run_requires_host_and_user() {
        let missing_host = Cli::parse_argv(argv(&["run", "--user", "u"]));
        assert!(matches!(missing_host, Err(CliParseError::MissingFlag(_))));
        let missing_user = Cli::parse_argv(argv(&["run", "--host", "h"]));
        assert!(matches!(missing_user, Err(CliParseError::MissingFlag(_))));
    }

    #[test]
    fn flag_value_parse_error_surfaces() {
        let result = Cli::parse_argv(argv(&["daemon", "--line-max", "not-a-number"]));
        assert!(matches!(result, Err(CliParseError::ParseIntFailed(_, _))));
    }

    #[test]
    fn flag_without_value_surfaces() {
        let result = Cli::parse_argv(argv(&["daemon", "--line-max"]));
        assert!(matches!(result, Err(CliParseError::MissingFlagValue(_))));
    }

    #[test]
    fn unknown_flag_surfaces() {
        let result = Cli::parse_argv(argv(&["daemon", "--frobnicate"]));
        assert!(matches!(result, Err(CliParseError::UnknownFlag(_))));
    }

    #[test]
    fn help_flag_surfaces() {
        let result = Cli::parse_argv(argv(&["--help"]));
        assert!(matches!(result, Err(CliParseError::HelpRequested)));
    }

    #[test]
    fn version_flag_surfaces() {
        let result = Cli::parse_argv(argv(&["--version"]));
        assert!(matches!(result, Err(CliParseError::VersionRequested)));
    }

    #[test]
    fn help_text_mentions_each_subcommand() {
        assert!(HELP_TEXT.contains("daemon"));
        assert!(HELP_TEXT.contains("run"));
        assert!(HELP_TEXT.contains("shell"));
    }
}
