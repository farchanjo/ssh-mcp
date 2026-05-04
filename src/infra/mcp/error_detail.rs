//! Centralised DETAIL pedagogy lookup for the v5 error taxonomy.
//!
//! Source of truth: [`docs/llm-ux/ERROR_HANDBOOK.md`] + [ADR 0007].
//!
//! Each wire code maps to a single one-sentence cure tuned for direct
//! LLM consumption. Both the markdown DETAIL line and the structured
//! `detail` field consult this lookup so the two channels never drift.
//!
//! When [`crate::infra::mcp::tool_router::classify_error`] returns a
//! per-call detail (the offending id, port, limit), [`with_detail`]
//! merges the static cure with the dynamic context using `; ` as the
//! separator — consistent with the closest-match suggestion pathway.

/// Static DETAIL string for the supplied wire code.
///
/// Returns `None` when the code is not in the v5 taxonomy. Callers
/// fall back to whatever per-call detail
/// [`crate::infra::mcp::tool_router::classify_error`] produced.
///
/// Implemented as a chain of `or_else` lookups, one per category, so
/// each helper stays under the 30-line ceiling enforced by
/// `clippy::too_many_lines`.
#[must_use]
pub fn detail_for(code: &str) -> Option<&'static str> {
    detail_for_auth(code)
        .or_else(|| detail_for_transport(code))
        .or_else(|| detail_for_remote(code))
        .or_else(|| detail_for_resource(code))
        .or_else(|| detail_for_policy(code))
        .or_else(|| detail_for_state(code))
        .or_else(|| detail_for_internal(code))
}

const fn detail_for_auth(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"AUTH_FAILED" => "Verify username, key path, and agent socket; re-issue ssh_connect with corrected credentials.",
        b"AUTH_KEY_PARSE" => "Convert key to OpenSSH or PKCS#8 PEM (ssh-keygen -p -m PEM); supply the correct passphrase.",
        _ => return None,
    })
}

const fn detail_for_transport(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"CONNECTION_FAILED" => "Auto-retry with exponential backoff (cap 10s); if persistent, check DNS/firewall.",
        b"CONNECTION_TIMEOUT" => "Auto-retry with backoff or raise SSH_CONNECT_TIMEOUT_S for slow paths.",
        b"TRANSPORT_ERROR" => "Auto-retry with backoff; the new connect re-establishes the channel.",
        b"TIMEOUT" => "Operation exceeded its deadline; retry with a longer wait or raise the relevant timeout.",
        _ => return None,
    })
}

const fn detail_for_remote(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"SFTP_ERROR" => "Inspect the underlying message; fix remote permissions/disk/quota and re-issue.",
        b"REMOTE_CMD_FAILED" => "Inspect exit_code and captured stdout/stderr; decide success vs recoverable.",
        _ => return None,
    })
}

const fn detail_for_resource(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"SESSION_NOT_FOUND" => "Use ssh_list_sessions; recreate via ssh_connect if the id is stale.",
        b"SHELL_NOT_FOUND" => "Recreate via ssh_shell_open; subscribe to shell://<id>/output to track lifecycle.",
        b"COMMAND_NOT_FOUND" => "Use ssh_list_commands; subscribe to command://<id>/output for completion events.",
        b"TRANSFER_NOT_FOUND" => "Re-issue ssh_upload or ssh_download to obtain a fresh transfer_id.",
        b"FORWARD_NOT_FOUND" => "Re-issue ssh_forward; verify the binary was built with the port_forward feature.",
        b"RESOURCE_GONE" => "Resource closed (lifecycle Releasing/Closed); recreate via ssh_shell_open / ssh_execute / ssh_upload.",
        b"SUB_NOT_FOUND" => "Use ssh_sub_list to enumerate active subscriptions.",
        b"GRACE_TIMER_EXPIRED" => "Recreate the resource and resubscribe within the grace window.",
        _ => return None,
    })
}

