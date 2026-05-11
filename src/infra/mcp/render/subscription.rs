//! Subscription-administration markdown renderers (v5 Phase 3).
//!
//! Each render function maps the matching `*Outcome` from
//! [`crate::application::subscription_admin`] onto the v5 block-style
//! markdown body the rmcp wrapper sends back. The shape mirrors the v4
//! tools — `TOOL_NAME: STATUS / KEY: value / NEXT / HINT`.
//!
//! v5 hygiene additions: a strong `HINT:` line at every `sub_open`
//! response (`HINT: REQUIRED NEXT STEP: ...`) so 27B-class models never
//! leave a lane open without claiming responsibility, and a
//! Hygiene-tail block on long-running tools (`Cleanup:` / `Cost:` /
//! `Idempotency:` / `Hygiene:`) — see ADR 0005.

use serde_json::{Value, json};

use crate::application::subscription_admin::{
    DaemonStatsOutcome, ListSubsOutcome, ReplayOutcome, SetFilterOutcome, SubStatsOutcome,
    SubToggleOutcome, SubscribeOutcome, UnsubscribeOutcome, summary_kind_str,
};
use crate::domain::subscription::{FilterRule, SubscriptionLifetime};
use crate::infra::mcp::helpers::output::sanitize_value;
use crate::ports::subscriber_lane::SubSummary;

/// Strength tag baked into the Phase 3 HINT lines.
///
/// ADR 0005 §"HINT line strength" defines two tiers:
/// * `Required` — skipping the next step turns the resource into a
///   zombie; the LLM MUST execute the call.
/// * `Recommended` — falls back gracefully if skipped, but reduces
///   latency / cost when honoured.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HintStrength {
    /// Required next step.
    Required,
    /// Recommended next step.
    Recommended,
}

impl HintStrength {
    /// Render the strength tag into the wire prefix.
    #[must_use]
    pub const fn as_prefix(self) -> &'static str {
        match self {
            Self::Required => "REQUIRED NEXT STEP",
            Self::Recommended => "RECOMMENDED",
        }
    }
}

/// Append a single `HINT: <prefix>: <body>` advisory line.
pub fn append_hint(out: &mut String, strength: HintStrength, body: &str) {
    out.push_str("\nHINT: ");
    out.push_str(strength.as_prefix());
    out.push_str(": ");
    out.push_str(body);
}

/// Append the `NEXT: <hint>` line.
pub fn append_next(out: &mut String, hint: &str) {
    out.push_str("\nNEXT: ");
    out.push_str(hint);
}

/// Append the v5 Hygiene tail. Writes one line per filled field.
pub fn append_hygiene(
    out: &mut String,
    cleanup: Option<&str>,
    cost: Option<&str>,
    idempotency: Option<&str>,
    hygiene: Option<&str>,
) {
    for (key, value) in [
        ("Cleanup", cleanup),
        ("Cost", cost),
        ("Idempotency", idempotency),
        ("Hygiene", hygiene),
    ] {
        if let Some(v) = value.filter(|s| !s.is_empty()) {
            out.push('\n');
            out.push_str(key);
            out.push_str(": ");
            out.push_str(v);
        }
    }
}

// ---------------------------------------------------------------------------
// Subscribe
// ---------------------------------------------------------------------------

/// Render the markdown body for a successful `sub_open`.
#[must_use]
pub fn subscribe_render(outcome: &SubscribeOutcome) -> String {
    let mut out = String::with_capacity(256);
    append_subscribe_header(&mut out, outcome);
    append_subscribe_inline_flags(&mut out, outcome);
    append_subscribe_advisories(&mut out, outcome);
    append_hygiene(
        &mut out,
        Some("sub_close"),
        Some("O(1) lane open + per-event mpsc"),
        Some("Pass _meta.idempotency_key to dedup retries"),
        Some(hygiene_line_for(outcome)),
    );
    out
}

fn append_subscribe_header(out: &mut String, outcome: &SubscribeOutcome) {
    out.push_str("SUB_OPEN: OK\nSUB_ID: ");
    out.push_str(outcome.sub_id.as_str());
    out.push_str("\nURI: ");
    out.push_str(&sanitize_value(&outcome.uri));
    out.push_str("\nLIFETIME: ");
    out.push_str(lifetime_label(outcome.lifetime));
    out.push_str("\nLAG_POLICY: ");
    out.push_str(outcome.lag_policy.as_str());
    out.push_str("\nGRACE_MS: ");
    out.push_str(&outcome.grace_ms.to_string());
}

/// ADR 0012 phase 5 -- emit the inline-push wire lines AFTER the
/// existing header but BEFORE any NEXT / HINT lines. Both lines are
/// gated on `inline_push_requested` so v7.0.x callers that omit the
/// arg keep byte-identical wires.
fn append_subscribe_inline_flags(out: &mut String, outcome: &SubscribeOutcome) {
    if !outcome.inline_push_requested {
        return;
    }
    out.push_str("\nINLINE_PUSH: yes");
    out.push_str("\nINLINE_PUSH_HONORED: ");
    out.push_str(if outcome.inline_push_honored {
        "yes"
    } else {
        "no"
    });
}

