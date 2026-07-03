//! Centralised DETAIL pedagogy lookup for the v5 error taxonomy.
//!
//! Source of truth: [`docs/LLM_GUIDE.md` → Error handbook] + [ADR 0007].
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
        .or_else(|| detail_for_resume(code))
        .or_else(|| detail_for_rsync(code))
        .or_else(|| detail_for_serial(code))
        .or_else(|| detail_for_inline_push(code))
}

const fn detail_for_auth(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"AUTH_FAILED" => {
            "Verify username, key path, and agent socket; re-issue ssh_connect with corrected credentials."
        }
        b"AUTH_KEY_PARSE" => {
            "Convert key to OpenSSH or PKCS#8 PEM (ssh-keygen -p -m PEM); supply the correct passphrase."
        }
        _ => return None,
    })
}

const fn detail_for_transport(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"CONNECTION_FAILED" => {
            "Auto-retry with exponential backoff (cap 10s); if persistent, check DNS/firewall."
        }
        b"CONNECTION_TIMEOUT" => {
            "Auto-retry with backoff or raise SSH_CONNECT_TIMEOUT_S for slow paths."
        }
        b"TRANSPORT_ERROR" => {
            "Auto-retry with backoff; the new connect re-establishes the channel."
        }
        b"TIMEOUT" => {
            "Operation exceeded its deadline; retry with a longer wait or raise the relevant timeout."
        }
        _ => return None,
    })
}

const fn detail_for_remote(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"SFTP_ERROR" => {
            "Inspect the underlying message; fix remote permissions/disk/quota and re-issue."
        }
        b"REMOTE_CMD_FAILED" => {
            "Inspect exit_code and captured stdout/stderr; decide success vs recoverable."
        }
        _ => return None,
    })
}

const fn detail_for_resource(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"SESSION_NOT_FOUND" => "Use ssh_sessions; recreate via ssh_connect if the id is stale.",
        b"SHELL_NOT_FOUND" => {
            "Recreate via ssh_shell_open; subscribe to shell://<id>/output to track lifecycle."
        }
        b"COMMAND_NOT_FOUND" => {
            "Use ssh_commands; subscribe to command://<id>/output for completion events."
        }
        b"TRANSFER_NOT_FOUND" => {
            "Re-issue ssh_upload or ssh_download to obtain a fresh transfer_id."
        }
        b"FORWARD_NOT_FOUND" => {
            "Re-issue ssh_forward; verify the binary was built with the port_forward feature."
        }
        b"FORWARD_PORT_NOT_FOUND" => {
            "No forwarder is bound to that local_port; verify it against the ssh_forward response before closing."
        }
        b"RESOURCE_GONE" => {
            "Resource closed (lifecycle Releasing/Closed); recreate via ssh_shell_open / ssh_exec / ssh_upload."
        }
        b"SUB_NOT_FOUND" => "Use sub_list to enumerate active subscriptions.",
        b"GRACE_TIMER_EXPIRED" => "Recreate the resource and resubscribe within the grace window.",
        _ => return None,
    })
}

const fn detail_for_policy(code: &str) -> Option<&'static str> {
    if let Some(detail) = detail_for_policy_max(code) {
        return Some(detail);
    }
    detail_for_policy_lag(code)
}

const fn detail_for_policy_max(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"MAX_SESSIONS_EXCEEDED" => {
            "Audit ssh_sessions; ssh_disconnect_agent stale agents; raise SSH_MAX_SESSIONS if needed."
        }
        b"MAX_SHELLS_EXCEEDED" => {
            "ssh_shell_close stale shells; consider release_when_no_subs=true so shells self-clean."
        }
        b"MAX_COMMANDS_EXCEEDED" => {
            "Cancel stale commands or wait for completions; subscribe to command://<id>/output."
        }
        b"MAX_TRANSFERS_EXCEEDED" => {
            "Wait for in-flight transfers or raise SSH_MAX_TRANSFERS_PER_SESSION."
        }
        b"MAX_SUBS_PER_URI_EXCEEDED" => {
            "Share an existing sub via fan-out client-side, or unsubscribe stale ones."
        }
        b"MAX_SUBS_TOTAL_EXCEEDED" => "Audit sub_list and unsubscribe stale subscriptions.",
        b"PORT_IN_USE" => {
            "Pick another local port or stop the conflicting listener and retry ssh_forward."
        }
        _ => return None,
    })
}

