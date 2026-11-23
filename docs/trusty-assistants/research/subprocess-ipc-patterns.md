# Subprocess & IPC Patterns in Rust — Research Report

**Date:** 2026-04-22  
**Scope:** Process spawning, bidirectional IPC, async process management, message framing for open-mpm

---

## 1. Core Primitives: `tokio::process`

Tokio provides async process management via `tokio::process::Command` and `tokio::process::Child`.

### Spawning a Child Process with Bidirectional Pipes

```rust
use tokio::process::Command;
use std::process::Stdio;

let mut child = Command::new("my-agent")
    .arg("--mode=subprocess")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .kill_on_drop(true)  // kill child when handle is dropped
    .spawn()
    .expect("failed to spawn agent");

let stdin = child.stdin.take().unwrap();   // AsyncWrite
let stdout = child.stdout.take().unwrap(); // AsyncRead
let stderr = child.stderr.take().unwrap(); // AsyncRead
```

`Child` exposes:
- `stdin: Option<ChildStdin>` — implements `AsyncWrite`
- `stdout: Option<ChildStdout>` — implements `AsyncRead`
- `stderr: Option<ChildStderr>` — implements `AsyncRead`
- `.wait()` — async, waits for process exit
- `.kill()` — sends kill signal

### Critical: Avoiding Deadlocks

The #1 pitfall with bidirectional pipes. The deadlock scenario: the parent is blocked writing to the child's stdin, while the child is blocked writing to its stdout (because the parent isn't reading). Always use separate tasks for reading and writing:

```rust
let mut child = Command::new("agent")
    .stdin(Stdio::piped())
    .stdout(Stdio::piped())
    .spawn()?;

let mut stdin = child.stdin.take().unwrap();
let mut stdout = child.stdout.take().unwrap();

// Write in a separate task to avoid blocking
let write_task = tokio::spawn(async move {
    stdin.write_all(b"{\"type\":\"request\",\"id\":1}\n").await?;
    // Drop stdin to signal EOF when done sending
    Ok::<(), std::io::Error>(())
});

// Read responses concurrently
let read_task = tokio::spawn(async move {
    let mut reader = BufReader::new(stdout);
    let mut line = String::new();
    while reader.read_line(&mut line).await? > 0 {
        // process line
        line.clear();
    }
    Ok::<(), std::io::Error>(())
});

let (_, _) = tokio::join!(write_task, read_task);
child.wait().await?;
```

### Process Lifecycle

```rust
// kill_on_drop: child dies when Child struct is dropped
let child = Command::new("agent").kill_on_drop(true).spawn()?;

// Explicit kill
child.kill().await?;

// Wait for exit
let status = child.wait().await?;

// Piping output of one process to input of another
let echo = Command::new("echo")
    .arg("hello")
    .stdout(Stdio::piped())
    .spawn()?;
let pipe_stdin: Stdio = echo.stdout.unwrap().try_into()?;
let tr = Command::new("tr")
    .stdin(pipe_stdin)
    .spawn()?;
```

---

## 2. Message Framing Protocols

For structured communication over stdin/stdout pipes, two dominant patterns:

### 2a. Newline-Delimited JSON (NDJSON / JSON Lines)

The simplest IPC format. Each message is one JSON object per line:

```
{"type":"request","id":1,"method":"run_tool","params":{"name":"bash","args":["ls"]}}\n
{"type":"response","id":1,"result":{"output":"file1.txt\nfile2.txt"}}\n
{"type":"event","event":"status","data":{"phase":"running"}}\n
```

**Specification:**
- Each JSON text MUST be followed by `\n` (0x0A)
- May optionally be preceded by `\r` (0x0D)
- JSON values MUST NOT contain literal newlines (must be `\n` in strings)
- UTF-8 encoding required

**Rust implementation with `tokio`:**

```rust
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use serde::{Deserialize, Serialize};

// Reading NDJSON from child stdout
let reader = BufReader::new(child_stdout);
let mut lines = reader.lines();
while let Some(line) = lines.next_line().await? {
    let msg: Message = serde_json::from_str(&line)?;
    handle_message(msg).await;
}

// Writing NDJSON to child stdin
let json = serde_json::to_string(&request)?;
child_stdin.write_all(json.as_bytes()).await?;
child_stdin.write_all(b"\n").await?;
child_stdin.flush().await?;
```