fn append_subscribe_advisories(out: &mut String, outcome: &SubscribeOutcome) {
    append_next(
        out,
        &format!(
            "sub_pause | sub_resume | sub_filter | sub_replay | sub_close sub_id={}",
            outcome.sub_id
        ),
    );
    append_hint(
        out,
        HintStrength::Required,
        &format!(
            "sub_close sub_id={} when done. Skip and the lane becomes a zombie.",
            outcome.sub_id
        ),
    );
    if outcome.inline_push_requested && !outcome.inline_push_honored {
        append_hint(
            out,
            HintStrength::Recommended,
            "client did not advertise ssh_inline_push capability; falling back to pull-mode",
        );
    }
    append_hint(
        out,
        HintStrength::Recommended,
        "Data arrives async via notifications/resources/updated. Wait for the push, then resources/read?cursor=auto. Do NOT use any MCP tool as a sleep. If you must idle between reads, sleep LOCALLY with low values: Unix `sleep 0.2` / `sleep 0.5`, PowerShell `Start-Sleep -Milliseconds 200`. Push fires on whichever fires first: ~1000ms debounce window (SSH_NOTIFY_DEBOUNCE_MS) OR 64KiB accumulated bytes (SSH_NOTIFY_FLUSH_BYTES). 200-500ms local sleeps cover both budgets.",
    );
}

/// ADR 0012 phase 5 (line 259) -- when inline-push was honoured, the
/// Hygiene line steers the LLM to drain bytes from
/// `notifications/ssh/output` instead of chaining
/// `resources/read?cursor=auto` calls. The pull-mode read stays
/// available as a verification path.
const fn hygiene_line_for(outcome: &SubscribeOutcome) -> &'static str {
    if outcome.inline_push_requested && outcome.inline_push_honored {
        "When inline_push=true, bytes arrive in notifications/ssh/output; you do not need to read resources/read?cursor=auto separately. Pull-mode resources/read?cursor=... is still available as a verification path. Hold the SUB_ID; never re-open the same URI without first unsubscribing."
    } else {
        "Hold the SUB_ID; never re-open the same URI without first unsubscribing"
    }
}

/// Render the structured JSON twin for `sub_open`.
#[must_use]
pub fn subscribe_structured(outcome: &SubscribeOutcome) -> Value {
    let mut value = json!({
        "tool": "sub_open",
        "status": "ok",
        "sub_id": outcome.sub_id.as_str(),
        "uri": outcome.uri,
        "lifetime": lifetime_label(outcome.lifetime),
        "lag_policy": outcome.lag_policy.as_str(),
        "grace_ms": outcome.grace_ms,
        "inline_push": outcome.inline_push_requested,
    });
    if outcome.inline_push_requested
        && let Some(map) = value.as_object_mut()
    {
        map.insert(
            "inline_push_honored".to_string(),
            Value::Bool(outcome.inline_push_honored),
        );
    }
    value
}

const fn lifetime_label(lifetime: SubscriptionLifetime) -> &'static str {
    match lifetime {
        SubscriptionLifetime::Manual => "manual",
        SubscriptionLifetime::AutoClose { .. } => "auto_close",
        SubscriptionLifetime::Lease { .. } => "lease",
    }
}

// ---------------------------------------------------------------------------
// Unsubscribe
// ---------------------------------------------------------------------------

/// Render the markdown body for `sub_close`.
#[must_use]
pub fn unsubscribe_render(outcome: &UnsubscribeOutcome) -> String {
    let mut out = String::with_capacity(192);
    out.push_str("SUB_CLOSE: OK\nSUB_ID: ");
    out.push_str(outcome.sub_id.as_str());
    if let Some(uri) = outcome.uri.as_deref() {
        out.push_str("\nURI: ");
        out.push_str(&sanitize_value(uri));
    }
    out.push_str("\nLIFECYCLE_STATE: ");
    out.push_str(outcome.lifecycle_state);
    if let Some(ms) = outcome.grace_remaining_ms {
        out.push_str("\nGRACE_REMAINING_MS: ");
        out.push_str(&ms.to_string());
    }
    append_next(&mut out, "sub_list | sub_open (new lane)");
    out
}

/// Render the structured JSON twin for `sub_close`.
#[must_use]
pub fn unsubscribe_structured(outcome: &UnsubscribeOutcome) -> Value {
    json!({
        "tool": "sub_close",
        "status": "ok",
        "sub_id": outcome.sub_id.as_str(),
        "uri": outcome.uri,
        "lifecycle_state": outcome.lifecycle_state,
        "grace_remaining_ms": outcome.grace_remaining_ms,
    })
}

// ---------------------------------------------------------------------------
// Pause / Resume / Filter / Replay (toggles)
// ---------------------------------------------------------------------------

/// Render the markdown body for `sub_pause`.
#[must_use]
pub fn pause_render(outcome: &SubToggleOutcome) -> String {
    toggle_render("SUB_PAUSE", outcome, "paused")
}