const fn detail_for_policy_lag(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"LANE_BUFFER_FULL" => "Increase SSH_LANE_BUFFER or switch lag_policy to snapshot.",
        b"MUX_BACKPRESSURE" => {
            "Outbound writer blocked; consume the daemon NDJSON output faster or raise SSH_MUX_BUFFER."
        }
        b"LAG_DETECTED" => {
            "Lagged N events; snapshot rebuilt; cursor adjusted. Consume faster or switch lag_policy."
        }
        b"LAG_BACKPRESSURE" => "Consume stdout faster or raise SSH_BP_BLOCK_TIMEOUT_MS.",
        b"RING_BUFFER_OVERFLOW" => "Head bytes dropped; use sub_replay from a more recent cursor.",
        b"SUB_LEAK_RISK" => {
            "Resource has no observers. Either sub_open or recreate with release_when_no_subs=true."
        }
        _ => return None,
    })
}

const fn detail_for_state(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"INVALID_ARGUMENT" => {
            "Inspect the offending field; correct against the tools/list schema and retry."
        }
        b"INVALID_REPEAT" => {
            "repeat must be in 1..=64; chain multiple ssh_shell_press calls if higher counts are required."
        }
        b"INVALID_LIFETIME" => "lifetime must be one of {manual, auto_close, lease}.",
        b"INVALID_LAG_POLICY" => {
            "lag_policy must be one of {block_slow, drop_oldest, drop_newest, snapshot}."
        }
        b"IDEMPOTENCY_KEY_MISMATCH" => {
            "Same key, different args; mint a fresh _meta.idempotency_key per distinct argument set."
        }
        b"INVALID_OP" => {
            "Op not in the daemon enum; verify against docs/DAEMON.md → NDJSON command schema."
        }
        b"EMPTY_PATTERNS" => "patterns must contain at least one entry.",
        b"TOO_MANY_PATTERNS" => {
            "patterns list exceeds the cap (max 16); split the wait into multiple calls."
        }
        b"PATTERN_TOO_LONG" => "Each pattern must fit within 1024 bytes.",
        b"MODIFIER_NOT_ALLOWED" => {
            "The chosen key does not accept the supplied modifier; consult ssh_shell_press docs."
        }
        _ => return None,
    })
}

const fn detail_for_internal(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"STORAGE_ERROR" => {
            "Repository operation failed; collect logs (RUST_LOG=ssh_mcp=debug) and report."
        }
        b"INTERNAL_ERROR" => "Internal error; collect logs (RUST_LOG=ssh_mcp=debug) and report.",
        b"LIFECYCLE_STATE_CONFLICT" => "Unexpected lifecycle CAS failure; collect logs and report.",
        b"SESSION_REFCOUNT_UNDERFLOW" => "Cascade decrement past zero; collect logs and report.",
        _ => return None,
    })
}

/// ADR 0010 (v6.1) SFTP resume preflight/verify codes. `STATE` category,
/// neither retryable.
const fn detail_for_resume(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"RESUME_OVERSHOOT" => {
            "Re-run with resume=false to overwrite the destination, or repoint remote_path / local_path at the correct partial file."
        }
        b"RESUME_MISMATCH" => {
            "Re-run with resume=false to overwrite the destination, or fix (truncate/repair) the partial file before retrying with verify=true."
        }
        _ => return None,
    })
}

/// ADR 0011 (v7.0) rsync hybrid transport codes: `RSYNC_NOT_FOUND`
/// (RESOURCE), `RSYNC_VERSION_TOO_OLD` / `RSYNC_FILE_LIST_TOO_LARGE` /
/// `SFTP_FEATURE_MISSING` (POLICY, conditional retry), and
/// `RSYNC_PROTOCOL_ERROR` / `RSYNC_PARTIAL_TRANSFER` (TRANSPORT, retryable).
const fn detail_for_rsync(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"RSYNC_NOT_FOUND" => {
            "Install rsync >= 3.2.0 on the remote, or re-issue with transport=sftp for the universal fallback."
        }
        b"RSYNC_VERSION_TOO_OLD" => {
            "Upgrade rsync on the remote to >= 3.2.0, or re-issue with transport=sftp; transport=auto picks this automatically."
        }
        b"RSYNC_PROTOCOL_ERROR" => {
            "Auto-retry on the same channel; if persistent, switch transports (transport=wire <-> transport=sftp)."
        }
        b"RSYNC_FILE_LIST_TOO_LARGE" => {
            "Tighten opts.exclude (node_modules/, target/, .git/) or raise SSH_RSYNC_FILE_LIST_LIMIT."
        }
        b"RSYNC_PARTIAL_TRANSFER" => {
            "Re-run with opts.partial=true so the transport resumes from the first non-overlapping block per file."
        }
        b"SFTP_FEATURE_MISSING" => {
            "Drop the unsupported preserve/checksum flag, or re-issue with transport=wire (needs rsync >= 3.2.0 on the remote)."
        }
        _ => return None,
    })
}