**Crates:**
- `json-lines` — provides `JsonLinesCodec` implementing `tokio_util::codec::{Decoder, Encoder}` — integrates with `tokio_util::codec::FramedRead`/`FramedWrite`
- `serde_json` + manual `\n` delimiters — simplest approach

**Pros:** Human-readable, `jq`-compatible for debugging, trivial to implement, works with `lines()` in tokio  
**Cons:** Must escape newlines in string values, scanning for `\n` slightly less efficient than length-prefix

### 2b. Length-Prefixed JSON

Each message is preceded by its byte length, allowing the receiver to read exactly N bytes without scanning for delimiters:

```
18{"some":"value\n"}55{"may":{"include":"nested","objects":["and","arrays"]}}
```

More commonly with a fixed-size header (e.g., 4 bytes big-endian u32):

```rust
// Writing
let json = serde_json::to_vec(&msg)?;
let len = json.len() as u32;
child_stdin.write_u32(len).await?;        // 4-byte big-endian length
child_stdin.write_all(&json).await?;

// Reading
let len = child_stdout.read_u32().await?;
let mut buf = vec![0u8; len as usize];
child_stdout.read_exact(&mut buf).await?;
let msg: Message = serde_json::from_slice(&buf)?;
```

**Pros:** No newline scanning, handles TCP fragmentation cleanly, faster parsing  
**Cons:** Not human-readable, harder to debug with standard CLI tools

### Recommendation for open-mpm

**Use NDJSON** for the PM ↔ sub-agent IPC protocol:
- Debuggable with `cat`, `jq`, logging
- MCP protocol itself uses JSON-RPC over NDJSON (line-delimited)
- Lower barrier to third-party agent implementations
- `tokio::io::AsyncBufReadExt::lines()` makes it trivial

If performance becomes a concern at high message volumes, consider length-prefix as an optimization later.

---

## 3. MCP stdio Transport (Production Reference)

The official Model Context Protocol uses NDJSON over stdio as its primary local IPC transport. This is the most battle-tested pattern in the Rust AI agent space.

From analyzing real implementations (e.g., claw-code):

```rust
// Write a JSON-RPC request to child stdin
fn write_frame(stdin: &mut ChildStdin, req: &JsonRpcRequest) -> Result<()> {
    let json = serde_json::to_string(req)?;
    // line-delimited JSON-RPC
    stdin.write_all(json.as_bytes())?;
    stdin.write_all(b"\n")?;
    stdin.flush()?;
    Ok(())
}

// Read a JSON-RPC response from child stdout
async fn read_frame(reader: &mut BufReader<ChildStdout>) -> Result<JsonRpcResponse> {
    let mut line = String::new();
    reader.read_line(&mut line).await?;
    Ok(serde_json::from_str(&line.trim_end())?)
}
```

MCP also defines timeouts:
- Initialize: 10 seconds
- List tools: 30 seconds
- Tool execution: 60 seconds (default)

These are good defaults for open-mpm to adopt.

---

## 4. `tokio_process_tools` — Higher-Level Abstraction

The `tokio_process_tools` crate provides production-quality process management on top of `tokio::process`:

- **Graceful termination cascade:** SIGINT → SIGTERM → SIGKILL
- **Timeout support** per operation
- **Backpressure control** on stdin
- **Correct EOF semantics** via `stdin().close()`
- **Collection safety** — prevents zombie processes

```rust
// Example: graceful shutdown
let mut proc = ManagedProcess::spawn("agent", args)?;
proc.stdin().write_all(b"shutdown\n").await?;
proc.stdin().close();
// Wait up to 5s for graceful exit, then SIGTERM, then SIGKILL
proc.shutdown(Duration::from_secs(5)).await?;
```

Crate: https://docs.rs/tokio-process-tools

---

## 5. Message Framing Design for open-mpm

### Recommended Message Schema (NDJSON, JSON-RPC 2.0 inspired)

```json
{
  "jsonrpc": "2.0",
  "id": "msg-uuid-or-int",
  "method": "delegate_task",
  "params": {
    "task": "Implement the user authentication module",
    "context": "...",
    "agent_type": "engineer",
    "tools": ["bash", "read_file", "write_file"]
  }
}
```