/// Render the structured JSON twin for `sub_pause`.
#[must_use]
pub fn pause_structured(outcome: &SubToggleOutcome) -> Value {
    toggle_structured("sub_pause", outcome)
}

/// Render the markdown body for `sub_resume`.
#[must_use]
pub fn resume_render(outcome: &SubToggleOutcome) -> String {
    toggle_render("SUB_RESUME", outcome, "resumed")
}

/// Render the structured JSON twin for `sub_resume`.
#[must_use]
pub fn resume_structured(outcome: &SubToggleOutcome) -> Value {
    toggle_structured("sub_resume", outcome)
}

fn toggle_render(tool: &str, outcome: &SubToggleOutcome, state_label: &str) -> String {
    let mut out = String::with_capacity(160);
    out.push_str(tool);
    out.push_str(": OK\nSUB_ID: ");
    out.push_str(outcome.sub_id.as_str());
    out.push_str("\nSTATE: ");
    out.push_str(state_label);
    append_next(&mut out, "sub_stats | sub_resume | sub_pause");
    out
}

fn toggle_structured(tool: &str, outcome: &SubToggleOutcome) -> Value {
    json!({
        "tool": tool,
        "status": "ok",
        "sub_id": outcome.sub_id.as_str(),
        "paused": outcome.paused,
    })
}

/// Render the markdown body for `sub_filter`.
#[must_use]
pub fn filter_render(outcome: &SetFilterOutcome) -> String {
    let mut out = String::with_capacity(192);
    out.push_str("SUB_FILTER: OK\nSUB_ID: ");
    out.push_str(outcome.sub_id.as_str());
    out.push_str("\nFILTER: ");
    out.push_str(&filter_label(&outcome.filter));
    append_next(&mut out, "sub_stats | sub_replay | sub_filter");
    out
}

/// Render the structured JSON twin for `sub_filter`.
#[must_use]
pub fn filter_structured(outcome: &SetFilterOutcome) -> Value {
    json!({
        "tool": "sub_filter",
        "status": "ok",
        "sub_id": outcome.sub_id.as_str(),
        "filter": filter_label(&outcome.filter),
    })
}

fn filter_label(filter: &FilterRule) -> String {
    match filter {
        FilterRule::None => "none".to_string(),
        FilterRule::Regex(s) => format!("regex:{s}"),
        FilterRule::Level(level) => format!("level:{level:?}").to_lowercase(),
    }
}

/// Render the markdown body for `sub_replay`.
#[must_use]
pub fn replay_render(outcome: &ReplayOutcome) -> String {
    let mut out = String::with_capacity(160);
    out.push_str("SUB_REPLAY: OK\nSUB_ID: ");
    out.push_str(outcome.sub_id.as_str());
    out.push_str("\nFROM_CURSOR: ");
    out.push_str(&outcome.from_cursor.to_string());
    append_next(&mut out, "sub_stats | sub_filter | sub_replay");
    out
}

/// Render the structured JSON twin for `sub_replay`.
#[must_use]
pub fn replay_structured(outcome: &ReplayOutcome) -> Value {
    json!({
        "tool": "sub_replay",
        "status": "ok",
        "sub_id": outcome.sub_id.as_str(),
        "from_cursor": outcome.from_cursor,
    })
}

// ---------------------------------------------------------------------------
// List
// ---------------------------------------------------------------------------

/// Render the markdown body for `sub_list`.
#[must_use]
pub fn list_render(outcome: &ListSubsOutcome) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("SUB_LIST: OK\nSUBS_TOTAL: ");
    out.push_str(&outcome.subs.len().to_string());
    for s in &outcome.subs {
        append_summary_line(&mut out, s);
    }
    append_next(&mut out, "sub_stats | sub_close | sub_open");
    out
}

fn append_summary_line(out: &mut String, s: &SubSummary) {
    out.push_str("\nSUB ");
    out.push_str(s.sub_id.as_str());
    out.push(' ');
    out.push_str(summary_kind_str(s));
    out.push(' ');
    out.push_str(&sanitize_value(&s.uri));
    out.push_str(" lag=");
    out.push_str(s.lag_policy.as_str());
    out.push_str(" paused=");
    out.push_str(if s.paused { "true" } else { "false" });
}

/// Render the structured JSON twin for `sub_list`.
#[must_use]
pub fn list_structured(outcome: &ListSubsOutcome) -> Value {
    let subs: Vec<Value> = outcome
        .subs
        .iter()
        .map(|s| {
            json!({
                "sub_id": s.sub_id.as_str(),
                "kind": summary_kind_str(s),
                "resource_id": s.resource_id,
                "uri": s.uri,
                "lag_policy": s.lag_policy.as_str(),
                "paused": s.paused,
                "events_sent": s.stats.events_sent,
                "bytes_sent": s.stats.bytes_sent,
            })
        })
        .collect();
    json!({
        "tool": "sub_list",
        "status": "ok",
        "subs_total": outcome.subs.len(),
        "subs": subs,
    })
}