/// ADR 0009 (v5.2) serial transport codes. `SERIAL_NOT_FOUND` is
/// RESOURCE (never retry without recreating); `SERIAL_ERROR` covers
/// open/read/write/close adapter failures.
const fn detail_for_serial(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"SERIAL_NOT_FOUND" => {
            "Recreate via serial_open; use serial_active to confirm the port is still tracked before serial_write/serial_press."
        }
        b"SERIAL_ERROR" => {
            "Inspect the underlying message; check device permissions/availability and retry serial_open."
        }
        _ => return None,
    })
}

/// ADR 0012 (v7.1) inline-push notification codes. `POLICY` category,
/// non-retryable.
const fn detail_for_inline_push(code: &str) -> Option<&'static str> {
    Some(match code.as_bytes() {
        b"INLINE_PUSH_OVERSIZE" => {
            "Split the payload via InlinePayload::split before calling notify_ssh_output, or raise SSH_INLINE_PUSH_MAX_BYTES_PER_NOTIFY."
        }
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
            "FORWARD_PORT_NOT_FOUND",
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
    fn resume_codes_have_pedagogy() {
        for code in ["RESUME_OVERSHOOT", "RESUME_MISMATCH"] {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn rsync_codes_have_pedagogy() {
        for code in [
            "RSYNC_NOT_FOUND",
            "RSYNC_VERSION_TOO_OLD",
            "RSYNC_PROTOCOL_ERROR",
            "RSYNC_FILE_LIST_TOO_LARGE",
            "RSYNC_PARTIAL_TRANSFER",
            "SFTP_FEATURE_MISSING",
        ] {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn serial_codes_have_pedagogy() {
        for code in ["SERIAL_NOT_FOUND", "SERIAL_ERROR"] {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
    }

    #[test]
    fn inline_push_codes_have_pedagogy() {
        assert!(detail_for("INLINE_PUSH_OVERSIZE").is_some());
    }

    #[test]
    fn taxonomy_covers_38_codes_minimum() {
        // Every code documented in docs/LLM_GUIDE.md (Error handbook) must
        // resolve. This is the canonical list from ADR 0007 §"38-code
        // taxonomy"; the assert acts as a regression gate.
        let codes = [
            "AUTH_FAILED",
            "AUTH_KEY_PARSE",
            "CONNECTION_FAILED",
            "CONNECTION_TIMEOUT",
            "TRANSPORT_ERROR",
            "SFTP_ERROR",
            "REMOTE_CMD_FAILED",
            "SESSION_NOT_FOUND",
            "SHELL_NOT_FOUND",
            "COMMAND_NOT_FOUND",
            "TRANSFER_NOT_FOUND",
            "FORWARD_NOT_FOUND",
            // v7.1.1 — close_forward NotFound fix: local_port has no
            // matching ForwardId in scope at the adapter boundary.
            "FORWARD_PORT_NOT_FOUND",
            "RESOURCE_GONE",
            "SUB_NOT_FOUND",
            "GRACE_TIMER_EXPIRED",
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
            "INVALID_ARGUMENT",
            "INVALID_REPEAT",
            "INVALID_LIFETIME",
            "INVALID_LAG_POLICY",
            "IDEMPOTENCY_KEY_MISMATCH",
            "INVALID_OP",
            "STORAGE_ERROR",
            "INTERNAL_ERROR",
            "LIFECYCLE_STATE_CONFLICT",
            "SESSION_REFCOUNT_UNDERFLOW",
            // v6.1 (ADR 0010) resume codes.
            "RESUME_OVERSHOOT",
            "RESUME_MISMATCH",
            // v7.0 (ADR 0011) rsync hybrid transport codes.
            "RSYNC_NOT_FOUND",
            "RSYNC_VERSION_TOO_OLD",
            "RSYNC_PROTOCOL_ERROR",
            "RSYNC_FILE_LIST_TOO_LARGE",
            "RSYNC_PARTIAL_TRANSFER",
            "SFTP_FEATURE_MISSING",
            // v5.2 (ADR 0009) serial transport codes.
            "SERIAL_NOT_FOUND",
            "SERIAL_ERROR",
            // v7.1 (ADR 0012) inline-push notification codes.
            "INLINE_PUSH_OVERSIZE",
        ];
        for code in codes {
            assert!(detail_for(code).is_some(), "missing DETAIL for {code}");
        }
        assert!(
            codes.len() >= 48,
            "expected >=48 entries, got {}",
            codes.len()
        );
    }
}
