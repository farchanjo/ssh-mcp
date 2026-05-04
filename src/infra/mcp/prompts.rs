//! MCP `prompts/*` catalog — five canonical workflows pre-baked so
//! smaller LLMs can scan `prompts/list` and execute the recipe step by
//! step.
//!
//! The catalog is intentionally tiny: each entry resolves to a single
//! `User` text message describing the canonical tool sequence for the
//! workflow. No server-side state is captured — every parameter is
//! interpolated at `prompts/get` time and the result is deterministic.
//!
//! # Catalogue
//!
//! | Name | Args | Purpose |
//! |------|------|---------|
//! | `run_one_shot_command` | `address`, `username`, `command` | Drive `ssh_run` with the canonical defaults. |
//! | `investigate_session` | `session_id` | Inspect commands + health, then disconnect. |
//! | `upload_and_verify` | `session_id`, `local_path`, `remote_path` | Upload, wait for completion, then `sha256sum`. |
//! | `interactive_shell_drive` | `session_id`, `prompt_pattern` | Open a PTY and gate on a prompt pattern. |
//! | `cleanup_agent` | `agent_id` | Bulk-disconnect every session bound to an agent. |
//!
//! Implementation note: rmcp's `Prompt` / `GetPromptResult` types are
//! marked `#[non_exhaustive]`, so the catalog goes through the public
//! constructors + `with_*` setters instead of struct-literal syntax.

use rmcp::ErrorData as McpError;
use rmcp::model::{
    GetPromptResult, Prompt, PromptArgument, PromptMessage, PromptMessageContent, PromptMessageRole,
};
use serde_json::{Map, Value};

/// Map shape consumed by [`get_prompt`]. Aliases `serde_json::Map`
/// (`BTreeMap`-backed) so the rmcp `GetPromptRequestParams::arguments`
/// payload feeds directly into the catalogue without an extra clone.
pub type PromptArguments = Map<String, Value>;

/// Rendered prompt-definition tuple consumed by [`list_prompts`].
struct PromptDef {
    name: &'static str,
    title: &'static str,
    description: &'static str,
    arguments: &'static [(&'static str, &'static str, bool)],
}