// ---------------------------------------------------------------------------
// Sub stats
// ---------------------------------------------------------------------------

/// Render the markdown body for `sub_stats`.
#[must_use]
pub fn sub_stats_render(outcome: &SubStatsOutcome) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("SUB_STATS: OK\nSUB_ID: ");
    out.push_str(outcome.sub_id.as_str());
    out.push_str("\nEVENTS_SENT: ");
    out.push_str(&outcome.stats.events_sent.to_string());
    out.push_str("\nBYTES_SENT: ");
    out.push_str(&outcome.stats.bytes_sent.to_string());
    out.push_str("\nLAGGED_DROPS: ");
    out.push_str(&outcome.stats.lagged_drops.to_string());
    out.push_str("\nLAGGED_RECOVERIES: ");
    out.push_str(&outcome.stats.lagged_recoveries.to_string());
    out.push_str("\nQUEUE_DEPTH: ");
    out.push_str(&outcome.stats.queue_depth.to_string());
    out.push_str("\nQUEUE_HIGH_WATERMARK: ");
    out.push_str(&outcome.stats.queue_high_watermark.to_string());
    out.push_str("\nBLOCK_TOTAL_MS: ");
    out.push_str(&outcome.stats.block_total_ms.to_string());
    append_sub_stats_inline_lines(&mut out, outcome);
    append_next(&mut out, "sub_filter | sub_replay | sub_close");
    out
}

/// ADR 0012 phase 5 -- emit the inline counters after the existing
/// per-lane stat lines, gated on lane opt-in. Opt-out lanes keep the
/// stats wire byte-identical to v7.0.x.
fn append_sub_stats_inline_lines(out: &mut String, outcome: &SubStatsOutcome) {
    let Some(inline) = outcome.inline else {
        return;
    };
    if !inline.inline_push {
        return;
    }
    out.push_str("\nINLINE_EVENTS_SENT: ");
    out.push_str(&inline.inline_events_sent.to_string());
    out.push_str("\nINLINE_BYTES_SENT: ");
    out.push_str(&inline.inline_bytes_sent.to_string());
}

/// Render the structured JSON twin for `sub_stats`.
#[must_use]
pub fn sub_stats_structured(outcome: &SubStatsOutcome) -> Value {
    let mut value = json!({
        "tool": "sub_stats",
        "status": "ok",
        "sub_id": outcome.sub_id.as_str(),
        "events_sent": outcome.stats.events_sent,
        "bytes_sent": outcome.stats.bytes_sent,
        "lagged_drops": outcome.stats.lagged_drops,
        "lagged_recoveries": outcome.stats.lagged_recoveries,
        "queue_depth": outcome.stats.queue_depth,
        "queue_high_watermark": outcome.stats.queue_high_watermark,
        "block_total_ms": outcome.stats.block_total_ms,
    });
    if let Some(inline) = outcome.inline
        && inline.inline_push
        && let Some(map) = value.as_object_mut()
    {
        map.insert(
            "inline_events_sent".to_string(),
            Value::from(inline.inline_events_sent),
        );
        map.insert(
            "inline_bytes_sent".to_string(),
            Value::from(inline.inline_bytes_sent),
        );
    }
    value
}

// ---------------------------------------------------------------------------
// Daemon stats
// ---------------------------------------------------------------------------

/// Render the markdown body for `sub_stats_all`.
#[must_use]
pub fn daemon_stats_render(outcome: &DaemonStatsOutcome) -> String {
    let mut out = String::with_capacity(256);
    out.push_str("SUB_STATS_ALL: OK\nLANES_TOTAL: ");
    out.push_str(&outcome.lanes_total.to_string());
    out.push_str("\nEVENTS_SENT_TOTAL: ");
    out.push_str(&outcome.events_sent_total.to_string());
    out.push_str("\nBYTES_SENT_TOTAL: ");
    out.push_str(&outcome.bytes_sent_total.to_string());
    out.push_str("\nLAGGED_DROPS_TOTAL: ");
    out.push_str(&outcome.lagged_drops_total.to_string());
    out.push_str("\nLAGGED_RECOVERIES_TOTAL: ");
    out.push_str(&outcome.lagged_recoveries_total.to_string());
    out.push_str("\nQUEUE_HIGH_WATERMARK_MAX: ");
    out.push_str(&outcome.queue_high_watermark_max.to_string());
    append_next(&mut out, "sub_list | sub_stats");
    out
}

/// Render the structured JSON twin for `sub_stats_all`.
#[must_use]
pub fn daemon_stats_structured(outcome: &DaemonStatsOutcome) -> Value {
    json!({
        "tool": "sub_stats_all",
        "status": "ok",
        "lanes_total": outcome.lanes_total,
        "events_sent_total": outcome.events_sent_total,
        "bytes_sent_total": outcome.bytes_sent_total,
        "lagged_drops_total": outcome.lagged_drops_total,
        "lagged_recoveries_total": outcome.lagged_recoveries_total,
        "queue_high_watermark_max": outcome.queue_high_watermark_max,
    })
}

