//! Resource template advertisement for the v4.7 inbound MCP layer.
//!
//! `resources/templates/list` lets clients discover the parameterized URI
//! shapes a server exposes without first walking every live instance via
//! `resources/list` (which is `O(N)` over current resources). Smaller LLMs
//! that want to construct subscribe URIs use the templates returned here as
//! a static catalogue.
//!
//! ## Templates
//!
//! Six entries are advertised; the [`build_list`] helper returns five when
//! the `port_forward` Cargo feature is disabled (no `forward://` stream).
//! The `serial://` template was added in v5.2 (ADR 0009) and is always
//! advertised regardless of `port_forward`.
//!
//! - `shell://{shell_id}/output{?cursor}`
//! - `command://{command_id}/output{?cursor}`
//! - `transfer://{transfer_id}/progress`
//! - `session://{session_id}/health`
//! - `serial://{serial_id}/output{?cursor}` *(always advertised; v5.2)*
//! - `forward://{forward_id}/events{?cursor}` *(feature `port_forward`)*
//!
//! Each template's `uri_template` follows RFC 6570 form-style query
//! expansion (`{?cursor}`); the path-segment parameters use the simple
//! variable expansion form (`{shell_id}`).

use rmcp::model::{AnnotateAble, RawResourceTemplate, ResourceTemplate};

/// Build the canonical list of [`ResourceTemplate`] entries advertised
/// by `resources/templates/list`.
///
/// Order matches the docs' enumeration so the list is stable across
/// builds and easy to assert in unit tests.
#[must_use]
pub fn build_list() -> Vec<ResourceTemplate> {
    let base = vec![
        shell_template(),
        command_template(),
        transfer_template(),
        session_template(),
        serial_template(),
    ];
    #[cfg(feature = "port_forward")]
    {
        let mut out = base;
        out.push(forward_template());
        out
    }
    #[cfg(not(feature = "port_forward"))]
    {
        base
    }
}

/// Shell PTY output stream — byte-stream resource with a cursor surface so
/// callers can fetch only the new bytes after each `notifications/resources/updated`.
fn shell_template() -> ResourceTemplate {
    RawResourceTemplate::new(
        "shell://{shell_id}/output{?cursor}",
        "Shell PTY output stream",
    )
    .with_title("Shell PTY output stream")
    .with_description(
        "Live PTY output buffer for an open shell. \
             Pass `cursor=auto` (or an absolute byte offset) to receive only the \
             new bytes since the last read; omit to start from the head of the buffer.",
    )
    .with_mime_type("text/plain")
    .no_annotation()
}

/// Async command stdout/stderr stream — block-shaped payload with a cursor
/// matching the shell stream semantics.
fn command_template() -> ResourceTemplate {
    RawResourceTemplate::new(
        "command://{command_id}/output{?cursor}",
        "Async command output stream",
    )
    .with_title("Async command output stream")
    .with_description(
        "Stdout/stderr block payload for an async command. \
         Pass `cursor=auto` (or an absolute byte offset) to receive only the new \
         bytes since the last read; omit to start from the head of the buffer.",
    )
    .with_mime_type("text/plain")
    .no_annotation()
}

/// SFTP transfer progress snapshot — point-in-time JSON payload, no cursor.
fn transfer_template() -> ResourceTemplate {
    RawResourceTemplate::new(
        "transfer://{transfer_id}/progress",
        "SFTP transfer progress snapshot",
    )
    .with_title("SFTP transfer progress snapshot")
    .with_description(
        "Point-in-time JSON progress snapshot for an SFTP transfer \
         (`status`, `bytes_transferred`, `total_bytes`). \
         Each subscription update fires a fresh snapshot; no cursor pagination.",
    )
    .with_mime_type("application/json")
    .no_annotation()
}

/// SSH session health snapshot — point-in-time JSON payload, no cursor.
fn session_template() -> ResourceTemplate {
    RawResourceTemplate::new(
        "session://{session_id}/health",
        "SSH session health snapshot",
    )
    .with_title("SSH session health snapshot")
    .with_description(
        "Point-in-time JSON health snapshot for an SSH session \
         (`healthy`, `host`, `expires_at`). \
         Each subscription update fires a fresh snapshot; no cursor pagination.",
    )
    .with_mime_type("application/json")
    .no_annotation()
}

/// Native UART / TTY / COM byte stream — cursor-paginated text payload.
/// ADR 0009 (v5.2). Always advertised — local serial transport is not
/// gated by `port_forward`.
fn serial_template() -> ResourceTemplate {
    RawResourceTemplate::new(
        "serial://{serial_id}/output{?cursor}",
        "Native UART/TTY/COM byte stream",
    )
    .with_title("Native UART/TTY/COM byte stream")
    .with_description(
        "Live byte stream from a local serial port (RS-232 / RS-485 / TTL UART / CDC-ACM). \
         Pass `cursor=auto` (or an absolute byte offset) to receive only the new bytes \
         since the last read; omit to start from the head of the buffer.",
    )
    .with_mime_type("text/plain")
    .no_annotation()
}