const fn detail_for_policy(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"MAX_SESSIONS_EXCEEDED" => "Audit ssh_list_sessions; ssh_disconnect_agent stale agents; raise SSH_MAX_SESSIONS if needed.",
        b"MAX_SHELLS_EXCEEDED" => "ssh_shell_close stale shells; consider release_when_no_subs=true so shells self-clean.",
        b"MAX_COMMANDS_EXCEEDED" => "Cancel stale commands or wait for completions; subscribe to command://<id>/output.",
        b"MAX_TRANSFERS_EXCEEDED" => "Wait for in-flight transfers or raise SSH_MAX_TRANSFERS_PER_SESSION.",
        b"MAX_SUBS_PER_URI_EXCEEDED" => "Share an existing sub via fan-out client-side, or unsubscribe stale ones.",
        b"MAX_SUBS_TOTAL_EXCEEDED" => "Audit ssh_sub_list and unsubscribe stale subscriptions.",
        b"LANE_BUFFER_FULL" => "Increase SSH_LANE_BUFFER or switch lag_policy to snapshot.",
        b"MUX_BACKPRESSURE" => "Outbound writer blocked; consume the daemon NDJSON output faster or raise SSH_MUX_BUFFER.",
        b"LAG_DETECTED" => "Lagged N events; snapshot rebuilt; cursor adjusted. Consume faster or switch lag_policy.",
        b"LAG_BACKPRESSURE" => "Consume stdout faster or raise SSH_BP_BLOCK_TIMEOUT_MS.",
        b"RING_BUFFER_OVERFLOW" => "Head bytes dropped; use ssh_sub_replay from a more recent cursor.",
        b"SUB_LEAK_RISK" => "Resource has no observers. Either ssh_subscribe or recreate with release_when_no_subs=true.",
        b"PORT_IN_USE" => "Pick another local port or stop the conflicting listener and retry ssh_forward.",
        _ => return None,
    })
}

const fn detail_for_state(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"INVALID_ARGUMENT" => "Inspect the offending field; correct against the tools/list schema and retry.",
        b"INVALID_REPEAT" => "repeat must be in 1..=64; chain multiple ssh_shell_send_key calls if higher counts are required.",
        b"INVALID_LIFETIME" => "lifetime must be one of {manual, auto_close, lease}.",
        b"INVALID_LAG_POLICY" => "lag_policy must be one of {block_slow, drop_oldest, drop_newest, snapshot}.",
        b"IDEMPOTENCY_KEY_MISMATCH" => "Same key, different args; mint a fresh _meta.idempotency_key per distinct argument set.",
        b"INVALID_OP" => "Op not in the daemon enum; verify against docs/api/ssh-mcp-ndjson.schema.json (Phase 4).",
        b"EMPTY_PATTERNS" => "patterns must contain at least one entry.",
        b"TOO_MANY_PATTERNS" => "patterns list exceeds the cap (max 16); split the wait into multiple calls.",
        b"PATTERN_TOO_LONG" => "Each pattern must fit within 1024 bytes.",
        b"MODIFIER_NOT_ALLOWED" => "The chosen key does not accept the supplied modifier; consult ssh_shell_send_key docs.",
        _ => return None,
    })
}

const fn detail_for_internal(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"STORAGE_ERROR" => "Repository operation failed; collect logs (RUST_LOG=ssh_mcp=debug) and report.",
        b"INTERNAL_ERROR" => "Internal error; collect logs (RUST_LOG=ssh_mcp=debug) and report.",
        b"LIFECYCLE_STATE_CONFLICT" => "Unexpected lifecycle CAS failure; collect logs and report.",
        b"SESSION_REFCOUNT_UNDERFLOW" => "Cascade decrement past zero; collect logs and report.",
        _ => return None,
    })
}