#[cfg(test)]
mod tests {
    use super::{
        HintStrength, append_hint, append_hygiene, append_next, daemon_stats_render,
        daemon_stats_structured, filter_render, filter_structured, list_render, list_structured,
        pause_render, pause_structured, replay_render, replay_structured, resume_render,
        resume_structured, sub_stats_render, sub_stats_structured, subscribe_render,
        subscribe_structured, unsubscribe_render, unsubscribe_structured,
    };
    use crate::application::subscription_admin::{
        DaemonStatsOutcome, ListSubsOutcome, ReplayOutcome, SetFilterOutcome, SubStatsOutcome,
        SubToggleOutcome, SubscribeOutcome, UnsubscribeOutcome,
    };
    use crate::domain::subscription::{
        FilterRule, LagPolicy, SubId, SubscriberStats, SubscriptionLifetime,
    };
    use crate::ports::subscriber_lane::SubSummary;
    use crate::ports::subscriber_registry::ResourceKind;

    fn sub_id(s: &str) -> SubId {
        SubId::new(s.to_string())
    }

    #[test]
    fn hint_strength_prefix_required_uses_loud_phrasing() {
        assert_eq!(HintStrength::Required.as_prefix(), "REQUIRED NEXT STEP");
        assert_eq!(HintStrength::Recommended.as_prefix(), "RECOMMENDED");
    }

    #[test]
    fn append_hint_produces_required_or_recommended_line() {
        let mut s = String::new();
        append_hint(&mut s, HintStrength::Required, "sub_close x");
        assert!(s.contains("HINT: REQUIRED NEXT STEP: sub_close x"));
        let mut t = String::new();
        append_hint(&mut t, HintStrength::Recommended, "sub_stats x");
        assert!(t.contains("HINT: RECOMMENDED: sub_stats x"));
    }

    #[test]
    fn append_next_emits_canonical_next_line() {
        let mut s = String::new();
        append_next(&mut s, "sub_open");
        assert_eq!(s, "\nNEXT: sub_open");
    }

    #[test]
    fn append_hygiene_skips_empty_fields() {
        let mut s = String::new();
        append_hygiene(&mut s, Some("close"), None, Some(""), Some("hold id"));
        assert!(s.contains("Cleanup: close"));
        assert!(!s.contains("Cost"));
        assert!(!s.contains("Idempotency"));
        assert!(s.contains("Hygiene: hold id"));
    }

    #[test]
    fn subscribe_render_carries_required_hint_and_hygiene_block() {
        let outcome = SubscribeOutcome {
            sub_id: sub_id("019"),
            uri: "shell://x/output".to_string(),
            lifetime: SubscriptionLifetime::AutoClose { grace_ms: 2_000 },
            lag_policy: LagPolicy::Snapshot,
            grace_ms: 2_000,
            inline_push_requested: false,
            inline_push_honored: false,
        };
        let body = subscribe_render(&outcome);
        assert!(body.starts_with("SUB_OPEN: OK"));
        assert!(body.contains("SUB_ID: 019"));
        assert!(body.contains("URI: shell://x/output"));
        assert!(body.contains("LIFETIME: auto_close"));
        assert!(body.contains("LAG_POLICY: snapshot"));
        assert!(body.contains("GRACE_MS: 2000"));
        assert!(body.contains("NEXT: "));
        assert!(body.contains("HINT: REQUIRED NEXT STEP: sub_close sub_id=019"));
        assert!(
            body.contains(
                "HINT: RECOMMENDED: Data arrives async via notifications/resources/updated"
            )
        );
        assert!(body.contains("Do NOT use any MCP tool as a sleep"));
        assert!(body.contains("Start-Sleep -Milliseconds 200"));
        assert!(body.contains("200-500ms local sleeps"));
        assert!(body.contains("Cleanup: sub_close"));
        assert!(body.contains("Cost: O(1) lane open"));
        assert!(body.contains("Idempotency: Pass _meta.idempotency_key"));
        assert!(body.contains("Hygiene: Hold the SUB_ID"));
    }

    #[test]
    fn subscribe_structured_round_trips_all_fields() {
        let outcome = SubscribeOutcome {
            sub_id: sub_id("019"),
            uri: "shell://x/output".to_string(),
            lifetime: SubscriptionLifetime::Lease { ttl_secs: 60 },
            lag_policy: LagPolicy::DropOldest,
            grace_ms: 0,
            inline_push_requested: false,
            inline_push_honored: false,
        };
        let json = subscribe_structured(&outcome);
        assert_eq!(json["tool"], "sub_open");
        assert_eq!(json["sub_id"], "019");
        assert_eq!(json["uri"], "shell://x/output");
        assert_eq!(json["lifetime"], "lease");
        assert_eq!(json["lag_policy"], "drop_oldest");
        assert_eq!(json["grace_ms"], 0);
    }