/// Port-forward event log — cursor-paginated JSON event stream. Feature-gated
/// so the no-forward build does not advertise a stream it cannot serve.
#[cfg(feature = "port_forward")]
fn forward_template() -> ResourceTemplate {
    RawResourceTemplate::new(
        "forward://{forward_id}/events{?cursor}",
        "Port-forward event log",
    )
    .with_title("Port-forward event log")
    .with_description(
        "Append-only JSON event log for a TCP port forwarder \
         (`accepted`, `connected`, `closed`, `error`). \
         Pass `cursor=auto` (or an absolute event index) to receive only events \
         since the last read; omit to start from the head of the log.",
    )
    .with_mime_type("application/json")
    .no_annotation()
}

#[cfg(test)]
mod tests {
    use super::build_list;

    /// Check basic RFC 6570 syntactic conformance: every URI template starts
    /// with a known scheme and includes at least one `{<param>}` expression.
    fn looks_like_rfc6570(uri: &str) -> bool {
        let scheme_ok = [
            "shell://",
            "command://",
            "transfer://",
            "session://",
            "forward://",
            "serial://",
        ]
        .iter()
        .any(|s| uri.starts_with(s));
        let has_expansion = uri.contains('{') && uri.contains('}');
        scheme_ok && has_expansion
    }

    #[cfg(not(feature = "port_forward"))]
    #[test]
    fn returns_five_templates_without_forward_feature() {
        let list = build_list();
        assert_eq!(list.len(), 5, "expected 5 templates without port_forward");
        let schemes: Vec<&str> = list.iter().map(|t| t.uri_template.as_str()).collect();
        assert!(schemes.iter().any(|s| s.starts_with("shell://")));
        assert!(schemes.iter().any(|s| s.starts_with("command://")));
        assert!(schemes.iter().any(|s| s.starts_with("transfer://")));
        assert!(schemes.iter().any(|s| s.starts_with("session://")));
        assert!(schemes.iter().any(|s| s.starts_with("serial://")));
        assert!(
            !schemes.iter().any(|s| s.starts_with("forward://")),
            "forward stream must not be advertised when port_forward is disabled"
        );
    }

    #[cfg(feature = "port_forward")]
    #[test]
    fn returns_six_templates_with_forward_feature() {
        let list = build_list();
        assert_eq!(list.len(), 6, "expected 6 templates with port_forward");
        let schemes: Vec<&str> = list.iter().map(|t| t.uri_template.as_str()).collect();
        assert!(schemes.iter().any(|s| s.starts_with("shell://")));
        assert!(schemes.iter().any(|s| s.starts_with("command://")));
        assert!(schemes.iter().any(|s| s.starts_with("transfer://")));
        assert!(schemes.iter().any(|s| s.starts_with("session://")));
        assert!(schemes.iter().any(|s| s.starts_with("forward://")));
        assert!(schemes.iter().any(|s| s.starts_with("serial://")));
    }

    #[test]
    fn every_template_has_rfc6570_uri() {
        for tpl in build_list() {
            assert!(
                looks_like_rfc6570(&tpl.uri_template),
                "uri_template {:?} fails RFC 6570 syntactic check",
                tpl.uri_template
            );
        }
    }

    #[test]
    fn every_template_carries_name_description_mime() {
        for tpl in build_list() {
            assert!(!tpl.name.is_empty(), "template name must be set");
            assert!(
                tpl.description.as_deref().is_some_and(|d| !d.is_empty()),
                "template description must be non-empty"
            );
            assert!(
                tpl.mime_type.as_deref().is_some_and(|m| !m.is_empty()),
                "template mime_type must be non-empty"
            );
        }
    }

    #[test]
    fn cursor_streams_advertise_query_param_template() {
        // Shell, command, serial, and (when enabled) forward templates expose
        // a cursor query param via RFC 6570 `{?cursor}` form-style expansion.
        let list = build_list();
        for tpl in &list {
            let uri = tpl.uri_template.as_str();
            if uri.starts_with("shell://")
                || uri.starts_with("command://")
                || uri.starts_with("serial://")
            {
                assert!(
                    uri.contains("{?cursor}"),
                    "byte-stream template {uri} must advertise {{?cursor}}"
                );
            }
            #[cfg(feature = "port_forward")]
            if uri.starts_with("forward://") {
                assert!(
                    uri.contains("{?cursor}"),
                    "forward template must advertise {{?cursor}}"
                );
            }
        }
    }
}
