//! Op dispatcher — translates [`Op`] values into rmcp `tools/call` and
//! `resources/subscribe` requests over the in-process duplex
//! transport, then emits the matching [`Event`] onto the formatter
//! mpsc.
//!
//! The dispatcher is single-task and serialises ops in arrival order.
//! Concurrency lives below us — each tool call is itself async on the
//! server, so the dispatcher does not need a pool. This keeps the
//! mapping op→event easy to reason about: every stdin line produces
//! at least one stdout event (`ack` or `err`); long-running tools
//! (commands, transfers) emit additional events later through the
//! notification bridge in [`crate::embed::event_mux`].

use std::sync::Arc;

use rmcp::model::{
    CallToolRequestParams, ReadResourceRequestParams, SubscribeRequestParams,
    UnsubscribeRequestParams,
};
use rmcp::service::{RoleClient, RunningService, Service};
use serde_json::{Map, Value};

use crate::domain::subscription::SubId;

use crate::embed::event_mux::EventTx;
use crate::embed::formatter::Event;
use crate::embed::parser::Op;

/// Standalone helper that returns the canonical MCP tool name for a
/// given op. Centralised so the integration tests can assert on the
/// mapping table.
#[must_use]
pub const fn op_tool_name(op: &Op) -> &'static str {
    match op {
        Op::Connect { .. } => "ssh_connect",
        Op::Exec { .. } => "ssh_exec",
        Op::Subscribe { .. } => "_resources_subscribe",
        Op::Unsubscribe { .. } => "_resources_unsubscribe",
        Op::Read { .. } => "_resources_read",
        Op::ShellOpen { .. } => "ssh_shell_open",
        Op::ShellWrite { .. } => "ssh_shell_write",
        Op::ShellKey { .. } => "ssh_shell_press",
        Op::Upload { .. } => "ssh_upload",
        Op::Download { .. } => "ssh_download",
        Op::Cancel { .. } => "ssh_exec_cancel",
        Op::Disconnect { .. } => "ssh_disconnect",
        Op::Shutdown { .. } => "_shutdown",
    }
}

/// Translate an [`Op`] into the rmcp `tools/call` argument map.
///
/// The resource ops (`subscribe`, `unsubscribe`, `read`) and the
/// `shutdown` sentinel return `None` because they do not call into
/// the `tools/call` surface — the dispatcher routes them through the
/// dedicated `Peer<RoleClient>::subscribe` / `unsubscribe` /
/// `read_resource` helpers.
#[must_use]
pub fn op_call_arguments(op: &Op) -> Option<Map<String, Value>> {
    match op {
        Op::Connect { .. } => Some(connect_args_for(op)),
        Op::Exec { .. } => Some(exec_args_for(op)),
        Op::ShellOpen { .. } => Some(shell_open_args_for(op)),
        Op::ShellWrite { .. } => Some(shell_write_args_for(op)),
        Op::ShellKey { .. } => Some(shell_key_args_for(op)),
        Op::Upload { .. } => Some(upload_args_for(op)),
        Op::Download { .. } => Some(download_args_for(op)),
        Op::Cancel { .. } => Some(cancel_args_for(op)),
        Op::Disconnect { .. } => Some(disconnect_args_for(op)),
        Op::Subscribe { .. } | Op::Unsubscribe { .. } | Op::Read { .. } | Op::Shutdown { .. } => {
            None
        }
    }
}

fn connect_args_for(op: &Op) -> Map<String, Value> {
    let Op::Connect {
        host,
        user,
        key,
        password,
        port,
        agent_id,
        reuse_policy,
        ..
    } = op
    else {
        return Map::new();
    };
    connect_arguments(
        host,
        user,
        key.as_deref(),
        password.as_deref(),
        *port,
        agent_id.as_deref(),
        reuse_policy.as_deref(),
    )
}

fn exec_args_for(op: &Op) -> Map<String, Value> {
    let Op::Exec {
        sid,
        cmd,
        pty,
        release_when_no_subs,
        ..
    } = op
    else {
        return Map::new();
    };
    exec_arguments(sid.as_str(), cmd, *pty, *release_when_no_subs)
}

