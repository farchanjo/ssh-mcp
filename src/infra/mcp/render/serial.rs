//! Markdown + structured renderers for the serial transport (v5.2; ADR 0009).

use serde_json::{Value, json};

use crate::adapters::serial::state::SerialPortState;
use crate::infra::mcp::results::{SerialOpenEntry, SerialOpenResult};

/// Append a `NEXT:` advisory.
fn next_line(out: &mut String, hint: &str) {
    out.push_str("\nNEXT: ");
    out.push_str(hint);
}

/// Append a `HINT:` advisory.
fn hint_line(out: &mut String, body: &str) {
    out.push_str("\nHINT: ");
    out.push_str(body);
}

/// Render the body of a successful `serial_open`.
#[must_use]
pub fn serial_open_render(result: &SerialOpenResult) -> String {
    let mut out = String::with_capacity(384);
    out.push_str("SERIAL_OPEN: OK\nSERIAL_ID: ");
    out.push_str(&result.serial_id);
    out.push_str("\nPATH: ");
    out.push_str(&result.path);
    out.push_str("\nBAUD: ");
    out.push_str(&result.baud_rate.to_string());
    out.push_str("\nFRAMING: ");
    out.push_str(&result.data_bits.to_string());
    out.push_str(&result.parity[..1].to_uppercase());
    out.push_str(&result.stop_bits);
    out.push_str("\nFLOW: ");
    out.push_str(&result.flow_control);
    out.push_str("\nURI: ");
    out.push_str(&result.uri);
    hint_line(
        &mut out,
        &format!(
            "RECOMMENDED: sub_open uri={} for push (debounce + 64 KiB byte-threshold flush; same pipeline as shell:// / command://). Read deltas via resources/read?cursor=auto. Do NOT poll.",
            result.uri
        ),
    );
    next_line(
        &mut out,
        &format!(
            "sub_open uri={} | serial_write serial_id={} | serial_press serial_id={} | serial_close serial_id={}",
            result.uri, result.serial_id, result.serial_id, result.serial_id
        ),
    );
    out
}

/// Build the structured JSON for `serial_open`.
#[must_use]
pub fn serial_open_structured(result: &SerialOpenResult) -> Value {
    json!({
        "tool":         result.tool,
        "status":       result.status,
        "serial_id":    result.serial_id,
        "path":         result.path,
        "baud_rate":    result.baud_rate,
        "data_bits":    result.data_bits,
        "stop_bits":    result.stop_bits,
        "parity":       result.parity,
        "flow_control": result.flow_control,
        "uri":          result.uri,
        "next": [
            format!("sub_open uri={}", result.uri),
            "serial_write".to_string(),
            "serial_press".to_string(),
            "serial_close".to_string(),
        ],
    })
}

/// Render `serial_close`.
#[must_use]
pub fn serial_close_render(serial_id: &str, closed: bool) -> String {
    let mut out = String::with_capacity(96);
    out.push_str("SERIAL_CLOSE: ");
    out.push_str(if closed { "OK" } else { "NOOP" });
    out.push_str("\nSERIAL_ID: ");
    out.push_str(serial_id);
    out
}

/// Render `serial_write`.
#[must_use]
pub fn serial_write_render(serial_id: &str, bytes_sent: usize) -> String {
    let mut out = String::with_capacity(192);
    out.push_str("SERIAL_WRITE: OK\nSERIAL_ID: ");
    out.push_str(serial_id);
    out.push_str("\nBYTES_SENT: ");
    out.push_str(&bytes_sent.to_string());
    hint_line(
        &mut out,
        &format!(
            "RECOMMENDED: response arrives via push on serial://{serial_id}/output. Wait for notifications/resources/updated, then drain with resources/read?cursor=auto."
        ),
    );
    next_line(
        &mut out,
        &format!(
            "sub_open uri=serial://{serial_id}/output | resources/read serial://{serial_id}/output?cursor=auto | serial_press serial_id={serial_id}"
        ),
    );
    out
}

/// Render `serial_press`.
#[must_use]
pub fn serial_send_key_render(
    serial_id: &str,
    key: &str,
    repeat: u32,
    bytes_sent: usize,
) -> String {
    let mut out = String::with_capacity(192);
    out.push_str("SERIAL_PRESS: OK\nSERIAL_ID: ");
    out.push_str(serial_id);
    out.push_str("\nKEY: ");
    out.push_str(key);
    out.push_str("\nREPEAT: ");
    out.push_str(&repeat.to_string());
    out.push_str("\nBYTES_SENT: ");
    out.push_str(&bytes_sent.to_string());
    out
}

/// Render `serial_scan`.
#[must_use]
pub fn serial_list_ports_render(paths: &[String]) -> String {
    let mut out = String::with_capacity(64 + paths.len() * 32);
    out.push_str("SERIAL_SCAN: OK\nTOTAL: ");
    out.push_str(&paths.len().to_string());
    for p in paths {
        out.push_str("\nPORT: ");
        out.push_str(p);
    }
    out
}

/// Render `serial_active`.
#[must_use]
pub fn serial_list_open_render(entries: &[SerialOpenEntry]) -> String {
    let mut out = String::with_capacity(128 + entries.len() * 64);
    out.push_str("SERIAL_ACTIVE: OK\nTOTAL: ");
    out.push_str(&entries.len().to_string());
    for e in entries {
        out.push_str("\nSERIAL_ID: ");
        out.push_str(&e.serial_id);
        out.push_str(" PATH=");
        out.push_str(&e.path);
        out.push_str(" BAUD=");
        out.push_str(&e.baud_rate.to_string());
        if let Some(label) = &e.label {
            out.push_str(" LABEL=");
            out.push_str(label);
        }
    }
    out
}

/// Snapshot a [`SerialPortState`] into the wire-level entry shape.
#[must_use]
pub fn open_entry_from_state(state: &SerialPortState) -> SerialOpenEntry {
    SerialOpenEntry {
        serial_id: state.id.as_str().to_string(),
        path: state.config.path.clone(),
        baud_rate: state.config.baud_rate,
        label: state.config.label.clone(),
        uri: state.uri(),
    }
}
