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

The complete lifecycle of an SSH session from creation to termination:

```mermaid
stateDiagram-v2
    [*] --> Disconnected: Initial State

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

    Disconnected --> [*]: Session Removed

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
| `Disconnected` | No active connection; session not in store |
| `Connecting` | TCP connection in progress with retry logic |
| `Authenticating` | Connection established; auth in progress |
| `Connected` | Fully connected and ready for operations |
| `Executing` | Command execution in progress |
| `Forwarding` | Port forwarding setup in progress |
| `Disconnecting` | Graceful disconnect in progress |

---

## SSH Connection Flow

Detailed flow of the `ssh_connect` operation:

```mermaid
flowchart TD
    Start([ssh_connect called]) --> ResolveConfig

    subgraph ConfigResolution["Configuration Resolution"]
        ResolveConfig[Resolve Configuration]
        ResolveConfig --> Timeout["timeout_secs<br/>param -> env -> 30s"]
        ResolveConfig --> Retries["max_retries<br/>param -> env -> 3"]
        ResolveConfig --> Delay["retry_delay_ms<br/>param -> env -> 1000"]
        ResolveConfig --> Compress["compress<br/>param -> env -> true"]
    end

    Timeout --> BuildBackoff
    Retries --> BuildBackoff
    Delay --> BuildBackoff
    Compress --> BuildConfig

    BuildBackoff[Build Exponential Backoff]
    BuildConfig[Build SSH Client Config]

    BuildBackoff --> RetryLoop
    BuildConfig --> RetryLoop

    subgraph RetryLoop["Retry Loop (backon)"]
        AttemptConnect[Attempt Connection]
        AttemptConnect --> ParseAddr[Parse Address]
        ParseAddr --> TCPConnect["client::connect()"]
        TCPConnect --> TimeoutCheck{Timeout?}

        TimeoutCheck -->|Yes| HandleError[Handle Error]
        TimeoutCheck -->|No| ConnectResult{Connected?}

        ConnectResult -->|No| HandleError
        ConnectResult -->|Yes| Authenticate

        HandleError --> ClassifyError{Retryable?}
        ClassifyError -->|Yes| WaitBackoff[Wait with Backoff + Jitter]
        ClassifyError -->|No| Fail

        WaitBackoff --> AttemptConnect
    end

    subgraph Authentication["Authentication"]
        Authenticate{Auth Method?}
        Authenticate -->|Password| PasswordAuth["authenticate_password()"]
        Authenticate -->|Key File| KeyAuth["authenticate_publickey()"]
        Authenticate -->|Agent| AgentAuth["authenticate_publickey_with()"]

        PasswordAuth --> AuthResult
        KeyAuth --> AuthResult
        AgentAuth --> AuthResult

        AuthResult{Success?}
        AuthResult -->|No| Fail
        AuthResult -->|Yes| StoreSession
    end

    subgraph SessionStorage["Session Storage"]
        StoreSession[Generate UUID]
        StoreSession --> CreateInfo["Create SessionInfo<br/>(metadata)"]
        CreateInfo --> WrapHandle["Wrap Handle in Arc<Mutex>"]
        WrapHandle --> LockStore["Lock SSH_SESSIONS"]
        LockStore --> Insert["Insert StoredSession"]
        Insert --> UnlockStore["Unlock SSH_SESSIONS"]
    end

    UnlockStore --> Success([Return SshConnectResponse])
    Fail([Return Error])

    style ConfigResolution fill:#e3f2fd
    style RetryLoop fill:#fff8e1
    style Authentication fill:#f3e5f5
    style SessionStorage fill:#e8f5e9
```

### Address Parsing

The address is parsed to extract host and port:

```mermaid
flowchart LR
    Input["Address String"] --> Check{Contains ':'?}

    Check -->|Yes| Split["rsplit_once(':')"]
    Check -->|No| Default["Use default port 22"]

    Split --> ParsePort["Parse port as u16"]
    ParsePort --> Valid{Valid Port?}

    Valid -->|Yes| Return["(host, port)"]
    Valid -->|No| Error["Error: Invalid port"]

    Default --> Return

    style Input fill:#e3f2fd
    style Return fill:#e8f5e9
    style Error fill:#ffebee