Response from sub-agent:
```json
{
  "jsonrpc": "2.0",
  "id": "msg-uuid-or-int",
  "result": {
    "status": "completed",
    "output": "...",
    "files_modified": ["src/auth.rs"]
  }
}
```

Events / streaming updates:
```json
{
  "jsonrpc": "2.0",
  "method": "progress_update",
  "params": {
    "task_id": "msg-uuid",
    "phase": "running",
    "content": "Writing authentication handler..."
  }
}
```

### Process Model for open-mpm

```
PM (orchestrator process)
  ├── spawns sub-agent A (tokio::process::Command)
  │     stdin  ← NDJSON messages from PM
  │     stdout → NDJSON messages to PM
  │     stderr → PM logs (separate reader task)
  ├── spawns sub-agent B
  └── spawns sub-agent C
      (all concurrent via tokio::spawn tasks)
```

Each sub-agent is itself a `open-mpm` binary in "agent mode" (`open-mpm run --agent engineer --subprocess`).

---

## 6. Parallel Process Management

Managing multiple concurrent sub-agent processes:

```rust
use std::collections::HashMap;
use tokio::sync::mpsc;

struct SubAgentPool {
    agents: HashMap<String, SubAgent>,
    result_tx: mpsc::Sender<AgentResult>,
}

impl SubAgentPool {
    async fn delegate(&mut self, task: Task) -> AgentHandle {
        let agent_id = task.agent_type.clone();
        let (stdin_tx, stdin_rx) = mpsc::channel(32);
        
        let handle = tokio::spawn(async move {
            run_sub_agent(agent_id, task, stdin_rx).await
        });
        
        handle
    }
    
    async fn wait_all(&mut self) -> Vec<AgentResult> {
        // Use tokio::join! or futures::future::join_all
        let handles: Vec<_> = self.agents.values_mut()
            .map(|a| a.handle)
            .collect();
        futures::future::join_all(handles).await
    }
}
```

---

## 7. Error Handling & Resilience

Critical patterns for subprocess IPC:

```rust
// Detect child process death
match child.try_wait() {
    Ok(Some(status)) => eprintln!("child exited: {}", status),
    Ok(None) => { /* still running */ }
    Err(e) => eprintln!("error checking child: {}", e),
}

// Handle broken pipe (child died, writing to its stdin)
match child_stdin.write_all(msg).await {
    Err(e) if e.kind() == std::io::ErrorKind::BrokenPipe => {
        // child has died, handle gracefully
    }
    Err(e) => return Err(e.into()),
    Ok(_) => {}
}

// Timeout for tool calls
let result = tokio::time::timeout(
    Duration::from_secs(60),
    read_response(&mut reader)
).await
.map_err(|_| anyhow!("sub-agent timed out after 60s"))??;
```

---

## 8. Unix Domain Sockets (Alternative to stdio)

For higher throughput or when processes need to communicate without parent-child relationship, Unix domain sockets are an option:

```rust
use tokio::net::UnixListener;

let socket_path = format!("/tmp/open-mpm-{}.sock", session_id);
let listener = UnixListener::bind(&socket_path)?;
let (stream, _) = listener.accept().await?;
// Now use BufReader/BufWriter over the UnixStream
```

claude-mpm uses a file-based message queue (HookEventBus) for sidecar-to-PM communication, which is simpler but less real-time. Unix sockets offer better performance for high-frequency message passing.

---

## Sources

- [tokio::process docs](https://docs.rs/tokio/latest/tokio/process/)
- [tokio::process::Child](https://docs.rs/tokio/latest/tokio/process/struct.Child.html)
- [tokio_process_tools crate](https://docs.rs/tokio-process-tools/latest/tokio_process_tools/)
- [Async Process I/O Gist (Technius)](https://gist.github.com/Technius/43977937a28e8846d917b53605e32cc3)
- [JSON Streaming — Wikipedia](https://en.wikipedia.org/wiki/JSON_streaming)
- [NDJSON Specification](https://github.com/ndjson/ndjson-spec)
- [json-lines crate](https://docs.rs/json-lines)
- [MCP Transports Specification](https://modelcontextprotocol.io/specification/2025-11-25/basic/transports)
- [rmcp Rust SDK](https://github.com/modelcontextprotocol/rust-sdk)
- [MCP stdio server lifecycle deep-dive](https://deepwiki.com/instructkr/claw-code/5.1-mcp-server-lifecycle-and-stdio-transport)
