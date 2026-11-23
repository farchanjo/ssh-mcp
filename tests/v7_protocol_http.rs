//! FR-004, FR-005, FR-006, FR-011: the HTTP transport negotiates each
//! requested revision, keeps protocol-level sessions, refuses the 2026-07-28
//! stateless lifecycle without `initialize`, and keeps the loopback-only
//! `Host` allow-list.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{Value, json};

const CEILING: &str = "2025-11-25";

fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind ephemeral port");
    listener.local_addr().expect("local addr").port()
}

struct Server {
    child: Child,
    port: u16,
}

impl Server {
    fn launch(rust_log: Option<&str>, log: Option<&std::path::Path>) -> Self {
        let port = free_port();
        let mut command = Command::new(env!("CARGO_BIN_EXE_ssh-mcp"));
        command
            .env("MCP_HOST", "127.0.0.1")
            .env("MCP_PORT", port.to_string())
            .stderr(Stdio::null());
        match log {
            Some(path) => {
                let file = std::fs::File::create(path).expect("create log");
                command.stdout(Stdio::from(file));
            }
            None => {
                command.stdout(Stdio::null());
            }
        }
        if let Some(value) = rust_log {
            command.env("RUST_LOG", value);
        } else {
            command.env_remove("RUST_LOG");
        }
        let child = command.spawn().expect("spawn ssh-mcp");
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            if TcpStream::connect(("127.0.0.1", port)).is_ok() {
                break Self { child, port };
            }
            assert!(
                Instant::now() < deadline,
                "ssh-mcp did not accept on port {port} within 15 s"
            );
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    fn start() -> Self {
        Self::launch(None, None)
    }

    fn start_logged(rust_log: Option<&str>) -> (Self, tempfile::TempDir, std::path::PathBuf) {
        let directory = tempfile::tempdir().expect("temp dir");
        let log = directory.path().join("server.log");
        (Self::launch(rust_log, Some(&log)), directory, log)
    }

    fn request(
        &self,
        method: &str,
        body: Option<&Value>,
        host: &str,
        session: Option<&str>,
        mcp_method: Option<&str>,
    ) -> HttpResponse {
        let payload = body
            .map(std::string::ToString::to_string)
            .unwrap_or_default();
        let mut request = format!("{method} / HTTP/1.1\r\nHost: {host}\r\n");
        if body.is_some() {
            request.push_str(
                "Content-Type: application/json\r\nAccept: application/json, text/event-stream\r\n",
            );
        }
        request.push_str(&format!("Content-Length: {}\r\n", payload.len()));
        request.push_str("Connection: close\r\n");
        if let Some(id) = session {
            request.push_str(&format!("Mcp-Session-Id: {id}\r\n"));
        }
        if let Some(mcp) = mcp_method {
            request.push_str(&format!(
                "Mcp-Method: {mcp}\r\nMCP-Protocol-Version: 2026-07-28\r\n"
            ));
        }
        request.push_str("\r\n");
        request.push_str(&payload);

        let mut stream = TcpStream::connect(("127.0.0.1", self.port)).expect("connect");
        stream
            .set_read_timeout(Some(Duration::from_secs(10)))
            .expect("read timeout");
        stream.write_all(request.as_bytes()).expect("write request");
        let mut raw = Vec::new();
        stream.read_to_end(&mut raw).expect("read response");
        HttpResponse::parse(&raw)
    }

    fn get(&self, host: &str) -> HttpResponse {
        self.request("GET", None, host, None, None)
    }

    fn post(
        &self,
        body: &Value,
        host: &str,
        session: Option<&str>,
        method: Option<&str>,
    ) -> HttpResponse {
        self.request("POST", Some(body), host, session, method)
    }

    fn loopback(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

struct HttpResponse {
    status: u16,
    session: Option<String>,
    body: String,
}

impl HttpResponse {
    fn parse(raw: &[u8]) -> Self {
        let split = raw
            .windows(4)
            .position(|w| w == b"\r\n\r\n")
            .expect("header end");
        let head = String::from_utf8_lossy(&raw[..split]).to_string();
        let raw_body = &raw[split + 4..];
        let body = if head
            .to_ascii_lowercase()
            .contains("transfer-encoding: chunked")
        {
            dechunk(raw_body)
        } else {
            String::from_utf8_lossy(raw_body).to_string()
        };
        let mut lines = head.split("\r\n");
        let status = lines
            .next()
            .and_then(|line| line.split_whitespace().nth(1))
            .and_then(|code| code.parse().ok())
            .expect("status code");
        let mut session = None;
        for line in lines {
            if let Some((name, value)) = line.split_once(':')
                && name.eq_ignore_ascii_case("mcp-session-id")
            {
                session = Some(value.trim().to_string());
            }
        }
        Self {
            status,
            session,
            body,
        }
    }

    fn json(&self) -> Value {
        if let Ok(value) = serde_json::from_str(&self.body) {
            return value;
        }
        let mut last = None;
        for line in self.body.lines() {
            if let Some(payload) = line.strip_prefix("data: ")
                && let Ok(value) = serde_json::from_str::<Value>(payload)
                && (value.get("result").is_some() || value.get("error").is_some())
            {
                last = Some(value);
            }
        }
        last.expect("JSON body")
    }
}

fn dechunk(raw: &[u8]) -> String {
    let mut out = Vec::new();
    let mut rest = raw;
    loop {
        let Some(end) = rest.windows(2).position(|w| w == b"\r\n") else {
            break;
        };
        let Ok(size) = usize::from_str_radix(
            std::str::from_utf8(&rest[..end])
                .expect("chunk size")
                .trim(),
            16,
        ) else {
            break;
        };
        if size == 0 {
            break;
        }
        let start = end + 2;
        out.extend_from_slice(&rest[start..start + size]);
        rest = &rest[start + size..];
        if rest.starts_with(b"\r\n") {
            rest = &rest[2..];
        }
    }
    String::from_utf8_lossy(&out).to_string()
}

fn initialize_body(version: &str) -> Value {
    json!({
        "jsonrpc": "2.0",
        "id": 1,
        "method": "initialize",
        "params": {
            "protocolVersion": version,
            "capabilities": {},
            "clientInfo": {"name": "protocol-http-test", "version": "0.0.0"},
        },
    })
}

#[test]
fn initialize_answers_each_revision_and_returns_a_session() {
    let server = Server::start();
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
        let response = server.post(&initialize_body(asked), &server.loopback(), None, None);
        assert_eq!(response.status, 200, "asked {asked}");
        assert!(
            response.session.is_some(),
            "initialize must return Mcp-Session-Id for {asked}"
        );
        assert_eq!(
            response.json()["result"]["protocolVersion"],
            json!(expected),
            "asked {asked}"
        );
    }
}

#[test]
fn a_returned_session_is_served_and_a_made_up_id_is_refused() {
    let server = Server::start();
    let init = server.post(&initialize_body(CEILING), &server.loopback(), None, None);
    let session = init.session.expect("session id");
    let list = json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"});
    let served = server.post(&list, &server.loopback(), Some(&session), None);
    assert_eq!(served.status, 200);
    let tools = served.json()["result"]["tools"]
        .as_array()
        .cloned()
        .expect("tools array");
    assert!(!tools.is_empty(), "tools/list must return tools");
    let refused = server.post(&list, &server.loopback(), Some("made-up-session"), None);
    assert_eq!(refused.status, 404);
}

#[test]
fn a_non_loopback_host_is_refused_and_a_loopback_host_is_served() {
    let server = Server::start();
    let refused = server.post(&initialize_body(CEILING), "example.com", None, None);
    assert_eq!(refused.status, 403);
    assert_eq!(refused.body, "Forbidden: Host header is not allowed");
    let served = server.post(&initialize_body(CEILING), &server.loopback(), None, None);
    assert_eq!(served.status, 200);
}

#[test]
fn discover_without_initialize_is_refused_with_unsupported_version() {
    let server = Server::start();
    let body = json!({
        "jsonrpc": "2.0",
        "id": 3,
        "method": "server/discover",
        "params": {"_meta": {
            "io.modelcontextprotocol/protocolVersion": "2026-07-28",
            "io.modelcontextprotocol/clientCapabilities": {},
        }},
    });
    let response = server.post(&body, &server.loopback(), None, Some("server/discover"));
    assert_eq!(response.status, 400);
    let json = response.json();
    assert_eq!(json["error"]["code"], json!(-32022));
    assert_eq!(json["error"]["data"]["requested"], json!("2026-07-28"));
    assert!(
        json["error"]["data"]["supported"]
            .as_array()
            .expect("supported array")
            .contains(&json!(CEILING))
    );
}

fn strip_ansi(input: &str) -> String {
    let mut out = String::new();
    let mut chars = input.chars();
    while let Some(character) = chars.next() {
        if character == '\u{1b}' {
            for next in chars.by_ref() {
                if next == 'm' {
                    break;
                }
            }
        } else {
            out.push(character);
        }
    }
    out
}

fn trace_lines(log: &std::path::Path) -> Vec<String> {
    let text = strip_ansi(&std::fs::read_to_string(log).expect("read log"));
    text.lines()
        .filter(|line| line.contains("tower_http::trace"))
        .map(str::to_string)
        .collect()
}

fn wait_for_lines(log: &std::path::Path, marker: &str, expected: usize) -> Vec<String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let text = strip_ansi(&std::fs::read_to_string(log).unwrap_or_default());
        let lines: Vec<String> = text
            .lines()
            .filter(|line| line.contains(marker))
            .map(str::to_string)
            .collect();
        if lines.len() >= expected {
            return lines;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for {expected} line(s) containing {marker:?}; log:\n{text}"
        );
        std::thread::sleep(Duration::from_millis(50));
    }
}