    #[test]
    fn unsubscribe_render_includes_uri_and_lifecycle_state() {
        let outcome = UnsubscribeOutcome {
            sub_id: sub_id("u"),
            uri: Some("shell://x/output".to_string()),
            lifecycle_state: "closed",
            grace_remaining_ms: Some(1_000),
        };
        let body = unsubscribe_render(&outcome);
        assert!(body.starts_with("SUB_CLOSE: OK"));
        assert!(body.contains("URI: shell://x/output"));
        assert!(body.contains("LIFECYCLE_STATE: closed"));
        assert!(body.contains("GRACE_REMAINING_MS: 1000"));
        let json = unsubscribe_structured(&outcome);
        assert_eq!(json["tool"], "sub_close");
        assert_eq!(json["uri"], "shell://x/output");
    }

    #[test]
    fn pause_resume_renders_distinct_state_labels() {
        let p = SubToggleOutcome {
            sub_id: sub_id("p"),
            paused: true,
        };
        let r = SubToggleOutcome {
            sub_id: sub_id("r"),
            paused: false,
        };
        assert!(pause_render(&p).contains("STATE: paused"));
        assert!(resume_render(&r).contains("STATE: resumed"));
        assert_eq!(pause_structured(&p)["paused"], true);
        assert_eq!(resume_structured(&r)["paused"], false);
    }

    #[test]
    fn filter_render_includes_filter_label() {
        let outcome = SetFilterOutcome {
            sub_id: sub_id("f"),
            filter: FilterRule::Regex("ERR".to_string()),
        };
        let body = filter_render(&outcome);
        assert!(body.contains("FILTER: regex:ERR"));
        let json = filter_structured(&outcome);
        assert_eq!(json["filter"], "regex:ERR");
    }

    #[test]
    fn replay_render_includes_cursor() {
        let outcome = ReplayOutcome {
            sub_id: sub_id("r2"),
            from_cursor: 1024,
        };
        let body = replay_render(&outcome);
        assert!(body.contains("FROM_CURSOR: 1024"));
        let json = replay_structured(&outcome);
        assert_eq!(json["from_cursor"], 1024);
    }

    fn sample_summary(label: &str, kind: ResourceKind, paused: bool) -> SubSummary {
        SubSummary {
            sub_id: sub_id(label),
            kind,
            resource_id: label.to_string(),
            uri: format!("shell://{label}/output"),
            lag_policy: LagPolicy::Snapshot,
            lifetime: SubscriptionLifetime::Manual,
            paused,
            stats: SubscriberStats::default(),
        }
    }

    #[test]
    fn list_render_emits_one_line_per_summary() {
        let outcome = ListSubsOutcome {
            subs: vec![
                sample_summary("a", ResourceKind::Shell, false),
                sample_summary("b", ResourceKind::Shell, true),
            ],
        };
        let body = list_render(&outcome);
        assert!(body.contains("SUB_LIST: OK"));
        assert!(body.contains("SUBS_TOTAL: 2"));
        assert!(body.contains("\nSUB a shell"));
        assert!(body.contains("\nSUB b shell"));
        assert!(body.contains("paused=false"));
        assert!(body.contains("paused=true"));
    }

    #[test]
    fn list_structured_carries_array_of_subs() {
        let outcome = ListSubsOutcome {
            subs: vec![sample_summary("a", ResourceKind::Shell, false)],
        };
        let json = list_structured(&outcome);
        assert_eq!(json["subs_total"], 1);
        assert!(json["subs"].is_array());
        assert_eq!(json["subs"][0]["sub_id"], "a");
        assert_eq!(json["subs"][0]["kind"], "shell");
    }

    #[test]
    fn sub_stats_render_carries_every_atomic_counter() {
        let stats = SubscriberStats {
            events_sent: 10,
            bytes_sent: 100,
            lagged_drops: 1,
            lagged_recoveries: 2,
            queue_depth: 3,
            queue_high_watermark: 4,
            block_total_ms: 5,
            byte_triggered_flushes: 0,
        };
        let outcome = SubStatsOutcome {
            sub_id: sub_id("s"),
            stats,
            inline: None,
        };
        let body = sub_stats_render(&outcome);
        assert!(body.contains("EVENTS_SENT: 10"));
        assert!(body.contains("BYTES_SENT: 100"));
        assert!(body.contains("LAGGED_DROPS: 1"));
        assert!(body.contains("LAGGED_RECOVERIES: 2"));
        assert!(body.contains("QUEUE_DEPTH: 3"));
        assert!(body.contains("QUEUE_HIGH_WATERMARK: 4"));
        assert!(body.contains("BLOCK_TOTAL_MS: 5"));
        let json = sub_stats_structured(&outcome);
        assert_eq!(json["events_sent"], 10);
        assert_eq!(json["block_total_ms"], 5);
    }