/// Static catalogue. Order matters: `prompts/list` returns entries in
/// declaration order so the LLM scans them top-down. The first five
/// entries mirror the v4.7 task list (refreshed in v5 to recommend
/// push-first observation); the next five are the v5 push-first
/// workflows from ADR 0005.
const CATALOG: &[PromptDef] = &[
    PromptDef {
        name: "run_one_shot_command",
        title: "Run a one-shot remote command",
        description: "Drive ssh_run with reuse=auto and disconnect_after=true to execute a short command.",
        arguments: &[
            (
                "address",
                "SSH endpoint in host:port form (port defaults to 22).",
                true,
            ),
            ("username", "SSH login user.", true),
            (
                "command",
                "Shell command line to execute on the remote host.",
                true,
            ),
        ],
    },
    PromptDef {
        name: "investigate_session",
        title: "Investigate an existing SSH session",
        description: "Snapshot async commands, read the session health resource, then disconnect.",
        arguments: &[(
            "session_id",
            "SESSION_ID returned from a previous ssh_connect.",
            true,
        )],
    },
    PromptDef {
        name: "upload_and_verify",
        title: "Upload a file via SFTP and verify its integrity",
        description: "Run ssh_upload, wait for completion, then ssh_run sha256sum on the remote path.",
        arguments: &[
            ("session_id", "SESSION_ID returned from ssh_connect.", true),
            (
                "local_path",
                "Absolute path to the local file to upload.",
                true,
            ),
            ("remote_path", "Absolute remote destination path.", true),
        ],
    },
    PromptDef {
        name: "interactive_shell_drive",
        title: "Open an interactive PTY and gate on a prompt pattern",
        description: "Open a shell, subscribe to its output, wait for the prompt pattern, then drive it.",
        arguments: &[
            ("session_id", "SESSION_ID returned from ssh_connect.", true),
            (
                "prompt_pattern",
                "Substring pattern that signals the shell is ready (e.g. \"$ \").",
                true,
            ),
        ],
    },
    PromptDef {
        name: "cleanup_agent",
        title: "Bulk-cleanup every session owned by an agent",
        description: "Call ssh_disconnect_agent against the supplied AGENT_ID.",
        arguments: &[(
            "agent_id",
            "AGENT_ID grouping the sessions to dispose.",
            true,
        )],
    },
    // v5 Phase 3 push-first workflows (ADR 0005).
    PromptDef {
        name: "push_first_long_command",
        title: "Run a long command and consume its output via push",
        description: "ssh_execute -> ssh_subscribe -> drain command://<id>/output without polling.",
        arguments: &[
            ("session_id", "SESSION_ID returned from ssh_connect.", true),
            ("command", "Long-running shell command line.", true),
        ],
    },
    PromptDef {
        name: "push_first_interactive_shell",
        title: "Drive an interactive shell with push-first reads",
        description: "ssh_shell_open -> ssh_subscribe shell://<id>/output -> ssh_shell_write / ssh_shell_send_key.",
        arguments: &[
            ("session_id", "SESSION_ID returned from ssh_connect.", true),
            (
                "prompt_pattern",
                "Substring pattern that signals the shell is ready (e.g. \"$ \").",
                true,
            ),
        ],
    },
    PromptDef {
        name: "push_first_file_transfer",
        title: "Upload / download a file with progress push",
        description: "ssh_upload (or ssh_download) -> ssh_subscribe transfer://<id>/progress -> drain until completed.",
        arguments: &[
            ("session_id", "SESSION_ID returned from ssh_connect.", true),
            ("local_path", "Absolute local path.", true),
            ("remote_path", "Absolute remote path.", true),
        ],
    },
    PromptDef {
        name: "subscription_hygiene_audit",
        title: "Audit and clean up zombie subscriptions",
        description: "ssh_sub_list -> identify SUB_LEAK_RISK lanes -> ssh_unsubscribe each.",
        arguments: &[(
            "uri_prefix",
            "Optional URI prefix to scope the audit (e.g. shell://).",
            false,
        )],
    },
    PromptDef {
        name: "chaos_resume_after_disconnect",
        title: "Recover a subscription after a transport drop",
        description: "ssh_sub_replay from cursor / ssh_sub_resume / refresh filter to chase a dropped peer.",
        arguments: &[
            ("sub_id", "SUB_ID returned from ssh_subscribe.", true),
            (
                "from_cursor",
                "Byte cursor to replay from. Use 0 to replay the full window.",
                false,
            ),
        ],
    },
];

/// Build the static prompt list advertised on `prompts/list`. Each
/// entry carries a stable `name`, a human-readable title, and the
/// typed argument schema the host exposes to the user.
#[must_use]
pub fn list_prompts() -> Vec<Prompt> {
    CATALOG.iter().map(build_prompt_definition).collect()
}

/// Render a single [`Prompt`] definition from its static [`PromptDef`].
fn build_prompt_definition(def: &PromptDef) -> Prompt {
    let arguments: Vec<PromptArgument> = def
        .arguments
        .iter()
        .map(|(name, desc, required)| {
            PromptArgument::new((*name).to_string())
                .with_description((*desc).to_string())
                .with_required(*required)
        })
        .collect();
    Prompt::new(
        def.name.to_string(),
        Some(def.description.to_string()),
        Some(arguments),
    )
    .with_title(def.title.to_string())
}

