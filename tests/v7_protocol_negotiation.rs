//! FR-004, FR-006, FR-011: the `initialize` answer is fixed per requested
//! revision — a known revision answers itself, an unknown one and
//! `2026-07-28` answer the `2025-11-25` ceiling — the capability set is
//! frozen, and a discover client without `initialize` is refused.

use std::io::{BufRead, BufReader, Write};
use std::process::{Child, Command, Stdio};

use serde_json::{Value, json};

const CEILING: &str = "2025-11-25";

fn spawn() -> Child {
    Command::new(env!("CARGO_BIN_EXE_ssh-mcp-stdio"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn ssh-mcp-stdio")
}

fn request(message: Value, id: u64) -> Value {
    let mut child = spawn();
    let mut stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    writeln!(stdin, "{message}").expect("write request");
    stdin.flush().expect("flush request");
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    let response = loop {
        line.clear();
        let read = reader.read_line(&mut line).expect("read response");
        assert_ne!(read, 0, "server closed stdout before answering");
        let Ok(message) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        if message.get("id") == Some(&json!(id)) {
            break message;
        }
    };
    let _ = child.kill();
    let _ = child.wait();
    response
}

fn initialize_body(asked: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": asked,
            "capabilities": {},
            "clientInfo": {"name": "protocol-test", "version": "0.0.0"},
        },
    })
}

fn initialize(asked: &str) -> Value {
    request(initialize_body(asked), 1)
}

fn expected_capabilities() -> Value {
    json!({
        "tools": {"listChanged": true},
        "resources": {"subscribe": true, "listChanged": true},
        "prompts": {},
        "experimental": {"ssh_inline_push": {"version": 1}},
    })
}

#[test]
fn initialize_answers_each_revision() {
    for (asked, expected) in [
        ("2024-11-05", "2024-11-05"),
        ("2025-03-26", "2025-03-26"),
        ("2025-06-18", "2025-06-18"),
        ("2025-11-25", "2025-11-25"),
        ("2026-07-28", CEILING),
        ("2024-10-07", CEILING),
        ("1999-01-01", CEILING),
        ("2027-01-01", CEILING),
        ("garbage", CEILING),
    ] {
        let response = initialize(asked);
        let result = response.get("result").expect("initialize result");
        assert_eq!(
            result.get("protocolVersion"),
            Some(&json!(expected)),
            "asked {asked}"
        );
        assert_eq!(
            result.get("capabilities"),
            Some(&expected_capabilities()),
            "asked {asked}"
        );
    }
}

#[test]
fn discover_without_initialize_is_refused_with_unsupported_version() {
    let response = request(
        json!({
            "jsonrpc": "2.0",
            "id": 9,
            "method": "server/discover",
            "params": {"_meta": {
                "io.modelcontextprotocol/protocolVersion": "2026-07-28",
                "io.modelcontextprotocol/clientCapabilities": {},
            }},
        }),
        9,
    );
    assert_eq!(response["error"]["code"], json!(-32022));
    assert_eq!(response["error"]["data"]["requested"], json!("2026-07-28"));
    assert!(
        response["error"]["data"]["supported"]
            .as_array()
            .expect("supported array")
            .contains(&json!(CEILING))
    );
}