    #[test]
    fn daemon_stats_render_emits_every_aggregate_counter() {
        let outcome = DaemonStatsOutcome {
            lanes_total: 7,
            events_sent_total: 70,
            bytes_sent_total: 7_000,
            lagged_drops_total: 3,
            lagged_recoveries_total: 4,
            queue_high_watermark_max: 12,
        };
        let body = daemon_stats_render(&outcome);
        assert!(body.starts_with("SUB_STATS_ALL: OK"));
        assert!(body.contains("LANES_TOTAL: 7"));
        assert!(body.contains("EVENTS_SENT_TOTAL: 70"));
        assert!(body.contains("BYTES_SENT_TOTAL: 7000"));
        assert!(body.contains("LAGGED_DROPS_TOTAL: 3"));
        assert!(body.contains("LAGGED_RECOVERIES_TOTAL: 4"));
        assert!(body.contains("QUEUE_HIGH_WATERMARK_MAX: 12"));
        let json = daemon_stats_structured(&outcome);
        assert_eq!(json["lanes_total"], 7);
        assert_eq!(json["queue_high_watermark_max"], 12);
    }

    #[test]
    fn list_render_with_zero_subs_emits_zero_total() {
        let outcome = ListSubsOutcome { subs: Vec::new() };
        let body = list_render(&outcome);
        assert!(body.contains("SUBS_TOTAL: 0"));
        let json = list_structured(&outcome);
        assert_eq!(json["subs_total"], 0);
        assert_eq!(json["subs"].as_array().map(Vec::len), Some(0));
    }

    #[test]
    fn unsubscribe_render_omits_uri_when_unknown() {
        let outcome = UnsubscribeOutcome {
            sub_id: sub_id("u"),
            uri: None,
            lifecycle_state: "closed",
            grace_remaining_ms: None,
        };
        let body = unsubscribe_render(&outcome);
        assert!(!body.contains("URI:"));
        assert!(!body.contains("GRACE_REMAINING_MS:"));
    }

    #[test]
    fn lifetime_label_covers_every_variant() {
        assert_eq!(
            super::lifetime_label(SubscriptionLifetime::Manual),
            "manual"
        );
        assert_eq!(
            super::lifetime_label(SubscriptionLifetime::AutoClose { grace_ms: 1 }),
            "auto_close"
        );
        assert_eq!(
            super::lifetime_label(SubscriptionLifetime::Lease { ttl_secs: 1 }),
            "lease"
        );
    }

    #[test]
    fn filter_label_covers_every_variant() {
        assert_eq!(super::filter_label(&FilterRule::None), "none");
        assert_eq!(
            super::filter_label(&FilterRule::Regex("X".to_string())),
            "regex:X"
        );
        let label = super::filter_label(&FilterRule::Level(
            crate::domain::subscription::LogLevel::Warn,
        ));
        assert!(label.starts_with("level:"));
    }

    #[test]
    fn subscribe_render_lag_policy_is_lowercase_wire_form() {
        for (variant, wire) in [
            (LagPolicy::BlockSlow, "block_slow"),
            (LagPolicy::DropOldest, "drop_oldest"),
            (LagPolicy::DropNewest, "drop_newest"),
            (LagPolicy::Snapshot, "snapshot"),
        ] {
            let outcome = SubscribeOutcome {
                sub_id: sub_id("x"),
                uri: "shell://x/output".to_string(),
                lifetime: SubscriptionLifetime::Manual,
                lag_policy: variant,
                grace_ms: 0,
                inline_push_requested: false,
                inline_push_honored: false,
            };
            let body = subscribe_render(&outcome);
            assert!(
                body.contains(&format!("LAG_POLICY: {wire}")),
                "missing wire form {wire}: {body}"
            );
        }
    }

    #[test]
    fn pause_structured_paused_is_true() {
        let outcome = SubToggleOutcome {
            sub_id: sub_id("p"),
            paused: true,
        };
        let json = pause_structured(&outcome);
        assert_eq!(json["paused"], true);
    }

    #[test]
    fn resume_structured_paused_is_false() {
        let outcome = SubToggleOutcome {
            sub_id: sub_id("r"),
            paused: false,
        };
        let json = resume_structured(&outcome);
        assert_eq!(json["paused"], false);
    }

    #[test]
    fn replay_render_carries_zero_cursor_when_unset() {
        let outcome = ReplayOutcome {
            sub_id: sub_id("z"),
            from_cursor: 0,
        };
        let body = replay_render(&outcome);
        assert!(body.contains("FROM_CURSOR: 0"));
    }

    #[test]
    fn next_line_ordering_pushes_subscribe_first_in_subscribe_render() {
        let outcome = SubscribeOutcome {
            sub_id: sub_id("019"),
            uri: "shell://x/output".to_string(),
            lifetime: SubscriptionLifetime::Manual,
            lag_policy: LagPolicy::Snapshot,
            grace_ms: 0,
            inline_push_requested: false,
            inline_push_honored: false,
        };
        let body = subscribe_render(&outcome);
        // The HINT line's REQUIRED tag steers the LLM straight at the
        // sub_close step before the broader NEXT enumeration.
        let hint_idx = body.find("HINT: REQUIRED").expect("hint present");
        let next_idx = body.find("NEXT: ").expect("next present");
        // NEXT precedes HINT in this render; both must exist.
        assert!(next_idx < hint_idx, "expected NEXT before HINT");
    }

