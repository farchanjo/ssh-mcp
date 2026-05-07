# SSH MCP Server

This project ships a subscribe-first **Model Context Protocol (MCP) server** that lets any MCP-capable LLM host drive remote shells, async commands, SFTP transfers, **rsync v31 wire-compat sync**, TCP port forwards, and local UART/TTY/COM ports — over a single, lock-free, hexagonal Rust core. This server is maintained by [@farchanjo](https://github.com/farchanjo).

## Contents

- [SSH MCP Server](#ssh-mcp-server)
  - [Contents](#contents)
  - [Description](#description)
  - [Requirements](#requirements)
    - [Rust version compatibility](#rust-version-compatibility)
    - [Operating system compatibility](#operating-system-compatibility)
    - [Crate dependencies](#crate-dependencies)
    - [External requirements](#external-requirements)
  - [Included content](#included-content)
    - [Binaries](#binaries)
    - [MCP tools](#mcp-tools)
    - [Push resource schemes](#push-resource-schemes)
  - [Testing](#testing)
  - [Installation](#installation)
  - [Support](#support)
  - [Release notes](#release-notes)
  - [More information](#more-information)
  - [License Information](#license-information)

## Description

This server enables any MCP host (Claude Desktop, Claude Code, Cline, IDE plugins, mcp-inspector, custom agents) to interact with SSH targets through automation. [MCP](https://modelcontextprotocol.io/) is a standardized protocol for communication between AI systems and external tools or data sources.

The server provides tools to open persistent SSH sessions, run async commands, drive interactive PTYs, transfer files via SFTP (with resume + verify), mirror directory trees through a built-in rsync wire-protocol v31 client (byte-identical against `rsync 3.2.7`), bind local TCP forwards, and talk to local serial ports — all streamed back to the model as MCP `notifications/resources/updated` push events the moment bytes arrive. No polling loops, no empty payloads, no wasted tokens.

Architecture is **hexagonal** ([ADR 0002](docs/adr/0002-adopt-hexagonal-architecture.md)) with **lock-free hot paths** (`DashMap`, `ArcSwap`, `Atomic*`, `tokio::sync::broadcast`, `mpsc` — zero `Mutex` on `RunningCommand` / `RunningShell` / `RunningTransfer` / `SessionRef` / `ForwardHandle` / lane state). Every long-lived resource carries a CAS state machine (`Owned → Observed → Releasing → Closed`) plus per-session refcount cascade ([ADR 0003](docs/adr/0003-lifecycle-binding.md)). Each `resources/subscribe` mints a `SubId` (UUIDv7) with its own `mpsc::channel`, lag policy, filter pipeline, replay buffer, and stats ([ADR 0004](docs/adr/0004-channel-mux-fairness.md)).

Full module map and design rationale: [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Requirements

### Rust version compatibility

This server has been tested against the following Rust versions: **>=1.95.0** (Rust 2024 edition, resolver 3).

### Operating system compatibility

| OS | Server runtime | SSH/SFTP target | Serial transport |
| --- | --- | --- | --- |
| Linux x86_64 / aarch64 | tier 1 | tier 1 | tier 1 |
| macOS arm64 / x86_64 | tier 1 | tier 1 | tier 1 |
| Windows x86_64 | tier 2 (HTTP + stdio binaries build) | tier 1 | tier 2 (`COM*` ports) |

### Crate dependencies

This server depends on the following Rust crates (top-level — full transitive set in `Cargo.lock`):

- `rmcp` 1.6 — MCP protocol
- `russh` — SSH/SFTP client
- `axum` 0.8 — HTTP transport
- `tokio` — async runtime
- `dashmap`, `arc-swap` — lock-free state
- `tracing`, `serde`, `thiserror`, `backon`, `uuid` (UUIDv7)

### External requirements

| Requirement | When required | Notes |
| --- | --- | --- |
| OpenSSH server (or compatible) | always | tested against OpenSSH 7.6+ (Linux, macOS, BSD); `MaxSessions` budget respected |
| SFTP subsystem | for `ssh_upload` / `ssh_download` / `ssh_rsync transport=Sftp` | enabled by default on OpenSSH |
| `rsync ≥ 3.2.0` on remote | for `ssh_rsync transport=Wire` | falls back to `Sftp` automatically when missing |
| Local serial device (`/dev/ttyUSB*`, `/dev/tty.usbserial*`, `COM*`) | for `serial_*` tools | no SSH needed |

## Included content

### Binaries

Name | Description
--- | ---
[`ssh-mcp`](src/main.rs) | HTTP transport (axum 0.8 + rmcp StreamableHttpService). Default bind `0.0.0.0:8000`, path `/`. Tracks sessions through `Mcp-Session-Id` header.
[`ssh-mcp-stdio`](src/bin/ssh_mcp_stdio.rs) | Stdio MCP transport (`rmcp::transport::io::stdio()`). For Claude Desktop / Cline / mcp-inspector / IDE plugins.
[`ssh-mcp-tail`](src/bin/ssh_mcp_tail.rs) | NDJSON daemon over stdin/stdout. Three subcommands (`run`, `shell`, `daemon`). Unix-pipeline composable. Reference: [docs/DAEMON.md](docs/DAEMON.md).

### MCP tools

39 tools (38 without the `port_forward` feature) split across three semantic axes. Catalogue and per-tool schemas: [docs/API.md](docs/API.md).

Axis | Count | Description
--- | --- | ---
`ssh_*` | 24 | Operations over SSH — connect, exec (async + sync), shell PTY, upload/download, rsync, port forward, agent management. See [docs/API.md → Connection / Execute / Shell / SFTP / Rsync / Forward](docs/API.md).
`sub_*` | 9 | Subscription lane management — `sub_open`, `sub_close`, `sub_pause/resume/filter/replay/list/stats`, `sub_stats_all`. Cross-resource. See [docs/API.md → Subscription administration](docs/API.md).
`serial_*` | 6 | Local UART / TTY / COM ports — no SSH. `serial_open/close/write/press/scan/active`. See [docs/API.md → Serial](docs/API.md).

### Push resource schemes

7 push-capable resource schemes. Subscribe with `sub_open uri=<scheme>://<id>/<channel>` (or legacy `resources/subscribe`). Full contract: [docs/RESOURCES.md](docs/RESOURCES.md).

Scheme | Channel | Producer
--- | --- | ---
`shell://<SHELL_ID>/output` | PTY stdout | russh shell channel
`command://<COMMAND_ID>/output` | merged stdout/stderr | russh exec channel
`transfer://<TRANSFER_ID>/progress` | bytes/sec + ETA | SFTP upload/download
`rsync://<RSYNC_ID>/progress` | per-file events + `SyncCompleted` | `WireRsyncTransport` / `SftpRsyncTransport`
`forward://<FORWARD_ID>/events` | TCP forward lifecycle | `port_forward` feature
`session://<SESSION_ID>/health` | keepalive + RTT | session reaper
`serial://<SERIAL_ID>/output` | UART RX | local serial port

## Testing

This server is tested using a layered local + CI gate. To learn more about testing, refer to [CI.md](CI.md).

Quick numbers — **1966 lib tests** + **134 integration tests** across 9 binaries + **27 loom invariants** + **8 e2e VM tests** (gated `e2e-vm`) + **21 Python integration tests** for the v7.0 `ssh_rsync` MCP surface (`scripts/test_v7_rsync_*.py`).

## Installation

```bash
git clone https://github.com/farchanjo/ssh-mcp.git
cd ssh-mcp
cargo build --release
sudo install -m 0755 target/release/ssh-mcp{,-stdio,-tail} /usr/local/bin/
```

Three binaries — pick the transport that matches your host:

```bash
# Local hosts (Claude Desktop, Cline, mcp-inspector, IDE plugins)
/usr/local/bin/ssh-mcp-stdio

# HTTP hosts (browser- or service-based)
/usr/local/bin/ssh-mcp --bind 0.0.0.0:8000 --path /

# Hosts without resources/subscribe; Unix pipelines (jq, vector, fluent-bit)
/usr/local/bin/ssh-mcp-tail daemon
```

Skip TCP forwarding via `cargo build --release --no-default-features` (38 tools, smaller binary).

You can also wire ssh-mcp into an MCP host config file (Claude Desktop, Cline, etc.):

```json
{
  "mcpServers": {
    "ssh": {
      "type": "stdio",
      "command": "/usr/local/bin/ssh-mcp-stdio"
    }
  }
}
```

To upgrade to the latest available version, pull and rebuild:

```bash
git -C ssh-mcp pull --ff-only
cargo build --release
sudo install -m 0755 target/release/ssh-mcp{,-stdio,-tail} /usr/local/bin/
```

You can also pin a specific version, for example, if you need to downgrade when something is broken in the latest version (please report an issue in this repository). Use the following syntax where `X.Y.Z` is any [released tag](https://github.com/farchanjo/ssh-mcp/releases):

```bash
git -C ssh-mcp checkout vX.Y.Z
cargo build --release
```

See [docs/OPERATIONS.md](docs/OPERATIONS.md) for production deployment, systemd units, Docker, and observability wiring.

## Support

For support and questions about this server:

- **Issues**: Report bugs or request features via [GitHub Issues](https://github.com/farchanjo/ssh-mcp/issues)
- **Discussions**: Open a thread under [GitHub Discussions](https://github.com/farchanjo/ssh-mcp/discussions)
- **Security**: Send security reports to the maintainer listed in [MAINTAINERS](MAINTAINERS) (do **not** open a public issue for embargoed disclosures)

## Release notes

See the [changelog](CHANGELOG.md).

## More information

- [MCP Documentation](https://modelcontextprotocol.io/)
- [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) — hexagonal layer map, lifecycle adapter, channel mux
- [docs/API.md](docs/API.md) — every MCP tool, schema, response shape
- [docs/RESOURCES.md](docs/RESOURCES.md) — push resource contracts and cursor semantics
- [docs/CONFIGURATION.md](docs/CONFIGURATION.md) — full env-var matrix (40 settings)
- [docs/LLM_GUIDE.md](docs/LLM_GUIDE.md) — push-first prompts, error handbook, hint escalation
- [docs/DAEMON.md](docs/DAEMON.md) — `ssh-mcp-tail` NDJSON wire format
- [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) — lock-free invariants, contributor narrative
- [docs/MIGRATION.md](docs/MIGRATION.md) — host migration guide across versions
- [docs/OPERATIONS.md](docs/OPERATIONS.md) — production runbook
- [docs/adr/](docs/adr/) — 11 ADRs (rmcp adoption, hexagonal, lifecycle, channel mux, LLM UX, backpressure, error taxonomy, NDJSON daemon, serial, SFTP resume, rsync hybrid)
- [CONTRIBUTING.md](CONTRIBUTING.md) — contributor guide
- [CODE_OF_CONDUCT.md](CODE_OF_CONDUCT.md) — community code of conduct
- [REVIEW_CHECKLIST.md](REVIEW_CHECKLIST.md) — PR review checklist
- [MAINTAINERS](MAINTAINERS) — current maintainers

## License Information

MIT License.

See [LICENSE](LICENSE) for the full text. Third-party portions of the rsync wire transport are derived from OpenBSD `openrsync` and retain the original ISC license — see [LICENSES/openrsync-ISC.txt](LICENSES/openrsync-ISC.txt).