/// Render a `prompts/get` response for `name` with the supplied
/// `args`. Returns a `method_not_found`-equivalent error for unknown
/// names and an `invalid_params` error for missing required arguments.
///
/// # Errors
///
/// - `invalid_request` when `name` is not in [`CATALOG`].
/// - `invalid_params` when a required argument is missing.
pub fn get_prompt(name: &str, args: &PromptArguments) -> Result<GetPromptResult, McpError> {
    let def = CATALOG
        .iter()
        .find(|d| d.name == name)
        .ok_or_else(|| McpError::invalid_request(format!("unknown prompt: {name}"), None))?;
    let text = render_prompt_text(def, args)?;
    Ok(GetPromptResult::new(vec![PromptMessage::new(
        PromptMessageRole::User,
        PromptMessageContent::text(text),
    )])
    .with_description(def.description.to_string()))
}

/// Build the parameterised prompt text for a single workflow.
fn render_prompt_text(def: &PromptDef, args: &PromptArguments) -> Result<String, McpError> {
    match def.name {
        "run_one_shot_command" => render_run_one_shot(args),
        "investigate_session" => render_investigate_session(args),
        "upload_and_verify" => render_upload_and_verify(args),
        "interactive_shell_drive" => render_interactive_shell_drive(args),
        "cleanup_agent" => render_cleanup_agent(args),
        "push_first_long_command" => render_push_first_long_command(args),
        "push_first_interactive_shell" => render_push_first_interactive_shell(args),
        "push_first_file_transfer" => render_push_first_file_transfer(args),
        "subscription_hygiene_audit" => render_subscription_hygiene_audit(args),
        "chaos_resume_after_disconnect" => render_chaos_resume(args),
        other => Err(McpError::invalid_request(
            format!("unknown prompt body: {other}"),
            None,
        )),
    }
}

/// Pull a required string argument by name, mapping `Value::Null` /
/// non-string variants to a structured `invalid_params` failure.
fn required_str<'a>(args: &'a PromptArguments, name: &str) -> Result<&'a str, McpError> {
    let value = args
        .get(name)
        .ok_or_else(|| McpError::invalid_params(format!("missing argument: {name}"), None))?;
    value
        .as_str()
        .ok_or_else(|| McpError::invalid_params(format!("argument {name} must be a string"), None))
}

fn render_run_one_shot(args: &PromptArguments) -> Result<String, McpError> {
    let address = required_str(args, "address")?;
    let username = required_str(args, "username")?;
    let command = required_str(args, "command")?;
    Ok(format!(
        "Run \"{command}\" on {address} as {username}. Use ssh_run with \
         reuse=auto and disconnect_after=true."
    ))
}

fn render_investigate_session(args: &PromptArguments) -> Result<String, McpError> {
    let session_id = required_str(args, "session_id")?;
    Ok(format!(
        "Investigate the SSH session {session_id}:\n\
         1. ssh_list_commands(session_id={session_id}) to see async commands\n\
         2. resources/read session://{session_id}/health for health snapshot\n\
         3. ssh_disconnect(session_id={session_id}) when finished."
    ))
}

fn render_upload_and_verify(args: &PromptArguments) -> Result<String, McpError> {
    let session_id = required_str(args, "session_id")?;
    let local_path = required_str(args, "local_path")?;
    let remote_path = required_str(args, "remote_path")?;
    Ok(format!(
        "Upload {local_path} to {remote_path} on session {session_id}:\n\
         1. ssh_upload(session_id={session_id}, local_path={local_path}, remote_path={remote_path}) -> TRANSFER_ID\n\
         2. ssh_get_transfer_progress(transfer_id=..., wait=true) until status=completed\n\
         3. ssh_run(... command=\"sha256sum {remote_path}\") to verify integrity."
    ))
}

fn render_interactive_shell_drive(args: &PromptArguments) -> Result<String, McpError> {
    let session_id = required_str(args, "session_id")?;
    let prompt_pattern = required_str(args, "prompt_pattern")?;
    Ok(format!(
        "Open an interactive shell on {session_id} and wait for prompt \"{prompt_pattern}\":\n\
         1. ssh_shell_open(session_id={session_id}) -> SHELL_ID\n\
         2. resources/subscribe shell://<SHELL_ID>/output\n\
         3. ssh_shell_wait_for(shell_id=..., patterns=[\"{prompt_pattern}\"], timeout_secs=10)\n\
         4. Drive with ssh_shell_write or ssh_shell_send_key."
    ))
}