#[test]
fn trace_layer_emits_six_debug_lines_for_two_responses_and_none_under_info() {
    let (server, directory, log) = Server::start_logged(Some("debug"));
    assert_eq!(server.get(&server.loopback()).status, 406);
    assert_eq!(
        server
            .post(&initialize_body(CEILING), &server.loopback(), None, None)
            .status,
        200
    );
    let lines = wait_for_lines(&log, "tower_http::trace", 6);
    assert_eq!(lines.len(), 6, "trace log:\n{lines:#?}");
    assert!(lines[0].contains("tower_http::trace::on_request"));
    assert!(lines[1].contains("tower_http::trace::on_response"));
    assert!(lines[1].contains("status=406"));
    assert!(lines[2].contains("tower_http::trace::on_eos"));
    assert!(lines[3].contains("tower_http::trace::on_request"));
    assert!(lines[4].contains("tower_http::trace::on_response"));
    assert!(lines[4].contains("status=200"));
    assert!(lines[5].contains("tower_http::trace::on_eos"));
    drop(server);
    drop(directory);

    let (server, directory, log) = Server::start_logged(None);
    assert_eq!(server.get(&server.loopback()).status, 406);
    assert_eq!(
        server
            .post(&initialize_body(CEILING), &server.loopback(), None, None)
            .status,
        200
    );
    wait_for_lines(&log, "ssh-mcp listening on", 1);
    let trace = trace_lines(&log);
    assert!(trace.is_empty(), "unexpected trace lines: {trace:#?}");
    drop(server);
    drop(directory);
}
