# ctrl Direct Chat Latency Analysis

**Date**: 2026-05-02  
**Scope**: `ctrl` persona direct chat path (AgentScope::User)  
**Method**: usage.jsonl measurements + static call-chain analysis

---

## 1. Latency Measurements from usage.jsonl

All recent `ctrl` entries (as of 2026-05-02):

| timestamp | agent | model | runner | input_tokens | output_tokens | duration_ms |
|---|---|---|---|---|---|---|
| 2026-05-02 13:03 | ctrl | claude-sonnet-4-6 | claude-code | 3 | 12 | 16,635 |
| 2026-05-02 13:18 | ctrl | claude-sonnet-4-6 | claude-code | 3 | 35 | 13,577 |
| 2026-05-02 13:28 | ctrl | claude-sonnet-4-6 | claude-code | 3 | 40 | 14,004 |

**Observations**:
- All ctrl turns route through `runner = "claude-code"` (CLAUDE_CODE_OAUTH_TOKEN path)
- Durations: **13–17 seconds** for trivial inputs ("hello", "good morning")
- `input_tokens = 3` is the logged value from the usage record — this reflects only the task prefix parsing, NOT the full prompt size. The actual context sent to the claude CLI is larger (see §4).
- `output_tokens = 12–40` for simple greetings — very short responses but still 13–17s

For comparison, `pm` agent turns on the same runner show similar 11–17s range.

---

## 2. Complete Call Chain Map

### Entry point: `Repl::attempt_forward` (`src/repl/mod.rs:459`)

```
attempt_forward(task_text)
├── [1] CtrlSocket::probe_default(&socket_path).await     ← 50ms timeout (fast)
│       If fails → goes to in-process path
│
├── Scope = User (direct ctrl chat):
│   └── crate::ctrl::run_pm_task_with_persona(
│           &project_dir, "ctrl", task_text, &history, None
│       ).await
│
└── run_pm_task_with_persona  (src/ctrl/mod.rs:1249)
    ├── [2] AgentConfig::load(&project_persona)             ← sync disk I/O (~1ms)
    │       or dirs::home_dir() + load fallback
    ├── [3] llm::credentials::pick_credentials()            ← reads env vars, fast
    ├── [4] apply_credential_routing(&mut cfg, &creds)      ← pure logic, no I/O
    │
    │   IF claude_cli_short_circuit == true (OAuth path):
    │   └── run_pm_task_via_claude_cli(...)
    │       ├── [5] ClaudeCodeAgentRunner::new().await       ← PATH scan, ~1ms
    │       │       find_claude() walks PATH entries
    │       │       NO CACHING — fresh scan every request
    │       ├── [6] String composition of history + task    ← pure, fast
    │       ├── [7] runner.run_with_config_public().await   ← BIG AWAIT
    │       │       ├── normalize_model()
    │       │       ├── prepend_harness_layers()             ← string concat
    │       │       ├── strip_finish_task_instructions()     ← string scan
    │       │       ├── Command::spawn(claude CLI)           ← ~10-50ms process spawn
    │       │       ├── BufReader::lines() loop              ← streaming JSON parse
    │       │       │   (blocks until claude emits {"type":"result"})
    │       │       │   DOMINANT COST: LLM network round-trip + claude CLI startup
    │       │       ├── child.wait().await
    │       │       └── tokio::spawn(append_usage())
    │       └── strip_cli_artifacts(result.content)
    │
    │   IF NOT short-circuit (OpenRouter/Anthropic REST path):
    │   ├── [3b] register_ticketing_tools(&mut registry).await
    │   │         GlobalConfig::load().await                 ← file I/O, no cache
    │   ├── [3c] (persona tool registry build)
    │   │         git tools, mcp tools, ticketing tools
    │   ├── [3d] (prompt assembly)
    │   │         build_user_context_prefix()
    │   └── [3e] llm::chat_with_tools_gated().await          ← BIG AWAIT
```

### ctrl_chat_turn (standalone CTRL process path) (`src/ctrl/mod.rs:3576`)

