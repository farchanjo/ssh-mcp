//! Argument types for the v5 Phase 3 `ssh_sub_*` subscription tools.
//!
//! These tools manage per-`SubId` lanes (Channel Mux v5 Phase 2) — open
//! / close / pause / resume / filter / replay / list / stats — plus a
//! daemon-wide stats snapshot. The args layer mirrors the existing
//! `connection.rs` / `execute.rs` style: every optional knob carries a
//! `#[schemars(default = "...")]` so smaller LLMs can read the schema
//! and pick reasonable values.

use schemars::JsonSchema;
use serde::Deserialize;

use crate::domain::subscription::LagPolicy;

// ---- defaults ---------------------------------------------------------

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_lifetime_kind() -> Option<LifetimeKind> {
    Some(LifetimeKind::Manual)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_grace_ms() -> Option<u32> {
    Some(2_000)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_lag_policy() -> Option<LagPolicy> {
    Some(LagPolicy::Snapshot)
}

#[expect(
    clippy::unnecessary_wraps,
    reason = "serde requires the fn return type to match the field type Option<T>"
)]
const fn default_replay_cursor() -> Option<u64> {
    Some(0)
}

// ---- shared enums -----------------------------------------------------

/// Lifetime selector exposed on the wire surface. Translates into a
/// [`crate::domain::subscription::SubscriptionLifetime`] inside the use
/// case once the optional `grace_ms` / `ttl_secs` is folded in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum LifetimeKind {
    /// Lane stays open until explicit unsubscribe (default).
    Manual,
    /// Auto-close after `grace_ms` of consumer inactivity.
    AutoClose,
    /// Hard cap on lane lifetime regardless of activity.
    Lease,
}

// ---- argument structs -------------------------------------------------

/// Arguments for the `ssh_subscribe` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshSubscribeArgs {
    /// Resource URI (e.g. `shell://<id>/output`, `command://<id>/output`).
    pub uri: String,

    /// Lifetime kind. Default: `manual`.
    #[schemars(default = "default_lifetime_kind")]
    pub lifetime: Option<LifetimeKind>,

    /// Grace window in ms — only honoured when `lifetime=auto_close`.
    /// Default: 2000.
    #[schemars(default = "default_grace_ms")]
    pub grace_ms: Option<u32>,

    /// TTL in seconds — only honoured when `lifetime=lease`.
    pub ttl_secs: Option<u32>,

    /// Lag policy applied when the lane mpsc fills. Default: `snapshot`.
    #[schemars(default = "default_lag_policy")]
    pub lag_policy: Option<LagPolicy>,

    /// Optional regex filter compiled at subscribe time. Empty / absent
    /// means no filter.
    pub filter: Option<String>,
}

/// Arguments for the `ssh_unsubscribe` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshUnsubscribeArgs {
    /// `SUB_ID` returned from `ssh_subscribe`.
    pub sub_id: String,
}

/// Arguments for the `ssh_sub_pause` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshSubPauseArgs {
    /// `SUB_ID` to pause.
    pub sub_id: String,
}

/// Arguments for the `ssh_sub_resume` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshSubResumeArgs {
    /// `SUB_ID` to resume.
    pub sub_id: String,
}

/// Arguments for the `ssh_sub_filter` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshSubFilterArgs {
    /// `SUB_ID` whose filter is being updated.
    pub sub_id: String,

    /// New regex filter. Empty string clears the filter (forward all
    /// events).
    pub regex: String,
}

/// Arguments for the `ssh_sub_replay` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshSubReplayArgs {
    /// `SUB_ID` to replay.
    pub sub_id: String,

    /// Cursor (byte offset) to replay from. Default: 0 — replays from
    /// the start of the available ring-buffer window.
    #[schemars(default = "default_replay_cursor")]
    pub from_cursor: Option<u64>,
}

