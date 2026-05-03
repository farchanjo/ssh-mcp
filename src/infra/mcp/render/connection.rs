//! Connection-domain markdown renderers.
//!
//! Mirrors v3 `src/mcp/message/builder.rs` — `ConnectOkBuilder`,
//! `ConnectSuggestedBuilder`, `render_disconnect_ok`, `ListSessionsBuilder`,
//! `render_disconnect_agent` — but takes the v4 use case Outcomes as
//! input.

use std::collections::HashMap;
use std::time::Duration;

use chrono::{DateTime, Utc};

use crate::application::connect_session::ConnectOutcome;
use crate::application::disconnect_agent::DisconnectAgentOutcome;
use crate::application::disconnect_session::DisconnectOutcome;
use crate::application::list_sessions::ListSessionsOutcome;
use crate::domain::ids::AgentId;
use crate::domain::session::SessionEntity;
use crate::infra::mcp::helpers::output::sanitize_value;

/// Threshold (per-agent session count) above which the list-sessions
/// renderer appends an anti-leak HINT line nudging the LLM toward
/// `ssh_disconnect_agent`. Improvement D — see `feat/v4.4-llm-steering`.
const ANTI_LEAK_HINT_THRESHOLD: usize = 5;

/// Render a [`ConnectOutcome`] as the v3 `SSH_CONNECT` block.
#[must_use]
pub fn connect_render(outcome: ConnectOutcome) -> String {
    match outcome {
        ConnectOutcome::Connected {
            session,
            replaced,
            retries,
            persistent,
            inactivity_timeout,
        } => render_connected(&session, replaced, retries, persistent, inactivity_timeout),
        ConnectOutcome::Reused {
            session,
            persistent,
            inactivity_timeout,
        } => render_reused(&session, persistent, inactivity_timeout),
        ConnectOutcome::Suggested { matches } => render_suggested(&matches),
    }
}

fn render_connected(
    session: &SessionEntity,
    replaced: usize,
    retries: u32,
    persistent: bool,
    inactivity_timeout: Duration,
) -> String {
    let host = format_host(session);
    let mut out = String::with_capacity(288);
    out.push_str("SSH_CONNECT: OK\n");
    out.push_str("SESSION_ID: ");
    out.push_str(session.id.as_str());
    out.push_str("\nHOST: ");
    out.push_str(&sanitize_value(&session.username));
    out.push('@');
    out.push_str(&sanitize_value(&host));
    if let Some(agent) = session.agent_id.as_ref() {
        out.push_str("\nAGENT_ID: ");
        out.push_str(&sanitize_value(agent.as_str()));
    }
    out.push_str("\nRETRY: ");
    out.push_str(&retries.to_string());
    append_persistent_or_expiry(
        &mut out,
        session.connected_at,
        persistent,
        inactivity_timeout,
    );
    if replaced > 0 {
        out.push_str("\nREPLACED: ");
        out.push_str(&replaced.to_string());
    }
    append_next_line(&mut out, &next_hint_for_session(session.id.as_str()));
    out
}

fn render_reused(
    session: &SessionEntity,
    persistent: bool,
    inactivity_timeout: Duration,
) -> String {
    let host = format_host(session);
    let mut out = String::with_capacity(256);
    out.push_str("SSH_CONNECT: REUSED\n");
    out.push_str("SESSION_ID: ");
    out.push_str(session.id.as_str());
    out.push_str("\nHOST: ");
    out.push_str(&sanitize_value(&session.username));
    out.push('@');
    out.push_str(&sanitize_value(&host));
    if let Some(agent) = session.agent_id.as_ref() {
        out.push_str("\nAGENT_ID: ");
        out.push_str(&sanitize_value(agent.as_str()));
    }
    append_persistent_or_expiry(
        &mut out,
        session.connected_at,
        persistent,
        inactivity_timeout,
    );
    append_next_line(&mut out, &next_hint_for_session(session.id.as_str()));
    out
}

/// Append a single `NEXT: <hint>` advisory line listing concrete tool
/// calls a smaller LLM can chain without consulting the cookbook.
fn append_next_line(out: &mut String, hint: &str) {
    out.push_str("\nNEXT: ");
    out.push_str(hint);
}

/// Successor tools after a connect-style response — execute / shell-open
/// / disconnect, each pre-filled with the freshly minted `SESSION_ID`.
fn next_hint_for_session(session_id: &str) -> String {
    format!(
        "ssh_execute(session_id={session_id}, command=...) | \
         ssh_shell_open(session_id={session_id}) | \
         ssh_disconnect(session_id={session_id})"
    )
}