Note: this path fires when running as a standalone ctrl process (not the repl's User scope). Included for completeness.

```
ctrl_chat_turn(ctrl, user_input)
├── [A] build_ctrl_registry(...)                           ← async, ~1ms in-memory
│       register_ticketing_tools().await
│       └── GlobalConfig::load().await                     ← file I/O every call
│       register_git_tools().await
│       └── GlobalConfig::load().await                     ← file I/O every call (2nd load)
├── [B] resolve_agent_config(self_path).await              ← disk I/O
├── [C] skill loading (FsSkillResolver::from_defaults())   ← disk reads
├── [D] GlobalConfig::load().await (MCP prompt)            ← file I/O (3rd load same turn)
├── [E] recall_project_memories(proj, q, 5).await
│       ├── RedbUsearchStore::open(&session_dir, ...)      ← DB open
│       ├── FastEmbedder::new()                             ← model init (cold path)
│       └── MemoryStore::search().await                    ← vector search
├── [F] llm::credentials::pick_credentials()
├── [G] apply_credential_routing()
│
│   IF claude_cli_short_circuit:
│   └── run_pm_task_via_claude_cli(...)                    ← see above
│
│   ELSE:
│   └── llm::chat_with_tools_gated(...).await              ← BIG AWAIT
```

---

## 3. Top Bottlenecks Ranked by Impact

### #1 — Claude CLI subprocess startup + LLM round-trip (dominant, ~12–16s)
**Location**: `claude_code_runner.rs:333` — `cmd.spawn()` through `child.wait()`  
**Cost**: The majority of the 13–17s duration. This is composed of:
- `claude` Node.js process startup: ~1–3s (Node.js cold start + CLI initialization)
- Claude API network round-trip: ~8–14s for claude-sonnet-4-6

This is not independently reducible without changing the runner model.

### #2 — `ClaudeCodeAgentRunner::new()` called fresh every request
**Location**: `ctrl/mod.rs:1511` inside `run_pm_task_via_claude_cli`  
**Cost**: ~1–5ms PATH scan on every single ctrl turn (every message)  
**Fix**: Cache the runner instance. The `Ctrl` struct or `Repl` struct could hold a `Option<Arc<ClaudeCodeAgentRunner>>` initialized at startup. Zero downside — the binary path doesn't change at runtime.

### #3 — `GlobalConfig::load()` called 3x per ctrl_chat_turn (no caching)
**Location**:
- `ctrl/mod.rs:3494` (`register_git_tools`)
- `ctrl/mod.rs:3534` (`register_ticketing_tools`)
- `ctrl/mod.rs:3664` (MCP prompt section)  

Each is a `tokio::fs::read_to_string` + TOML parse of `~/.open-mpm/config.toml`. While fast (~1ms each), it's unnecessary triplication and compounds in the `run_pm_task_with_history` path which also calls it.

**Fix**: Load once at turn entry, pass by reference. Or add a per-turn `OnceLock`/short-lived cache.

### #4 — `run_pm_task_with_persona` re-resolves persona config every request
**Location**: `ctrl/mod.rs:1270` — `AgentConfig::load()` (sync disk I/O) on every turn  
**Cost**: ~1ms disk I/O, but also all the `prepend_harness_layers` + `strip_finish_task_instructions` string processing repeats fresh each call  
**Fix**: Cache `AgentConfig` for the `ctrl` persona (it changes only on file modification). A simple `Arc<RwLock<AgentConfig>>` seeded at REPL startup and reloaded only when the file's mtime changes would eliminate this.

### #5 — No prompt caching on the claude CLI path
**Location**: `claude_code_runner.rs` — the `--system-prompt` flag sends the full prompt every invocation  
**Cost**: The ctrl system prompt is ~8,400 tokens (ctrl.toml ~929 tokens + harness_protocol ~655 tokens + project-index ~6,819 tokens). Without prompt caching, every single ctrl turn re-tokenizes and pays for this context in full.  
**Note**: The `input_tokens = 3` logged in usage.jsonl only captures the CLI output's `usage.input_tokens` field, which may report 3 because it's counting only the task text portion. The actual cost is higher.

### #6 — `FastEmbedder::new()` on cold recall path
**Location**: `ctrl/mod.rs:124` — called inside `recall_project_memories` on every turn  
**Cost**: First call initializes a FastEmbed model (potentially slow). Subsequent calls should be faster if the model is cached, but the function creates a new instance each time.  
**Fix**: Hoist the embedder to CTRL struct level, initialized once at startup.

---

## 4. System Prompt Size Estimate

Components injected into ctrl's system prompt per turn:

| Component | Approx bytes | Approx tokens |
|---|---|---|
| ctrl.toml system_prompt.content | 3,717 | ~929 |
| harness_protocol (BASE + CLAUDE_CODE) | 2,623 | ~655 |
| project-index.md (full, pre-filter) | 27,276 | ~6,819 |
| User context block | ~100 | ~25 |
| Deployment footer | ~200 | ~50 |
| **Total** | **~34,000** | **~8,478** |

The project-index.md (27KB) is the largest single component. The `context_filter.rs` filtering helps for workflow engine paths, but it is **not applied** on the `run_pm_task_via_claude_cli` path — the full index may be included.

---

## 5. Quick Wins (< 1 hour each)

### QW-1: Cache `ClaudeCodeAgentRunner` in the `Repl` struct
**File**: `src/repl/mod.rs` (add field) + `src/ctrl/mod.rs` (pass via parameter or `Repl`)  
**Change**: Add `claude_runner: Option<Arc<ClaudeCodeAgentRunner>>` to `Repl`. Initialize at `Repl::new()` when `CLAUDE_CODE_OAUTH_TOKEN` is set. Pass to `run_pm_task_via_claude_cli` instead of calling `::new()` there.  
**Savings**: ~1–5ms PATH scan eliminated per turn. More importantly, prevents any future latency if binary discovery becomes slower (e.g. NFS mounts, large PATH).

### QW-2: Deduplicate `GlobalConfig::load()` calls per turn
**File**: `src/ctrl/mod.rs`  
**Change**: In `ctrl_chat_turn` and `run_pm_task_with_history`, call `GlobalConfig::load().await` once, store in a local variable, pass it to `register_git_tools`, `register_ticketing_tools`, and the MCP prompt section builder.  
**Savings**: 2 unnecessary file reads + TOML parses eliminated per turn.

### QW-3: Cache `AgentConfig` for `ctrl` persona (file-mtime gated)
**File**: `src/ctrl/mod.rs`  
**Change**: In `run_pm_task_with_persona`, add a static `OnceLock<(SystemTime, AgentConfig)>` (or cache on the `Repl` struct). Reload only when mtime of the ctrl.toml file changes.  
**Savings**: Eliminates sync disk I/O + string processing (harness layer prepend, finish_task strip) on every turn.

### QW-4: Apply `context_filter` on the claude CLI path
**File**: `src/ctrl/mod.rs:1511–1574` (`run_pm_task_via_claude_cli`)  
**Change**: Before composing the task string passed to `run_with_config_public`, filter the system prompt's project-index section using `context_filter::filter_index_entries`. Currently the workflow engine applies this filter but the ctrl direct chat path does not.  
**Savings**: Reduces system prompt tokens from ~8,478 to potentially ~3,000–4,000 tokens for narrow queries. On Anthropic's prompt caching, cache misses are charged at full price — smaller prompts = lower cost + faster tokenization.

---

## 6. Systemic Improvements

### S-1: Per-session claude CLI process (persistent conversation)
Currently the claude CLI is spawned fresh for every ctrl turn. The CLI supports `--resume <session-id>` for session continuation, but the harness passes history as raw text in the prompt instead.  
**Opportunity**: Use `--resume` or `claude -i` interactive mode to maintain a persistent claude process for the REPL session, amortizing the ~1–3s Node.js startup across all turns.  
**Risk**: Complex error handling for process death; session isolation between projects.

### S-2: Structured prompt caching awareness
The usage.jsonl `input_tokens = 3` values suggest the CLI is either using prompt caching (cache hits report low input tokens) or the token counting is from the result event only. If prompt caching IS active, QW-3 and QW-4 still matter for cache miss scenarios.  
**Action**: Add a `cache_read_tokens` vs `cache_creation_tokens` breakdown to the usage log display so cache effectiveness is visible.

### S-3: TTFT instrumentation surfacing
The runner already emits `debug!(ttft_ms = ...)` at the first stdout line from the claude CLI (`claude_code_runner.rs:376–379`). This is not surfaced to the user or to the usage log.  
**Opportunity**: Log `ttft_ms` alongside `duration_ms` in `UsageRecord` to distinguish "claude CLI slow to start" from "model slow to generate". With the current data (13–17s total, no TTFT breakdown), it's impossible to attribute blame.

### S-4: In-process runner for ctrl persona
`ctrl.toml` uses `runner = "claude-code"` which forces the subprocess path. If a direct Anthropic API key were available, `runner = "in-process"` with `use_anthropic_direct = true` would eliminate the claude CLI subprocess entirely and reduce latency to pure network time (~2–5s for conversational turns vs 13–17s currently).  
**Note**: Requires `ANTHROPIC_API_KEY` (not the OAuth token). The OAuth token (sk-ant-oat01-*) is only valid for the claude CLI path.

---

## 7. Call Chain Summary Table

| Step | Location | Type | Estimated cost |
|---|---|---|---|
| CtrlSocket::probe_default | `repl/mod.rs:471` | async net | 0–50ms (timeout) |
| AgentConfig::load | `ctrl/mod.rs:1271` | sync disk I/O | ~1ms |
| pick_credentials | `ctrl/mod.rs:1297` | env read | <0.1ms |
| ClaudeCodeAgentRunner::new | `ctrl/mod.rs:1511` | PATH scan | ~1–5ms |
| History composition | `ctrl/mod.rs:1529–1538` | string concat | <0.1ms |
| prepend_harness_layers | `claude_code_runner.rs:279` | string ops | <0.1ms |
| strip_finish_task_instructions | `claude_code_runner.rs:280` | string scan | <0.1ms |
| Command::spawn (claude CLI) | `claude_code_runner.rs:333` | process spawn | ~1–3s |
| BufReader lines (LLM streaming) | `claude_code_runner.rs:370` | network I/O | ~10–14s |
| child.wait | `claude_code_runner.rs:457` | process reap | <1ms |
| tokio::spawn(append_usage) | `claude_code_runner.rs:479` | background I/O | non-blocking |
| strip_cli_artifacts | `ctrl/mod.rs:1574` | string ops | <0.1ms |

**Dominant cost**: Step 8+9 (claude CLI startup + LLM network) = ~12–16s of the 13–17s total.
