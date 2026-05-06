//! Argument types for the ADR 0011 `ssh_rsync*` tool surface.
//!
//! v7.0.0-alpha.2 architectural retrenchment: the deployed-agent path
//! was retracted. The `transport` enum is now `Auto | Wire | Sftp`;
//! both `Wire` and `Sftp` resolve to in-process transports living
//! inside the host crate. Today every code path returns a "being
//! implemented" wire error; the surface freezes the public-API shape
//! so that future work is purely body-replacement.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};

/// Auto-detect transport (default) — probe the remote rsync version
/// and prefer the wire-compat client; fall back to the SFTP transport
/// if rsync is missing or older than v31.
const DEFAULT_TRANSPORT_AUTO: &str = "auto";

/// `transport` enum surfaced to the MCP host. Mirrors ADR 0011 §
/// "Tool surface" and `LocalRsyncTransportPort` impls.
///
/// Serialised as a `snake_case` string so the wire shape stays
/// self-describing (`auto` / `wire` / `sftp`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum RsyncTransportArg {
    /// Probe the remote and prefer wire-compat (rsync v31+); fall
    /// back to the SFTP transport if rsync is missing / older.
    #[default]
    Auto,
    /// Force the wire-compat client. Returns `RSYNC_VERSION_TOO_OLD`
    /// or `RSYNC_NOT_FOUND` if the remote does not have rsync v31+.
    Wire,
    /// Skip the probe; drive the SFTP fallback. Universal — works on
    /// any host with a working SFTP subsystem (i.e. every host
    /// `ssh_upload` already works against), at the cost of slower
    /// throughput compared to delta-sync over `rsync --server`.
    Sftp,
}

/// Attribute-preservation flags.
///
/// Mirrors [`crate::adapters::rsync::types::PreserveFlags`] verbatim
/// — keeping a host-side mirror lets us derive `JsonSchema` here
/// without coupling the inbound DTO to the value-object module's
/// serde-only shape.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema)]
#[expect(
    clippy::struct_excessive_bools,
    reason = "field-by-field 1:1 mirror of rsync preserve flags; matches the proto crate's PreserveFlags shape and the rsync long-flags surface."
)]
pub struct PreserveFlagsArg {
    /// Preserve POSIX mode bits (`-p`).
    pub perms: bool,
    /// Preserve modification time (`-t`).
    pub mtime: bool,
    /// Preserve numeric owner (`-o`; root only on remote).
    pub owner: bool,
    /// Preserve numeric group (`-g`).
    pub group: bool,
    /// Preserve symbolic links as-is, never follow (`-l`).
    pub links: bool,
    /// Preserve hard-link graph (`-H`).
    pub hardlinks: bool,
    /// Preserve sparse holes (`-S`).
    pub sparse: bool,
    /// Preserve block / character devices, fifos, sockets (`-D`;
    /// root only on remote).
    pub devices: bool,
}

impl Default for PreserveFlagsArg {
    /// Defaults match `rsync -a` minus `-D` and `-H` (matches the
    /// host-side [`crate::adapters::rsync::types::PreserveFlags::default`]).
    fn default() -> Self {
        Self {
            perms: true,
            mtime: true,
            owner: true,
            group: true,
            links: true,
            hardlinks: false,
            sparse: false,
            devices: false,
        }
    }
}

/// Free-form rsync feature-flag bundle handed to `ssh_rsync`.
#[expect(
    clippy::struct_excessive_bools,
    reason = "field-by-field 1:1 mirror of rsync long flags; collapsing into a bitmask would lose serde / schemars docstrings and obscure the mapping for callers reading the JSON schema."
)]
#[derive(Debug, Clone, Default, Serialize, Deserialize, JsonSchema)]
pub struct RsyncOptsArg {
    /// `-r` — recurse into directories.
    #[serde(default)]
    pub recursive: bool,
    /// `-a` — alias for `-rlptgoD`. Pre-empts `recursive` and
    /// `preserve` when set.
    #[serde(default)]
    pub archive: bool,
    /// `--delete` — remove destination paths that are not in source.
    #[serde(default)]
    pub delete: bool,
    /// `--exclude=PATTERN` — gitignore-style exclude list.
    #[serde(default)]
    pub exclude: Vec<String>,
    /// `--include=PATTERN` — overrides `--exclude` when both match.
    #[serde(default)]
    pub include: Vec<String>,
    /// `-n` / `--dry-run` — emit `FileSkipped { reason: DryRun }`
    /// for every would-be op without changing the destination.
    #[serde(default)]
    pub dry_run: bool,
    /// `--bwlimit=KBPS` — token-bucket bandwidth cap on the
    /// transport writer side.
    #[serde(default)]
    pub bwlimit_kbps: Option<u64>,
    /// `-z` — request rsync-side compression. Passed through verbatim
    /// to `rsync --server` on the wire path; the agent path ignores
    /// this flag (the russh channel already gets compression for free
    /// if the SSH config enables it).
    #[serde(default)]
    pub compress: bool,
    /// `--partial` — inherit ADR 0010 resume semantics. When `false`
    /// (default), an interrupted transfer truncates the destination
    /// and starts from byte 0 on retry; when `true`, the destination
    /// is preserved between retries.
    #[serde(default)]
    pub partial: bool,
    /// `-c` / `--checksum` — force a content checksum even when size
    /// + mtime match.
    #[serde(default)]
    pub verify_checksum: bool,
    /// Attribute-preservation mask.
    #[serde(default)]
    pub preserve: PreserveFlagsArg,
}

