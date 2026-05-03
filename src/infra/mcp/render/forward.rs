//! Port-forward markdown renderer.
//!
//! Mirrors v3 `src/mcp/message/builder.rs::render_forward_ok` but takes
//! the v4 use case Outcome as input.

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
    let local = format!("0.0.0.0:{local_port}");
    let remote = format!("{remote_address}:{remote_port}");
    let mut out = String::with_capacity(160);
    out.push_str("SSH_FORWARD: OK\nFORWARD_ID: ");
    out.push_str(&sanitize_value(forward_id.as_str()));
    out.push_str("\nSESSION_ID: ");
    out.push_str(&sanitize_value(session_id.as_str()));
    out.push_str("\nLOCAL: ");
    out.push_str(&sanitize_value(&local));
    out.push_str("\nREMOTE: ");
    out.push_str(&sanitize_value(&remote));
    out.push_str("\nACTIVE: true");
    out
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
        assert_eq!(
            m,
            "SSH_FORWARD: OK\nFORWARD_ID: fwd-1\nSESSION_ID: sess-1\nLOCAL: 0.0.0.0:8080\nREMOTE: localhost:3306\nACTIVE: true"
        );
    }
}