/// Append either `PERSISTENT: true` (when the session opts out of the
/// inactivity sweeper) or `EXPIRES_AT: <rfc3339>` (deadline = `connected_at`
/// plus the configured inactivity timeout). LLM steering: clients can ping
/// before the TTL fires. Improvement B — see `feat/v4.4-llm-steering`.
fn append_persistent_or_expiry(
    out: &mut String,
    connected_at: DateTime<Utc>,
    persistent: bool,
    inactivity_timeout: Duration,
) {
    if persistent {
        out.push_str("\nPERSISTENT: true");
        return;
    }
    out.push_str("\nPERSISTENT: false");
    if let Some(expires_at) = compute_expires_at(connected_at, inactivity_timeout) {
        out.push_str("\nEXPIRES_AT: ");
        out.push_str(&expires_at.to_rfc3339());
    }
}

/// Saturating add `inactivity_timeout` to `connected_at`. Returns `None`
/// when the timeout is zero (treat zero as "never expires" — the
/// inactivity sweeper is disabled).
fn compute_expires_at(
    connected_at: DateTime<Utc>,
    inactivity_timeout: Duration,
) -> Option<DateTime<Utc>> {
    if inactivity_timeout.is_zero() {
        return None;
    }
    let secs = i64::try_from(inactivity_timeout.as_secs()).ok()?;
    let dur = chrono::TimeDelta::try_seconds(secs)?;
    connected_at.checked_add_signed(dur)
}

fn render_suggested(matches: &[SessionEntity]) -> String {
    if matches.len() == 1 {
        if let Some(m) = matches.first() {
            return render_suggested_single(m);
        }
    }
    render_suggested_multi(matches)
}

fn render_suggested_single(m: &SessionEntity) -> String {
    let host = format_host(m);
    let mut out = String::with_capacity(320);
    out.push_str("SSH_CONNECT: SUGGESTED\nEXISTING_SESSION_ID: ");
    out.push_str(m.id.as_str());
    out.push_str("\nHOST: ");
    out.push_str(&sanitize_value(&m.username));
    out.push('@');
    out.push_str(&sanitize_value(&host));
    if let Some(agent) = m.agent_id.as_ref() {
        out.push_str("\nAGENT_ID: ");
        out.push_str(&sanitize_value(agent.as_str()));
    }
    if let Some(name) = m.name.as_ref() {
        out.push_str("\nNAME: ");
        out.push_str(&sanitize_value(name));
    }
    out.push_str("\nCONNECTED_AT: ");
    out.push_str(&m.connected_at.to_rfc3339());
    out.push_str("\nHEALTHY: ");
    out.push_str(if m.healthy.unwrap_or(false) {
        "true"
    } else {
        "false"
    });
    out.push_str("\nHINT: use existing SESSION_ID, or retry with reuse=\"force_new\"");
    append_next_line(&mut out, &next_hint_for_suggested(m.id.as_str()));
    out
}

fn render_suggested_multi(matches: &[SessionEntity]) -> String {
    let mut out = String::with_capacity(64 + matches.len() * 128);
    out.push_str("SSH_CONNECT: SUGGESTED\nMATCHES: ");
    out.push_str(&matches.len().to_string());
    for m in matches {
        out.push_str("\n- ");
        out.push_str(m.id.as_str());
        out.push(' ');
        out.push_str(&sanitize_value(&m.username));
        out.push('@');
        out.push_str(&sanitize_value(&format_host(m)));
        append_match_decorations(&mut out, m);
    }
    out.push_str("\nHINT: pick an existing SESSION_ID, or retry with reuse=\"force_new\"");
    append_next_line(&mut out, &next_hint_for_suggested("<existing>"));
    out
}

/// Successor tools after a `SUGGESTED` response — caller picks an
/// existing `session_id` or forces a new connection.
fn next_hint_for_suggested(session_id: &str) -> String {
    format!(
        "ssh_connect(session_id={session_id}) | \
         ssh_connect(reuse=\"force_new\")"
    )
}

