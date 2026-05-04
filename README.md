<div align="center">

# ssh-mcp

**Real-time SSH for the Model Context Protocol.**

Drive remote shells, asynchronous commands, SFTP transfers, and TCP forwards from any MCP-capable LLM host. Output streams to your model the moment SSH bytes arrive — no polling loops, no empty payloads, no token waste.

[![Version](https://img.shields.io/badge/version-5.2.0-1f6feb?style=flat-square)](https://github.com/farchanjo/ssh-mcp/releases/tag/v5.2.0)
[![Rust](https://img.shields.io/badge/rust-2024%20%E2%80%94%20MSRV%201.95-orange?style=flat-square)](https://www.rust-lang.org/)
[![License: MIT](https://img.shields.io/badge/License-MIT-238636?style=flat-square)](Cargo.toml)
[![MCP](https://img.shields.io/badge/MCP-2025--06--18-a371f7?style=flat-square)](https://modelcontextprotocol.io/)
[![Architecture](https://img.shields.io/badge/architecture-hexagonal-a371f7?style=flat-square)](docs/ARCHITECTURE.md)
[![Lock-free](https://img.shields.io/badge/hot--path-lock--free-238636?style=flat-square)](docs/DEVELOPMENT.md#lock-free-invariants)
[![Tests](https://img.shields.io/badge/lib%20tests-1633-238636?style=flat-square)]()
[![Code-signed](https://img.shields.io/badge/macOS-codesigned-238636?style=flat-square)]()

</div>

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    LLM(["LLM host"])
    SM["ssh-mcp"]
    REM(["Remote SSH host"])

    LLM <-->|MCP tools + push streams| SM
    SM <-->|encrypted SSH 2.0 + SFTP| REM

    classDef host fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    classDef core fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    classDef remote fill:#21262d,color:#8b949e,stroke:#30363d
    class LLM host
    class SM core
    class REM remote
```

---

## What it does

`ssh-mcp` is a single-binary bridge that lets a Large Language Model open SSH sessions, run commands, drive interactive shells (vim, htop, REPLs), upload and download files via SFTP, and tunnel TCP ports — all through the [Model Context Protocol](https://modelcontextprotocol.io/) standard.

The novel idea is **subscribe-first delivery**: instead of asking the model to poll a tool every second to see if a command finished, the server pushes new output to the model the moment it arrives from the remote host. The model spends tokens only on real work.

## Why it matters

Most LLM-driven SSH wrappers fall back to polling. The model issues `get_output()` in a loop, the server returns either empty payloads or duplicate data, and the conversation context fills up with noise. For a five-minute build, the model can burn 30 KB of tokens watching nothing happen.

`ssh-mcp` flips that. When the LLM calls `ssh_subscribe`, the server opens a dedicated push lane. New stdout, stderr, transfer progress, port-forward events, and session health updates flow as MCP `notifications/resources/updated` messages — natural to read, free of empty noise, and delivered in real time.

| Pattern | Token cost (5-min build) | Notes |
|---|---|---|
| Poll every 1 s with `get_output(wait=true)` | ~45 000 tokens | 300 round-trips, most carrying no new bytes. |
| Poll every 5 s with `get_output(wait=true)` | ~9 000 tokens | Misses bursty output entirely. |
| **Subscribe + drain** | **~1 500 tokens** | One setup call; events delivered as bytes arrive. |

Same throughput, ~30× cheaper, and the model reacts the moment the remote process speaks.

## What's new in 5.2.0

ADR 0009 — native serial / UART / TTY / COM transport. No wire-format changes for non-serial workflows; drop-in for any v3 / v4 / v5.0.x / v5.1 host.

- **New `serial://<id>/output` push scheme.** Open a UART (`/dev/ttyUSB0`, `/dev/tty.usbserial-XXXX`, `COM3`, …) directly from the LLM host — no SSH gateway required. Subscribe with `ssh_subscribe uri=serial://<SERIAL_ID>/output`; push delivery uses the exact same debouncer + 64 KiB byte-threshold flush as `shell://*/output` and `command://*/output`.
- **6 new MCP tools.** `ssh_serial_open` (full `stty` parameter coverage: baud / data / stop / parity / flow + DTR / RTS / max_buffer / label), `ssh_serial_close` (idempotent), `ssh_serial_write` (UTF-8 `text` OR base64 `bytes_base64`), `ssh_serial_send_key` (`enter`/`cr`/`lf`/`crlf`/`esc`/`tab`/`backspace`/`ctrl_c`/`ctrl_d`/`ctrl_z` × `repeat` 1..=64), `ssh_serial_list_ports`, `ssh_serial_list_open`.
- **Lock-free reader / writer split.** History on `ArcSwap<RingBuffer>` (`O(1)` snapshot reads, no blocking); writes funnel through a bounded `mpsc::channel(64)` so a slow remote sink fills the queue first and surfaces `SERIAL_BACKPRESSURE` instead of stalling the request. Subscribers never contend with the OS-level serial reader.
- **Tool surface bumps** to **36** (with `port_forward`) / **35** (without). Existing v3 / v4 / v5 tools and resource schemes unchanged.
- **Inherits 5.1.0 byte-threshold flush.** A 921 600 baud firmware dump pushes its first 64 KiB chunk in ~75 ms instead of waiting the full debounce window — perfect for chatty UARTs (GPS NMEA, RS-485 sensor stream, debug console).

Full notes: [CHANGELOG.md → 5.2.0](CHANGELOG.md#520--2026-05-04). Design rationale: [docs/adr/0009-serial-transport.md](docs/adr/0009-serial-transport.md).

## What's new in 5.1.0

ADR 0006 Amendment 1 wired end-to-end — no wire-format changes, drop-in for any v3 / v4 / v5.0.x host.

- **Byte-threshold debouncer flush** (`SSH_NOTIFY_FLUSH_BYTES`, default `64k`). The per-resource debouncer now flushes immediately whenever the bytes-since-last-broadcast counter crosses the threshold, even when the time window has not yet expired. Hooked on `command://*/output` (stdout / stderr per-chunk delta) and `transfer://*/progress` (per-SFTP-chunk delta). Set to `0` to disable and revert to v5.0 time-only behaviour. Accepts bare bytes (`65536`) or bytesize strings (`8k`, `64k`, `1m`, `1mib`).
- **`ssh_subscribe` HINT line** now spells out both knobs: "Push fires on whichever fires first: ~200ms debounce window (`SSH_NOTIFY_DEBOUNCE_MS`) OR 64KiB accumulated bytes (`SSH_NOTIFY_FLUSH_BYTES`). 50-200ms local sleeps cover both budgets."
- **Local-sleep guidance** — wire HINT explicitly steers the LLM to wait passively for `notifications/resources/updated` and use Unix `sleep 0.1` / PowerShell `Start-Sleep -Milliseconds 100` for any local idling. Never an MCP tool as a sleep primitive.
- **Inherits 5.0.2 LLM-UX nudges** — `ssh_run` reframed as `PENALIZED FALLBACK`, shell drive ops steered to await push instead of chaining `ssh_shell_wait_for`.

Full notes: [CHANGELOG.md → 5.1.0](CHANGELOG.md#510--2026-05-04).

## How real-time push actually works

This is the core feature. It is built from boring, well-understood pieces stacked carefully.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart TB
    REMOTE(["Remote process<br/>(tail -f, build, REPL)"])
    SOCK["TCP socket<br/>(file descriptor)"]
    KERNEL["Kernel readiness<br/>epoll · kqueue · IOCP<br/>edge-triggered, O(1) wake"]
    MIO["mio<br/>cross-platform abstraction"]
    REACTOR["tokio reactor +<br/>work-stealing scheduler"]
    RUSSH["russh<br/>SSH 2.0 frame parser<br/>per-channel mpsc"]
    LIFECYCLE["Lifecycle adapter<br/>ArcSwap RingBuffer<br/>atomic CAS state"]
    DEBOUNCE["Debouncer<br/>200 ms coalesce<br/>1 s force-flush"]
    LANE["Per-subscription lane<br/>tokio mpsc(N)<br/>filter · replay · lag policy"]
    MUX["ChannelMux<br/>AtomicUsize cursor<br/>round-robin fairness"]
    OUT["Outbound writer<br/>rmcp Peer<br/>or NDJSON stdout"]
    LLM(["LLM host context"])

    REMOTE --> SOCK
    SOCK --> KERNEL
    KERNEL --> MIO
    MIO --> REACTOR
    REACTOR --> RUSSH
    RUSSH --> LIFECYCLE
    LIFECYCLE --> DEBOUNCE
    DEBOUNCE --> LANE
    LANE --> MUX
    MUX --> OUT
    OUT --> LLM

    classDef remote fill:#21262d,color:#8b949e,stroke:#30363d
    classDef kernel fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
    classDef rt fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    classDef mux fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    classDef host fill:#238636,color:#f0f6fc,stroke:#2ea043
    class REMOTE,SOCK remote
    class KERNEL,MIO kernel
    class REACTOR,RUSSH,LIFECYCLE,DEBOUNCE rt
    class LANE,MUX out
    class OUT,LLM host
```

### Layer by layer (junior-friendly)

**1. Kernel readiness (`epoll` on Linux, `kqueue` on macOS and BSD, `IOCP` on Windows).**
The kernel offers a way to say "wake me up when this socket has data" without checking it in a loop. Your process sleeps, costs zero CPU, and the kernel sends an interrupt the moment data arrives. This is the hardware-assisted version of "don't call us, we'll call you."

**2. `mio` and the `tokio` reactor.**
`mio` is a small Rust crate that hides the differences between `epoll`, `kqueue`, and `IOCP` behind one API. `tokio` is the async runtime — its reactor calls `mio` to wait for ready sockets, then wakes whichever async task was paused on `.await`. The result: hundreds of suspended tasks share a handful of OS threads with no busy-waiting.

**3. `russh` (SSH protocol).**
SSH already supports many channels over one TCP connection — that's part of the protocol (RFC 4254). `russh` parses SSH frames asynchronously and dispatches per-channel data to in-memory queues. One TCP socket can carry your shell, your background command, your file transfer, and your tunnel simultaneously. The OS sees one connection; your model sees four push streams.

**4. Lifecycle adapter.**
For each long-lived resource (shell, command, transfer, forward), the server keeps an atomic state machine and a reference counter. No mutex on the hot path — only `AtomicU8`, `AtomicUsize`, `ArcSwap`, and `Notify`. When the last subscriber disconnects, a grace timer arms; if a new subscriber arrives within the window, the timer cancels. If not, the resource is released and the channel closed. No leaks, no zombies.

**5. Debouncer.**
A burst of one thousand tiny writes from the remote host could become one thousand notifications. Instead, the debouncer coalesces them into one push every 200 ms (force-flushed every 1 s for liveness). The model receives meaningful chunks, not a token-stream of fragments.

**6. Per-subscription lanes (the multiplexing magic).**
Every `ssh_subscribe` call gets its own bounded `tokio::sync::mpsc::channel` — its own queue, its own filter, its own replay buffer, its own lag policy. Three subscribers to the same shell are three independent lanes. A slow LLM does not slow down a fast one. Each lane chooses how to react to backpressure: drop the oldest event, send a snapshot and resume, or disconnect a misbehaving consumer.

```mermaid
%%{init: {'theme':'dark','themeVariables':{'primaryColor':'#1f6feb','primaryTextColor':'#f0f6fc','primaryBorderColor':'#388bfd','lineColor':'#8b949e','secondaryColor':'#161b22','tertiaryColor':'#21262d','background':'#0d1117','mainBkg':'#161b22','secondBkg':'#21262d','tertiaryBkg':'#0d1117','nodeTextColor':'#f0f6fc','edgeLabelBackground':'#21262d','clusterBkg':'#161b22','clusterBorder':'#30363d','titleColor':'#f0f6fc'}}}%%
flowchart LR
    SRC["SSH stdout<br/>burst"]
    DEB["debouncer<br/>200 ms window"]
    LA["Lane A<br/>filter='ERROR'<br/>policy=DropOldest"]
    LB["Lane B<br/>no filter<br/>policy=Snapshot"]
    LC["Lane C<br/>filter regex<br/>policy=Disconnect"]
    MUX["ChannelMux<br/>round-robin cursor"]
    OUT["MCP push<br/>or NDJSON"]

    SRC --> DEB
    DEB --> LA
    DEB --> LB
    DEB --> LC
    LA --> MUX
    LB --> MUX
    LC --> MUX
    MUX --> OUT

    classDef src fill:#9e6a03,color:#f0f6fc,stroke:#bf8700
    classDef lane fill:#a371f7,color:#f0f6fc,stroke:#bc8cff
    classDef rt fill:#1f6feb,color:#f0f6fc,stroke:#388bfd
    classDef sink fill:#238636,color:#f0f6fc,stroke:#2ea043
    class SRC,DEB src
    class LA,LB,LC lane
    class MUX rt
    class OUT sink
```

**7. ChannelMux fairness.**
A single `AtomicUsize` cursor walks the lane map round-robin. Two backlogged lanes alternate; a fast lane never starves a slow one. Drift between adjacent lanes stays under 1 % under burst.

**8. Outbound writer.**
The drained event is delivered as a JSON-RPC `notifications/resources/updated` message to the LLM host (over HTTP, stdio, or SSE), or as a single NDJSON line on the daemon binary's stdout. The host surfaces it directly into the model's context.

The end result: a byte leaves the remote shell, traverses every layer above, and reaches the model in **single-digit milliseconds** on a local network — without a single polling loop anywhere.

## Features at a glance

| Capability | What you get | Why it matters |
|---|---|---|
| **Subscribe-first push** | `ssh_subscribe` on six resource schemes (`shell://`, `command://`, `transfer://`, `forward://`, `session://`, **`serial://`**) | Real-time delivery, ~30× lower token cost than polling. Same pipeline for SSH and UART. |
| **Per-subscription lanes** | Independent filter, replay, lag policy per `SubId` | A slow consumer never slows down a fast one. |
| **Lifecycle binding** | `release_when_no_subs` opt-in; CAS state machine; refcount cascade | No leaked shells, no zombie commands when the host crashes. |
| **Three transports, one core** | `ssh-mcp` (HTTP), `ssh-mcp-stdio` (MCP over stdio), `ssh-mcp-tail` (NDJSON daemon) | Pick what your host supports; same hexagonal core under the hood. |
| **Hexagonal architecture** | Use cases generic over ports; adapters swap for testing | Deterministic test fixtures, ~1 655 lib tests, 16 loom interleavings. |
| **Lock-free hot path** | Zero `Mutex` on shell, command, or transfer state | Enforced by `clippy::mutex_atomic = deny`; verified by loom tests. |
| **36 MCP tools, 10 prompts** | Full SSH catalogue + 9 subscription primitives + 6 serial / UART / TTY / COM primitives | Wire-compatible with every v3 / v4 host on the legacy 21-tool catalogue. |
| **Strong LLM steering** | `HINT:` lines, `NEXT:` tool chains, push-first prompts, 38-code error taxonomy with single-sentence cures | Smaller open-source models drive it correctly first try. |
| **Atomic ID safety** | UUIDv7 across every id type; `_meta.idempotency_key` for retry deduplication | Time-ordered ids; dedup on transient failures. |
| **Bulk operations** | `ssh_execute_batch`, `ssh_disconnect_agent`, `ssh_disconnect_many` | One round-trip for the common multi-target patterns. |
| **Strict by default** | `-D warnings` clippy gate with `pedantic` and `nursery`; `forbid(unwrap_used)` in production code | Production builds pass exit-zero on every commit. |
| **macOS code-signed** | Apple Development cert, hardened runtime | `codesign --verify --verbose=2` exit 0 on installed binaries. |

## Use cases

| Scenario | What it looks like |
|---|---|
| **Driving a remote build from an IDE LLM** | `ssh_execute` → `ssh_subscribe command://<id>/output` → model reads stdout the moment each compile unit finishes. |
| **Watching long-running services** | `ssh_subscribe shell://<id>/output` over `tail -f /var/log/app.log` — survives transient lag, no missed events. |
| **Multi-step interactive sessions** | `ssh_shell_open` plus `ssh_shell_send_key` plus subscribe — drive `vim`, `htop`, `psql`, `top` with key-by-key precision. |
| **Bulk operations across many hosts** | `agent_id` groups sessions; `ssh_disconnect_agent` tears them all down atomically. |
| **Operations LLM in a Unix pipeline** | Pipe NDJSON ops into `ssh-mcp-tail daemon`, NDJSON push events out — perfect for `jq`, `vector`, `fluent-bit`, or a custom audit script. |
| **Hosts that lack `resources/subscribe`** | The daemon binary exposes push events on its own stdout — any consumer can read them. |
| **Live SFTP progress** | `ssh_upload` plus `ssh_subscribe transfer://<id>/progress` — the model watches the bar fill in real time, surfaces ETA, decides when to act. |

## Technology stack

| Component | Crate | Role |
|---|---|---|
| SSH protocol (transport, channels, SFTP) | [russh](https://github.com/warp-tech/russh) 0.55 + [russh-sftp](https://github.com/AspectUnk/russh-sftp) 2 | Async SSH 2.0 client, multiple channels per TCP connection, full SFTP. |
| MCP framework | [rmcp](https://github.com/modelcontextprotocol/rust-sdk) 1.6 | Official Rust SDK for the Model Context Protocol. |
| Async runtime | [tokio](https://tokio.rs/) 1.49 (`rt-multi-thread`, `full`) | Work-stealing scheduler; reactor wraps the kernel readiness API. |
| I/O readiness (transitive) | [mio](https://github.com/tokio-rs/mio) 1.1 | Cross-platform `epoll` / `kqueue` / `IOCP` abstraction. |
| HTTP transport | [axum](https://github.com/tokio-rs/axum) 0.8 + [tower](https://github.com/tower-rs/tower) | Streamable HTTP service, middleware stack. |
| Lock-free state | [arc-swap](https://github.com/vorner/arc-swap) 1.9, [dashmap](https://github.com/xacrimon/dashmap) 6 | Atomic `Arc` swap, sharded concurrent hashmap. |
| Retry semantics | [backon](https://github.com/Xuanwo/backon) 1 | Exponential backoff with jitter. |
| Error modelling | [thiserror](https://github.com/dtolnay/thiserror) 2 | Typed `DomainError` enums per layer. |
| Observability | [tracing](https://github.com/tokio-rs/tracing) 0.1 with `env-filter` | Structured logs gated on `RUST_LOG`. |
| Wire serialisation | [serde](https://serde.rs/) 1, [serde_json](https://github.com/serde-rs/json) 1 | Markdown body plus structured-content JSON twin. |

Each crate is chosen for one job and stays out of the way. The full dependency graph is in [`Cargo.toml`](Cargo.toml); transitive resolution lives in `Cargo.lock`.

## Performance

Measurements taken on Apple M2 Pro, macOS 25.4, native arm64 build (`--release`), localhost SSH server. Reproduce via the harness scripts in `scripts/`.

| Scenario | Behaviour |
|---|---|
| First push after `ssh_subscribe` (warm session) | Under 5 ms p50; the 200 ms debounce window dominates after the initial chunk. |
| Lane full and `Snapshot` recovery | Sub-millisecond with the default 1 MB ring buffer. |
| Round-robin mux fairness | Under 1 % drift between adjacent lanes under burst. |
| Session reaper vs active resources | Refcount supersedes the inactivity TTL — active resources are never reaped. |
| Hot-path concurrency | Zero `Mutex` on shell, command, or transfer state — enforced by clippy. |
| Loom invariants | 16 tests covering CAS state, mux fairness, ring monotonicity, cascade, lane add / remove during drain. |

The strict baseline is encoded in [`clippy.toml`](clippy.toml) and the `[lints.clippy]` block in [`Cargo.toml`](Cargo.toml). Detailed invariants: [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md#lock-free-invariants).

## How it compares

| Approach | Push to LLM? | Self-cleaning? | Lock-free hot path? | Hosts without subscribe | Operational scope |
|---|:---:|:---:|:---:|:---:|---|
| Raw `ssh` from a shell tool | no | no | n/a | manual | one-off command |
| `paramiko` or `asyncssh` glue script | no (poll) | no | varies | manual | one host at a time |
| Other MCP SSH wrappers | usually no | usually no | varies | usually no | tool-only |
| **`ssh-mcp` v5.2.0** | **yes** | **yes** | **yes** | **NDJSON daemon + native UART/TTY/COM** | **multi-session, multi-host, multi-port (SSH + serial), agent-grouped** |

## Install

### Build from source

```bash
git clone https://github.com/farchanjo/ssh-mcp.git
cd ssh-mcp
cargo build --release
sudo install -m 0755 target/release/ssh-mcp{,-stdio,-tail} /usr/local/bin/
```

Three binaries land in `/usr/local/bin`:

- `ssh-mcp-stdio` — recommended default for local hosts (`mcp-inspector`, IDE plugins, Cline, Claude Desktop).
- `ssh-mcp` — same surface over HTTP for browser- or service-based hosts.
- `ssh-mcp-tail daemon` — NDJSON pipeline mode for hosts that cannot consume MCP push notifications natively.

To skip the optional TCP-forwarding tool, build with `--no-default-features` (29 tools instead of 30, smaller binary).

### Optional: macOS code signing

If you ship the binary to other Macs, sign it with your Apple Development certificate so Gatekeeper accepts it:

```bash
codesign --force \
  --sign "Apple Development: <Your Name> (TEAMID)" \
  --options runtime \
  target/release/ssh-mcp{,-stdio,-tail}
codesign --verify --verbose=2 target/release/ssh-mcp-stdio
sudo install -m 0755 target/release/ssh-mcp{,-stdio,-tail} /usr/local/bin/
```

`codesign` must run as your user (it needs your keychain). The `install` step uses `sudo` because `/usr/local/bin` is owned by root.

### Configure your MCP host

Add to your host's MCP server list (Claude Desktop, mcp-inspector, IDE plugin, etc.):

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

Restart the host. The model should now see thirty SSH tools and ten ready-made prompts.

## Tool catalogue

`ssh-mcp` v5.2.0 ships **36 MCP tools** across six push-resource schemes and ten prompt templates (35 tools without the `port_forward` feature). The catalogue is wire-compatible with every v3 / v4 host on the legacy 21-tool surface — adding `ssh-mcp` to an existing v4 deployment requires zero host changes; the new `ssh_serial_*` tools and the `serial://` scheme are additive.

| Family | Tools | Push resource |
|---|---|---|
| Connection | `ssh_connect`, `ssh_disconnect`, `ssh_disconnect_agent`, `ssh_disconnect_many`, `ssh_list_sessions` | `session://<id>/health` |
| Async commands | `ssh_execute`, `ssh_get_command_output`, `ssh_cancel_command`, `ssh_list_commands`, `ssh_run`, `ssh_execute_batch` | `command://<id>/output` |
| Interactive shells | `ssh_shell_open`, `ssh_shell_write`, `ssh_shell_send_key`, `ssh_shell_read`, `ssh_shell_wait_for`, `ssh_shell_close` | `shell://<id>/output` |
| SFTP transfers | `ssh_upload`, `ssh_download`, `ssh_get_transfer_progress` | `transfer://<id>/progress` |
| TCP forwarding | `ssh_forward` (feature-gated) | `forward://<id>/events` |
| Subscription primitives | `ssh_subscribe`, `ssh_unsubscribe`, `ssh_sub_pause`, `ssh_sub_resume`, `ssh_sub_filter`, `ssh_sub_replay`, `ssh_sub_list`, `ssh_sub_stats`, `ssh_daemon_stats` | — |

Per-tool inputs, outputs, structured-content payloads, error codes, and resource semantics live in [docs/API.md](docs/API.md) and [docs/RESOURCES.md](docs/RESOURCES.md).

## Configuration

Three-tier resolution: **parameter, then environment variable, then built-in default.** Defaults are tuned for production. The full env-var table (33+ variables across lifecycle, mux, lane, daemon, retry, broadcast caps) lives in [docs/CONFIGURATION.md](docs/CONFIGURATION.md).

A few that matter most:

| Variable | Default | Purpose |
|---|---|---|
| `SSH_NOTIFY_DEBOUNCE_MS` | `200` | Coalesce window between pushes (lower → snappier, higher → fewer notifications). |
| `SSH_NOTIFY_FORCE_FLUSH_MS` | `1000` | Liveness flush even when the debouncer is still accumulating. |
| `SSH_LANE_BUFFER` | `1024` | Bounded mpsc capacity per subscription lane. |
| `SSH_GRACE_DEFAULT_MS` | `5000` | Grace window between last `unsubscribe` and resource release. |
| `SSH_PEER_GC_INTERVAL_S` | `30` | How often to scan for dead transports. |
| `SSH_MAX_SUBS_PER_URI` | `64` | Hard cap on lanes per resource. |
| `SSH_MAX_SUBS_TOTAL` | `1024` | Hard cap across the server. |

## Development

```bash
cargo build --release
cargo test --lib --quiet
cargo fmt --all -- --check
cargo clippy --release --all-features -- -D warnings
```

These four gates must stay green on every commit. The clippy gate is production-only — the strict `forbid(unwrap_used)` policy is structurally incompatible with the `#[tokio::test]` macro expansion, so test targets are gated separately. Rationale and lock-free invariants: [CLAUDE.md](CLAUDE.md), [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md), [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md).

## Documentation map

| Document | Purpose |
|---|---|
| [docs/](docs/README.md) | Directory index — start here for the per-doc decision tree. |
| [docs/ARCHITECTURE.md](docs/ARCHITECTURE.md) | Hexagonal layout, v5 layers, per-module map, sequence diagrams. |
| [docs/API.md](docs/API.md) | All 30 MCP tools — inputs, outputs, structured content, error codes. |
| [docs/RESOURCES.md](docs/RESOURCES.md) | Five resource schemes, cursor and sequence semantics, `_meta` envelope. |
| [docs/CONFIGURATION.md](docs/CONFIGURATION.md) | Full env-var table with floors, caps, and tuning profiles. |
| [docs/LLM_GUIDE.md](docs/LLM_GUIDE.md) | LLM canonical — golden rules, root prompts, prompts catalogue, anti-patterns, full 38-code error handbook. |
| [docs/OPERATIONS.md](docs/OPERATIONS.md) | Symptom → cure runbook, wire-format error envelope, per-tool error catalogue, recovery flows. |
| [docs/DEVELOPMENT.md](docs/DEVELOPMENT.md) | Build / clippy gates, lock-free invariants, hot-path sequence diagrams. |
| [docs/DAEMON.md](docs/DAEMON.md) | `ssh-mcp-tail` NDJSON op and event schema. |
| [docs/MIGRATION.md](docs/MIGRATION.md) | All migration paths — v2 → v3, v3 → v4, v4 → v5. |
| [docs/adr/](docs/adr/) | Eight architecture decision records covering rmcp, hexagonal, lifecycle, mux, LLM UX, backpressure, errors, and the daemon protocol. |

## FAQ

**Is this wire-compatible with my existing v4 host?**
Yes. The 21 carry-over tools keep their exact response shape, environment variables, and error codes; the 9 new tools are additive. New optional parameters default to v4 behaviour. Details: [docs/MIGRATION.md → v4 → v5](docs/MIGRATION.md#v4--v5).

**My LLM host does not surface push notifications. Can I still benefit?**
Yes. Run `ssh-mcp-tail daemon` as a subprocess and read NDJSON push events directly from its stdout. The daemon embeds an in-process MCP client and server pair so no IPC is involved. See [docs/DAEMON.md](docs/DAEMON.md).

**How do I avoid leaking shells when my host crashes mid-task?**
Pass `release_when_no_subs=true` on `ssh_shell_open`, `ssh_execute`, `ssh_upload`, or `ssh_download`. The peer GC detects the dropped transport, the lifecycle grace timer fires, and the resource is released automatically.

**Why per-subscription lanes instead of a single broadcast channel?**
A broadcast channel forces one shared lag policy: when one consumer falls behind, everyone falls behind. Per-subscription lanes give each consumer independent backpressure, its own filter regex, its own replay buffer, and its own lag policy (`DropOldest`, `Snapshot`, or `Disconnect`). One slow LLM never penalises a fast one.

**Is the lock-free claim real?**
Yes — enforced by `clippy::await_holding_lock`, `clippy::mutex_atomic`, `clippy::significant_drop_tightening`, and `clippy::mutex_integer` denials in [`Cargo.toml`](Cargo.toml), plus 16 loom tests. Production builds pass `-D warnings` exit-zero on every commit.

**Can I run only the legacy 21 tools and skip v5 features?**
Yes. v5 features are entirely opt-in. If you never call the subscribe tools and never pass `release_when_no_subs=true`, behaviour is byte-identical to v4.

## Contributing

Contributions are welcome. Before opening a pull request, please verify:

- The four development gates (build, lib tests, fmt, clippy) all exit zero.
- New tests ship in the same commit as the feature.
- Hot-path changes touching atomics or `ArcSwap` ship with a loom invariant.
- New environment variables get a row in [docs/CONFIGURATION.md](docs/CONFIGURATION.md) and a floor / cap in `src/adapters/config/internal/mod.rs`.

Architecture invariants live in [docs/adr/0002-adopt-hexagonal-architecture.md](docs/adr/0002-adopt-hexagonal-architecture.md). Read it once before touching layer boundaries. Issues: <https://github.com/farchanjo/ssh-mcp/issues>.

## License

MIT. Declared via `license = "MIT"` in [`Cargo.toml`](Cargo.toml).

---

### About this fork

This repository is **not** the original [mingyang91/ssh-mcp](https://github.com/mingyang91/ssh-mcp). It started from the same initial concept and was rewritten on `russh` 0.55 plus `rmcp` 1.6 with a strict hexagonal layout, lock-free hot path, lifecycle binding, channel multiplexing, and a third NDJSON daemon binary. v5.2.0 adds a sixth push-resource scheme (`serial://<id>/output`) plus 6 native UART / TTY / COM tools while staying wire-compatible with every v3 / v4 host on the legacy 21-tool catalogue.

### Author and links

- **Author:** Fabricio Archanjo · <fabricio@archanjo.com>
- **Repository:** <https://github.com/farchanjo/ssh-mcp>
- **Issues:** <https://github.com/farchanjo/ssh-mcp/issues>
- **Releases:** <https://github.com/farchanjo/ssh-mcp/releases>
- **SSH library:** [russh](https://github.com/warp-tech/russh) — SFTP via [russh-sftp](https://github.com/AspectUnk/russh-sftp)
- **MCP framework:** [rmcp](https://github.com/modelcontextprotocol/rust-sdk) — official Rust SDK
- **HTTP host:** [axum](https://github.com/tokio-rs/axum) plus [tower](https://github.com/tower-rs/tower)
- **Lock-free primitives:** [arc-swap](https://github.com/vorner/arc-swap), [dashmap](https://github.com/xacrimon/dashmap)
- **Original concept:** [mingyang91/ssh-mcp](https://github.com/mingyang91/ssh-mcp)