fn render_cleanup_agent(args: &PromptArguments) -> Result<String, McpError> {
    let agent_id = required_str(args, "agent_id")?;
    Ok(format!(
        "Bulk-cleanup all sessions owned by agent \"{agent_id}\":\n\
         ssh_disconnect_agent(agent_id={agent_id})."
    ))
}

fn render_push_first_long_command(args: &PromptArguments) -> Result<String, McpError> {
    let session_id = required_str(args, "session_id")?;
    let command = required_str(args, "command")?;
    Ok(format!(
        "Run a long-running command on {session_id} with push-first output:\n\
         1. ssh_execute(session_id={session_id}, command=\"{command}\") -> COMMAND_ID\n\
         2. ssh_subscribe(uri=\"command://<COMMAND_ID>/output\", lifetime=\"manual\") -> SUB_ID\n\
         3. Drain command://<COMMAND_ID>/output via push notifications (no polling)\n\
         4. ssh_unsubscribe(sub_id=<SUB_ID>) when the command terminates."
    ))
}

fn render_push_first_interactive_shell(args: &PromptArguments) -> Result<String, McpError> {
    let session_id = required_str(args, "session_id")?;
    let prompt_pattern = required_str(args, "prompt_pattern")?;
    Ok(format!(
        "Drive an interactive PTY on {session_id} with push-first reads:\n\
         1. ssh_shell_open(session_id={session_id}, release_when_no_subs=true) -> SHELL_ID\n\
         2. ssh_subscribe(uri=\"shell://<SHELL_ID>/output\", filter=\"{prompt_pattern}\") -> SUB_ID\n\
         3. ssh_shell_send_key / ssh_shell_write while drains arrive on the lane\n\
         4. ssh_unsubscribe(sub_id=<SUB_ID>) and ssh_shell_close when finished."
    ))
}

fn render_push_first_file_transfer(args: &PromptArguments) -> Result<String, McpError> {
    let session_id = required_str(args, "session_id")?;
    let local_path = required_str(args, "local_path")?;
    let remote_path = required_str(args, "remote_path")?;
    Ok(format!(
        "Upload {local_path} -> {remote_path} on {session_id} with progress push:\n\
         1. ssh_upload(session_id={session_id}, local_path={local_path}, remote_path={remote_path}, release_when_no_subs=true) -> TRANSFER_ID\n\
         2. ssh_subscribe(uri=\"transfer://<TRANSFER_ID>/progress\") -> SUB_ID\n\
         3. Drain transfer://<TRANSFER_ID>/progress until status=completed\n\
         4. ssh_unsubscribe(sub_id=<SUB_ID>)."
    ))
}

fn render_subscription_hygiene_audit(args: &PromptArguments) -> Result<String, McpError> {
    // The dispatch signature must match the other renderers; reject a
    // non-string `uri_prefix` so the prompt body stays well-formed.
    let prefix = match args.get("uri_prefix") {
        Some(value) if !value.is_null() => value.as_str().ok_or_else(|| {
            McpError::invalid_params("argument uri_prefix must be a string".to_string(), None)
        })?,
        _ => "",
    };
    let prefix_clause = if prefix.is_empty() {
        "all schemes".to_string()
    } else {
        format!("uri_prefix={prefix}")
    };
    Ok(format!(
        "Audit and clean up zombie subscriptions ({prefix_clause}):\n\
         1. ssh_sub_list(uri_prefix={prefix})\n\
         2. For each entry tagged WARN: SUB_LEAK_RISK, call ssh_unsubscribe(sub_id=...)\n\
         3. ssh_daemon_stats to confirm lanes_total and lagged_drops are bounded."
    ))
}