#[allow(
    clippy::useless_let_if_seq,
    reason = "accumulator tracking multiple optional fields is clearer than nested if/else"
)]
fn append_match_decorations(out: &mut String, m: &SessionEntity) {
    out.push_str(" [");
    let mut wrote_field = false;
    if let Some(agent) = m.agent_id.as_ref() {
        out.push_str("agent: ");
        out.push_str(&sanitize_value(agent.as_str()));
        wrote_field = true;
    }
    if let Some(name) = m.name.as_ref() {
        if wrote_field {
            out.push_str(", ");
        }
        out.push_str("name: ");
        out.push_str(&sanitize_value(name));
        wrote_field = true;
    }
    if wrote_field {
        out.push_str(", ");
    }
    out.push_str("connected: ");
    out.push_str(extract_time(&m.connected_at.to_rfc3339()));
    out.push_str(", ");
    out.push_str(if m.healthy.unwrap_or(false) {
        "healthy"
    } else {
        "unhealthy"
    });
    out.push(']');
}

/// Extract `HH:MM:SS` from an RFC 3339 timestamp.
fn extract_time(ts: &str) -> &str {
    if let Some((_, rest)) = ts.split_once('T') {
        let end = rest.find(['Z', '+', '-']).unwrap_or(rest.len());
        &rest[..end]
    } else {
        ts
    }
}

/// Format the host portion of a session entity, omitting the default port.
fn format_host(session: &SessionEntity) -> String {
    let host = session.address.host();
    let port = session.address.port();
    if port == 22 {
        host.to_string()
    } else {
        format!("{host}:{port}")
    }
}

/// Render a [`DisconnectOutcome`] as the v3 `SSH_DISCONNECT` block.
#[must_use]
pub fn disconnect_render(outcome: &DisconnectOutcome) -> String {
    let mut out = String::with_capacity(48);
    out.push_str("SSH_DISCONNECT: OK\nSESSION_ID: ");
    out.push_str(outcome.session_id.as_str());
    out
}

/// Render a [`ListSessionsOutcome`] as the v3 `SSH_LIST_SESSIONS` block.
#[must_use]
pub fn list_sessions_render(outcome: ListSessionsOutcome) -> String {
    let ListSessionsOutcome {
        healthy,
        removed_dead: _,
        total,
    } = outcome;
    if healthy.is_empty() && total == 0 {
        return String::from("SSH_LIST_SESSIONS: OK\nCOUNT: 0");
    }
    let mut out = String::with_capacity(64 + healthy.len() * 128);
    out.push_str("SSH_LIST_SESSIONS: OK\nCOUNT: ");
    out.push_str(&healthy.len().to_string());
    if total > healthy.len() {
        out.push_str(" (showing ");
        out.push_str(&healthy.len().to_string());
        out.push_str(" of ");
        out.push_str(&total.to_string());
        out.push(')');
    }
    for s in &healthy {
        out.push_str("\n- ");
        append_session_item(&mut out, s);
    }
    append_anti_leak_hint(&mut out, &healthy);
    append_next_line(&mut out, &next_hint_for_list_sessions(&healthy));
    out
}

/// Successor tools after a non-empty `ssh_list_sessions` — bulk-cleanup
/// with `ssh_disconnect_agent` when an agent owns sessions, else a
/// targeted `ssh_disconnect` against any listed `session_id`.
fn next_hint_for_list_sessions(healthy: &[SessionEntity]) -> String {
    let agent = healthy
        .iter()
        .find_map(|s| s.agent_id.as_ref().map(AgentId::as_str));
    let session_id = healthy.first().map_or("<id>", |s| s.id.as_str());
    agent.map_or_else(
        || format!("ssh_disconnect(session_id={session_id})"),
        |agent_id| {
            format!(
                "ssh_disconnect_agent(agent_id={agent_id}) | \
                 ssh_disconnect(session_id={session_id})"
            )
        },
    )
}

/// When any agent owns more than [`ANTI_LEAK_HINT_THRESHOLD`] healthy
/// sessions, append a `HINT` line nudging the LLM toward
/// `ssh_disconnect_agent`. Improvement D — see `feat/v4.4-llm-steering`.
fn append_anti_leak_hint(out: &mut String, healthy: &[SessionEntity]) {
    let mut counts: HashMap<&str, usize> = HashMap::new();
    for s in healthy {
        if let Some(agent) = s.agent_id.as_ref() {
            *counts.entry(agent.as_str()).or_insert(0) += 1;
        }
    }
    let leak: Option<(&str, usize)> = counts
        .into_iter()
        .filter(|(_, n)| *n > ANTI_LEAK_HINT_THRESHOLD)
        .max_by_key(|(_, n)| *n);
    if let Some((agent, n)) = leak {
        out.push_str("\nHINT: agent '");
        out.push_str(&sanitize_value(agent));
        out.push_str("' owns ");
        out.push_str(&n.to_string());
        out.push_str(" sessions; consider ssh_disconnect_agent to bulk-cleanup");
    }
}