    // ------------------------------------------------------------------
    // ADR 0012 phase 5 -- inline-push wire-line renderers.
    // ------------------------------------------------------------------

    fn outcome_with_inline(requested: bool, honored: bool) -> SubscribeOutcome {
        SubscribeOutcome {
            sub_id: sub_id("019phase5"),
            uri: "shell://phase5/output".to_string(),
            lifetime: SubscriptionLifetime::Manual,
            lag_policy: LagPolicy::Snapshot,
            grace_ms: 0,
            inline_push_requested: requested,
            inline_push_honored: honored,
        }
    }

    fn sub_stats_outcome(
        inline_push: bool,
        inline_events_sent: u64,
        inline_bytes_sent: u64,
    ) -> SubStatsOutcome {
        let inline = Some(crate::ports::subscriber_lane::InlineLaneCounters {
            inline_push,
            inline_events_sent,
            inline_bytes_sent,
        });
        SubStatsOutcome {
            sub_id: sub_id("019stats"),
            stats: SubscriberStats {
                events_sent: 1,
                bytes_sent: 32,
                lagged_drops: 0,
                lagged_recoveries: 0,
                queue_depth: 0,
                queue_high_watermark: 0,
                block_total_ms: 0,
                byte_triggered_flushes: 0,
            },
            inline,
        }
    }

    #[test]
    fn renders_inline_push_yes_with_honored_no_and_hint() {
        let outcome = outcome_with_inline(true, false);
        let body = subscribe_render(&outcome);
        assert!(body.contains("INLINE_PUSH: yes"));
        assert!(body.contains("INLINE_PUSH_HONORED: no"));
        assert!(
            body.contains("HINT: RECOMMENDED: client did not advertise ssh_inline_push capability"),
            "fallback hint missing: {body}"
        );
        let json = subscribe_structured(&outcome);
        assert_eq!(json["inline_push"], true);
        assert_eq!(json["inline_push_honored"], false);
    }

    #[test]
    fn renders_inline_push_yes_with_honored_yes_and_hygiene_line() {
        let outcome = outcome_with_inline(true, true);
        let body = subscribe_render(&outcome);
        assert!(body.contains("INLINE_PUSH: yes"));
        assert!(body.contains("INLINE_PUSH_HONORED: yes"));
        assert!(
            body.contains("When inline_push=true, bytes arrive in notifications/ssh/output"),
            "hygiene line missing inline guidance: {body}"
        );
        // Fallback hint must NOT fire when the capability was honoured.
        assert!(
            !body
                .contains("HINT: RECOMMENDED: client did not advertise ssh_inline_push capability"),
            "fallback hint must not fire when honoured: {body}"
        );
        let json = subscribe_structured(&outcome);
        assert_eq!(json["inline_push"], true);
        assert_eq!(json["inline_push_honored"], true);
    }

    #[test]
    fn renders_no_inline_lines_when_inline_push_false() {
        let outcome = outcome_with_inline(false, false);
        let body = subscribe_render(&outcome);
        assert!(
            !body.contains("INLINE_PUSH:"),
            "v7.0.x-compatible wire must not carry INLINE_PUSH line: {body}"
        );
        assert!(
            !body.contains("INLINE_PUSH_HONORED:"),
            "wire-additive: honored line gated on opt-in: {body}"
        );
        let json = subscribe_structured(&outcome);
        // Structured-content still carries the boolean for parsers that
        // want to read it; just defaults to false.
        assert_eq!(json["inline_push"], false);
        assert!(
            json.get("inline_push_honored").is_none(),
            "honored field omitted when opt-out"
        );
    }

    #[test]
    fn emits_inline_counters_when_inline_enabled() {
        let outcome = sub_stats_outcome(true, 12, 8_743);
        let body = sub_stats_render(&outcome);
        assert!(body.contains("INLINE_EVENTS_SENT: 12"));
        assert!(body.contains("INLINE_BYTES_SENT: 8743"));
        let json = sub_stats_structured(&outcome);
        assert_eq!(json["inline_events_sent"], 12);
        assert_eq!(json["inline_bytes_sent"], 8_743);
    }

    #[test]
    fn omits_inline_counters_when_inline_disabled() {
        let outcome = sub_stats_outcome(false, 0, 0);
        let body = sub_stats_render(&outcome);
        assert!(
            !body.contains("INLINE_EVENTS_SENT:"),
            "opt-out lane must keep wire byte-identical to v7.0.x"
        );
        assert!(
            !body.contains("INLINE_BYTES_SENT:"),
            "opt-out lane must keep wire byte-identical to v7.0.x"
        );
        let json = sub_stats_structured(&outcome);
        assert!(
            json.get("inline_events_sent").is_none(),
            "structured twin must omit inline field on opt-out lane"
        );
        assert!(
            json.get("inline_bytes_sent").is_none(),
            "structured twin must omit inline field on opt-out lane"
        );
    }
}