fn render_chaos_resume(args: &PromptArguments) -> Result<String, McpError> {
    let sub_id = required_str(args, "sub_id")?;
    let cursor = match args.get("from_cursor") {
        Some(value) if !value.is_null() => value.as_u64().ok_or_else(|| {
            McpError::invalid_params(
                "argument from_cursor must be a non-negative integer".to_string(),
                None,
            )
        })?,
        _ => 0,
    };
    Ok(format!(
        "Recover subscription {sub_id} after a transport drop:\n\
         1. ssh_sub_replay(sub_id={sub_id}, from_cursor={cursor})\n\
         2. ssh_sub_resume(sub_id={sub_id}) if the lane was paused\n\
         3. ssh_sub_filter to refresh the regex when the input shape changed\n\
         4. ssh_sub_stats to verify queue_depth and lagged_drops are bounded."
    ))
}

#[cfg(test)]
mod tests {
    use super::{CATALOG, PromptArguments, get_prompt, list_prompts};
    use rmcp::model::PromptMessageContent;
    use serde_json::{Value, json};

    fn build_args(pairs: &[(&str, Value)]) -> PromptArguments {
        pairs
            .iter()
            .map(|(k, v)| ((*k).to_string(), v.clone()))
            .collect()
    }

    #[test]
    fn prompts_catalog_lists_ten_entries() {
        let prompts = list_prompts();
        assert_eq!(prompts.len(), 10);
        let names: Vec<&str> = prompts.iter().map(|p| p.name.as_str()).collect();
        for required in [
            "run_one_shot_command",
            "investigate_session",
            "upload_and_verify",
            "interactive_shell_drive",
            "cleanup_agent",
            "push_first_long_command",
            "push_first_interactive_shell",
            "push_first_file_transfer",
            "subscription_hygiene_audit",
            "chaos_resume_after_disconnect",
        ] {
            assert!(names.contains(&required), "missing prompt: {required}");
        }
    }

    #[test]
    fn each_prompt_carries_title_description_and_arguments() {
        for prompt in list_prompts() {
            let title = prompt.title.as_deref().unwrap_or_default();
            assert!(
                !title.is_empty(),
                "prompt {} must carry a non-empty title",
                prompt.name
            );
            assert!(
                prompt.description.is_some(),
                "prompt {} must carry a description",
                prompt.name
            );
            let arguments = prompt.arguments.as_deref().unwrap_or_default();
            assert!(
                !arguments.is_empty(),
                "prompt {} must carry at least one argument",
                prompt.name
            );
        }
    }

    #[test]
    fn get_prompt_substitutes_arguments() {
        let args = build_args(&[
            ("address", json!("h.example.com:22")),
            ("username", json!("alice")),
            ("command", json!("uptime")),
        ]);
        let result = get_prompt("run_one_shot_command", &args).expect("prompt body");
        let message = result.messages.first().expect("at least one message");
        let PromptMessageContent::Text { text } = &message.content else {
            panic!("expected text content");
        };
        assert!(text.contains("uptime"));
        assert!(text.contains("h.example.com:22"));
        assert!(text.contains("alice"));
        assert!(text.contains("ssh_run"));
        assert!(text.contains("disconnect_after=true"));
    }

    #[test]
    fn get_prompt_unknown_name_returns_invalid_request() {
        let args = PromptArguments::new();
        let err = get_prompt("nope", &args).expect_err("must fail");
        assert!(err.message.contains("unknown prompt"));
    }

    #[test]
    fn get_prompt_missing_required_argument_yields_invalid_params() {
        let args = build_args(&[("agent_id", Value::Null)]);
        let err = get_prompt("cleanup_agent", &args).expect_err("must fail");
        assert!(err.message.contains("agent_id"));
    }

