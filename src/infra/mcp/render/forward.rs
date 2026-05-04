//! Port-forward markdown renderer.
//!
//! Mirrors v3 `src/mcp/message/builder.rs::render_forward_ok` but takes
//! the v4 use case Outcome as input.

use serde_json::{Value, json};

use crate::application::forward_port::ForwardPortOutcome;
use crate::infra::mcp::helpers::output::sanitize_value;

/// Render a [`ForwardPortOutcome`] as the `SSH_FORWARD: OK` block.
///
/// v4.5: emits `FORWARD_ID` and `SESSION_ID` so callers can construct
/// the matching `forward://<FORWARD_ID>/events` resource subscribe URI
/// without round-tripping `resources/list`.
#[must_use]
pub fn forward_render(outcome: ForwardPortOutcome) -> String {
    let ForwardPortOutcome {
        forward_id,
        session_id,
        local_port,
        remote_address,
        remote_port,
        started_at: _,
    } = outcome;
    let forward = sanitize_value(forward_id.as_str());
    let local = format!("0.0.0.0:{local_port}");
    let remote = format!("{remote_address}:{remote_port}");
    let mut out = String::with_capacity(288);
    append_forward_header(&mut out, &forward, session_id.as_str(), &local, &remote);
    append_forward_advisories(&mut out, &forward);
    out
}

fn append_forward_header(
    out: &mut String,
    forward: &str,
    session_id: &str,
    local: &str,
    remote: &str,
) {
    out.push_str("SSH_FORWARD: OK\nFORWARD_ID: ");
    out.push_str(forward);
    out.push_str("\nSESSION_ID: ");
    out.push_str(&sanitize_value(session_id));
    out.push_str("\nLOCAL: ");
    out.push_str(&sanitize_value(local));
    out.push_str("\nREMOTE: ");
    out.push_str(&sanitize_value(remote));
    out.push_str("\nACTIVE: true");
}

fn append_forward_advisories(out: &mut String, forward: &str) {
    // v5 Phase 3 — forwarders only emit events on connect/error, so
    // subscribe is RECOMMENDED (no polling alternative — but skipping
    // simply means missing the event log).
    append_subscribe_hint(
        out,
        &format!(
            "RECOMMENDED: ssh_subscribe uri=forward://{forward}/events. Falls back gracefully if you skip (no polling alternative)."
        ),
    );
    append_next_line(out, &format!("ssh_subscribe uri=forward://{forward}/events"));
}

/// Append a single `NEXT: <hint>` advisory line listing concrete tool
/// calls a smaller LLM can chain without consulting the cookbook.
fn append_next_line(out: &mut String, hint: &str) {
    out.push_str("\nNEXT: ");
    out.push_str(hint);
}

/// Append a `HINT:` line steering 27B-class models toward the
/// subscribe-first resource pattern (push notifications) instead of
/// hot-polling tool calls.
fn append_subscribe_hint(out: &mut String, hint: &str) {
    out.push_str("\nHINT: ");
    out.push_str(hint);
}

// ---------------------------------------------------------------------------
// v4.7 — structured_content payload (JSON parallel to the Markdown body)
// ---------------------------------------------------------------------------

/// Build the forward-port structured payload mirroring [`forward_render`].
#[must_use]
pub fn forward_structured(outcome: &ForwardPortOutcome) -> Value {
    let forward_id = outcome.forward_id.as_str();
    json!({
        "tool":     "ssh_forward",
        "status":   "ok",
        "forward_id": forward_id,
        "session_id": outcome.session_id.as_str(),
        "local":      format!("0.0.0.0:{}", outcome.local_port),
        "remote":     format!("{}:{}", outcome.remote_address, outcome.remote_port),
        "active":     true,
        "next": [
            format!("resources/subscribe forward://{forward_id}/events"),
        ],
    })
}

#[cfg(test)]
mod tests {
    use super::forward_render;
    use crate::application::forward_port::ForwardPortOutcome;
    use crate::domain::ids::{ForwardId, SessionId};

    #[test]
    fn forward_block_format() {
        let m = forward_render(ForwardPortOutcome {
            forward_id: ForwardId::new("fwd-1".to_string()),
            session_id: SessionId::new("sess-1".to_string()),
            local_port: 8080,
            remote_address: "localhost".to_string(),
            remote_port: 3306,
            started_at: "2026-04-18T10:30:00+00:00".to_string(),
        });
        assert!(m.starts_with(
            "SSH_FORWARD: OK\n\
             FORWARD_ID: fwd-1\n\
             SESSION_ID: sess-1\n\
             LOCAL: 0.0.0.0:8080\n\
             REMOTE: localhost:3306\n\
             ACTIVE: true\n"
        ));
        assert!(m.contains("HINT: RECOMMENDED: ssh_subscribe uri=forward://fwd-1/events"));
        assert!(m.contains("NEXT: ssh_subscribe uri=forward://fwd-1/events"));
    }
}
