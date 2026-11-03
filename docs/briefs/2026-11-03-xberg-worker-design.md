# Xberg Extraction Worker Design (2026-11-03)

## Overview

An optional separate extraction worker process for Xberg integration that solves MSRV isolation (Xberg 1.98 vs trusty-tools 1.94), enforces resource limits, and provides panic isolation. The worker runs as a standalone binary spawned by trusty-okf for document extraction during imports, communicating via stdin/stdout JSON-RPC. Aligned with SPEC-OKF-01 boundaries: trusty-okf owns the extraction call for imports (not the Xberg binary or plugins); extraction stays file-I/O only with no network.

## Process Protocol

### Spawn

**Binary location:**
- Binary name: `xberg-worker` (separate crate in `crates/xberg-worker/`)
- Installed via `cargo install xberg-worker --locked` (not bundled in trusty-okf)
- Discovery: `which xberg-worker` or explicit path in config (`~/.trusty-okf/config.toml`)

**Command line:**
```bash
xberg-worker --protocol-version 1 --max-file-bytes 10485760 --max-text-bytes 5242880
```

**Environment (SPEC-OKF-01 B10 compliance):**
- Cleared environment except allow-listed: `LANG`, `TZ=UTC`
- No `HOME`, `PATH`, `TMPDIR`, `CARGO_TARGET_DIR`, or credentials
- Worker creates its own temp directory internally (no parent-provided path)

**Working directory:**
- Fresh empty temporary directory created by parent (`tempfile::tempdir()`)
- Removed after worker exit (whether success or failure)
- Worker never receives filesystem paths to extract; bytes arrive on stdin only