fn append_session_item(out: &mut String, s: &SessionEntity) {
    out.push_str(s.id.as_str());
    out.push(' ');
    out.push_str(&sanitize_value(&s.username));
    out.push('@');
    out.push_str(&sanitize_value(&format_host(s)));
    append_session_flags(out, s);
}

#[allow(
    clippy::useless_let_if_seq,
    clippy::too_many_lines,
    reason = "accumulator tracking multiple optional fields is clearer than nested if/else; restructuring to ranges hurts readability"
)]
fn append_session_flags(out: &mut String, s: &SessionEntity) {
    let show_compression = !s.compression_enabled;
    let health_label = match s.healthy {
        Some(true) => Some("healthy"),
        Some(false) => Some("unhealthy"),
        None => None,
    };
    if s.agent_id.is_none() && s.name.is_none() && !show_compression && health_label.is_none() {
        return;
    }
    out.push_str(" [");
    let mut wrote = false;
    if let Some(agent) = s.agent_id.as_ref() {
        out.push_str("agent: ");
        out.push_str(&sanitize_value(agent.as_str()));
        wrote = true;
    }
    if let Some(name) = s.name.as_ref() {
        if wrote {
            out.push_str(", ");
        }
        out.push_str("name: ");
        out.push_str(&sanitize_value(name));
        wrote = true;
    }
    if show_compression {
        if wrote {
            out.push_str(", ");
        }
        out.push_str("compression: off");
        wrote = true;
    }
    if let Some(label) = health_label {
        if wrote {
            out.push_str(", ");
        }
        out.push_str(label);
    }
    out.push(']');
}

/// Render a [`DisconnectAgentOutcome`] as the v3
/// `SSH_DISCONNECT_AGENT` block.
#[must_use]
pub fn disconnect_agent_render(outcome: &DisconnectAgentOutcome) -> String {
    let mut out = String::with_capacity(96);
    out.push_str("SSH_DISCONNECT_AGENT: OK\nAGENT_ID: ");
    out.push_str(&sanitize_value(outcome.agent_id.as_str()));
    out.push_str("\nSESSIONS: ");
    out.push_str(&outcome.sessions_disconnected.to_string());
    out.push_str("\nCOMMANDS: ");
    out.push_str(&outcome.commands_cancelled.to_string());
    out
}

#[cfg(test)]
mod tests {
    use super::{
        ANTI_LEAK_HINT_THRESHOLD, connect_render, disconnect_agent_render, disconnect_render,
        list_sessions_render,
    };
    use crate::application::connect_session::ConnectOutcome;
    use crate::application::disconnect_agent::DisconnectAgentOutcome;
    use crate::application::disconnect_session::DisconnectOutcome;
    use crate::application::list_sessions::ListSessionsOutcome;
    use crate::domain::identity::Address;
    use crate::domain::ids::{AgentId, SessionId};
    use crate::domain::session::SessionEntity;
    use chrono::{TimeZone, Utc};
    use std::time::Duration;

    fn sample_session(id: &str, agent: Option<&str>) -> SessionEntity {
        SessionEntity {
            id: SessionId::new(id.to_string()),
            name: None,
            agent_id: agent.map(|a| AgentId::new(a.to_string())),
            address: Address::new("h.example.com".to_string(), 22).expect("address"),
            username: "alice".to_string(),
            connected_at: Utc.with_ymd_and_hms(2026, 5, 3, 12, 0, 0).unwrap(),
            default_timeout: Duration::from_secs(30),
            retry_attempts: 0,
            compression_enabled: true,
            last_health_check: None,
            healthy: Some(true),
        }
    }

    #[test]
    fn disconnect_render_emits_block() {
        let m = disconnect_render(&DisconnectOutcome {
            session_id: SessionId::new("sess-abc".to_string()),
            commands_cancelled: 0,
            shells_closed: 0,
            transfers_aborted: 0,
        });
        assert_eq!(m, "SSH_DISCONNECT: OK\nSESSION_ID: sess-abc");
    }

    #[test]
    fn list_sessions_empty() {
        let outcome = ListSessionsOutcome {
            healthy: vec![],
            removed_dead: vec![],
            total: 0,
        };
        assert_eq!(
            list_sessions_render(outcome),
            "SSH_LIST_SESSIONS: OK\nCOUNT: 0"
        );
    }