```

---

## Authentication Flow

Detailed authentication flow supporting multiple methods:

```mermaid
sequenceDiagram
    participant Client as MCP Client
    participant SSH as ssh_connect
    participant Handle as SSH Handle
    participant Server as SSH Server
    participant Agent as SSH Agent

    Client->>SSH: ssh_connect(address, user, ...)

    alt Password Authentication
        SSH->>Handle: authenticate_password(user, pass)
        Handle->>Server: SSH_MSG_USERAUTH_REQUEST (password)
        Server-->>Handle: SSH_MSG_USERAUTH_SUCCESS/FAILURE
        Handle-->>SSH: AuthResult
    else Key File Authentication
        SSH->>SSH: load_secret_key(path)
        SSH->>Handle: authenticate_publickey(user, key)
        Handle->>Server: SSH_MSG_USERAUTH_REQUEST (publickey)
        Server-->>Handle: SSH_MSG_USERAUTH_SUCCESS/FAILURE
        Handle-->>SSH: AuthResult
    else SSH Agent Authentication
        SSH->>Agent: connect_env()
        Agent-->>SSH: Connected
        SSH->>Agent: request_identities()
        Agent-->>SSH: List of Keys

        loop For each identity
            SSH->>Handle: authenticate_publickey_with(user, identity, agent)
            Handle->>Server: SSH_MSG_USERAUTH_REQUEST (publickey)
            Server-->>Handle: SSH_MSG_USERAUTH_SUCCESS/FAILURE
            alt Success
                Handle-->>SSH: AuthResult (success)
            else Failure
                Note over SSH: Try next identity
            end
        end
    end

    alt Auth Success
        SSH->>SSH: Store session
        SSH-->>Client: SshConnectResponse
    else Auth Failure
        SSH-->>Client: Error
    end
```

### Authentication Method Priority

```mermaid
flowchart TD
    Start([Authenticate]) --> CheckPassword{Password<br/>provided?}

    CheckPassword -->|Yes| PasswordAuth[Password Authentication]
    CheckPassword -->|No| CheckKey{Key path<br/>provided?}

    CheckKey -->|Yes| KeyAuth[Key File Authentication]
    CheckKey -->|No| AgentAuth[SSH Agent Authentication]

    PasswordAuth --> Result{Success?}
    KeyAuth --> Result
    AgentAuth --> Result

    Result -->|Yes| Success([Authenticated])
    Result -->|No| Fail([Authentication Failed])

    style Start fill:#e3f2fd
    style Success fill:#e8f5e9
    style Fail fill:#ffebee
```

---

## Command Execution Flow

Flow of the `ssh_execute` operation:

```mermaid
flowchart TD
    Start([ssh_execute called]) --> ResolveTimeout

    subgraph Preparation["Preparation"]
        ResolveTimeout["Resolve timeout<br/>param -> env -> 180s"]
        ResolveTimeout --> GetSession["Get session from store"]
        GetSession --> CloneArc["Clone Arc<Handle>"]
        CloneArc --> ReleaseLock["Release global lock"]
    end

    ReleaseLock --> WrapTimeout["Wrap in tokio::timeout"]

    subgraph Execution["Command Execution"]
        WrapTimeout --> LockHandle["Lock session handle"]
        LockHandle --> OpenChannel["channel_open_session()"]
        OpenChannel --> ExecCommand["channel.exec(command)"]
        ExecCommand --> DropLock["Drop handle lock"]
        DropLock --> ReadLoop["Read channel messages"]
    end

    subgraph MessageLoop["Message Processing Loop"]
        ReadLoop --> WaitMsg["channel.wait()"]
        WaitMsg --> MsgType{Message Type?}

        MsgType -->|Data| StoreStdout["Append to stdout"]
        MsgType -->|ExtendedData ext=1| StoreStderr["Append to stderr"]
        MsgType -->|ExitStatus| StoreExit["Store exit code"]
        MsgType -->|Eof| CheckExit{Exit received?}
        MsgType -->|Close| Done
        MsgType -->|None| Done

        StoreStdout --> WaitMsg
        StoreStderr --> WaitMsg
        StoreExit --> WaitMsg
        CheckExit -->|Yes| Done
        CheckExit -->|No| WaitMsg
    end

    Done --> CloseChannel["Close channel"]

    subgraph Results["Result Handling"]
        CloseChannel --> BuildResponse["Build SshCommandResponse"]
        BuildResponse --> Success([Return Response])
    end

    WrapTimeout -->|Timeout| TimeoutError([Return Timeout Error])

    style Preparation fill:#e3f2fd
    style Execution fill:#fff8e1
    style MessageLoop fill:#f3e5f5
    style Results fill:#e8f5e9