    #[test]
    fn investigate_session_lists_three_steps() {
        let args = build_args(&[("session_id", json!("sess-x"))]);
        let result = get_prompt("investigate_session", &args).expect("body");
        let PromptMessageContent::Text { text } = &result.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("ssh_list_commands(session_id=sess-x)"));
        assert!(text.contains("session://sess-x/health"));
        assert!(text.contains("ssh_disconnect(session_id=sess-x)"));
    }

    #[test]
    fn cleanup_agent_renders_disconnect_agent_call() {
        let args = build_args(&[("agent_id", json!("claude-9"))]);
        let result = get_prompt("cleanup_agent", &args).expect("body");
        let PromptMessageContent::Text { text } = &result.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("ssh_disconnect_agent(agent_id=claude-9)"));
    }

    #[test]
    fn catalog_entries_match_listed_prompts() {
        // Sanity: listed prompts must mirror the static catalogue array.
        let listed: Vec<String> = list_prompts().into_iter().map(|p| p.name).collect();
        let catalogued: Vec<String> = CATALOG.iter().map(|d| d.name.to_string()).collect();
        assert_eq!(listed, catalogued);
    }

    // --- v5 Phase 3 push-first prompts -----------------------------

    #[test]
    fn push_first_long_command_renders_subscribe_step() {
        let args = build_args(&[
            ("session_id", json!("s-1")),
            ("command", json!("tail -f /var/log/app.log")),
        ]);
        let result = get_prompt("push_first_long_command", &args).expect("body");
        let PromptMessageContent::Text { text } = &result.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("ssh_execute(session_id=s-1"));
        assert!(text.contains("ssh_subscribe"));
        assert!(text.contains("command://<COMMAND_ID>/output"));
        assert!(text.contains("ssh_unsubscribe"));
    }

    #[test]
    fn push_first_interactive_shell_renders_release_when_no_subs() {
        let args = build_args(&[
            ("session_id", json!("s-2")),
            ("prompt_pattern", json!("$ ")),
        ]);
        let result = get_prompt("push_first_interactive_shell", &args).expect("body");
        let PromptMessageContent::Text { text } = &result.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("release_when_no_subs=true"));
        assert!(text.contains("ssh_subscribe"));
        assert!(text.contains("filter=\"$ \""));
    }

    #[test]
    fn push_first_file_transfer_renders_progress_subscribe() {
        let args = build_args(&[
            ("session_id", json!("s-3")),
            ("local_path", json!("/tmp/x")),
            ("remote_path", json!("/r/x")),
        ]);
        let result = get_prompt("push_first_file_transfer", &args).expect("body");
        let PromptMessageContent::Text { text } = &result.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("ssh_upload"));
        assert!(text.contains("transfer://<TRANSFER_ID>/progress"));
        assert!(text.contains("status=completed"));
    }

    #[test]
    fn subscription_hygiene_audit_supports_optional_prefix() {
        let mut args = PromptArguments::new();
        let result = get_prompt("subscription_hygiene_audit", &args).expect("body");
        let PromptMessageContent::Text { text } = &result.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("all schemes"));
        args.insert("uri_prefix".to_string(), json!("shell://"));
        let with_prefix = get_prompt("subscription_hygiene_audit", &args).expect("body");
        let PromptMessageContent::Text { text } = &with_prefix.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("uri_prefix=shell://"));
    }

    #[test]
    fn chaos_resume_uses_sub_id_and_optional_cursor() {
        let mut args = PromptArguments::new();
        args.insert("sub_id".to_string(), json!("019"));
        let result = get_prompt("chaos_resume_after_disconnect", &args).expect("body");
        let PromptMessageContent::Text { text } = &result.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("ssh_sub_replay(sub_id=019, from_cursor=0)"));
        args.insert("from_cursor".to_string(), json!(2_048));
        let with_cursor = get_prompt("chaos_resume_after_disconnect", &args).expect("body");
        let PromptMessageContent::Text { text } = &with_cursor.messages[0].content else {
            panic!("text expected");
        };
        assert!(text.contains("from_cursor=2048"));
    }
}