/// Arguments for the `ssh_sub_list` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshSubListArgs {
    /// Optional URI prefix filter. Returns only subs whose canonical
    /// URI starts with this prefix.
    pub uri_prefix: Option<String>,

    /// Optional peer-id filter. Currently a placeholder — Phase 3 lanes
    /// are not peer-keyed; reserved for future wiring.
    pub peer_id: Option<String>,
}

/// Arguments for the `ssh_sub_stats` MCP tool.
#[derive(Debug, Deserialize, JsonSchema)]
pub struct SshSubStatsArgs {
    /// `SUB_ID` to inspect.
    pub sub_id: String,
}

/// Arguments for the `ssh_daemon_stats` MCP tool. Currently empty —
/// the tool returns aggregate counters across every lane in scope.
///
/// Carries a `_reserved` field with `#[serde(default, skip)]` so the
/// struct stays non-empty (avoids `clippy::empty_structs_with_brackets`)
/// while remaining wire-compatible with `{}`.
#[derive(Debug, Default, Deserialize, JsonSchema)]
pub struct SshDaemonStatsArgs {
    /// Reserved field — not deserialised, never serialised. Phase 4
    /// may layer optional knobs (e.g. `top_n`) here.
    #[serde(default, skip)]
    #[allow(dead_code, reason = "reserved field for forward-compat")]
    _reserved: (),
}

#[cfg(test)]
mod tests {
    use super::{
        LifetimeKind, SshDaemonStatsArgs, SshSubFilterArgs, SshSubListArgs, SshSubPauseArgs,
        SshSubReplayArgs, SshSubResumeArgs, SshSubStatsArgs, SshSubscribeArgs, SshUnsubscribeArgs,
    };
    use schemars::schema_for;
    use serde_json::{Value, json};