```

### Channel Message Types

```mermaid
sequenceDiagram
    participant Client as ssh_execute
    participant Channel as SSH Channel
    participant Server as Remote Server

    Client->>Channel: exec("ls -la")
    Channel->>Server: SSH_MSG_CHANNEL_REQUEST (exec)
    Server-->>Channel: SSH_MSG_CHANNEL_SUCCESS

    loop Until EOF/Close
        Server-->>Channel: SSH_MSG_CHANNEL_DATA (stdout)
        Channel-->>Client: ChannelMsg::Data

        Server-->>Channel: SSH_MSG_CHANNEL_EXTENDED_DATA (stderr)
        Channel-->>Client: ChannelMsg::ExtendedData

        Server-->>Channel: SSH_MSG_CHANNEL_REQUEST (exit-status)
        Channel-->>Client: ChannelMsg::ExitStatus

        Server-->>Channel: SSH_MSG_CHANNEL_EOF
        Channel-->>Client: ChannelMsg::Eof

        Server-->>Channel: SSH_MSG_CHANNEL_CLOSE
        Channel-->>Client: ChannelMsg::Close
    end

    Client->>Channel: close()
```

---

## Port Forwarding Flow

Flow of the `ssh_forward` operation (requires `port_forward` feature):

```mermaid
flowchart TD
    Start([ssh_forward called]) --> GetSession

    subgraph Setup["Setup Phase"]
        GetSession["Get session from store"]
        GetSession --> CloneArc["Clone Arc<Handle>"]
        CloneArc --> BindLocal["TcpListener::bind(127.0.0.1:local_port)"]
        BindLocal --> BindResult{Bind Success?}
        BindResult -->|No| BindError([Return Bind Error])
        BindResult -->|Yes| SpawnTask["tokio::spawn(forward_task)"]
    end

    SpawnTask --> ReturnResponse([Return PortForwardingResponse])

    subgraph ListenerTask["Background Listener Task"]
        AcceptLoop["Accept Loop"]
        AcceptLoop --> Accept["listener.accept()"]
        Accept --> AcceptResult{Connection?}
        AcceptResult -->|Error| LogBreak["Log error, break"]
        AcceptResult -->|Ok| SpawnHandler["tokio::spawn(handle_connection)"]
        SpawnHandler --> AcceptLoop
    end

    subgraph ConnectionHandler["Connection Handler Task"]
        HandleConn["handle_port_forward_connection"]
        HandleConn --> LockHandle["Lock session handle"]
        LockHandle --> OpenDirect["channel_open_direct_tcpip()<br/>(remote_host, remote_port)"]
        OpenDirect --> DropLock["Drop handle lock"]
        DropLock --> ConvertStream["channel.into_stream()"]
        ConvertStream --> SplitStreams["Split both streams"]

        subgraph Bidirectional["Bidirectional Copy"]
            SplitStreams --> LocalToRemote["tokio::io::copy<br/>local -> channel"]
            SplitStreams --> RemoteToLocal["tokio::io::copy<br/>channel -> local"]

            LocalToRemote --> Select["tokio::select!"]
            RemoteToLocal --> Select
        end

        Select --> CloseConn["Connection closed"]
    end

    style Setup fill:#e3f2fd
    style ListenerTask fill:#fff8e1
    style ConnectionHandler fill:#f3e5f5
    style Bidirectional fill:#e8f5e9