fn shell_open_args_for(op: &Op) -> Map<String, Value> {
    let Op::ShellOpen {
        sid,
        cols,
        rows,
        release_when_no_subs,
        inactivity_ttl_secs,
        max_buffer_size,
        ..
    } = op
    else {
        return Map::new();
    };
    shell_open_arguments(
        sid.as_str(),
        *cols,
        *rows,
        *release_when_no_subs,
        *inactivity_ttl_secs,
        *max_buffer_size,
    )
}

fn shell_write_args_for(op: &Op) -> Map<String, Value> {
    let Op::ShellWrite { shid, bytes, .. } = op else {
        return Map::new();
    };
    shell_write_arguments(shid.as_str(), bytes)
}

fn shell_key_args_for(op: &Op) -> Map<String, Value> {
    let Op::ShellKey {
        shid, key, repeat, ..
    } = op
    else {
        return Map::new();
    };
    shell_key_arguments(shid.as_str(), key, *repeat)
}

fn upload_args_for(op: &Op) -> Map<String, Value> {
    let Op::Upload {
        sid,
        local,
        remote,
        release_when_no_subs,
        resume,
        verify,
        ..
    } = op
    else {
        return Map::new();
    };
    upload_arguments(
        sid.as_str(),
        local,
        remote,
        *release_when_no_subs,
        *resume,
        *verify,
    )
}

fn download_args_for(op: &Op) -> Map<String, Value> {
    let Op::Download {
        sid,
        remote,
        local,
        release_when_no_subs,
        resume,
        verify,
        ..
    } = op
    else {
        return Map::new();
    };
    download_arguments(
        sid.as_str(),
        remote,
        local,
        *release_when_no_subs,
        *resume,
        *verify,
    )
}

fn cancel_args_for(op: &Op) -> Map<String, Value> {
    let Op::Cancel { cid, .. } = op else {
        return Map::new();
    };
    cancel_arguments(cid.as_str())
}

fn disconnect_args_for(op: &Op) -> Map<String, Value> {
    let Op::Disconnect { sid, .. } = op else {
        return Map::new();
    };
    disconnect_arguments(sid.as_str())
}

fn connect_arguments(
    host: &str,
    user: &str,
    key: Option<&str>,
    password: Option<&str>,
    port: Option<u16>,
    agent_id: Option<&str>,
    reuse_policy: Option<&str>,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("address".to_string(), Value::String(host.to_string()));
    map.insert("username".to_string(), Value::String(user.to_string()));
    if let Some(k) = key {
        map.insert("private_key".to_string(), Value::String(k.to_string()));
    }
    if let Some(p) = password {
        map.insert("password".to_string(), Value::String(p.to_string()));
    }
    if let Some(p) = port {
        map.insert("port".to_string(), Value::Number(p.into()));
    }
    if let Some(a) = agent_id {
        map.insert("agent_id".to_string(), Value::String(a.to_string()));
    }
    if let Some(r) = reuse_policy {
        map.insert("reuse".to_string(), Value::String(r.to_string()));
    }
    map
}

fn exec_arguments(
    sid: &str,
    cmd: &str,
    pty: Option<bool>,
    release_when_no_subs: Option<bool>,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("session_id".to_string(), Value::String(sid.to_string()));
    map.insert("command".to_string(), Value::String(cmd.to_string()));
    if let Some(p) = pty {
        map.insert("pty".to_string(), Value::Bool(p));
    }
    if let Some(r) = release_when_no_subs {
        map.insert("release_when_no_subs".to_string(), Value::Bool(r));
    }
    map
}

fn shell_open_arguments(
    sid: &str,
    cols: u16,
    rows: u16,
    release_when_no_subs: Option<bool>,
    inactivity_ttl_secs: Option<u64>,
    max_buffer_size: Option<u64>,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("session_id".to_string(), Value::String(sid.to_string()));
    map.insert("cols".to_string(), Value::Number(cols.into()));
    map.insert("rows".to_string(), Value::Number(rows.into()));
    if let Some(r) = release_when_no_subs {
        map.insert("release_when_no_subs".to_string(), Value::Bool(r));
    }
    if let Some(t) = inactivity_ttl_secs {
        map.insert("inactivity_ttl_secs".to_string(), Value::Number(t.into()));
    }
    if let Some(b) = max_buffer_size {
        map.insert("max_buffer_size".to_string(), Value::Number(b.into()));
    }
    map
}

