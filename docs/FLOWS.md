# SSH MCP Operation Flows

This document describes the operational flows of the SSH MCP server, including connection establishment, command execution, port forwarding, and session lifecycle management.

## Table of Contents

- [Session Lifecycle](#session-lifecycle)
- [SSH Connection Flow](#ssh-connection-flow)
- [Authentication Flow](#authentication-flow)
- [Command Execution Flow](#command-execution-flow)
- [Async Command Execution Flow](#async-command-execution-flow)
- [Async Command Lifecycle](#async-command-lifecycle)
- [Command Cancellation Flow](#command-cancellation-flow)
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

### Session Properties

| Property | Description |
|----------|-------------|
| `name` | Optional human-readable identifier for LLM identification |
| `persistent` | When true, disables inactivity timeout (keepalive still active) |

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
    Cmd->>Cmd: Create SessionInfo with optional name
    Cmd->>Cmd: Set persistent flag if requested
    Cmd->>Cmd: Wrap Handle in Arc Mutex
    Cmd->>Store: Lock SSH_SESSIONS
    Cmd->>Store: Insert StoredSession
    Cmd->>Store: Unlock SSH_SESSIONS

    Cmd-->>Client: SshConnectResponse with persistent indicator
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
        Note over Cmd: Timeout reached
        Cmd->>Cmd: Close channel gracefully
        Cmd->>Cmd: Set timed_out = true
        Cmd->>Cmd: Set exit_code = -1
        Note over Cmd: Session stays alive
        Cmd-->>Client: SshCommandResponse with partial output
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
    WaitingForData --> Timeout: Timeout reached

    ProcessData --> WaitingForData: Append to stdout
    ProcessStderr --> WaitingForData: Append to stderr
    StoreExit --> WaitingForData: Store exit code

    CheckExit --> Done: Exit code received
    CheckExit --> WaitingForData: Continue waiting

    Timeout --> Done: Close channel gracefully

    Done --> [*]: Return response

    note right of WaitingForData
        Loop until channel closes
        or EOF with exit status
    end note

    note right of Timeout
        Returns partial output
        timed_out = true
        exit_code = -1
        Session stays alive
    end note
```

### Command Timeout Handling

When a command exceeds the configured timeout (`SSH_COMMAND_TIMEOUT`), the system handles it gracefully without disconnecting the session.

```mermaid
flowchart TD
    Start([Command Execution]) --> Execute[Execute command with timeout wrapper]
    Execute --> Wait{Timeout reached?}

    Wait -->|No| Complete[Command completes normally]
    Wait -->|Yes| Timeout[Timeout triggered]

    Complete --> BuildResponse[Build response with actual exit code]
    BuildResponse --> ReturnSuccess([Return SshCommandResponse])

    Timeout --> CollectPartial[Collect partial stdout/stderr]
    CollectPartial --> CloseChannel[Close channel gracefully]
    CloseChannel --> SetFlags[Set timed_out = true and exit_code = -1]
    SetFlags --> KeepSession[Session remains in SSH_SESSIONS]
    KeepSession --> ReturnPartial([Return SshCommandResponse with partial output])

    style Start fill:#e3f2fd
    style ReturnSuccess fill:#e8f5e9
    style ReturnPartial fill:#fff8e1
    style Timeout fill:#fff8e1
```

**Timeout Response Fields:**

| Field | Value | Description |
|-------|-------|-------------|
| `stdout` | Partial output | Any stdout received before timeout |
| `stderr` | Partial output | Any stderr received before timeout |
| `exit_code` | `-1` | Indicates abnormal termination |
| `timed_out` | `true` | Signals timeout occurred |

**Key Behavior:**
- The SSH session remains connected and can be reused for subsequent commands
- No error is returned; instead, a valid response with partial output is provided
- The channel is closed gracefully to avoid resource leaks
- Clients should check the `timed_out` flag to detect timeout conditions

---

## Async Command Execution Flow

Flow of the `ssh_execute_async` operation for long-running commands that return immediately with a command ID for polling.

```mermaid
sequenceDiagram
    participant Client as MCP Client
    participant Cmd as McpSSHCommands
    participant AsyncStore as ASYNC_COMMANDS
    participant Store as SSH_SESSIONS
    participant Task as Background Task
    participant Handle as russh Handle
    participant Channel as SSH Channel
    participant Server as Remote Server

    Client->>Cmd: ssh_execute_async request

    Note over Cmd,AsyncStore: Check session command limit
    Cmd->>AsyncStore: count_session_commands
    AsyncStore-->>Cmd: current_count

    alt Limit reached
        Cmd-->>Client: Error max commands reached
    end

    Note over Cmd,Store: Get session handle
    Cmd->>Store: Lock SSH_SESSIONS
    Cmd->>Store: Get session by ID
    Cmd->>Store: Clone Arc of handle
    Cmd->>Store: Unlock SSH_SESSIONS

    Note over Cmd: Generate command_id UUID

    Note over Cmd,AsyncStore: Create shared state
    Cmd->>Cmd: Create watch channel for status
    Cmd->>Cmd: Create OutputBuffer Arc Mutex
    Cmd->>Cmd: Create exit_code Arc Mutex
    Cmd->>Cmd: Create error Arc Mutex
    Cmd->>Cmd: Create timed_out AtomicBool
    Cmd->>Cmd: Create CancellationToken

    Note over Cmd,AsyncStore: Store running command
    Cmd->>AsyncStore: Lock ASYNC_COMMANDS
    Cmd->>AsyncStore: Insert RunningCommand
    Cmd->>AsyncStore: Unlock ASYNC_COMMANDS

    Note over Cmd,Task: Spawn background task
    Cmd->>Task: tokio spawn execute_ssh_command_async

    Note over Cmd,Client: Return immediately
    Cmd-->>Client: SshExecuteAsyncResponse with command_id

    Note over Task: Background execution begins

    Task->>Handle: channel_open_session
    Handle->>Server: SSH_MSG_CHANNEL_OPEN session
    Server-->>Handle: SSH_MSG_CHANNEL_OPEN_CONFIRMATION
    Handle-->>Task: Channel

    Task->>Channel: exec with command
    Channel->>Server: SSH_MSG_CHANNEL_REQUEST exec
    Server-->>Channel: SSH_MSG_CHANNEL_SUCCESS

    Note over Task: tokio select with biased ordering

    loop Collect output via select
        alt Cancellation signaled
            Task->>Task: cancel_token.cancelled
            Task->>Channel: close
            Task->>Task: status_tx.send Cancelled
        else Timeout reached
            Task->>Task: tokio time sleep timeout
            Task->>Task: timed_out.store true
            Task->>Channel: close
            Task->>Task: status_tx.send Completed
        else Output received
            Task->>Task: collect_async_output
            Channel-->>Task: ChannelMsg Data or ExtendedData
            Task->>Task: Append to output buffer
            Task->>Task: Store exit_code
            Task->>Task: status_tx.send Completed
        end
    end
```

### Polling for Output

The `ssh_get_command_output` tool allows clients to poll for command status and output.

```mermaid
sequenceDiagram
    participant Client as MCP Client
    participant Cmd as McpSSHCommands
    participant AsyncStore as ASYNC_COMMANDS
    participant Task as Background Task

    Client->>Cmd: ssh_get_command_output request

    Note over Cmd,AsyncStore: Get command state
    Cmd->>AsyncStore: Lock ASYNC_COMMANDS
    Cmd->>AsyncStore: Get command by ID
    Cmd->>Cmd: Clone status_rx output exit_code error timed_out
    Cmd->>AsyncStore: Unlock ASYNC_COMMANDS

    alt wait equals true
        Note over Cmd: Wait for completion with timeout
        Cmd->>Cmd: Clone status_rx
        loop Wait loop with timeout
            Cmd->>Cmd: Check status_rx.borrow
            alt Status not Running
                Note over Cmd: Break loop
            else Status Running
                Cmd->>Cmd: status_rx.changed await
            end
        end
    end

    Note over Cmd: Read current state
    Cmd->>Cmd: status_rx.borrow
    Cmd->>Cmd: output.lock
    Cmd->>Cmd: exit_code.lock
    Cmd->>Cmd: error.lock
    Cmd->>Cmd: timed_out.load

    Cmd-->>Client: SshAsyncOutputResponse
```

### Async Command State Machine

```mermaid
stateDiagram-v2
    [*] --> NotStarted

    NotStarted --> Running: ssh_execute_async called

    Running --> Collecting: Channel opened

    Collecting --> Collecting: Data received
    Collecting --> Completed: Channel closed normally
    Collecting --> Completed: Timeout reached
    Collecting --> Cancelled: Cancel token triggered
    Collecting --> Failed: Channel open error
    Collecting --> Failed: Exec error

    Completed --> [*]
    Cancelled --> [*]
    Failed --> [*]

    note right of Running
        Background task spawned
        command_id returned to client
    end note

    note right of Collecting
        Output buffered incrementally
        Client can poll anytime
    end note

    note right of Completed
        timed_out flag indicates
        if timeout was hit
    end note
```

### Async Command Limits

```mermaid
flowchart TD
    Start([ssh_execute_async]) --> CountCommands[Count session commands]
    CountCommands --> CheckLimit{count >= 10?}

    CheckLimit -->|Yes| RejectError([Error: Max commands reached])
    CheckLimit -->|No| GetSession[Get session handle]

    GetSession --> SessionExists{Session found?}
    SessionExists -->|No| SessionError([Error: No active session])
    SessionExists -->|Yes| CreateState[Create shared state]

    CreateState --> Store[Store in ASYNC_COMMANDS]
    Store --> Spawn[Spawn background task]
    Spawn --> Return([Return command_id])

    style Start fill:#e3f2fd
    style Return fill:#e8f5e9
    style RejectError fill:#ffebee
    style SessionError fill:#ffebee
```

---

## Async Command Lifecycle

Complete lifecycle of an async command from creation to cleanup.

```mermaid
stateDiagram-v2
    [*] --> Pending: Client calls ssh_execute_async

    state Pending {
        [*] --> ValidateSession
        ValidateSession --> CheckLimit
        CheckLimit --> CreateState
        CreateState --> StoreCommand
        StoreCommand --> SpawnTask
    }

    Pending --> Running: Task spawned successfully
    Pending --> Error: Validation failed

    state Running {
        [*] --> OpenChannel
        OpenChannel --> ExecuteCommand
        ExecuteCommand --> CollectOutput

        state CollectOutput {
            [*] --> WaitForData
            WaitForData --> ProcessData: Data received
            WaitForData --> ProcessStderr: Stderr received
            ProcessData --> WaitForData
            ProcessStderr --> WaitForData
            WaitForData --> StoreExit: Exit status received
            StoreExit --> WaitForData
            WaitForData --> [*]: Channel closed
        }
    }

    Running --> Completed: Normal completion
    Running --> Completed: Timeout with partial output
    Running --> Cancelled: Cancel requested
    Running --> Failed: Channel or exec error

    state Completed {
        [*] --> OutputAvailable
        OutputAvailable --> AwaitingCleanup
    }

    state Cancelled {
        [*] --> PartialOutput
        PartialOutput --> AwaitingCleanup
    }

    state Failed {
        [*] --> ErrorStored
        ErrorStored --> AwaitingCleanup
    }

    Completed --> Cleaned: Automatic cleanup after 5 minutes
    Cancelled --> Cleaned: Automatic cleanup after 5 minutes
    Failed --> Cleaned: Automatic cleanup after 5 minutes

    Cleaned --> [*]

    note right of Running
        Client can poll with
        ssh_get_command_output
        at any time
    end note

    note right of Completed
        exit_code and timed_out
        fields indicate result
    end note
```

### Status Transitions

| From | To | Trigger |
|------|-----|---------|
| Pending | Running | Task spawned, channel open successful |
| Running | Completed | Normal exit or timeout |
| Running | Cancelled | `ssh_cancel_command` called |
| Running | Failed | Channel open error, exec error |

### AsyncCommandStatus Values

```mermaid
flowchart LR
    subgraph StatusValues["AsyncCommandStatus Enum"]
        Running["Running"]
        Completed["Completed"]
        Cancelled["Cancelled"]
        Failed["Failed"]
    end

    subgraph Indicators["Response Indicators"]
        ExitCode["exit_code: Option<i32>"]
        TimedOut["timed_out: bool"]
        Error["error: Option<String>"]
    end

    Running --> |"Normal finish"| Completed
    Running --> |"Cancel called"| Cancelled
    Running --> |"Error occurred"| Failed
    Running --> |"Timeout"| Completed

    Completed --- ExitCode
    Completed --- TimedOut
    Cancelled --- ExitCode
    Failed --- Error

    style Running fill:#fff8e1
    style Completed fill:#e8f5e9
    style Cancelled fill:#e3f2fd
    style Failed fill:#ffebee
```

### Output Collection Flow

```mermaid
flowchart TD
    subgraph BackgroundTask["Background Task"]
        OpenCh["Open channel"]
        ExecCmd["Execute command"]
        SelectLoop["tokio select loop"]

        subgraph SelectBranches["Select Branches - Biased"]
            CancelBranch["1. cancel_token.cancelled"]
            TimeoutBranch["2. tokio time sleep"]
            OutputBranch["3. collect_async_output"]
        end
    end

    subgraph SharedState["Shared State Arc Mutex"]
        OutputBuf["OutputBuffer stdout stderr"]
        ExitCode["exit_code Option i32"]
        ErrorMsg["error Option String"]
        TimedOutFlag["timed_out AtomicBool"]
        StatusTx["status_tx watch Sender"]
    end

    OpenCh --> ExecCmd
    ExecCmd --> SelectLoop
    SelectLoop --> SelectBranches

    CancelBranch --> |"Set"| StatusTx
    TimeoutBranch --> |"Set true"| TimedOutFlag
    TimeoutBranch --> |"Set"| StatusTx
    OutputBranch --> |"Append"| OutputBuf
    OutputBranch --> |"Set"| ExitCode
    OutputBranch --> |"Set"| StatusTx

    style BackgroundTask fill:#e3f2fd
    style SharedState fill:#f3e5f5
    style SelectBranches fill:#fff8e1
```

---

## Command Cancellation Flow

Flow of the `ssh_cancel_command` operation to stop a running async command.

```mermaid
sequenceDiagram
    participant Client as MCP Client
    participant Cmd as McpSSHCommands
    participant AsyncStore as ASYNC_COMMANDS
    participant Task as Background Task
    participant Channel as SSH Channel

    Client->>Cmd: ssh_cancel_command request

    Note over Cmd,AsyncStore: Get command state
    Cmd->>AsyncStore: Lock ASYNC_COMMANDS
    Cmd->>AsyncStore: Get command by ID
    Cmd->>Cmd: Check current status

    alt Status not Running
        Cmd-->>Client: Error command not running
    end

    Cmd->>Cmd: Clone cancel_token output status_rx
    Cmd->>AsyncStore: Unlock ASYNC_COMMANDS

    Note over Cmd,Task: Signal cancellation
    Cmd->>Task: cancel_token.cancel

    Note over Cmd: Wait briefly for effect
    Cmd->>Cmd: status_rx.changed with 2s timeout

    Note over Task: Background task receives signal
    Task->>Task: cancel_token.cancelled returns
    Task->>Channel: close
    Task->>Task: status_tx.send Cancelled

    Note over Cmd: Get final output
    Cmd->>Cmd: output.lock
    Cmd->>Cmd: Convert bytes to strings

    Cmd-->>Client: SshCancelCommandResponse with partial output
```

### Cancellation Signal Flow

```mermaid
flowchart TD
    subgraph ClientSide["Client Side"]
        CancelCall["ssh_cancel_command called"]
        GetToken["Get cancel_token from store"]
        SignalCancel["cancel_token.cancel"]
        WaitStatus["Wait for status change 2s"]
        ReturnOutput["Return partial output"]
    end

    subgraph BackgroundTask["Background Task - tokio select"]
        SelectWait["select biased wait"]
        CancelCheck["cancel_token.cancelled - Branch 1"]
        TimeoutCheck["sleep timeout - Branch 2"]
        OutputCheck["collect_async_output - Branch 3"]
    end

    subgraph Cleanup["Cleanup Actions"]
        CloseChannel["Close SSH channel"]
        UpdateStatus["status_tx.send Cancelled"]
        LogCancel["Log cancellation"]
    end

    CancelCall --> GetToken
    GetToken --> SignalCancel
    SignalCancel -.->|"Token signaled"| CancelCheck
    SignalCancel --> WaitStatus

    SelectWait --> CancelCheck
    CancelCheck -->|"Token cancelled"| CloseChannel
    CloseChannel --> UpdateStatus
    UpdateStatus --> LogCancel

    WaitStatus -->|"Status changed"| ReturnOutput

    style ClientSide fill:#e3f2fd
    style BackgroundTask fill:#fff8e1
    style Cleanup fill:#e8f5e9
```

### Cancellation State Transitions

```mermaid
stateDiagram-v2
    [*] --> Running: Command executing

    Running --> CancelRequested: ssh_cancel_command called

    CancelRequested --> TokenSignaled: cancel_token.cancel

    TokenSignaled --> SelectDetects: tokio select biased

    SelectDetects --> ChannelClosing: cancel branch wins

    ChannelClosing --> StatusUpdated: status_tx.send Cancelled

    StatusUpdated --> Cancelled: Final state

    Cancelled --> [*]

    note right of CancelRequested
        Client sends cancel request
        Gets partial output back
    end note

    note right of TokenSignaled
        CancellationToken is shared
        between client and task
    end note

    note right of SelectDetects
        Biased select checks cancel
        token first before other branches
    end note
```

### Partial Output Recovery

When a command is cancelled, the client receives all output collected up to that point.

```mermaid
flowchart LR
    subgraph BeforeCancel["Before Cancellation"]
        Stdout1["stdout: partial data"]
        Stderr1["stderr: partial data"]
        Status1["status: Running"]
    end

    subgraph CancelAction["Cancel Action"]
        Signal["cancel_token.cancel"]
        Wait["Wait up to 2s"]
    end

    subgraph AfterCancel["After Cancellation"]
        Stdout2["stdout: preserved"]
        Stderr2["stderr: preserved"]
        Status2["status: Cancelled"]
        Cancelled2["cancelled: true"]
    end

    BeforeCancel --> CancelAction
    CancelAction --> AfterCancel

    Stdout1 -.->|"Preserved"| Stdout2
    Stderr1 -.->|"Preserved"| Stderr2

    style BeforeCancel fill:#fff8e1
    style CancelAction fill:#ffebee
    style AfterCancel fill:#e8f5e9
```

### Cancel Response Fields

| Field | Type | Description |
|-------|------|-------------|
| `command_id` | String | The cancelled command's ID |
| `cancelled` | bool | Always `true` on success |
| `message` | String | Confirmation message |
| `stdout` | String | Output collected before cancellation |
| `stderr` | String | Error output collected before cancellation |

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
