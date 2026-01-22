# SSH MCP Operation Flows

This document describes the operational flows of the SSH MCP server, including connection establishment, command execution, port forwarding, and session lifecycle management.

## Table of Contents

- [Session Lifecycle](#session-lifecycle)
- [SSH Connection Flow](#ssh-connection-flow)
- [Authentication Flow](#authentication-flow)
- [Command Execution Flow](#command-execution-flow)
- [Port Forwarding Flow](#port-forwarding-flow)
- [Error Handling and Retry Logic](#error-handling-and-retry-logic)

---

## Session Lifecycle

The complete lifecycle of an SSH session from creation to termination.

```mermaid
stateDiagram-v2
    [*] --> Disconnected

    Disconnected --> Connecting: ssh_connect called

    Connecting --> Authenticating: TCP Connected
    Connecting --> RetryLogic: Connection Failed

    RetryLogic --> Connecting: Retryable Error
    RetryLogic --> Disconnected: Max Retries Exceeded
    RetryLogic --> Disconnected: Auth Error

    Authenticating --> Connected: Auth Success
    Authenticating --> Disconnected: Auth Failed

    Connected --> Executing: ssh_execute called
    Connected --> Forwarding: ssh_forward called
    Connected --> Disconnecting: ssh_disconnect called

    Executing --> Connected: Command Complete
    Executing --> Connected: Timeout

    Forwarding --> Connected: Forwarding Active
    Forwarding --> Connected: Setup Failed

    Disconnecting --> Disconnected: Cleanup Complete

    Disconnected --> [*]

    note right of Connected
        Session stored in SSH_SESSIONS
        with unique UUID
    end note

    note right of Forwarding
        Background task spawned
        for port listener
    end note
```

### Session States

| State | Description |
|-------|-------------|
| `Disconnected` | No active connection, session not in store |
| `Connecting` | TCP connection in progress with retry logic |
| `Authenticating` | Connection established, auth in progress |
| `Connected` | Fully connected and ready for operations |
| `Executing` | Command execution in progress |
| `Forwarding` | Port forwarding setup in progress |
| `Disconnecting` | Graceful disconnect in progress |

---

## SSH Connection Flow

Detailed flow of the `ssh_connect` operation using russh native async.

```mermaid
sequenceDiagram
    participant Client as MCP Client
    participant Cmd as McpSSHCommands
    participant Config as config.rs
    participant Retry as client.rs with backon
    participant SSH as russh client
    participant Server as SSH Server
    participant Store as SSH_SESSIONS

    Client->>Cmd: ssh_connect request

    Note over Cmd,Config: Configuration Resolution Phase
    Cmd->>Config: resolve_connect_timeout
    Config-->>Cmd: timeout value
    Cmd->>Config: resolve_max_retries
    Config-->>Cmd: max_retries value
    Cmd->>Config: resolve_retry_delay_ms
    Config-->>Cmd: retry_delay_ms value
    Cmd->>Config: resolve_compression
    Config-->>Cmd: compress value

    Cmd->>Retry: connect_to_ssh_with_retry

    Note over Retry: Build ExponentialBuilder backoff
    Retry->>Retry: Build backoff with min_delay and max_delay

    loop Retry Loop via backon
        Retry->>SSH: connect_to_ssh
        SSH->>SSH: build_client_config
        SSH->>SSH: parse_address
        SSH->>Server: client::connect with timeout
        alt Connection Success
            Server-->>SSH: Handle returned
            Note over SSH: Authenticate with RSA hash negotiation
            alt Password provided
                SSH->>Server: authenticate_password
            else Key path provided
                SSH->>SSH: keys::load_secret_key
                SSH->>Server: best_supported_rsa_hash
                Server-->>SSH: Preferred RSA hash algorithm
                SSH->>SSH: PrivateKeyWithHashAlg wrap key
                SSH->>Server: authenticate_publickey
            else No credentials - Agent auth
                SSH->>SSH: keys::agent::AgentClient::connect_env
                SSH->>SSH: request_identities
                loop For each identity
                    SSH->>Server: best_supported_rsa_hash
                    Server-->>SSH: Preferred RSA hash algorithm
                    SSH->>Server: authenticate_publickey_with
                end
            end
            Server-->>SSH: AuthResult
            alt Auth Success
                SSH-->>Retry: Handle
            else Auth Failed
                SSH-->>Retry: Error not retryable
            end
        else Connection Failed
            Server-->>SSH: Error
            SSH-->>Retry: Error
            Retry->>Retry: is_retryable_error check
            alt Retryable
                Note over Retry: Wait with backoff plus jitter
            else Not Retryable
                Retry-->>Cmd: Error
            end
        end
    end

    Retry-->>Cmd: Handle and retry_count

    Note over Cmd,Store: Session Storage Phase
    Cmd->>Cmd: Generate UUID
    Cmd->>Cmd: Create SessionInfo
    Cmd->>Cmd: Wrap Handle in Arc Mutex
    Cmd->>Store: Lock SSH_SESSIONS
    Cmd->>Store: Insert StoredSession
    Cmd->>Store: Unlock SSH_SESSIONS

    Cmd-->>Client: SshConnectResponse
```

### Configuration Resolution Priority

Each configuration value follows the same resolution pattern.

```mermaid
flowchart TD
    Start([Resolve Config Value]) --> CheckParam{Parameter provided?}

    CheckParam -->|Yes| UseParam[Use parameter value]
    CheckParam -->|No| CheckEnv{Environment variable set?}

    CheckEnv -->|Yes| ParseEnv{Parse successful?}
    CheckEnv -->|No| UseDefault[Use default value]

    ParseEnv -->|Yes| UseEnv[Use environment value]
    ParseEnv -->|No| UseDefault

    UseParam --> Return([Return value])
    UseEnv --> Return
    UseDefault --> Return

    style Start fill:#e3f2fd
    style Return fill:#e8f5e9
```

### Address Parsing

The address is parsed to extract host and port using rsplit_once.

```mermaid
flowchart LR
    Input["Address String"] --> Check{Contains colon?}

    Check -->|Yes| Split["rsplit_once on colon"]
    Check -->|No| Default["Use default port 22"]

    Split --> ParsePort["Parse port as u16"]
    ParsePort --> Valid{Valid Port?}

    Valid -->|Yes| Return["Return host and port"]
    Valid -->|No| Error["Error Invalid port"]

    Default --> Return

    style Input fill:#e3f2fd
    style Return fill:#e8f5e9
    style Error fill:#ffebee
```

---

## Authentication Flow

Detailed authentication flow supporting multiple methods with RSA hash algorithm negotiation.

For RSA keys, the client negotiates the hash algorithm with the server using `best_supported_rsa_hash()`. This ensures modern algorithms like `rsa-sha2-256` or `rsa-sha2-512` are used instead of the legacy `ssh-rsa` with SHA1.

```mermaid
sequenceDiagram
    participant SSH as connect_to_ssh
    participant Handle as russh Handle
    participant Keys as russh keys
    participant Agent as SSH Agent
    participant Server as SSH Server

    Note over SSH: Check authentication method

    alt Password Authentication
        SSH->>Handle: authenticate_password with user and pass
        Handle->>Server: SSH_MSG_USERAUTH_REQUEST password
        Server-->>Handle: SSH_MSG_USERAUTH_SUCCESS or FAILURE
        Handle-->>SSH: AuthResult

    else Key File Authentication
        SSH->>Keys: load_secret_key from path
        Keys-->>SSH: KeyPair

        Note over SSH,Server: RSA Hash Algorithm Negotiation
        SSH->>Handle: best_supported_rsa_hash
        Handle->>Server: Query supported RSA algorithms
        Server-->>Handle: Supported algorithms list
        Handle-->>SSH: Preferred hash - rsa-sha2-256 or rsa-sha2-512

        SSH->>SSH: PrivateKeyWithHashAlg wrap key with hash_alg
        SSH->>Handle: authenticate_publickey with user and wrapped key
        Handle->>Server: SSH_MSG_USERAUTH_REQUEST publickey with negotiated algorithm
        Server-->>Handle: SSH_MSG_USERAUTH_SUCCESS or FAILURE
        Handle-->>SSH: AuthResult

    else SSH Agent Authentication
        SSH->>Agent: AgentClient::connect_env
        Agent-->>SSH: Connected
        SSH->>Agent: request_identities
        Agent-->>SSH: List of Keys

        loop For each identity
            Note over SSH,Server: RSA Hash Algorithm Negotiation per identity
            SSH->>Handle: best_supported_rsa_hash
            Handle->>Server: Query supported RSA algorithms
            Server-->>Handle: Supported algorithms list
            Handle-->>SSH: Preferred hash - rsa-sha2-256 or rsa-sha2-512

            SSH->>Handle: authenticate_publickey_with identity agent and hash_alg
            Handle->>Server: SSH_MSG_USERAUTH_REQUEST publickey with negotiated algorithm
            Server-->>Handle: SSH_MSG_USERAUTH_SUCCESS or FAILURE
            alt Success
                Handle-->>SSH: AuthResult success
                Note over SSH: Break loop
            else Failure
                Note over SSH: Try next identity
            end
        end
    end

    alt Auth Success
        SSH-->>SSH: Return handle
    else Auth Failure
        SSH-->>SSH: Return error
    end
```

### RSA Hash Algorithm Negotiation

The `best_supported_rsa_hash()` function queries the server for supported RSA signature algorithms and returns the best available option.

```mermaid
flowchart TD
    Start([best_supported_rsa_hash]) --> Query[Query server capabilities]
    Query --> CheckSupport{Server supports modern RSA?}

    CheckSupport -->|Yes| CheckSHA512{Supports rsa-sha2-512?}
    CheckSupport -->|No| Legacy[Return None - use legacy ssh-rsa]

    CheckSHA512 -->|Yes| UseSHA512[Return rsa-sha2-512]
    CheckSHA512 -->|No| CheckSHA256{Supports rsa-sha2-256?}

    CheckSHA256 -->|Yes| UseSHA256[Return rsa-sha2-256]
    CheckSHA256 -->|No| Legacy

    UseSHA512 --> End([Hash algorithm selected])
    UseSHA256 --> End
    Legacy --> End

    style Start fill:#e3f2fd
    style End fill:#e8f5e9
    style Legacy fill:#fff8e1
```

### Authentication Method Priority

```mermaid
flowchart TD
    Start([Authenticate]) --> CheckPassword{Password provided?}

    CheckPassword -->|Yes| PasswordAuth[authenticate_password]
    CheckPassword -->|No| CheckKey{Key path provided?}

    CheckKey -->|Yes| KeyAuth[authenticate_with_key]
    CheckKey -->|No| AgentAuth[authenticate_with_agent]

    subgraph KeyAuthFlow["Key File Authentication"]
        KeyAuth --> LoadKey[Load secret key from file]
        LoadKey --> NegotiateKey[best_supported_rsa_hash]
        NegotiateKey --> WrapKey[Wrap key with PrivateKeyWithHashAlg]
        WrapKey --> AuthKey[authenticate_publickey]
    end

    subgraph AgentAuthFlow["SSH Agent Authentication"]
        AgentAuth --> ConnectAgent[Connect to SSH agent]
        ConnectAgent --> GetIdentities[Request identities]
        GetIdentities --> LoopStart[For each identity]
        LoopStart --> NegotiateAgent[best_supported_rsa_hash]
        NegotiateAgent --> AuthAgent[authenticate_publickey_with hash_alg]
        AuthAgent --> CheckAgentResult{Success?}
        CheckAgentResult -->|No| LoopStart
        CheckAgentResult -->|Yes| AgentDone[Authentication complete]
    end

    PasswordAuth --> Result{auth_result.success?}
    AuthKey --> Result
    AgentDone --> Result

    Result -->|Yes| Success([Return handle])
    Result -->|No| Fail([Return error])

    style Start fill:#e3f2fd
    style Success fill:#e8f5e9
    style Fail fill:#ffebee
    style KeyAuthFlow fill:#f3e5f5
    style AgentAuthFlow fill:#fff8e1
```

---

## Command Execution Flow

Flow of the `ssh_execute` operation.

```mermaid
sequenceDiagram
    participant Client as MCP Client
    participant Cmd as McpSSHCommands
    participant Config as config.rs
    participant Store as SSH_SESSIONS
    participant Exec as execute_ssh_command
    participant Handle as russh Handle
    participant Channel as SSH Channel
    participant Server as Remote Server

    Client->>Cmd: ssh_execute request

    Note over Cmd,Config: Resolve timeout
    Cmd->>Config: resolve_command_timeout
    Config-->>Cmd: timeout value

    Note over Cmd,Store: Get session handle
    Cmd->>Store: Lock SSH_SESSIONS
    Cmd->>Store: Get session by ID
    Cmd->>Store: Clone Arc of handle
    Cmd->>Store: Unlock SSH_SESSIONS

    Note over Cmd: Wrap in tokio timeout
    Cmd->>Exec: execute_ssh_command with timeout

    Exec->>Handle: Lock handle mutex
    Exec->>Handle: channel_open_session
    Handle->>Server: SSH_MSG_CHANNEL_OPEN session
    Server-->>Handle: SSH_MSG_CHANNEL_OPEN_CONFIRMATION
    Handle-->>Exec: Channel

    Exec->>Channel: exec with command
    Channel->>Server: SSH_MSG_CHANNEL_REQUEST exec
    Server-->>Channel: SSH_MSG_CHANNEL_SUCCESS

    Exec->>Handle: Drop handle lock

    loop Read channel messages
        Channel->>Channel: wait
        alt Data message
            Channel-->>Exec: ChannelMsg Data
            Exec->>Exec: Append to stdout
        else ExtendedData with ext equals 1
            Channel-->>Exec: ChannelMsg ExtendedData
            Exec->>Exec: Append to stderr
        else ExitStatus message
            Channel-->>Exec: ChannelMsg ExitStatus
            Exec->>Exec: Store exit code
        else Eof message
            Channel-->>Exec: ChannelMsg Eof
            Note over Exec: Check if exit received
        else Close or None
            Channel-->>Exec: ChannelMsg Close or None
            Note over Exec: Break loop
        end
    end

    Exec->>Channel: close
    Exec->>Exec: Convert bytes to strings
    Exec-->>Cmd: SshCommandResponse

    alt Timeout
        Cmd-->>Client: Timeout error
    else Success
        Cmd-->>Client: SshCommandResponse
    end
```

### Channel Message Types

```mermaid
stateDiagram-v2
    [*] --> WaitingForData

    WaitingForData --> ProcessData: ChannelMsg Data
    WaitingForData --> ProcessStderr: ChannelMsg ExtendedData ext=1
    WaitingForData --> StoreExit: ChannelMsg ExitStatus
    WaitingForData --> CheckExit: ChannelMsg Eof
    WaitingForData --> Done: ChannelMsg Close
    WaitingForData --> Done: None

    ProcessData --> WaitingForData: Append to stdout
    ProcessStderr --> WaitingForData: Append to stderr
    StoreExit --> WaitingForData: Store exit code

    CheckExit --> Done: Exit code received
    CheckExit --> WaitingForData: Continue waiting

    Done --> [*]: Close channel and return

    note right of WaitingForData
        Loop until channel closes
        or EOF with exit status
    end note
```

---

## Port Forwarding Flow

Flow of the `ssh_forward` operation when port_forward feature is enabled.

```mermaid
sequenceDiagram
    participant Client as MCP Client
    participant Cmd as McpSSHCommands
    participant Store as SSH_SESSIONS
    participant Fwd as setup_port_forwarding
    participant Listener as TCP Listener
    participant Handler as Connection Handler
    participant Handle as russh Handle
    participant Channel as SSH Channel
    participant Remote as Remote Server

    Client->>Cmd: ssh_forward request

    Note over Cmd,Store: Get session handle
    Cmd->>Store: Lock SSH_SESSIONS
    Cmd->>Store: Get session by ID
    Cmd->>Store: Clone Arc of handle
    Cmd->>Store: Unlock SSH_SESSIONS

    Cmd->>Fwd: setup_port_forwarding

    Fwd->>Listener: TcpListener bind 127.0.0.1 local_port
    alt Bind Success
        Listener-->>Fwd: Listener
        Fwd->>Fwd: Get local address
        Fwd->>Fwd: tokio spawn listener task
        Fwd-->>Cmd: Local address
        Cmd-->>Client: PortForwardingResponse
    else Bind Failed
        Listener-->>Fwd: Error
        Fwd-->>Cmd: Error
        Cmd-->>Client: Error
    end

    Note over Listener: Background listener task

    loop Accept connections
        Listener->>Listener: accept
        alt Connection received
            Listener->>Handler: tokio spawn handler task
        else Accept error
            Note over Listener: Break loop
        end
    end

    Note over Handler: Per-connection handler

    Handler->>Handle: Lock handle mutex
    Handler->>Handle: channel_open_direct_tcpip
    Handle->>Remote: SSH_MSG_CHANNEL_OPEN direct-tcpip
    Remote-->>Handle: SSH_MSG_CHANNEL_OPEN_CONFIRMATION
    Handle-->>Handler: Channel
    Handler->>Handle: Drop handle lock

    Handler->>Channel: into_stream
    Handler->>Handler: Split local and channel streams

    par Bidirectional copy
        Handler->>Handler: tokio io copy local to channel
    and
        Handler->>Handler: tokio io copy channel to local
    end

    Note over Handler: tokio select completes when either direction ends
```

### Port Forwarding Data Flow

```mermaid
flowchart TD
    subgraph Setup["Setup Phase"]
        GetSession["Get session from store"]
        CloneArc["Clone Arc of Handle"]
        BindLocal["TcpListener bind 127.0.0.1 local_port"]
        SpawnTask["tokio spawn forward_task"]
    end

    GetSession --> CloneArc
    CloneArc --> BindLocal
    BindLocal --> BindResult{Bind Success?}
    BindResult -->|No| BindError([Return Bind Error])
    BindResult -->|Yes| SpawnTask
    SpawnTask --> ReturnResponse([Return PortForwardingResponse])

    subgraph ListenerTask["Background Listener Task"]
        AcceptLoop["Accept Loop"]
        Accept["listener.accept"]
        SpawnHandler["tokio spawn handle_connection"]
    end

    AcceptLoop --> Accept
    Accept --> AcceptResult{Connection?}
    AcceptResult -->|Error| LogBreak["Log error and break"]
    AcceptResult -->|Ok| SpawnHandler
    SpawnHandler --> AcceptLoop

    subgraph ConnectionHandler["Connection Handler Task"]
        LockHandle["Lock session handle"]
        OpenDirect["channel_open_direct_tcpip"]
        DropLock["Drop handle lock"]
        IntoStream["channel.into_stream"]
        SplitStreams["Split both streams"]

        subgraph Bidirectional["Bidirectional Copy"]
            LocalToRemote["tokio io copy local to channel"]
            RemoteToLocal["tokio io copy channel to local"]
            Select["tokio select waits for either"]
        end
    end

    LockHandle --> OpenDirect
    OpenDirect --> DropLock
    DropLock --> IntoStream
    IntoStream --> SplitStreams
    SplitStreams --> LocalToRemote
    SplitStreams --> RemoteToLocal
    LocalToRemote --> Select
    RemoteToLocal --> Select
    Select --> CloseConn["Connection closed"]

    style Setup fill:#e3f2fd
    style ListenerTask fill:#fff8e1
    style ConnectionHandler fill:#f3e5f5
    style Bidirectional fill:#e8f5e9
```

---

## Error Handling and Retry Logic

### Error Classification

The `is_retryable_error` function in error.rs classifies errors.

```mermaid
flowchart TD
    Error["Error Message"] --> ToLower["Convert to lowercase"]
    ToLower --> CheckAuth{Contains auth keyword?}

    CheckAuth -->|Yes| NonRetryable([Not Retryable])
    CheckAuth -->|No| CheckConn{Contains connection keyword?}

    CheckConn -->|Yes| Retryable([Retryable])
    CheckConn -->|No| CheckSSH{Contains ssh?}

    CheckSSH -->|No| DefaultRetry([Retryable - conservative default])
    CheckSSH -->|Yes| CheckTimeout{Contains timeout or connect?}

    CheckTimeout -->|Yes| Retryable
    CheckTimeout -->|No| NonRetryable

    subgraph AuthKeywords["Authentication Error Keywords"]
        Auth1["authentication failed"]
        Auth2["password authentication failed"]
        Auth3["key authentication failed"]
        Auth4["agent authentication failed"]
        Auth5["permission denied"]
        Auth6["publickey"]
        Auth7["auth fail"]
        Auth8["no authentication"]
        Auth9["all authentication methods failed"]
    end

    subgraph ConnKeywords["Connection Error Keywords"]
        Conn1["connection refused"]
        Conn2["connection reset"]
        Conn3["connection timed out"]
        Conn4["timeout"]
        Conn5["network is unreachable"]
        Conn6["no route to host"]
        Conn7["host is down"]
        Conn8["temporary failure"]
        Conn9["resource temporarily unavailable"]
        Conn10["handshake failed"]
        Conn11["failed to connect"]
        Conn12["broken pipe"]
        Conn13["would block"]
    end

    style NonRetryable fill:#ffebee
    style Retryable fill:#e8f5e9
    style DefaultRetry fill:#e8f5e9
    style AuthKeywords fill:#ffebee
    style ConnKeywords fill:#e8f5e9
```

### Exponential Backoff with Jitter

The retry logic uses backon ExponentialBuilder.

```mermaid
sequenceDiagram
    participant Client as connect_to_ssh_with_retry
    participant Backoff as backon Retryable
    participant SSH as connect_to_ssh
    participant Error as is_retryable_error

    Client->>Client: Build ExponentialBuilder
    Note over Client: min_delay from retry_delay_ms
    Note over Client: max_delay MAX_RETRY_DELAY_SECS 10s
    Note over Client: max_times from max_retries
    Note over Client: with_jitter enabled

    Client->>Backoff: Wrap connect_fn in retry

    loop Until success or max retries
        Backoff->>SSH: Attempt connection
        alt Success
            SSH-->>Backoff: Handle
            Backoff-->>Client: Handle and retry_count
        else Failure
            SSH-->>Backoff: Error
            Backoff->>Error: when callback
            Error-->>Backoff: is_retryable result
            alt Retryable
                Backoff->>Backoff: notify callback logs retry
                Note over Backoff: Wait with exponential delay plus jitter
                Note over Backoff: Delay doubles each attempt
                Note over Backoff: Capped at 10 seconds
            else Not Retryable
                Backoff-->>Client: Error immediately
            end
        end
    end

    Note over Client: Return handle with retry_count or final error
```

### Retry Timeline Example

```mermaid
flowchart LR
    subgraph Backoff["Backoff Configuration"]
        Min["min_delay 1000ms"]
        Max["max_delay 10s cap"]
        MaxRetries["max_times 3"]
        Jitter["jitter enabled"]
    end

    subgraph Timeline["Retry Timeline Example"]
        T0["Attempt 1"] --> D1["Delay approx 1s"]
        D1 --> T1["Attempt 2"]
        T1 --> D2["Delay approx 2s"]
        D2 --> T2["Attempt 3"]
        T2 --> D3["Delay approx 4s"]
        D3 --> T3["Attempt 4"]
        T3 --> End["Max retries exceeded"]
    end

    Min --> Timeline
    Jitter --> D1
    Jitter --> D2
    Jitter --> D3

    style Backoff fill:#e3f2fd
    style Timeline fill:#fff8e1
```

### Retry Notification Flow

```mermaid
sequenceDiagram
    participant Client as ssh_connect
    participant Backon as backon Retryable
    participant SSH as connect_to_ssh
    participant Server as SSH Server

    Client->>Backon: retry connect_fn

    Backon->>SSH: Attempt 1
    SSH->>Server: Connect
    Server-->>SSH: Connection refused
    SSH-->>Backon: Error

    Backon->>Backon: when is_retryable returns true
    Backon->>Backon: notify logs retry with delay
    Note over Backon: Wait approximately 1.2s with jitter

    Backon->>SSH: Attempt 2
    SSH->>Server: Connect
    Server-->>SSH: Timeout
    SSH-->>Backon: Error

    Backon->>Backon: when is_retryable returns true
    Backon->>Backon: notify logs retry with delay
    Note over Backon: Wait approximately 2.5s with jitter

    Backon->>SSH: Attempt 3
    SSH->>Server: Connect
    Server-->>SSH: Connected
    SSH->>Server: Authenticate
    Server-->>SSH: Auth Success
    SSH-->>Backon: Success

    Backon-->>Client: Handle with retry_count equals 2
```

---

## Module Responsibilities

| Module | Responsibility |
|--------|----------------|
| `commands.rs` | MCP tool entry points and response building |
| `client.rs` | SSH connection, authentication, and command execution |
| `session.rs` | Global session storage and russh handler |
| `config.rs` | Configuration resolution with priority chain |
| `error.rs` | Error classification for retry decisions |
| `forward.rs` | Port forwarding with bidirectional IO |