**Standard streams:**
- `stdin`: JSON-RPC requests (extract messages with base64-encoded file bytes)
- `stdout`: JSON-RPC responses (extracted text, metadata, errors)
- `stderr`: Diagnostic logs only (not parsed by parent; logged to parent's `tracing::error!`)

### IPC Mechanism

**Protocol: Line-delimited JSON-RPC over stdin/stdout**

Each message is one line terminated by `\n`. Parent writes requests to worker stdin; worker writes responses to stdout.

**Request schema (parent → worker):**
```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "method": "extract",
  "params": {
    "bytes": "<base64-encoded file bytes>",
    "type_hint": "application/pdf",
    "file_name": "document.pdf",
    "max_text_bytes": 5242880
  }
}
```

**Success response schema (worker → parent):**
```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "result": {
    "text": "<extracted text, UTF-8>",
    "fields": {
      "title": "Document Title",
      "author": "Author Name",
      "pages": "12"
    },
    "notes": ["OCR confidence: 0.95", "Language: en"]
  }
}
```

**Error response schema (worker → parent):**
```json
{
  "jsonrpc": "2.0",
  "id": 42,
  "error": {
    "code": -32001,
    "message": "Extraction timeout after 30 seconds",
    "data": {
      "file_name": "document.pdf",
      "elapsed_ms": 30042
    }
  }
}
```

**Error codes:**
| Code | Meaning | Recovery |
|------|---------|----------|
| `-32001` | Timeout | Log, skip file |
| `-32002` | File too large | Log, skip file |
| `-32003` | Extracted text too large | Truncate, log warning |
| `-32004` | Unsupported format | Log, skip file |
| `-32005` | Malformed input | Log, skip file (likely parent bug) |
| `-32700` | Parse error | Log, kill worker, respawn |
| `-32600` | Invalid request | Log, skip file (likely parent bug) |

### Error Propagation

**Worker-side errors:**
- Xberg panic: caught by `std::panic::catch_unwind`, returned as JSON-RPC error code `-32099` (internal error)
- Timeout: worker detects via `tokio::time::timeout`, returns `-32001`
- Resource limit: worker checks limits before extraction, returns `-32002` or `-32003`

**Parent-side errors:**
- Worker crash (exit != 0): parent logs stderr, treats in-flight request as `-32099`, respawns worker for next file
- Worker hung (no response for 35 seconds = timeout + 5s grace): parent sends SIGTERM, waits 2s, sends SIGKILL, logs failure, respawns
- Malformed JSON from worker: parent logs, kills worker, respawns (broken worker)

**Retry strategy:**
- No retry per file (extraction failure = skip file, log error)
- Worker respawn on crash/hang (max 3 respawns per reindex batch; after that, disable worker, fall back to built-in extraction)

## Resource Limits

### Per-File Limits

| Limit | Value | Enforcement |
|-------|-------|-------------|
| Max file size | 10 MB (10,485,760 bytes) | Parent checks before spawn; worker double-checks on receipt |
| Max extracted text | 5 MB (5,242,880 bytes) | Worker checks after extraction; truncates with warning if exceeded |
| Max CPU time | 30 seconds | Worker enforces via `tokio::time::timeout` around `xberg::extract()` |
| Max memory | 512 MB (soft) | OS-level (ulimit -m on Linux/macOS); worker monitors via `jemalloc` stats (advisory) |

### Enforcement Mechanism

**File size (pre-extraction):**
- Parent: checks `bytes.len()` before encoding to JSON-RPC request; skips if > `max_file_bytes`
- Worker: checks `base64::decode(params.bytes)?.len()` after decode; returns error `-32002` if > `max_file_bytes`

**Extracted text size (post-extraction):**
- Worker: checks `result.text.len()` after extraction
  - If ≤ `max_text_bytes`: return full text
  - If > `max_text_bytes`: truncate to `max_text_bytes` (UTF-8 char boundary), add note `"text_truncated: true"`, return warning code `-32003` with partial result

**CPU timeout:**
- Worker wraps `xberg::extract()` in `tokio::time::timeout(Duration::from_secs(30), ...)`
- On timeout: returns error `-32001` with `elapsed_ms`

**Memory (soft limit, advisory):**
- Worker built with `jemalloc` as allocator (`#[global_allocator]`)
- Periodic check (every 5s): if allocated > 512 MB, log warning, continue (no hard kill)
- OS-level hard limit (Linux/macOS): `ulimit -m 524288` set by parent before spawn (RLIMIT_RSS); kernel kills worker on breach

### Limit Exceeded Behavior

| Limit | Behavior | Exit Code | Parent Action |
|-------|----------|-----------|---------------|
| File size (before) | Skip file, no spawn | N/A | Log, mark item as `extraction_skipped: file_too_large` |
| File size (after) | Error response `-32002` | 0 | Log, skip file |
| Text size | Truncate, return with warning `-32003` | 0 | Log warning, store truncated text with `text_truncated: true` field |
| Timeout | Error response `-32001` | 0 | Log, skip file |
| Memory (soft) | Log warning, continue | 0 | Continue (no action unless repeated) |
| Memory (hard) | OS kills process | 137 (SIGKILL) | Log, treat as crash, respawn worker |

## Lifecycle Management

### Graceful Shutdown

**On parent shutdown signal (SIGTERM to parent):**
1. Parent sets shutdown flag
2. Parent stops accepting new extract requests
3. Parent waits up to 30 seconds for in-flight request to complete
4. If worker responds within 30s: collect response, send SIGTERM to worker, wait 2s for clean exit
5. If worker does not exit after 2s: send SIGKILL
6. Parent calls `waitpid()` to reap worker
7. Parent exits

**Worker shutdown handling:**
- Worker catches SIGTERM via `tokio::signal::ctrl_c()` (or Unix signal handler)
- On SIGTERM: worker flushes stdout, closes stdin, exits with code 0
- No attempt to finish in-flight extraction (parent will retry or skip)

### Force Kill

**Trigger conditions:**
1. Worker does not respond to SIGTERM within 2 seconds (shutdown grace period)
2. Worker does not respond to extract request within 35 seconds (timeout 30s + 5s grace)
3. Worker crashes (exit code != 0) or hangs (no stdout activity for 40 seconds during active request)

**Force kill sequence:**
```rust
// After SIGTERM timeout or hang detection
nix::sys::signal::kill(worker_pid, Signal::SIGKILL)?;
// Immediately followed by waitpid to reap
let status = nix::sys::wait::waitpid(worker_pid, None)?;
tracing::error!("Worker killed: pid={worker_pid}, status={status:?}");
```

**Cleanup after force kill:**
- Remove worker's temp directory (`drop(tempdir)` handles this automatically)
- Log error with worker PID, file name, elapsed time
- Increment `worker_force_kill_count` metric
- Respawn worker for next file (unless respawn limit reached)

### Zombie Prevention

**Parent reaping strategy:**
- Spawn worker with `tokio::process::Command` in async context
- Call `.wait()` on `Child` handle immediately after detecting exit (normal or killed)
- If `.wait()` times out (shouldn't happen, but defensive): retry `waitpid()` with `WNOHANG` flag, log warning

**Edge case: parent crash before reaping:**
- Worker becomes orphaned, adopted by PID 1 (`init` / `launchd`)
- OS reaps on exit (no zombie leak)
- Temp directory cleaned by OS on next boot (not ideal, but bounded)

**Mitigation for temp directory leak on parent crash:**
- Worker temp dirs created under `$TMPDIR/xberg-worker-<uuid>/`
- Parent periodically (every 1000 files or 1 hour) scans `$TMPDIR/xberg-worker-*` for directories older than 1 hour with no active worker PID → removes orphans

### Shutdown Edge Cases

| Case | Detection | Handling |
|------|-----------|----------|
| Parent crash during extract | Worker detects stdin EOF | Worker exits with code 0 (clean shutdown) |
| Worker crash during extract | Parent detects exit code != 0 | Parent logs stderr, treats request as failed, respawns worker |
| Worker orphaned (parent killed -9) | Worker detects stdin EOF | Worker exits; OS reaps; temp dir cleaned by OS or next parent scan |
| SIGKILL to parent before SIGTERM to worker | Worker keeps running until stdin EOF | Worker exits when stdin closes; OS reaps |
| Worker spawns child processes (unlikely) | Worker crashes, children orphaned | Children adopted by PID 1 (acceptable; Xberg does not spawn subprocesses) |

## Provenance Tracking

### Per-Extraction Metadata

**Recorded in `ImportItem` fields (SPEC-OKF-01 §2.16 criterion 105):**
```rust
pub struct ExtractionProvenance {
    pub worker_pid: u32,
    pub start_timestamp: DateTime<Utc>,
    pub completion_timestamp: DateTime<Utc>,
    pub file_path: String,               // Source path (for logging only; not sent to worker)
    pub status: ExtractionStatus,        // Success | Timeout | FileTooLarge | WorkerCrash | ...
    pub extracted_by: String,            // "xberg-worker@0.1.0"
    pub extract_protocol: String,        // "xberg-worker-jsonrpc/v1"
    pub xberg_version: String,           // "1.2.3" (from worker handshake)
    pub extract_config: String,          // "<sha256 of worker config TOML>"
}

pub enum ExtractionStatus {
    Success,
    Timeout,
    FileTooLarge,
    TextTruncated,
    WorkerCrash,
    Unsupported,
    Skipped,
}
```

### Logging Format

**Structured JSON logs (via `tracing::info!`):**
```json
{
  "timestamp": "2026-11-03T14:23:45.123Z",
  "level": "INFO",
  "target": "trusty_okf::extract::worker",
  "fields": {
    "event": "extract_complete",
    "worker_pid": 12345,
    "file_path": "/tmp/import-batch-001/document.pdf",
    "file_size_bytes": 2048576,
    "extracted_text_bytes": 102400,
    "elapsed_ms": 1234,
    "status": "Success",
    "xberg_version": "1.2.3",
    "protocol_version": 1
  }
}
```

**Error log example (worker crash):**
```json
{
  "timestamp": "2026-11-03T14:24:10.456Z",
  "level": "ERROR",
  "target": "trusty_okf::extract::worker",
  "fields": {
    "event": "extract_failed",
    "worker_pid": 12346,
    "file_path": "/tmp/import-batch-001/corrupted.pdf",
    "file_size_bytes": 5242880,
    "elapsed_ms": 8765,
    "status": "WorkerCrash",
    "exit_code": 139,
    "stderr": "Segmentation fault (core dumped)"
  }
}
```

**Logs written to:**
- Parent process: `~/.trusty-okf/logs/import-<batch-id>.jsonl`
- Worker process: stderr only (not persisted; captured by parent on crash)

### Error Attribution

**Tracing failure back to specific file + worker instance:**
1. Parent logs every extract request with `worker_pid`, `file_path`, `start_timestamp`
2. Parent logs every response (success or error) with `completion_timestamp`, `status`, `elapsed_ms`
3. On worker crash: parent logs `exit_code`, `stderr`, `worker_pid`, `file_path` of in-flight request
4. Query logs by `file_path` to see full extraction history (all attempts, including retries)
5. Query logs by `worker_pid` to see all files processed by one worker instance (useful for detecting memory leak patterns)

**Failure aggregation:**
- Parent tracks counts per `ExtractionStatus` (e.g., 5 timeouts, 2 crashes, 1 unsupported)
- At end of import batch, log summary: `"Extraction summary: 1000 success, 5 timeout, 2 crash, 1 unsupported"`

## Test Fixtures

### Minimal Corpus

**Location:** `crates/xberg-worker/testdata/`

1. **`test.pdf`** — 2-page text-only PDF, 45 KB
   - Content: Lorem ipsum placeholder text, two pages
   - Expected: ~400 words extracted, metadata `{"pages": "2", "title": "Test Document"}`
   - Tests: basic extraction, metadata parsing

2. **`test.docx`** — Microsoft Word document, 12 KB
   - Content: Single-page doc with heading, body, footer
   - Expected: ~200 words extracted, metadata `{"author": "Test Author", "title": "Sample DOCX"}`
   - Tests: Office format extraction, field parsing

3. **`scanned.pdf`** — Scanned image PDF requiring OCR, 1.2 MB
   - Content: One page, scanned at 300 DPI, English text
   - Expected: ~150 words extracted via OCR, metadata `{"pages": "1"}`, note `"ocr_confidence: 0.85"`
   - Tests: OCR pipeline, confidence scoring, image-based extraction

4. **`timeout-bomb.pdf`** — Crafted PDF designed to trigger timeout (if possible; else use 50 MB file as proxy)
   - Content: Heavily nested objects or huge embedded image
   - Expected: Error `-32001` (timeout), no extracted text
   - Tests: timeout enforcement, graceful failure

5. **`malformed.pdf`** — Corrupted PDF header
   - Content: `%PDF-1.4` header but truncated body
   - Expected: Error `-32004` (unsupported) or `-32099` (internal error), stderr logged
   - Tests: error handling, worker survival after extraction failure

6. **`huge.pdf`** — 15 MB PDF (exceeds 10 MB limit)
   - Content: High-resolution images
   - Expected: Skipped by parent (no spawn), logged as `FileTooLarge`
   - Tests: pre-spawn size check, no worker invocation

### Success Criteria

- [ ] **SC-1:** Worker extracts `test.pdf` and returns ~400 words with correct metadata in <2 seconds
- [ ] **SC-2:** Worker extracts `test.docx` and returns ~200 words with author field in <2 seconds
- [ ] **SC-3:** Worker extracts `scanned.pdf` via OCR and returns ~150 words with OCR note in <15 seconds
- [ ] **SC-4:** Worker times out on `timeout-bomb.pdf` (or 50 MB proxy) after 30 seconds and returns error `-32001`
- [ ] **SC-5:** Worker survives extraction failure on `malformed.pdf` (returns error, continues to next file)
- [ ] **SC-6:** Parent skips `huge.pdf` without spawning worker, logs `FileTooLarge`
- [ ] **SC-7:** Provenance tracking logs correct PID, timestamps, status for all 6 files
- [ ] **SC-8:** Worker exits cleanly (code 0) after SIGTERM when no request is in-flight
- [ ] **SC-9:** Parent reaps worker zombie after force kill (SIGKILL) with no orphan processes

### Failure Cases

| Case | Trigger | Expected Behavior | Verification |
|------|---------|-------------------|--------------|
| Timeout | `timeout-bomb.pdf` | Error `-32001`, elapsed ≈30s, worker survives | Check logs for `status: Timeout`, next file succeeds |
| Crash | `malformed.pdf` + Xberg bug | Exit code 139, stderr captured, respawn, next file succeeds | Check logs for `status: WorkerCrash`, `exit_code: 139`, respawn count increments |
| Malformed JSON from worker | Worker outputs invalid line | Parent logs parse error, kills worker, respawns | Check logs for `"Malformed response from worker"`, next file succeeds after respawn |
| Parent SIGTERM during extract | Parent receives SIGTERM while worker is active | Parent waits 30s, then sends SIGTERM to worker, waits 2s, sends SIGKILL if needed, reaps | Verify no zombie, temp dir removed |
| Worker orphaned (parent SIGKILL) | Kill parent with `-9` while worker is active | Worker detects stdin EOF, exits cleanly | Verify no zombie (`ps aux | grep xberg-worker`), temp dir removed (eventually) |
| Text too large | PDF with 10 MB text (synthetic) | Worker returns `-32003`, text truncated to 5 MB, note `text_truncated: true` | Check result has `error.code == -32003`, text length ≈ 5 MB |
| Memory limit (soft) | PDF triggering high memory usage | Worker logs warning, continues | Check logs for `"Memory usage high: 520 MB"`, extraction completes |
| Memory limit (hard) | PDF triggering >512 MB | OS kills worker (exit 137), parent logs, respawns | Check logs for `exit_code: 137`, `status: WorkerCrash` |

## SPEC-OKF-01 Alignment

### OKF Boundaries

**What trusty-okf owns (per SPEC-OKF-01 §2.16):**
- The extraction call for imports (via `trusty_okf::extract::worker::extract()`)
- The worker process protocol (JSON-RPC schema, spawn, kill, reap)
- Resource limits configuration and enforcement boundaries
- Provenance tracking (`ExtractionProvenance` struct)

**What trusty-okf does NOT own:**
- Xberg binary or plugins (user installs `xberg-worker` separately via `cargo install`)
- Authenticated source acquisition (stays in source adapters / trusty-agents)
- Chunking, embedding, ranking (stays in trusty-search)

**Extraction use cases:**
- trusty-okf: extraction for imports (this worker serves this use case)
- trusty-search: extraction for indexing (separate code path, not this worker — future possibility per §2.16 reconciliation)

### I/O Constraints

**SPEC-OKF-01 B8 (Network-Free Core):**
- Worker has no network code (no `tokio::net`, no HTTP client, no DNS)
- Worker only reads stdin, writes stdout/stderr
- Feature `extract-worker` (parent side) has no async runtime dependency (uses `std::process::Command` or `tokio::process` if already in async context)

**SPEC-OKF-01 B9 (No Xberg Linking):**
- `trusty-okf` never links `xberg` or `ort` or `candle-core`
- `trusty_okf::extract::worker` (feature `extract-worker`) only contains process protocol code (JSON-RPC, spawn, IPC)
- `xberg-worker` binary (separate crate) links Xberg; user installs it separately

**SPEC-OKF-01 B10 (No Credentials to Child Processes):**
- Worker spawned with cleared environment (only `LANG`, `TZ=UTC` allowed)
- No `HOME`, `PATH`, `CARGO_TARGET_DIR`, `OPENAI_API_KEY`, or any secret
- Worker receives raw bytes on stdin only, never filesystem paths to user files

**Verification (SPEC-OKF-01 §6.2 criterion 99):**
```bash
# Verify trusty-okf has no xberg dependency
cargo tree -p trusty-okf --all-features | grep -q xberg && echo "FAIL" || echo "PASS"

# Verify xberg-worker binary exists and is separate
cargo metadata --format-version 1 | jq '.packages[] | select(.name == "xberg-worker")'
```

### Integration Point

**Where worker plugs into trusty-okf import flow (SPEC-OKF-01 §4.10 phases):**

1. **Plan phase:** Assigns extractor based on type hint
   - If `type_hint` in `["application/pdf", "application/vnd.openxmlformats-officedocument.wordprocessingml.document", ...]` and feature `extract-worker` enabled: assign `WorkerExtractor`
   - Else: assign `BuiltInExtractor` (UTF-8 text / Markdown)

2. **Apply phase:** Calls `Extractor::extract(bytes, type_hint)`
   - `WorkerExtractor::extract()` spawns worker if not already running, sends JSON-RPC request, waits for response
   - On success: returns `Extracted { text, fields, notes }`
   - On error: logs, returns `ExtractionError::WorkerFailed`, item marked as `extraction_skipped`

3. **Feed phase:** Pushes indexed content to trusty-search (unchanged; worker is transparent to this phase)

**Code location:**
- `trusty-okf/src/extract/mod.rs` — `Extractor` trait, `BuiltInExtractor`
- `trusty-okf/src/extract/worker.rs` (feature `extract-worker`) — `WorkerExtractor`, spawn, IPC, lifecycle
- `xberg-worker/src/main.rs` — Worker binary, JSON-RPC loop, Xberg calls

## Implementation Notes

### Dependencies

**Parent side (`trusty-okf` with feature `extract-worker`):**
- `serde` / `serde_json` — JSON-RPC serialization
- `tokio` (optional, if already in async context) or `std::process::Command` — process spawn
- `nix` — Signal handling (SIGTERM, SIGKILL), `waitpid`
- `tempfile` — Temp directory creation/cleanup
- `tracing` — Structured logging
- `base64` — Encode file bytes for JSON-RPC

**Worker side (`xberg-worker` binary):**
- `xberg` — Document extraction (Xberg SDK)
- `serde` / `serde_json` — JSON-RPC (de)serialization
- `tokio` — Async runtime, timeout enforcement
- `base64` — Decode file bytes from JSON-RPC
- `tracing` / `tracing-subscriber` — Logs to stderr
- `jemallocator` — Memory allocator (for memory stats)

### Platform Support

**Supported platforms:**
- macOS (primary target, SPEC-OKF-01 §2.3)
- Linux (secondary, for CI and future Docker deployment)

**Not supported:**
- Windows (Xberg has limited Windows support; defer until user demand)

**Platform-specific considerations:**
- **macOS:** Use `launchd` sandbox (future, not v1) via `sandbox-exec` for file access restrictions
- **Linux:** Use `seccomp` or `landlock` (future, not v1) for syscall filtering
- **Signal handling:** SIGTERM/SIGKILL portable across macOS/Linux; use `nix` crate for cross-platform API

### Security Considerations

**V1 (minimal security, functional correctness):**
- No sandboxing (worker can access any file on disk that parent process can)
- No syscall filtering
- Cleared environment (no credentials leaked)
- Temp directory isolation (worker cannot write to user directories, but can read them)

**V2 (hardened, future):**
- macOS: `sandbox-exec` with profile restricting file access to temp directory only
- Linux: `landlock` to restrict file access + `seccomp` to filter syscalls (no network, no exec)
- Process limits: `setrlimit` for memory, CPU time (in addition to timeout)
- Capability dropping: `CAP_SYS_ADMIN`, `CAP_NET_RAW`, etc. dropped before exec

**Threat model:**
- **Not defended:** Malicious PDF designed to exploit Xberg vulnerability (user responsibility to trust input files)
- **Defended:** Worker cannot leak credentials, cannot access parent's files beyond temp dir (future), cannot spawn subprocesses (future), cannot make network requests

## Estimated LoE

**Proof of concept (stdin/stdout JSON-RPC, basic extraction, no lifecycle):**
- 2 days (worker binary + parent spawn/IPC + test.pdf success case)

**Full integration (resource limits, timeout, SIGTERM/SIGKILL, reaping, provenance, test suite):**
- 5 days (lifecycle management, error handling, logging, all 6 test fixtures)

**Hardening (memory limits, respawn logic, orphan temp dir cleanup, edge cases):**
- 3 days (soft/hard memory limits, respawn after crash, shutdown edge cases, zombie prevention)

**Documentation + review + merge:**
- 2 days (SPEC-OKF-01 alignment verification, changelog fragment, integration testing)

**Total: 12 days** (2.5 weeks for one engineer, end-to-end from design to merge)

**Critical path dependencies:**
- Xberg SDK stable release (currently 1.2.x, MSRV 1.98)
- `trusty-okf` import flow implementation (SPEC-OKF-01 §4.10 phases — currently in progress)

**Post-merge work (not included in LoE):**
- Xberg plugin framework (TOML manifest, discovery, per-plugin limits) — 10 days
- macOS sandboxing (`sandbox-exec`) — 3 days
- Linux sandboxing (`landlock` + `seccomp`) — 5 days
