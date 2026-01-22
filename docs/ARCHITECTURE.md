# SSH MCP Architecture

This document describes the system architecture of the SSH Model Context Protocol (MCP) Server, providing a comprehensive overview of components, their relationships, and the underlying threading model.

## Table of Contents

- [Overview](#overview)
- [Module Structure](#module-structure)
- [Module Dependency Graph](#module-dependency-graph)
- [Component Architecture](#component-architecture)
- [Authentication Flow](#authentication-flow)
- [Session Storage Architecture](#session-storage-architecture)
- [Async Command Architecture](#async-command-architecture)
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

## Module Structure

The codebase consists of **9 source files** organized into a modular structure:

| File | Location | Visibility | Description |
|------|----------|------------|-------------|
| `lib.rs` | `src/` | `pub` | Library crate root, exposes `mcp` module |
| `mod.rs` | `src/mcp/` | - | Module root, re-exports `McpSSHCommands` |
| `types.rs` | `src/mcp/` | `pub` | Serializable response types for MCP tools |
| `config.rs` | `src/mcp/` | `pub(crate)` | Configuration resolution with environment variable support |
| `error.rs` | `src/mcp/` | `pub(crate)` | Error classification for retry logic |
| `session.rs` | `src/mcp/` | `pub` | Session storage and SSH client handler |
| `client.rs` | `src/mcp/` | `pub(crate)` | SSH connection, authentication, and command execution |
| `forward.rs` | `src/mcp/` | `pub(crate)` | Port forwarding implementation (feature-gated) |
| `async_command.rs` | `src/mcp/` | `pub(crate)` | Async command storage, tracking, and helper functions |
| `commands.rs` | `src/mcp/` | `pub` | MCP tool implementations via `#[Tools]` macro |

### Module Responsibilities

**lib.rs** - Library Root
- Exposes the `mcp` module for external use
- Entry point for the library crate

**mod.rs** - Module Root
- Declares and organizes submodules
- Controls visibility (pub, pub(crate))
- Re-exports `McpSSHCommands` for convenience

**types.rs** - Response Types
- `SessionInfo` - Session metadata for tracking connections (includes optional `name` field)
- `SshConnectResponse` - Connection result with retry information
- `SshCommandResponse` - Command output with stdout, stderr, exit code, and `timed_out` flag
- `PortForwardingResponse` - Port forwarding status (feature-gated)
- `SessionListResponse` - List of active sessions

**config.rs** - Configuration Management
- Default constants using `Duration` type (`DEFAULT_CONNECT_TIMEOUT`, `DEFAULT_COMMAND_TIMEOUT`, `DEFAULT_RETRY_DELAY`, `MAX_RETRY_DELAY`)
- Environment variable names and parsing
- `resolve_*` functions returning `Duration` (except `resolve_max_retries` and `resolve_compression`) implementing Parameter -> Env -> Default priority

**error.rs** - Error Classification
- `is_retryable_error()` - Classifies errors as transient or permanent
- Authentication errors (non-retryable) vs connection errors (retryable)

**session.rs** - Session Management
- `SshClientHandler` - russh client handler that accepts all host keys
- `StoredSession` - Combines SessionInfo with SSH handle
- `SSH_SESSIONS` - Global lazy-initialized session storage

**client.rs** - SSH Client Operations
- `build_client_config()` - Builds russh configuration with compression preferences
- `parse_address()` - Parses host and port from address string
- `connect_to_ssh_with_retry()` - Connection with exponential backoff via backon
- `authenticate_with_key()` - Private key authentication with RSA hash negotiation
- `authenticate_with_agent()` - SSH agent authentication with RSA hash negotiation
- `execute_ssh_command()` - Command execution via channel-based async I/O (timeout returns partial output instead of error)

**forward.rs** - Port Forwarding (feature-gated)
- `setup_port_forwarding()` - Creates TCP listener and spawns forwarder
- `handle_port_forward_connection()` - Bidirectional I/O via direct-tcpip

**async_command.rs** - Async Command Storage
- `RunningCommand` - State for a running async command including:
  - `cancel_token: CancellationToken` - Token to cancel the command via `tokio_util::sync::CancellationToken`
  - `status_rx: watch::Receiver<AsyncCommandStatus>` - Receiver for status updates
  - `status_tx: watch::Sender<AsyncCommandStatus>` - Sender for status updates
  - `output: Arc<Mutex<OutputBuffer>>` - Output buffer for stdout/stderr collection
  - `exit_code: Arc<Mutex<Option<i32>>>` - Exit code when completed
  - `error: Arc<Mutex<Option<String>>>` - Error message if failed
  - `timed_out: Arc<AtomicBool>` - Whether the command timed out
- `OutputBuffer` - Simple struct holding `stdout: Vec<u8>` and `stderr: Vec<u8>`
- `ASYNC_COMMANDS` - Global storage `Lazy<Mutex<HashMap<String, RunningCommand>>>`
- `MAX_ASYNC_COMMANDS_PER_SESSION` - Constant limit of 10 concurrent async commands per session
- `count_session_commands()` - Helper to count async commands for a session
- `get_session_command_ids()` - Helper to get all command IDs for cleanup during disconnect

**commands.rs** - MCP Tools
- `McpSSHCommands` struct with `#[Tools]` impl
- `ssh_connect` - Connect and authenticate (supports `name` and `persistent` options)
- `ssh_execute` - Run commands with timeout (returns partial output with `timed_out: true` on timeout, session stays alive)
- `ssh_forward` - Setup port forwarding (feature-gated)
- `ssh_disconnect` - Graceful session cleanup
- `ssh_list_sessions` - List active sessions (includes session names when set)

---

## Module Dependency Graph

```mermaid
flowchart TB
    subgraph Binaries["Binary Targets"]
        main["main.rs<br/>HTTP Server"]
        stdio["ssh_mcp_stdio.rs<br/>Stdio Transport"]
    end

    subgraph Library["Library - src/lib.rs"]
        lib["lib.rs"]
    end

    subgraph Public["Public Modules"]
        commands["commands.rs<br/>McpSSHCommands"]
        types["types.rs<br/>Response Types"]
        session["session.rs<br/>Session Storage"]
    end

    subgraph Internal["Internal Modules"]
        client["client.rs<br/>SSH Client Logic"]
        config["config.rs<br/>Configuration"]
        error["error.rs<br/>Error Classification"]
        forward["forward.rs<br/>Port Forwarding"]
        async_cmd["async_command.rs<br/>Async Command Storage"]
        modrs["mod.rs<br/>Module Root"]
    end

    subgraph External["External Crates"]
        russh["russh"]
        backon["backon"]
        tokio["tokio"]
        tokio_util["tokio-util"]
        poem_mcp["poem-mcpserver"]
        tracing["tracing"]
    end

    main --> modrs
    stdio --> lib
    lib --> modrs

    modrs --> commands
    modrs --> types
    modrs --> session
    modrs --> client
    modrs --> config
    modrs --> error
    modrs --> forward
    modrs --> async_cmd

    commands --> client
    commands --> config
    commands --> session
    commands --> types
    commands --> forward
    commands --> async_cmd
    commands --> poem_mcp

    client --> config
    client --> error
    client --> session
    client --> types
    client --> async_cmd
    client --> russh
    client --> backon
    client --> tokio
    client --> tracing

    async_cmd --> types
    async_cmd --> tokio
    async_cmd --> tokio_util

    forward --> session
    forward --> russh
    forward --> tokio

    session --> types
    session --> russh
    session --> tokio

    stdio --> tracing

    style Binaries fill:#fce4ec
    style Library fill:#e8f5e9
    style Public fill:#e8f5e9
    style Internal fill:#fff3e0
    style External fill:#e3f2fd
```

---

## Component Architecture

The following diagram illustrates the relationships between the main components:

```mermaid
classDiagram
    class McpSSHCommands {
        +ssh_connect() StructuredContent~SshConnectResponse~
        +ssh_execute() StructuredContent~SshCommandResponse~
        +ssh_execute_async() StructuredContent~AsyncCommandResponse~
        +ssh_poll_async() StructuredContent~AsyncPollResponse~
        +ssh_list_async() StructuredContent~AsyncListResponse~
        +ssh_cancel_async() Text~String~
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
        +name: Option~String~
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
        +timed_out: bool
    }

    class RunningCommand {
        +info: AsyncCommandInfo
        +cancel_token: CancellationToken
        +status_rx: watch~Receiver~
        +status_tx: watch~Sender~
        +output: Arc~Mutex~OutputBuffer~~
        +exit_code: Arc~Mutex~Option~i32~~~
        +error: Arc~Mutex~Option~String~~~
        +timed_out: Arc~AtomicBool~
    }

    class OutputBuffer {
        +stdout: Vec~u8~
        +stderr: Vec~u8~
    }

    class AsyncCommandInfo {
        +command_id: String
        +session_id: String
        +command: String
        +status: AsyncCommandStatus
        +started_at: String
    }

    class AsyncCommandStatus {
        <<enumeration>>
        Running
        Completed
        Cancelled
        Failed
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

    class ASYNC_COMMANDS {
        <<global>>
        Lazy~Mutex~HashMap~~
    }

    McpSSHCommands ..> StoredSession : manages
    McpSSHCommands ..> SshConnectResponse : returns
    McpSSHCommands ..> SshCommandResponse : returns
    McpSSHCommands ..> RunningCommand : manages async
    McpSSHCommands ..> PortForwardingResponse : returns
    McpSSHCommands ..> SessionListResponse : returns
    StoredSession *-- SessionInfo : contains
    StoredSession --> SshClientHandler : uses
    SSH_SESSIONS --> StoredSession : stores
    ASYNC_COMMANDS --> RunningCommand : stores
    RunningCommand *-- AsyncCommandInfo : contains
    RunningCommand *-- OutputBuffer : contains
    AsyncCommandInfo --> AsyncCommandStatus : has

    note for SSH_SESSIONS "Global session store using\nLazy Mutex HashMap String StoredSession"
    note for ASYNC_COMMANDS "Global async command store using\nLazy Mutex HashMap String RunningCommand\nMax 10 commands per session"
```

### Component Descriptions

| Component | Module | Description |
|-----------|--------|-------------|
| `McpSSHCommands` | commands.rs | Main struct implementing MCP tools via the `#[Tools]` attribute macro |
| `StoredSession` | session.rs | Wraps session metadata with the actual SSH handle |
| `SessionInfo` | types.rs | Serializable metadata for tracking connection information |
| `SshClientHandler` | session.rs | Implements `russh::client::Handler` for host key verification |
| `SSH_SESSIONS` | session.rs | Global thread-safe storage for active SSH sessions |
| `RunningCommand` | async_command.rs | State container for async commands including output buffers and cancellation |
| `OutputBuffer` | async_command.rs | Simple struct for collecting stdout/stderr from async commands |
| `AsyncCommandInfo` | types.rs | Serializable metadata for async command tracking |
| `AsyncCommandStatus` | types.rs | Enum representing command states: Running, Completed, Cancelled, Failed |
| `ASYNC_COMMANDS` | async_command.rs | Global thread-safe storage for running async commands (max 10 per session) |

---

## Authentication Flow

The client.rs module handles three authentication methods with modern RSA hash negotiation:

```mermaid
flowchart TB
    subgraph Entry["Authentication Entry Point"]
        Start["connect_to_ssh"]
    end

    subgraph Methods["Authentication Methods"]
        Password["Password Auth"]
        KeyFile["Key File Auth"]
        Agent["SSH Agent Auth"]
    end

    subgraph KeyAuth["Key File Authentication"]
        LoadKey["Load key from file"]
        QueryHash1["Query best_supported_rsa_hash"]
        WrapKey["Wrap key with PrivateKeyWithHashAlg"]
        AuthKey["authenticate_publickey"]
    end

    subgraph AgentAuth["SSH Agent Authentication"]
        ConnectAgent["Connect to SSH_AUTH_SOCK"]
        GetIdentities["Request identities from agent"]
        LoopStart["For each identity"]
        QueryHash2["Query best_supported_rsa_hash"]
        TryAuth["authenticate_publickey_with"]
        CheckResult["Check result"]
        NextId["Try next identity"]
        AgentSuccess["Return success"]
    end

    subgraph RSAHash["RSA Hash Negotiation"]
        Negotiate["Server negotiates supported hashes"]
        SelectHash["Select rsa-sha2-512 or rsa-sha2-256"]
        FallbackSHA1["Fallback to ssh-rsa SHA1 if needed"]
    end

    Start --> Password
    Start --> KeyFile
    Start --> Agent

    Password --> AuthSuccess

    KeyFile --> LoadKey
    LoadKey --> QueryHash1
    QueryHash1 --> RSAHash
    RSAHash --> WrapKey
    WrapKey --> AuthKey
    AuthKey --> AuthSuccess

    Agent --> ConnectAgent
    ConnectAgent --> GetIdentities
    GetIdentities --> LoopStart
    LoopStart --> QueryHash2
    QueryHash2 --> RSAHash
    RSAHash --> TryAuth
    TryAuth --> CheckResult
    CheckResult --> NextId
    CheckResult --> AgentSuccess
    NextId --> LoopStart
    AgentSuccess --> AuthSuccess

    Negotiate --> SelectHash
    SelectHash --> FallbackSHA1

    AuthSuccess["Authentication Success"]

    style Entry fill:#e3f2fd
    style Methods fill:#fff3e0
    style KeyAuth fill:#e8f5e9
    style AgentAuth fill:#f3e5f5
    style RSAHash fill:#fce4ec
```

### RSA Hash Algorithm Negotiation

Modern SSH servers often disable legacy `ssh-rsa` (SHA-1) signatures for security. The client.rs module uses `best_supported_rsa_hash()` to negotiate modern algorithms:

| Priority | Algorithm | Description |
|----------|-----------|-------------|
| 1 | `rsa-sha2-512` | RSA with SHA-512 - strongest option |
| 2 | `rsa-sha2-256` | RSA with SHA-256 - widely supported |
| 3 | `ssh-rsa` | Legacy RSA with SHA-1 - fallback only |

```rust
// Query server for best supported RSA hash algorithm
let hash_alg = handle
    .best_supported_rsa_hash()
    .await
    .ok()
    .flatten()
    .flatten();

// Wrap the key with the negotiated algorithm
let key_with_hash = keys::PrivateKeyWithHashAlg::new(Arc::new(key_pair), hash_alg);
```

This negotiation happens automatically for both key file and SSH agent authentication, ensuring compatibility with modern SSH servers while maintaining backward compatibility.

---

## Session Storage Architecture

SSH sessions are stored in a global, thread-safe data structure that allows concurrent access from multiple async tasks.

```mermaid
flowchart LR
    subgraph Storage["SSH_SESSIONS"]
        direction TB
        Lazy["Lazy<br/>once_cell"]
        Mutex["Mutex<br/>tokio sync"]
        HashMap["HashMap<br/>String to StoredSession"]

        Lazy --> Mutex
        Mutex --> HashMap
    end

    subgraph Sessions["Active Sessions"]
        S1["StoredSession 1<br/>uuid abc-123"]
        S2["StoredSession 2<br/>uuid def-456"]
        S3["StoredSession 3<br/>uuid ghi-789"]
    end

    HashMap --> S1
    HashMap --> S2
    HashMap --> S3

    subgraph SessionDetail["StoredSession Structure"]
        Info["SessionInfo<br/>Metadata"]
        Handle["Arc Mutex Handle<br/>SSH Connection"]
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

## Async Command Architecture

The async command system enables long-running SSH commands to execute in the background while allowing clients to poll for output, check status, and cancel commands.

```mermaid
flowchart TB
    subgraph Storage["ASYNC_COMMANDS Storage"]
        direction TB
        Lazy2["Lazy<br/>once_cell"]
        Mutex2["Mutex<br/>tokio sync"]
        HashMap2["HashMap<br/>String to RunningCommand"]

        Lazy2 --> Mutex2
        Mutex2 --> HashMap2
    end

    subgraph RunningCmd["RunningCommand Structure"]
        Info["AsyncCommandInfo<br/>Metadata"]
        CancelToken["CancellationToken<br/>tokio-util"]
        StatusChan["watch::channel<br/>Status Updates"]
        OutputBuf["OutputBuffer<br/>stdout/stderr"]
        ExitCode["exit_code<br/>Arc Mutex Option"]
        Error["error<br/>Arc Mutex Option"]
        TimedOut["timed_out<br/>Arc AtomicBool"]
    end

    subgraph Execution["Background Execution"]
        Spawn["tokio::spawn"]
        ExecAsync["execute_ssh_command_async"]
        SSHChannel["SSH Channel I/O"]
    end

    HashMap2 --> RunningCmd
    Spawn --> ExecAsync
    ExecAsync --> SSHChannel
    ExecAsync -.-> OutputBuf
    ExecAsync -.-> StatusChan
    CancelToken -.-> ExecAsync

    style Storage fill:#e8f5e9
    style RunningCmd fill:#fff3e0
    style Execution fill:#e3f2fd
```

### Async Command Flow

1. **Start Command** (`ssh_execute_async`):
   - Check session limit (`MAX_ASYNC_COMMANDS_PER_SESSION = 10`)
   - Create `RunningCommand` with status channel and cancellation token
   - Store in `ASYNC_COMMANDS` global storage
   - Spawn background task via `tokio::spawn(execute_ssh_command_async(...))`
   - Return `command_id` immediately to client

2. **Poll for Output** (`ssh_poll_async`):
   - Look up command by ID in `ASYNC_COMMANDS`
   - Read current output from `OutputBuffer`
   - Check status via `watch::Receiver`
   - Return partial output and current status

3. **Cancel Command** (`ssh_cancel_async`):
   - Look up command by ID
   - Call `cancel_token.cancel()` to signal cancellation
   - Background task detects cancellation and exits gracefully
   - Status updated to `Cancelled`

4. **List Commands** (`ssh_list_async`):
   - Filter `ASYNC_COMMANDS` by session ID
   - Optionally filter by status
   - Return list of `AsyncCommandInfo`

### Concurrency Controls

| Control | Value | Purpose |
|---------|-------|---------|
| `MAX_ASYNC_COMMANDS_PER_SESSION` | 10 | Prevents resource exhaustion per session |
| `Arc<Mutex<OutputBuffer>>` | - | Thread-safe output collection from background task |
| `Arc<AtomicBool>` for `timed_out` | - | Lock-free timeout flag |
| `watch::channel` | - | Efficient status broadcasting without locks |

### Session Cleanup

When `ssh_disconnect` is called, all async commands for that session are automatically cleaned up:
1. `get_session_command_ids()` retrieves all command IDs for the session
2. Each command's `cancel_token` is triggered
3. Commands are removed from `ASYNC_COMMANDS` storage

---

## Threading and Async Model

The system uses Tokio's multi-threaded async runtime with native async SSH operations via russh.

```mermaid
flowchart TB
    subgraph Runtime["Tokio Runtime - Multi-threaded"]
        direction TB

        subgraph MainLoop["Main Event Loop"]
            HTTP["HTTP Request Handler"]
            STDIO["Stdio Message Handler"]
        end

        subgraph Tasks["Async Tasks"]
            Connect["ssh_connect<br/>+ Retry Logic"]
            Execute["ssh_execute<br/>+ Timeout"]
            AsyncExec["ssh_execute_async<br/>Background Task"]
            Forward["Port Forward<br/>Listener"]
            Disconnect["ssh_disconnect"]
        end

        subgraph AsyncCommands["Async Command Management"]
            Spawn["tokio::spawn<br/>Background Execution"]
            Cancel["CancellationToken<br/>Graceful Cancel"]
            Status["watch::channel<br/>Status Updates"]
        end

        subgraph Channels["SSH Channels"]
            Chan1["Channel 1<br/>Session Command"]
            Chan2["Channel 2<br/>Direct-TCPIP"]
            ChanAsync["Channel N<br/>Async Command"]
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
    AsyncExec --> AsyncCommands
    Spawn --> ChanAsync
    Cancel --> Spawn
    Status --> Spawn
    Forward --> Chan2
    Forward --> LocalPort
    Chan1 --> SSHServer
    Chan2 --> SSHServer
    ChanAsync --> SSHServer

    style Runtime fill:#e8eaf6
    style Tasks fill:#fff8e1
    style AsyncCommands fill:#e8f5e9
```

### Native Async Architecture

Unlike implementations using blocking SSH libraries, this system uses **russh** which provides native async support:

| Operation | Async Pattern | Notes |
|-----------|---------------|-------|
| SSH Connect | `tokio::time::timeout` | Wrapped with configurable timeout |
| Retry Logic | `backon::Retryable` | Exponential backoff with jitter |
| Command Execution | Channel-based async I/O | Non-blocking read/write via `ChannelMsg`; timeout returns partial output with `timed_out: true` |
| Async Command Execution | `tokio::spawn` | Background task via `execute_ssh_command_async()` |
| Async Command Cancellation | `CancellationToken` | `tokio_util::sync::CancellationToken` for graceful cancellation |
| Async Status Updates | `watch::channel` | `tokio::sync::watch` for real-time status broadcasting |
| Port Forwarding | `tokio::spawn` | Background task per listener |
| Session Lock | `tokio::sync::Mutex` | Async-aware mutex |
| Bidirectional I/O | `tokio::io::copy` + `select!` | Efficient zero-copy forwarding |

### Key Differences from Blocking Libraries

- **No `spawn_blocking`**: All SSH operations are natively async
- **Channel-based I/O**: Uses russh `ChannelMsg` enum for stdout, stderr, exit status
- **Direct-TCPIP**: Port forwarding uses `channel_open_direct_tcpip` with `into_stream()`
- **Graceful disconnect**: Uses `Disconnect::ByApplication` for clean session termination

### Retry Logic with Backoff

```mermaid
stateDiagram-v2
    [*] --> Attempt1

    Attempt1 --> Success: Connected
    Attempt1 --> CheckRetry1: Failed

    CheckRetry1 --> Delay1: Retryable Error
    CheckRetry1 --> [*]: Auth Error - Stop

    Delay1 --> Attempt2: Wait with backoff

    Attempt2 --> Success: Connected
    Attempt2 --> CheckRetry2: Failed

    CheckRetry2 --> Delay2: Retryable Error
    CheckRetry2 --> [*]: Auth Error - Stop

    Delay2 --> Attempt3: Wait with backoff

    Attempt3 --> Success: Connected
    Attempt3 --> CheckRetry3: Failed

    CheckRetry3 --> Delay3: Retryable Error
    CheckRetry3 --> [*]: Auth Error - Stop

    Delay3 --> Attempt4: Wait with backoff max 10s

    Attempt4 --> Success: Connected
    Attempt4 --> [*]: Max Retries Exceeded

    Success --> [*]
```

**Retry Classification (error.rs)**:
- **Non-retryable**: Authentication failures, permission denied, publickey errors
- **Retryable**: Connection refused, timeout, network unreachable, broken pipe

---

## Binary Targets

### HTTP Server (`ssh-mcp`)

```mermaid
flowchart LR
    subgraph Binary["ssh-mcp Binary"]
        Main["main.rs"]
        Route["Poem Route"]
        Streamable["streamable_http endpoint"]
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
- Initializes tracing with `info` level default

### Stdio Transport (`ssh-mcp-stdio`)

```mermaid
flowchart LR
    subgraph Binary["ssh-mcp-stdio Binary"]
        Main["main.rs"]
        TracingInit["Tracing Init<br/>stderr output"]
        Stdio["poem_mcpserver stdio"]
    end

    subgraph IO["Standard I/O"]
        STDIN["stdin"]
        STDOUT["stdout"]
        STDERR["stderr - logs"]
    end

    STDIN --> Main
    Main --> TracingInit
    TracingInit --> STDERR
    Main --> Stdio
    Stdio --> STDOUT

    style Binary fill:#e8f5e9
    style IO fill:#fff3e0
```

**Features:**
- Minimal binary for direct MCP integration
- No HTTP overhead
- Ideal for embedding in LLM tools
- Tracing initialized with `RUST_LOG` environment filter
- Logs directed to stderr to avoid interfering with MCP protocol on stdout

---

## Key Dependencies

```mermaid
flowchart TB
    subgraph Core["Core Dependencies"]
        Russh["russh 0.55<br/>Async SSH Client"]
        Tokio["tokio 1.x<br/>Async Runtime"]
        TokioUtil["tokio-util<br/>Async Utilities"]
        Poem["poem 3.1<br/>HTTP Framework"]
    end

    subgraph MCP["MCP Integration"]
        PoemMCP["poem-mcpserver 0.2.9<br/>MCP Protocol"]
    end

    subgraph Utilities["Utility Crates"]
        Backon["backon 1.x<br/>Retry Logic"]
        Serde["serde 1.0<br/>Serialization"]
        UUID["uuid 1.16<br/>Session IDs"]
        OnceCell["once_cell 1.21<br/>Lazy Statics"]
    end

    PoemMCP --> Poem
    PoemMCP --> Tokio
    Russh --> Tokio
    TokioUtil --> Tokio

    style Core fill:#e1f5fe
    style MCP fill:#f3e5f5
    style Utilities fill:#e8f5e9
```

| Dependency | Version | Purpose |
|------------|---------|---------|
| `russh` | 0.55 | Pure Rust async SSH client implementation |
| `tokio` | 1.x | Async runtime with full features |
| `tokio-util` | 0.7 | Async utilities including `CancellationToken` for async command cancellation |
| `poem` | 3.1 | HTTP framework matching poem-mcpserver |
| `poem-mcpserver` | 0.2.9 | MCP protocol implementation |
| `backon` | 1.x | Retry logic with exponential backoff |
| `serde` | 1.0 | JSON serialization/deserialization |
| `uuid` | 1.16 | UUID v4 generation for session and command IDs |
| `once_cell` | 1.21 | Lazy static initialization |
| `tracing` | 0.1 | Structured logging |
| `tracing-subscriber` | 0.3 | Tracing output and filtering |
| `chrono` | 0.4 | Timestamp generation |

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