fn shell_write_arguments(shid: &str, bytes: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("shell_id".to_string(), Value::String(shid.to_string()));
    map.insert("input".to_string(), Value::String(bytes.to_string()));
    map
}

fn shell_key_arguments(shid: &str, key: &str, repeat: Option<u32>) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("shell_id".to_string(), Value::String(shid.to_string()));
    map.insert("key".to_string(), Value::String(key.to_string()));
    if let Some(r) = repeat {
        map.insert("repeat".to_string(), Value::Number(r.into()));
    }
    map
}

fn upload_arguments(
    sid: &str,
    local: &str,
    remote: &str,
    release_when_no_subs: Option<bool>,
    resume: Option<bool>,
    verify: Option<bool>,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("session_id".to_string(), Value::String(sid.to_string()));
    map.insert("local_path".to_string(), Value::String(local.to_string()));
    map.insert("remote_path".to_string(), Value::String(remote.to_string()));
    if let Some(r) = release_when_no_subs {
        map.insert("release_when_no_subs".to_string(), Value::Bool(r));
    }
    if let Some(r) = resume {
        map.insert("resume".to_string(), Value::Bool(r));
    }
    if let Some(v) = verify {
        map.insert("verify".to_string(), Value::Bool(v));
    }
    map
}

fn download_arguments(
    sid: &str,
    remote: &str,
    local: &str,
    release_when_no_subs: Option<bool>,
    resume: Option<bool>,
    verify: Option<bool>,
) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("session_id".to_string(), Value::String(sid.to_string()));
    map.insert("remote_path".to_string(), Value::String(remote.to_string()));
    map.insert("local_path".to_string(), Value::String(local.to_string()));
    if let Some(r) = release_when_no_subs {
        map.insert("release_when_no_subs".to_string(), Value::Bool(r));
    }
    if let Some(r) = resume {
        map.insert("resume".to_string(), Value::Bool(r));
    }
    if let Some(v) = verify {
        map.insert("verify".to_string(), Value::Bool(v));
    }
    map
}

fn cancel_arguments(cid: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("command_id".to_string(), Value::String(cid.to_string()));
    map
}

fn disconnect_arguments(sid: &str) -> Map<String, Value> {
    let mut map = Map::new();
    map.insert("session_id".to_string(), Value::String(sid.to_string()));
    map
}

/// Outcome of dispatching a single op.
///
/// The dispatcher returns this flag-only enum so the daemon main
/// loop can decide whether to keep reading stdin (`Continue`) or to
/// break out of the loop and trigger graceful drain (`Shutdown`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DispatchOutcome {
    /// Continue reading the next op from stdin.
    Continue,
    /// `shutdown` op encountered — drain and exit.
    Shutdown,
}

/// Translate one [`Op`] into the matching rmcp request, await the
/// response, and forward an [`Event::Ack`] / [`Event::Err`] onto the
/// outbound mpsc.
///
/// Generic over the rmcp client `Service` so unit tests can stub the
/// dispatcher with a fake transport while production wiring uses
/// [`crate::embed::event_mux::EmbedClient`].
///
/// # Errors
/// Surfaces only the `EventTx::send` failure (closed receiver —
/// fatal). Per-op errors from the rmcp client are encoded as
/// `Event::Err` and forwarded onto the outbound mpsc; the dispatcher
/// never propagates them to the caller.
pub async fn dispatch_one<S>(
    client: &Arc<RunningService<RoleClient, S>>,
    tx: &EventTx,
    op: Op,
) -> Result<DispatchOutcome, DispatchError>
where
    S: Service<RoleClient>,
{
    let correlation = op.correlation_id().map(str::to_string);
    if matches!(op, Op::Shutdown { .. }) {
        forward(tx, ack_with_id(correlation)).await?;
        return Ok(DispatchOutcome::Shutdown);
    }

    let event = if op_call_arguments(&op).is_some() {
        dispatch_tool_call(client, &op, correlation.as_deref()).await
    } else {
        dispatch_resource_op(client, &op, correlation.as_deref()).await
    };

    forward(tx, event).await?;
    Ok(DispatchOutcome::Continue)
}

const fn ack_with_id(correlation: Option<String>) -> Event {
    Event::Ack {
        id: correlation,
        sid: None,
        cid: None,
        shid: None,
        sub_id: None,
        tid: None,
        uri: None,
    }
}