```

### Port Forwarding Data Flow

```mermaid
sequenceDiagram
    participant LocalApp as Local Application
    participant Listener as TCP Listener<br/>(127.0.0.1:local_port)
    participant Handler as Connection Handler
    participant Channel as SSH Channel
    participant Remote as Remote Server<br/>(remote_host:remote_port)

    Note over Listener: ssh_forward spawns background task

    LocalApp->>Listener: TCP Connect
    Listener->>Handler: Spawn new task

    Handler->>Channel: channel_open_direct_tcpip()
    Channel->>Remote: SSH_MSG_CHANNEL_OPEN (direct-tcpip)
    Remote-->>Channel: SSH_MSG_CHANNEL_OPEN_CONFIRMATION

    Note over Handler,Channel: Bidirectional forwarding active

    loop Data Transfer
        LocalApp->>Listener: Send data
        Listener->>Handler: Forward to handler
        Handler->>Channel: Write to channel stream
        Channel->>Remote: SSH_MSG_CHANNEL_DATA

        Remote->>Channel: SSH_MSG_CHANNEL_DATA
        Channel->>Handler: Read from channel stream
        Handler->>Listener: Forward to listener
        Listener->>LocalApp: Receive data
    end

    LocalApp->>Listener: Close connection
    Handler->>Channel: Close channel
    Channel->>Remote: SSH_MSG_CHANNEL_CLOSE
```

---

## Error Handling and Retry Logic

### Error Classification

```mermaid
flowchart TD
    Error["Error Message"] --> Classify{Classify Error}

    subgraph NonRetryable["Non-Retryable Errors"]
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

    subgraph Retryable["Retryable Errors"]
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

    Classify --> CheckAuth{Contains auth<br/>keyword?}
    CheckAuth -->|Yes| NonRetryable
    CheckAuth -->|No| CheckConn{Contains conn<br/>keyword?}
    CheckConn -->|Yes| Retryable
    CheckConn -->|No| CheckSSH{Contains 'ssh'?}
    CheckSSH -->|No| DefaultRetry["Default: Retryable"]
    CheckSSH -->|Yes| CheckTimeout{Contains 'timeout'<br/>or 'connect'?}
    CheckTimeout -->|Yes| Retryable
    CheckTimeout -->|No| NonRetryable

    NonRetryable --> Fail([Fail Immediately])
    Retryable --> Retry([Retry with Backoff])
    DefaultRetry --> Retry

    style NonRetryable fill:#ffebee
    style Retryable fill:#e8f5e9
```

### Exponential Backoff with Jitter

```mermaid
flowchart LR
    subgraph Backoff["Backoff Configuration"]
        Min["min_delay: 1000ms"]
        Max["max_delay: 10s (cap)"]
        MaxRetries["max_times: 3"]
        Jitter["jitter: enabled"]
    end

    subgraph Timeline["Retry Timeline"]
        T0["Attempt 1"] --> D1["Delay: ~1s"]
        D1 --> T1["Attempt 2"]
        T1 --> D2["Delay: ~2s"]
        D2 --> T2["Attempt 3"]
        T2 --> D3["Delay: ~4s"]
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
    participant Backon as backon::Retryable
    participant SSH as connect_to_ssh
    participant Server as SSH Server

    Client->>Backon: retry(connect_fn)

    Backon->>SSH: Attempt 1
    SSH->>Server: Connect
    Server-->>SSH: Connection refused
    SSH-->>Backon: Error

    Backon->>Backon: when(is_retryable) = true
    Backon->>Backon: notify("Connection refused, retrying in 1.2s")
    Note over Backon: Wait 1.2s (with jitter)

    Backon->>SSH: Attempt 2
    SSH->>Server: Connect
    Server-->>SSH: Timeout
    SSH-->>Backon: Error

    Backon->>Backon: when(is_retryable) = true
    Backon->>Backon: notify("Timeout, retrying in 2.5s")
    Note over Backon: Wait 2.5s (with jitter)

    Backon->>SSH: Attempt 3
    SSH->>Server: Connect
    Server-->>SSH: Connected
    SSH->>Server: Authenticate
    Server-->>SSH: Auth Success
    SSH-->>Backon: Success

    Backon-->>Client: (handle, retry_count=2)
```