/// Schemars 1.2 default-fn helper — `release_when_no_subs` defaults to
/// `false` so v6.1 hosts that never set the field continue to behave
/// like manual close.
#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the default fn return type to match the field type Option<bool>"
)]
const fn default_release_when_no_subs() -> Option<bool> {
    Some(false)
}

/// Arguments for the `ssh_rsync` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshRsyncArgs {
    /// `SESSION_ID` returned from `ssh_connect`.
    pub session_id: String,

    /// Local path or remote path. Direction inferred from which side
    /// has the `host:` prefix — exactly one of `src` / `dst` must be
    /// remote.
    pub src: String,

    /// See `src`. Exactly one of `src` / `dst` must be remote.
    pub dst: String,

    /// Rsync feature flags. Default mirrors `rsync -a` minus `-D` /
    /// `-H` (see [`PreserveFlagsArg::default`]).
    #[serde(default)]
    pub opts: RsyncOptsArg,

    /// Transport selection. Default `Auto` (probe; prefer
    /// wire-compat).
    #[serde(default)]
    pub transport: RsyncTransportArg,

    /// ADR 0003 lifecycle binding — auto-release the rsync session
    /// when the last subscriber detaches. Default `false` preserves
    /// v6.1 manual-close semantics.
    #[schemars(default = "default_release_when_no_subs")]
    pub release_when_no_subs: Option<bool>,
}

/// Arguments for the `ssh_rsync_cancel` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshRsyncCancelArgs {
    /// `RSYNC_ID` returned from `ssh_rsync`.
    pub rsync_id: String,
}

/// Arguments for the `ssh_rsync_stats` MCP tool.
#[derive(Debug, Deserialize, Serialize, JsonSchema)]
pub struct SshRsyncStatsArgs {
    /// `RSYNC_ID` returned from `ssh_rsync`.
    pub rsync_id: String,
}

/// Default transport-arg discriminator string used for documentation.
/// The `default` impl above is the runtime source of truth.
#[must_use]
pub const fn default_transport_label() -> &'static str {
    DEFAULT_TRANSPORT_AUTO
}

#[cfg(test)]
mod tests {
    use super::{
        PreserveFlagsArg, RsyncOptsArg, RsyncTransportArg, SshRsyncArgs, SshRsyncCancelArgs,
        SshRsyncStatsArgs, default_transport_label,
    };
    use schemars::schema_for;

    #[test]
    fn transport_arg_default_is_auto() {
        assert_eq!(RsyncTransportArg::default(), RsyncTransportArg::Auto);
        assert_eq!(default_transport_label(), "auto");
    }

    #[test]
    fn preserve_flags_default_matches_archive_minus_root_only() {
        let f = PreserveFlagsArg::default();
        assert!(f.perms);
        assert!(f.mtime);
        assert!(f.owner);
        assert!(f.group);
        assert!(f.links);
        assert!(!f.hardlinks);
        assert!(!f.sparse);
        assert!(!f.devices);
    }

    #[test]
    fn rsync_opts_default_is_empty() {
        let o = RsyncOptsArg::default();
        assert!(!o.recursive);
        assert!(!o.archive);
        assert!(!o.delete);
        assert!(o.exclude.is_empty());
        assert!(o.include.is_empty());
        assert!(!o.dry_run);
        assert_eq!(o.bwlimit_kbps, None);
        assert!(!o.compress);
        assert!(!o.partial);
        assert!(!o.verify_checksum);
    }

    #[test]
    fn ssh_rsync_args_schema_renders_required_fields() {
        let schema = schema_for!(SshRsyncArgs);
        let json = serde_json::to_value(&schema).expect("schema -> json");
        // session_id, src, and dst are all required; opts /
        // transport / agent_path / release_when_no_subs default.
        let required = json
            .get("required")
            .and_then(|r| r.as_array())
            .cloned()
            .unwrap_or_default();
        let names: Vec<String> = required
            .iter()
            .filter_map(|v| v.as_str().map(str::to_string))
            .collect();
        assert!(names.iter().any(|s| s == "session_id"));
        assert!(names.iter().any(|s| s == "src"));
        assert!(names.iter().any(|s| s == "dst"));
    }

    #[test]
    fn ssh_rsync_cancel_args_carries_rsync_id() {
        let args: SshRsyncCancelArgs =
            serde_json::from_str(r#"{"rsync_id":"rs-1"}"#).expect("parse");
        assert_eq!(args.rsync_id, "rs-1");
    }

    #[test]
    fn ssh_rsync_stats_args_carries_rsync_id() {
        let args: SshRsyncStatsArgs =
            serde_json::from_str(r#"{"rsync_id":"rs-2"}"#).expect("parse");
        assert_eq!(args.rsync_id, "rs-2");
    }
}