fn opt_owned(value: Option<&str>) -> Option<String> {
    value.map(str::to_string)
}

async fn dispatch_tool_call<S>(
    client: &Arc<RunningService<RoleClient, S>>,
    op: &Op,
    correlation: Option<&str>,
) -> Event
where
    S: Service<RoleClient>,
{
    let Some(arguments) = op_call_arguments(op) else {
        return tool_call_unrouted(correlation);
    };
    let name = op_tool_name(op);
    let params = CallToolRequestParams::new(name).with_arguments(arguments);
    match client.peer().call_tool(params).await {
        Ok(_result) => ack_with_id(opt_owned(correlation)),
        Err(err) => Event::Err {
            id: opt_owned(correlation),
            code: "TOOL_CALL_FAILED".to_string(),
            reason: format!("{name} failed"),
            detail: Some(err.to_string()),
        },
    }
}

fn tool_call_unrouted(correlation: Option<&str>) -> Event {
    Event::Err {
        id: opt_owned(correlation),
        code: "UNROUTED_OP".to_string(),
        reason: "internal: op fell through dispatch table".to_string(),
        detail: None,
    }
}

async fn dispatch_resource_op<S>(
    client: &Arc<RunningService<RoleClient, S>>,
    op: &Op,
    correlation: Option<&str>,
) -> Event
where
    S: Service<RoleClient>,
{
    match op {
        Op::Subscribe { uri, .. } => dispatch_subscribe(client, uri, correlation).await,
        Op::Unsubscribe { sub_id, .. } => {
            dispatch_unsubscribe(client, sub_id.as_str(), correlation).await
        }
        Op::Read { uri, .. } => dispatch_read(client, uri, correlation).await,
        Op::Connect { .. }
        | Op::Exec { .. }
        | Op::ShellOpen { .. }
        | Op::ShellWrite { .. }
        | Op::ShellKey { .. }
        | Op::Upload { .. }
        | Op::Download { .. }
        | Op::Cancel { .. }
        | Op::Disconnect { .. }
        | Op::Shutdown { .. } => tool_call_unrouted(correlation),
    }
}

async fn dispatch_subscribe<S>(
    client: &Arc<RunningService<RoleClient, S>>,
    uri: &str,
    correlation: Option<&str>,
) -> Event
where
    S: Service<RoleClient>,
{
    match client
        .peer()
        .subscribe(SubscribeRequestParams::new(uri.to_string()))
        .await
    {
        Ok(()) => Event::Ack {
            id: opt_owned(correlation),
            sid: None,
            cid: None,
            shid: None,
            sub_id: None,
            tid: None,
            uri: Some(uri.to_string()),
        },
        Err(err) => Event::Err {
            id: opt_owned(correlation),
            code: "SUBSCRIBE_FAILED".to_string(),
            reason: format!("subscribe {uri} failed"),
            detail: Some(err.to_string()),
        },
    }
}

async fn dispatch_unsubscribe<S>(
    client: &Arc<RunningService<RoleClient, S>>,
    sub_id: &str,
    correlation: Option<&str>,
) -> Event
where
    S: Service<RoleClient>,
{
    match client
        .peer()
        .unsubscribe(UnsubscribeRequestParams::new(sub_id.to_string()))
        .await
    {
        Ok(()) => Event::Ack {
            id: opt_owned(correlation),
            sid: None,
            cid: None,
            shid: None,
            sub_id: Some(SubId::new(sub_id.to_string())),
            tid: None,
            uri: None,
        },
        Err(err) => Event::Err {
            id: opt_owned(correlation),
            code: "UNSUBSCRIBE_FAILED".to_string(),
            reason: format!("unsubscribe {sub_id} failed"),
            detail: Some(err.to_string()),
        },
    }
}

async fn dispatch_read<S>(
    client: &Arc<RunningService<RoleClient, S>>,
    uri: &str,
    correlation: Option<&str>,
) -> Event
where
    S: Service<RoleClient>,
{
    match client
        .peer()
        .read_resource(ReadResourceRequestParams::new(uri.to_string()))
        .await
    {
        Ok(_result) => Event::Ack {
            id: opt_owned(correlation),
            sid: None,
            cid: None,
            shid: None,
            sub_id: None,
            tid: None,
            uri: Some(uri.to_string()),
        },
        Err(err) => Event::Err {
            id: opt_owned(correlation),
            code: "READ_FAILED".to_string(),
            reason: format!("read {uri} failed"),
            detail: Some(err.to_string()),
        },
    }
}

