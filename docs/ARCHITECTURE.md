# SSH MCP Architecture

This document describes the system architecture of the SSH Model Context Protocol (MCP) Server, providing a comprehensive overview of components, their relationships, and the underlying threading model.

## Table of Contents

- [Overview](#overview)
- [Component Architecture](#component-architecture)
- [Session Storage Architecture](#session-storage-architecture)
- [Threading and Async Model](#threading-and-async-model)
- [Binary Targets](#binary-targets)
- [Key Dependencies](#key-dependencies)

---

## Overview

SSH MCP is a Rust-based server that exposes SSH operations as MCP tools, enabling LLM-based systems to interact with remote servers via SSH. The system provides two transport modes:

1. **HTTP Transport** (`ssh-mcp`) - Poem-based HTTP server on port 8000
2. **Stdio Transport** (`ssh-mcp-stdio`) - Direct stdio communication for MCP integration

```mermaid
flowchart TB
    subgraph Clients["MCP Clients"]
        LLM["LLM / AI Agent"]
        CLI["CLI Client"]
    end

    subgraph Transport["Transport Layer"]
        HTTP["HTTP Server<br/>(Poem Framework)"]
        STDIO["Stdio Transport"]
    end

    subgraph Core["SSH MCP Core"]
        MCP["McpSSHCommands<br/>(MCP Tools)"]
        Sessions["Session Store<br/>(SSH_SESSIONS)"]
    end

    subgraph SSH["SSH Layer"]
        Russh["russh<br/>(Async SSH Client)"]
        Agent["SSH Agent"]
    end

    subgraph Remote["Remote Systems"]
        Server1["SSH Server 1"]
        Server2["SSH Server 2"]
        ServerN["SSH Server N"]
    end

    LLM --> HTTP
    CLI --> STDIO
    HTTP --> MCP
    STDIO --> MCP
    MCP --> Sessions
    MCP --> Russh
    Russh --> Agent
    Russh --> Server1
    Russh --> Server2
    Russh --> ServerN

    style Core fill:#e1f5fe
    style Transport fill:#fff3e0
    style SSH fill:#f3e5f5
```

---

## Component Architecture

The following diagram illustrates the relationships between the main components:

```mermaid
classDiagram
    class McpSSHCommands {
        +ssh_connect() StructuredContent~SshConnectResponse~
        +ssh_execute() StructuredContent~SshCommandResponse~
        +ssh_forward() StructuredContent~PortForwardingResponse~
        +ssh_disconnect() Text~String~
        +ssh_list_sessions() StructuredContent~SessionListResponse~
    }

    class StoredSession {
        +info: SessionInfo
        +handle: Arc~Mutex~Handle~~
    }

    class SessionInfo {
        +session_id: String
        +host: String
        +username: String
        +connected_at: String
        +default_timeout_secs: u64
        +retry_attempts: u32
        +compression_enabled: bool
    }

    class SshConnectResponse {
        +session_id: String
        +message: String
        +authenticated: bool
        +retry_attempts: u32
    }

    class SshCommandResponse {
        +stdout: String
        +stderr: String
        +exit_code: i32
    }

    class PortForwardingResponse {
        +local_address: String
        +remote_address: String
        +active: bool
    }

    class SessionListResponse {
        +sessions: Vec~SessionInfo~
        +count: usize
    }

    class SshClientHandler {
        +check_server_key() Result~bool~
    }

    class SSH_SESSIONS {
        <<global>>
        Lazy~Mutex~HashMap~~
    }

    McpSSHCommands ..> StoredSession : manages
    McpSSHCommands ..> SshConnectResponse : returns
    McpSSHCommands ..> SshCommandResponse : returns
    McpSSHCommands ..> PortForwardingResponse : returns
    McpSSHCommands ..> SessionListResponse : returns
    StoredSession *-- SessionInfo : contains
    StoredSession --> SshClientHandler : uses
    SSH_SESSIONS --> StoredSession : stores

    note for SSH_SESSIONS "Global session store using\nLazy<Mutex<HashMap<String, StoredSession>>>"
```

### Component Descriptions

| Component | Description |
|-----------|-------------|
| `McpSSHCommands` | Main struct implementing MCP tools via the `#[Tools]` attribute macro |
| `StoredSession` | Wraps session metadata with the actual SSH handle |
| `SessionInfo` | Serializable metadata for tracking connection information |
| `SshClientHandler` | Implements `russh::client::Handler` for host key verification |
| `SSH_SESSIONS` | Global thread-safe storage for active SSH sessions |

---

## Session Storage Architecture

SSH sessions are stored in a global, thread-safe data structure that allows concurrent access from multiple async tasks.

```mermaid
flowchart LR
    subgraph Storage["SSH_SESSIONS"]
        direction TB
        Lazy["Lazy<br/>(once_cell)"]
        Mutex["Mutex<br/>(tokio::sync)"]
        HashMap["HashMap<br/>String -> StoredSession"]

        Lazy --> Mutex
        Mutex --> HashMap
    end

    subgraph Sessions["Active Sessions"]
        S1["StoredSession 1<br/>uuid: abc-123"]
        S2["StoredSession 2<br/>uuid: def-456"]
        S3["StoredSession 3<br/>uuid: ghi-789"]
    end

    HashMap --> S1
    HashMap --> S2
    HashMap --> S3

    subgraph SessionDetail["StoredSession Structure"]
        Info["SessionInfo<br/>(Metadata)"]
        Handle["Arc<Mutex<Handle>><br/>(SSH Connection)"]
    end

    S1 -.-> SessionDetail

    style Storage fill:#e8f5e9
    style Sessions fill:#fff3e0
    style SessionDetail fill:#e3f2fd
```

### Storage Design Decisions

1. **`Lazy` Initialization**: Sessions store is initialized on first access using `once_cell::sync::Lazy`
2. **`tokio::sync::Mutex`**: Async-aware mutex for non-blocking lock acquisition in async contexts
3. **`Arc<Mutex<Handle>>`**: Session handles are wrapped in `Arc<Mutex>` to allow sharing across tasks while maintaining exclusive access during operations
4. **UUID Session IDs**: Each session receives a unique UUID v4 identifier for tracking

### Lock Scope Optimization

The codebase follows a strict pattern of minimizing lock scope:

```rust
// Clone Arc and release global lock immediately
let handle_arc = {
    let sessions = SSH_SESSIONS.lock().await;
    sessions
        .get(&session_id)
        .map(|s| s.handle.clone())
        .ok_or_else(|| format!("No active SSH session with ID: {}", session_id))?
};

// Actual SSH operations happen outside the global lock
// Only the specific session's handle mutex is held
```

---

## Threading and Async Model

The system uses Tokio's multi-threaded async runtime with careful handling of blocking operations.

```mermaid
flowchart TB
    subgraph Runtime["Tokio Runtime (Multi-threaded)"]
        direction TB

        subgraph MainLoop["Main Event Loop"]
            HTTP["HTTP Request Handler"]
            STDIO["Stdio Message Handler"]
        end

        subgraph Tasks["Async Tasks"]
            Connect["ssh_connect<br/>+ Retry Logic"]
            Execute["ssh_execute<br/>+ Timeout"]
            Forward["Port Forward<br/>Listener"]
            Disconnect["ssh_disconnect"]
        end

        subgraph Channels["SSH Channels"]
            Chan1["Channel 1<br/>(Command)"]
            Chan2["Channel 2<br/>(Direct-TCPIP)"]
        end
    end

    subgraph External["External I/O"]
        SSHServer["SSH Server"]
        LocalPort["Local TCP Port"]
    end

    HTTP --> Tasks
    STDIO --> Tasks
    Connect --> SSHServer
    Execute --> Chan1
    Forward --> Chan2
    Forward --> LocalPort
    Chan1 --> SSHServer
    Chan2 --> SSHServer

    style Runtime fill:#e8eaf6
    style Tasks fill:#fff8e1
```

### Async Operations

| Operation | Async Pattern | Notes |
|-----------|---------------|-------|
| SSH Connect | `tokio::time::timeout` | Wrapped with configurable timeout |
| Retry Logic | `backon::Retryable` | Exponential backoff with jitter |
| Command Execution | Channel-based async I/O | Non-blocking read/write |
| Port Forwarding | `tokio::spawn` | Background task per listener |
| Session Lock | `tokio::sync::Mutex` | Async-aware mutex |

### Retry Logic with Backoff

```mermaid
stateDiagram-v2
    [*] --> Attempt1: Initial Connect

    Attempt1 --> Success: Connected
    Attempt1 --> CheckRetry1: Failed

    CheckRetry1 --> Delay1: Retryable Error
    CheckRetry1 --> [*]: Auth Error

    Delay1 --> Attempt2: Wait with backoff

    Attempt2 --> Success: Connected
    Attempt2 --> CheckRetry2: Failed

    CheckRetry2 --> Delay2: Retryable Error
    CheckRetry2 --> [*]: Auth Error

    Delay2 --> Attempt3: Wait with backoff

    Attempt3 --> Success: Connected
    Attempt3 --> CheckRetry3: Failed

    CheckRetry3 --> Delay3: Retryable Error
    CheckRetry3 --> [*]: Auth Error

    Delay3 --> Attempt4: Wait with backoff, max 10s

    Attempt4 --> Success: Connected
    Attempt4 --> [*]: Max Retries Exceeded

    Success --> [*]

    note right of Delay1: Jitter added to prevent thundering herd
    note right of CheckRetry1: Non-retryable errors include auth failures
```

---

## Binary Targets

### HTTP Server (`ssh-mcp`)

```mermaid
flowchart LR
    subgraph Binary["ssh-mcp Binary"]
        Main["main.rs"]
        Route["Poem Route"]
        Streamable["streamable_http::endpoint"]
    end

    subgraph Server["HTTP Server"]
        TCP["TcpListener<br/>0.0.0.0:8000"]
        Tracing["Tracing Middleware"]
    end

    Main --> Route
    Route --> Streamable
    Streamable --> Server
    TCP --> Tracing

    style Binary fill:#e3f2fd
    style Server fill:#f3e5f5
```

**Features:**
- Runs on port 8000 (configurable via `MCP_PORT`)
- Uses Poem's streamable HTTP transport
- Includes tracing middleware for debugging
- Loads environment from `.env` file

### Stdio Transport (`ssh-mcp-stdio`)

```mermaid
flowchart LR
    subgraph Binary["ssh-mcp-stdio Binary"]
        Main["main.rs"]
        Stdio["poem_mcpserver::stdio"]
    end

    subgraph IO["Standard I/O"]
        STDIN["stdin"]
        STDOUT["stdout"]
    end

    STDIN --> Main
    Main --> Stdio
    Stdio --> STDOUT

    style Binary fill:#e8f5e9
    style IO fill:#fff3e0
```

**Features:**
- Minimal binary for direct MCP integration
- No HTTP overhead
- Ideal for embedding in LLM tools

---

## Key Dependencies

```mermaid
flowchart TB
    subgraph Core["Core Dependencies"]
        Russh["russh 0.55<br/>(Async SSH Client)"]
        Tokio["tokio 1.x<br/>(Async Runtime)"]
        Poem["poem 3.1<br/>(HTTP Framework)"]
    end

    subgraph MCP["MCP Integration"]
        PoemMCP["poem-mcpserver 0.2.9<br/>(MCP Protocol)"]
    end

    subgraph Utilities["Utility Crates"]
        Backon["backon 1.x<br/>(Retry Logic)"]
        Serde["serde 1.0<br/>(Serialization)"]
        UUID["uuid 1.16<br/>(Session IDs)"]
        OnceCell["once_cell 1.21<br/>(Lazy Statics)"]
    end

    PoemMCP --> Poem
    PoemMCP --> Tokio
    Russh --> Tokio

    style Core fill:#e1f5fe
    style MCP fill:#f3e5f5
    style Utilities fill:#e8f5e9
```

| Dependency | Version | Purpose |
|------------|---------|---------|
| `russh` | 0.55 | Pure Rust async SSH client implementation |
| `tokio` | 1.x | Async runtime with full features |
| `poem` | 3.1 | HTTP framework matching poem-mcpserver |
| `poem-mcpserver` | 0.2.9 | MCP protocol implementation |
| `backon` | 1.x | Retry logic with exponential backoff |
| `serde` | 1.0 | JSON serialization/deserialization |
| `uuid` | 1.16 | UUID v4 generation for session IDs |
| `once_cell` | 1.21 | Lazy static initialization |
| `tracing` | 0.1 | Structured logging |

---

## Feature Flags

The project supports optional features via Cargo:

| Feature | Default | Description |
|---------|---------|-------------|
| `port_forward` | Yes | Enables SSH port forwarding support via `ssh_forward` tool |

To build without port forwarding:

```bash
cargo build --release --no-default-features
```