    fn property_default<'a>(schema_json: &'a Value, field: &str) -> Option<&'a Value> {
        schema_json.get("properties")?.get(field)?.get("default")
    }

    #[test]
    fn lifetime_kind_serde_round_trip() {
        for (variant, wire) in [
            (LifetimeKind::Manual, "\"manual\""),
            (LifetimeKind::AutoClose, "\"auto_close\""),
            (LifetimeKind::Lease, "\"lease\""),
        ] {
            let parsed: LifetimeKind = serde_json::from_str(wire).expect("parse");
            assert_eq!(parsed, variant);
        }
    }

    #[test]
    fn ssh_subscribe_args_emits_documented_defaults() {
        // schemars 1.x folds `Option<EnumByRef>` defaults onto the
        // referenced subschema rather than the property; for the
        // numeric/string-typed fields the default lands directly on
        // the property. This test asserts the latter (numeric grace_ms)
        // and the schema's mere existence for the enum-typed fields.
        let schema = schema_for!(SshSubscribeArgs);
        let json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&json, "grace_ms"),
            Some(&Value::from(2_000_u64))
        );
        // Enum-typed fields still appear as properties with a $ref.
        assert!(json.get("properties").and_then(|p| p.get("lifetime")).is_some());
        assert!(json.get("properties").and_then(|p| p.get("lag_policy")).is_some());
    }

    #[test]
    fn ssh_subscribe_args_parse_minimal_payload() {
        let v = json!({"uri": "shell://x/output"});
        let parsed: SshSubscribeArgs = serde_json::from_value(v).expect("parse");
        assert_eq!(parsed.uri, "shell://x/output");
    }

    #[test]
    fn ssh_unsubscribe_args_round_trip() {
        let v = json!({"sub_id": "019028a3"});
        let parsed: SshUnsubscribeArgs = serde_json::from_value(v).expect("parse");
        assert_eq!(parsed.sub_id, "019028a3");
    }

    #[test]
    fn ssh_sub_pause_resume_filter_replay_args_parse() {
        let _: SshSubPauseArgs =
            serde_json::from_value(json!({"sub_id": "x"})).expect("pause");
        let _: SshSubResumeArgs =
            serde_json::from_value(json!({"sub_id": "x"})).expect("resume");
        let f: SshSubFilterArgs =
            serde_json::from_value(json!({"sub_id": "x", "regex": "ERR.*"})).expect("filter");
        assert_eq!(f.regex, "ERR.*");
        let r: SshSubReplayArgs =
            serde_json::from_value(json!({"sub_id": "x", "from_cursor": 42})).expect("replay");
        assert_eq!(r.from_cursor, Some(42));
    }

    #[test]
    fn ssh_sub_list_args_accept_empty_payload() {
        let parsed: SshSubListArgs = serde_json::from_value(json!({})).expect("parse");
        assert!(parsed.uri_prefix.is_none());
        assert!(parsed.peer_id.is_none());
    }

    #[test]
    fn ssh_sub_stats_args_parse() {
        let v = json!({"sub_id": "abc"});
        let parsed: SshSubStatsArgs = serde_json::from_value(v).expect("parse");
        assert_eq!(parsed.sub_id, "abc");
    }

    #[test]
    fn ssh_daemon_stats_args_round_trip_empty() {
        let v = json!({});
        let _: SshDaemonStatsArgs = serde_json::from_value(v).expect("parse");
        let v2 = SshDaemonStatsArgs::default();
        let _ = format!("{v2:?}");
    }

    #[test]
    fn ssh_sub_replay_default_cursor_is_zero() {
        let schema = schema_for!(SshSubReplayArgs);
        let json = serde_json::to_value(&schema).expect("schema -> json");
        assert_eq!(
            property_default(&json, "from_cursor"),
            Some(&Value::from(0_u64))
        );
    }

    #[test]
    fn ssh_subscribe_args_round_trip_with_full_payload() {
        let v = json!({
            "uri": "shell://x/output",
            "lifetime": "auto_close",
            "grace_ms": 5000,
            "ttl_secs": 60,
            "lag_policy": "drop_oldest",
            "filter": "ERR.*"
        });
        let parsed: SshSubscribeArgs = serde_json::from_value(v).expect("parse");
        assert_eq!(parsed.uri, "shell://x/output");
        assert_eq!(parsed.lifetime, Some(LifetimeKind::AutoClose));
        assert_eq!(parsed.grace_ms, Some(5_000));
        assert_eq!(parsed.ttl_secs, Some(60));
        assert_eq!(parsed.filter.as_deref(), Some("ERR.*"));
    }

    #[test]
    fn ssh_sub_filter_args_accept_empty_regex() {
        // Empty regex clears the filter — the args struct accepts it.
        let v = json!({"sub_id": "x", "regex": ""});
        let parsed: SshSubFilterArgs = serde_json::from_value(v).expect("parse");
        assert!(parsed.regex.is_empty());
    }

    #[test]
    fn ssh_sub_replay_args_negative_cursor_is_rejected() {
        // u64 deserialisation rejects negative values via serde.
        let v = json!({"sub_id": "x", "from_cursor": -1});
        assert!(serde_json::from_value::<SshSubReplayArgs>(v).is_err());
    }

    #[test]
    fn ssh_subscribe_args_rejects_unknown_lifetime() {
        let v = json!({"uri": "shell://x/output", "lifetime": "forever"});
        assert!(serde_json::from_value::<SshSubscribeArgs>(v).is_err());
    }

    #[test]
    fn ssh_subscribe_args_rejects_unknown_lag_policy() {
        let v = json!({"uri": "shell://x/output", "lag_policy": "nope"});
        assert!(serde_json::from_value::<SshSubscribeArgs>(v).is_err());
    }

    #[test]
    fn ssh_sub_list_args_round_trip_with_both_filters() {
        let v = json!({"uri_prefix": "shell://", "peer_id": "p-1"});
        let parsed: SshSubListArgs = serde_json::from_value(v).expect("parse");
        assert_eq!(parsed.uri_prefix.as_deref(), Some("shell://"));
        assert_eq!(parsed.peer_id.as_deref(), Some("p-1"));
    }

    #[test]
    fn lifetime_kind_is_copy() {
        let k = LifetimeKind::Manual;
        let twin = k;
        assert_eq!(k, twin);
    }
}