async fn forward(tx: &EventTx, event: Event) -> Result<(), DispatchError> {
    tx.send(event)
        .await
        .map_err(|_send| DispatchError::TxClosed)
}

/// Errors surfaced by the dispatcher.
///
/// The variants are intentionally narrow: only fatal problems
/// (closed mpsc) bubble up. Per-op failures (auth errors, invalid
/// args) become `Event::Err` rather than `DispatchError`.
#[derive(Debug, thiserror::Error)]
pub enum DispatchError {
    /// The outbound mpsc receiver has been dropped (typically because
    /// the formatter exited). The daemon main loop should treat this
    /// as a fatal stop signal.
    #[error("event mpsc receiver closed")]
    TxClosed,
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    reason = "test-only assertions are deliberately direct"
)]
mod tests {
    use super::*;
    use crate::domain::ids::{CommandId, SessionId, ShellId};
    use crate::domain::subscription::SubId;

    fn connect_op() -> Op {
        Op::Connect {
            host: "h".to_string(),
            user: "u".to_string(),
            key: Some("/tmp/k".to_string()),
            password: None,
            port: Some(2222),
            agent_id: Some("agent-1".to_string()),
            reuse_policy: Some("auto".to_string()),
            id: Some("c1".to_string()),
        }
    }

    #[test]
    fn op_to_tool_name_table() {
        assert_eq!(op_tool_name(&connect_op()), "ssh_connect");
        assert_eq!(
            op_tool_name(&Op::Disconnect {
                sid: SessionId::new("s".to_string()),
                id: None,
            }),
            "ssh_disconnect"
        );
        assert_eq!(
            op_tool_name(&Op::Cancel {
                cid: CommandId::new("c".to_string()),
                id: None,
            }),
            "ssh_exec_cancel"
        );
        assert_eq!(
            op_tool_name(&Op::Subscribe {
                uri: "command://x/output".to_string(),
                lifetime: None,
                grace_ms: None,
                lag_policy: None,
                filter: None,
                start_cursor: None,
                id: None,
            }),
            "_resources_subscribe"
        );
    }

    #[test]
    fn connect_arguments_serialise_required_fields() {
        let args = op_call_arguments(&connect_op()).unwrap();
        assert_eq!(args.get("address"), Some(&Value::String("h".to_string())));
        assert_eq!(args.get("username"), Some(&Value::String("u".to_string())));
        assert!(args.contains_key("private_key"));
        assert!(args.contains_key("agent_id"));
        assert!(args.contains_key("reuse"));
    }