/// Merge the static cure with a per-call dynamic detail.
///
/// Returns the static cure when `dynamic` is `None`/empty, the
/// dynamic detail when no static cure exists, or `"<static>; <dynamic>"`
/// when both are present. Mirrors the closest-match suggestion merge
/// in [`crate::infra::mcp::tool_router::render_tool_error_with_suggestions`].
#[must_use]
pub fn with_detail(code: &str, dynamic: Option<&str>) -> Option<String> {
    let dyn_detail = dynamic.filter(|s| !s.is_empty());
    match (detail_for(code), dyn_detail) {
        (Some(s), Some(d)) => Some(format!("{s}; {d}")),
        (Some(s), None) => Some(s.to_string()),
        (None, Some(d)) => Some(d.to_string()),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{detail_for, with_detail};

    #[test]
    fn known_code_returns_static_detail() {
        assert!(detail_for("RESOURCE_GONE").is_some());
        assert!(detail_for("SUB_NOT_FOUND").is_some());
        assert!(detail_for("SUB_LEAK_RISK").is_some());
    }

    #[test]
    fn unknown_code_returns_none() {
        assert!(detail_for("DOES_NOT_EXIST").is_none());
        assert!(detail_for("").is_none());
    }

    #[test]
    fn auth_codes_have_pedagogy() {
        for code in ["AUTH_FAILED", "AUTH_KEY_PARSE"] {
            let d = detail_for(code).unwrap_or("");
            assert!(!d.is_empty(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn transport_codes_have_pedagogy() {
        for code in ["CONNECTION_FAILED", "CONNECTION_TIMEOUT", "TRANSPORT_ERROR"] {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn resource_codes_have_pedagogy() {
        for code in [
            "SESSION_NOT_FOUND",
            "SHELL_NOT_FOUND",
            "COMMAND_NOT_FOUND",
            "TRANSFER_NOT_FOUND",
            "FORWARD_NOT_FOUND",
            "RESOURCE_GONE",
            "SUB_NOT_FOUND",
            "GRACE_TIMER_EXPIRED",
        ] {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn policy_codes_have_pedagogy() {
        for code in [
            "MAX_SESSIONS_EXCEEDED",
            "MAX_SHELLS_EXCEEDED",
            "MAX_COMMANDS_EXCEEDED",
            "MAX_TRANSFERS_EXCEEDED",
            "MAX_SUBS_PER_URI_EXCEEDED",
            "MAX_SUBS_TOTAL_EXCEEDED",
            "LANE_BUFFER_FULL",
            "MUX_BACKPRESSURE",
            "LAG_DETECTED",
            "LAG_BACKPRESSURE",
            "RING_BUFFER_OVERFLOW",
            "SUB_LEAK_RISK",
        ] {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn state_codes_have_pedagogy() {
        for code in [
            "INVALID_ARGUMENT",
            "INVALID_REPEAT",
            "INVALID_LIFETIME",
            "INVALID_LAG_POLICY",
            "IDEMPOTENCY_KEY_MISMATCH",
            "INVALID_OP",
        ] {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn internal_codes_have_pedagogy() {
        for code in [
            "STORAGE_ERROR",
            "INTERNAL_ERROR",
            "LIFECYCLE_STATE_CONFLICT",
            "SESSION_REFCOUNT_UNDERFLOW",
        ] {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn with_detail_uses_static_when_dynamic_missing() {
        let merged = with_detail("RESOURCE_GONE", None).expect("present");
        assert!(merged.starts_with("Resource closed"));
    }

    #[test]
    fn with_detail_merges_static_and_dynamic() {
        let merged = with_detail("SHELL_NOT_FOUND", Some("sh-stale")).expect("present");
        assert!(merged.contains("Recreate via ssh_shell_open"));
        assert!(merged.ends_with("sh-stale"));
        assert!(merged.contains("; "));
    }

    #[test]
    fn with_detail_falls_back_to_dynamic_only() {
        let merged = with_detail("UNKNOWN_CODE_XYZ", Some("dyn")).expect("present");
        assert_eq!(merged, "dyn");
    }

    #[test]
    fn with_detail_returns_none_when_both_absent() {
        assert!(with_detail("UNKNOWN_CODE_XYZ", None).is_none());
        assert!(with_detail("UNKNOWN_CODE_XYZ", Some("")).is_none());
    }

    #[test]
    fn taxonomy_covers_38_codes_minimum() {
        // Every code documented in docs/llm-ux/ERROR_HANDBOOK.md must
        // resolve. This is the canonical list from ADR 0007 §"38-code
        // taxonomy"; the assert acts as a regression gate.
        let codes = [
            "AUTH_FAILED", "AUTH_KEY_PARSE",
            "CONNECTION_FAILED", "CONNECTION_TIMEOUT", "TRANSPORT_ERROR",
            "SFTP_ERROR", "REMOTE_CMD_FAILED",
            "SESSION_NOT_FOUND", "SHELL_NOT_FOUND", "COMMAND_NOT_FOUND",
            "TRANSFER_NOT_FOUND", "FORWARD_NOT_FOUND",
            "RESOURCE_GONE", "SUB_NOT_FOUND", "GRACE_TIMER_EXPIRED",
            "MAX_SESSIONS_EXCEEDED", "MAX_SHELLS_EXCEEDED",
            "MAX_COMMANDS_EXCEEDED", "MAX_TRANSFERS_EXCEEDED",
            "MAX_SUBS_PER_URI_EXCEEDED", "MAX_SUBS_TOTAL_EXCEEDED",
            "LANE_BUFFER_FULL", "MUX_BACKPRESSURE",
            "LAG_DETECTED", "LAG_BACKPRESSURE", "RING_BUFFER_OVERFLOW",
            "SUB_LEAK_RISK",
            "INVALID_ARGUMENT", "INVALID_REPEAT", "INVALID_LIFETIME",
            "INVALID_LAG_POLICY", "IDEMPOTENCY_KEY_MISMATCH", "INVALID_OP",
            "STORAGE_ERROR", "INTERNAL_ERROR", "LIFECYCLE_STATE_CONFLICT",
            "SESSION_REFCOUNT_UNDERFLOW",
        ];
        for code in codes {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
        assert!(codes.len() >= 37, "expected >=37 entries, got {}", codes.len());
    }
}