    #[test]
    fn disconnect_agent_renders_counts() {
        let outcome = DisconnectAgentOutcome {
            agent_id: AgentId::new("alpha".to_string()),
            sessions_disconnected: 3,
            commands_cancelled: 5,
            shells_closed: 0,
            transfers_aborted: 0,
        };
        assert_eq!(
            disconnect_agent_render(&outcome),
            "SSH_DISCONNECT_AGENT: OK\nAGENT_ID: alpha\nSESSIONS: 3\nCOMMANDS: 5"
        );
    }

    // --- v4.4 LLM steering: EXPIRES_AT line ----------------------------

    #[test]
    fn connect_render_includes_expires_at_when_not_persistent() {
        let session = sample_session("sess-1", None);
        let body = connect_render(ConnectOutcome::Connected {
            session,
            replaced: 0,
            retries: 0,
            persistent: false,
            inactivity_timeout: Duration::from_secs(300),
        });
        assert!(body.contains("PERSISTENT: false"));
        // 12:00 + 300s = 12:05
        assert!(
            body.contains("EXPIRES_AT: 2026-05-03T12:05:00+00:00"),
            "body: {body}"
        );
    }

    #[test]
    fn connect_render_skips_expires_at_when_persistent() {
        let session = sample_session("sess-1", None);
        let body = connect_render(ConnectOutcome::Connected {
            session,
            replaced: 0,
            retries: 0,
            persistent: true,
            inactivity_timeout: Duration::from_secs(300),
        });
        assert!(body.contains("PERSISTENT: true"));
        assert!(
            !body.contains("EXPIRES_AT"),
            "persistent sessions must omit the EXPIRES_AT line, body: {body}"
        );
    }

    #[test]
    fn reused_render_includes_expires_at() {
        let session = sample_session("sess-2", Some("agent-X"));
        let body = connect_render(ConnectOutcome::Reused {
            session,
            persistent: false,
            inactivity_timeout: Duration::from_secs(120),
        });
        assert!(body.starts_with("SSH_CONNECT: REUSED"));
        assert!(body.contains("AGENT_ID: agent-X"));
        // 12:00 + 120s = 12:02
        assert!(
            body.contains("EXPIRES_AT: 2026-05-03T12:02:00+00:00"),
            "body: {body}"
        );
    }

    #[test]
    fn connect_render_omits_expires_at_when_inactivity_zero() {
        let session = sample_session("sess-1", None);
        let body = connect_render(ConnectOutcome::Connected {
            session,
            replaced: 0,
            retries: 0,
            persistent: false,
            inactivity_timeout: Duration::ZERO,
        });
        assert!(body.contains("PERSISTENT: false"));
        assert!(!body.contains("EXPIRES_AT"));
    }

    // --- v4.4 LLM steering: anti-leak HINT -----------------------------

    #[test]
    fn list_sessions_emits_anti_leak_hint_above_threshold() {
        let healthy: Vec<SessionEntity> = (0..=ANTI_LEAK_HINT_THRESHOLD)
            .map(|i| sample_session(&format!("s-{i}"), Some("agent-spammer")))
            .collect();
        let total = healthy.len();
        let outcome = ListSessionsOutcome {
            healthy,
            removed_dead: vec![],
            total,
        };
        let body = list_sessions_render(outcome);
        assert!(
            body.contains("HINT: agent 'agent-spammer' owns 6 sessions"),
            "body: {body}"
        );
        assert!(body.contains("ssh_disconnect_agent"), "body: {body}");
    }

    #[test]
    fn list_sessions_omits_hint_at_or_below_threshold() {
        let healthy: Vec<SessionEntity> = (0..ANTI_LEAK_HINT_THRESHOLD)
            .map(|i| sample_session(&format!("s-{i}"), Some("agent-okay")))
            .collect();
        let total = healthy.len();
        let outcome = ListSessionsOutcome {
            healthy,
            removed_dead: vec![],
            total,
        };
        let body = list_sessions_render(outcome);
        assert!(
            !body.contains("HINT:"),
            "agents at or below threshold must not trigger the leak hint, body: {body}"
        );
    }

    #[test]
    fn list_sessions_omits_hint_when_sessions_have_no_agent() {
        let healthy: Vec<SessionEntity> = (0..=ANTI_LEAK_HINT_THRESHOLD)
            .map(|i| sample_session(&format!("s-{i}"), None))
            .collect();
        let total = healthy.len();
        let outcome = ListSessionsOutcome {
            healthy,
            removed_dead: vec![],
            total,
        };
        let body = list_sessions_render(outcome);
        assert!(!body.contains("HINT:"), "body: {body}");
    }
}