    #[test]
    fn exec_arguments_include_pty_flag_when_set() {
        let op = Op::Exec {
            sid: SessionId::new("s".to_string()),
            cmd: "ls".to_string(),
            pty: Some(true),
            release_when_no_subs: Some(true),
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(args.get("pty"), Some(&Value::Bool(true)));
        assert_eq!(args.get("release_when_no_subs"), Some(&Value::Bool(true)));
    }

    #[test]
    fn shell_open_arguments_include_dimensions() {
        let op = Op::ShellOpen {
            sid: SessionId::new("s".to_string()),
            cols: 120,
            rows: 30,
            release_when_no_subs: None,
            inactivity_ttl_secs: Some(900),
            max_buffer_size: Some(1_048_576),
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(args.get("cols"), Some(&Value::Number(120.into())));
        assert_eq!(args.get("rows"), Some(&Value::Number(30.into())));
    }

    #[test]
    fn shell_write_arguments_have_input_field() {
        let op = Op::ShellWrite {
            shid: ShellId::new("sh".to_string()),
            bytes: "ls\n".to_string(),
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(args.get("input"), Some(&Value::String("ls\n".to_string())));
    }

    #[test]
    fn shell_key_arguments_optionally_include_repeat() {
        let op = Op::ShellKey {
            shid: ShellId::new("sh".to_string()),
            key: "ctrl_c".to_string(),
            repeat: Some(3),
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(args.get("repeat"), Some(&Value::Number(3.into())));
    }

    #[test]
    fn upload_arguments_have_local_and_remote() {
        let op = Op::Upload {
            sid: SessionId::new("s".to_string()),
            local: "/tmp/a".to_string(),
            remote: "/srv/a".to_string(),
            release_when_no_subs: None,
            resume: None,
            verify: None,
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(
            args.get("local_path"),
            Some(&Value::String("/tmp/a".to_string()))
        );
        assert_eq!(
            args.get("remote_path"),
            Some(&Value::String("/srv/a".to_string()))
        );
        // ADR 0010 — opt-in fields are omitted when caller does not set
        // them; v6.0 NDJSON callers see byte-identical CallTool args.
        assert!(!args.contains_key("resume"));
        assert!(!args.contains_key("verify"));
    }

    #[test]
    fn download_arguments_match_upload_shape() {
        let op = Op::Download {
            sid: SessionId::new("s".to_string()),
            remote: "/srv/a".to_string(),
            local: "/tmp/a".to_string(),
            release_when_no_subs: None,
            resume: None,
            verify: None,
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert!(args.contains_key("local_path"));
        assert!(args.contains_key("remote_path"));
    }

    /// ADR 0010 — when the NDJSON caller passes `resume` / `verify`, the
    /// dispatcher must surface them as `bool` fields on the
    /// `ssh_upload` CallTool argument map so the tool router picks them
    /// up via `args.resume.unwrap_or(false)` / `args.verify.unwrap_or`.
    #[test]
    fn upload_arguments_carry_resume_and_verify_when_set() {
        let op = Op::Upload {
            sid: SessionId::new("s".to_string()),
            local: "/tmp/a".to_string(),
            remote: "/srv/a".to_string(),
            release_when_no_subs: None,
            resume: Some(true),
            verify: Some(true),
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(args.get("resume"), Some(&Value::Bool(true)));
        assert_eq!(args.get("verify"), Some(&Value::Bool(true)));
    }

    /// Mirror invariant for the download direction.
    #[test]
    fn download_arguments_carry_resume_and_verify_when_set() {
        let op = Op::Download {
            sid: SessionId::new("s".to_string()),
            remote: "/srv/a".to_string(),
            local: "/tmp/a".to_string(),
            release_when_no_subs: None,
            resume: Some(true),
            verify: Some(false),
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(args.get("resume"), Some(&Value::Bool(true)));
        assert_eq!(args.get("verify"), Some(&Value::Bool(false)));
    }

    #[test]
    fn cancel_arguments_carry_command_id() {
        let op = Op::Cancel {
            cid: CommandId::new("c".to_string()),
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(
            args.get("command_id"),
            Some(&Value::String("c".to_string()))
        );
    }

    #[test]
    fn disconnect_arguments_carry_session_id() {
        let op = Op::Disconnect {
            sid: SessionId::new("s".to_string()),
            id: None,
        };
        let args = op_call_arguments(&op).unwrap();
        assert_eq!(
            args.get("session_id"),
            Some(&Value::String("s".to_string()))
        );
    }

    #[test]
    fn subscribe_op_returns_no_tool_call_arguments() {
        let op = Op::Subscribe {
            uri: "command://x/output".to_string(),
            lifetime: None,
            grace_ms: None,
            lag_policy: None,
            filter: None,
            start_cursor: None,
            id: None,
        };
        assert!(op_call_arguments(&op).is_none());
    }

    #[test]
    fn unsubscribe_op_returns_no_tool_call_arguments() {
        let op = Op::Unsubscribe {
            sub_id: SubId::new("sub-1".to_string()),
            id: None,
        };
        assert!(op_call_arguments(&op).is_none());
    }

    #[test]
    fn read_op_returns_no_tool_call_arguments() {
        let op = Op::Read {
            uri: "shell://x/output".to_string(),
            cursor: Some(0),
            id: None,
        };
        assert!(op_call_arguments(&op).is_none());
    }

    #[test]
    fn shutdown_op_returns_no_tool_call_arguments() {
        let op = Op::Shutdown { id: None };
        assert!(op_call_arguments(&op).is_none());
    }

    #[test]
    fn dispatch_outcome_continue_default() {
        let outcome = DispatchOutcome::Continue;
        assert_ne!(outcome, DispatchOutcome::Shutdown);
    }
}
