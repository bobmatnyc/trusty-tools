# Contributing to open-mpm

## Development Setup

### Prerequisites

- Rust stable 1.80+ (`rustup show`)
- Git
- A `.env.local` with at minimum `OPENROUTER_API_KEY` for live integration tests

### First-time setup

```bash
git clone <repo-url> open-mpm
cd open-mpm
cat > .env.local <<'EOF'
OPENROUTER_API_KEY=sk-or-v1-...
# ANTHROPIC_API_KEY=sk-ant-api03-...
# BRAVE_API_KEY=BSA...
EOF

cargo build
cargo test
```

All unit tests should pass without API keys. Integration tests that require a
live API key are gated on the env var being set.

---

## Project Layout

```
open-mpm/
├── Cargo.toml              Rust package manifest
├── build.rs                Captures GIT_COMMIT_HASH at compile time
├── CLAUDE.md               Architectural reference for AI-assisted development
├── Makefile                Convenience wrappers around cargo commands
├── .env.local              API keys (not committed; in .gitignore)
├── config/
│   ├── agents/             Agent TOML definitions
│   └── workflows/          Workflow JSON definitions
├── docs/
│   ├── architecture.md     System architecture and module map
│   ├── getting-started.md  Quick start guide (this repo's docs)
│   ├── api-reference.md    Public API, config schemas, CLI flags
│   ├── contributing.md     This file
│   ├── performance/        Per-run telemetry JSON (auto-generated)
│   └── research/           Design research notes
└── src/                    Rust source tree (see architecture.md)
```

---

## Build System

```bash
# Check compilation without linking (fastest)
make check        # cargo check

# Run tests
make test         # cargo test

# Lint
make clippy       # cargo clippy --all-targets -- -D warnings
make fmt          # cargo fmt

# Both
make lint         # clippy + fmt

# Debug build
make build        # cargo build

# Release (optimized)
make release      # cargo build --release

# Run the CTRL REPL
make ctrl         # cargo run -- --ctrl

# Run a full prescriptive workflow
make run-task TASK_FILE=./my-task.md

# Print crate version
make version

# Wipe build artifacts
make clean
```

---

## Testing Strategy

### Unit tests

Unit tests live in `#[cfg(test)]` modules inside the source file they test.
They are self-contained and run without any external dependencies:

```bash
cargo test
```

**Key invariants covered by existing tests:**

| Test | What it guards |
|---|---|
| `ipc::tests::task_roundtrip` | NDJSON serialize/parse symmetry |
| `ipc::tests::extract_files_from_content_*` | File extraction from LLM output |
| `agents::tests::resolve_model_*` | Model resolution priority chain |
| `agents::tests::tools_config_parses_allowed` | Per-agent tool allowlist parsing |
| `tools::tests::registry_*` | ToolRegistry register/dispatch/schema |
| `tools::tests::dispatch_gated_*` | Allowlist enforcement |
| `llm::tests::tool_discipline_*` | Plain-text retry logic |
| `llm::tests::parallel_tool_dispatch_*` | Concurrent tool dispatch correctness |
| `workflow::config::tests::*` | WorkflowDef / Assignments / PhaseDef parsing |
| `workflow::config::tests::validate_*` | Assignment path traversal safety |
| `registry_tests::*` (main.rs) | Per-agent tool registry construction |

### Integration tests

Integration tests exist in `tests/` (when present) and require a live
`OPENROUTER_API_KEY`. They are automatically skipped when the env var is unset.

### Manual integration

The most reliable way to validate a change end-to-end is to run a workflow:

```bash
echo "Write a Python bubble sort with tests" > /tmp/test-task.md
cargo run -- --workflow prescriptive \
  --task-file /tmp/test-task.md \
  --out-dir /tmp/open-mpm-test-$(date +%s)
```

Watch the phase logs on stderr. The observe-agent report is printed to stdout.

---

## Coding Conventions

### Error handling

- Use `anyhow::Result` for all application-level error propagation.
- Use `thiserror` for library-level errors with stable discriminants.
- Add `.context("description")` at every `?` site that adds information.
- Tool implementations must return `ToolResult::err(...)` for recoverable errors
  and `ToolResult::fatal(...)` only for genuinely non-recoverable states. The
  LLM loop converts `err` results into `is_error: true` tool results so the
  model can reason about the failure. Never return `Err(...)` from `execute`.

### Async

- All I/O should be async. Prefer `tokio::fs` over `std::fs` in async contexts.
- Never call `std::fs::read_to_string` in async code — it blocks the worker
  thread. Use `tokio::fs::read_to_string` instead (see #96 / MAJ-4).
- Avoid `tokio::sync::Mutex` for coarse-grained state; prefer shorter critical
  sections with `std::sync::Mutex` when the lock is never held across an `.await`.

### Unsafe

- `std::env::set_var` and `std::env::remove_var` are `unsafe` in Rust 2024
  because they are not thread-safe. They are permitted only in single-threaded
  startup code (before tokio spawns worker threads) or in tests guarded by a
  `static Mutex`. Document every site with a `// SAFETY:` comment.
- Do not use `std::env::set_var` from within the async tool-calling loop.
  Thread per-invocation state via `RunContext` instead (see #89).

### Logging

- Use `tracing::{trace, debug, info, warn, error}` macros throughout.
- Structured fields: `tracing::info!(agent = %name, model = %model, "message")`.
- All tracing output goes to `stderr` (stdout is reserved for NDJSON IPC).
- Use `debug!` for high-frequency per-turn events; `info!` for phase-level events.

### Comments

Each public item should have three-line doc structure:

```rust
/// One-line summary.
///
/// Why: reason the item exists.
/// What: what it does mechanically.
/// Test: how it is tested (test name or "manual: <command>").
```

This convention is already used throughout the codebase; follow it for new code.

---

## Adding a New Agent

1. Create `config/agents/<name>.toml`:

```toml
[agent]
name = "my-agent"
role = "specialist"
model = "anthropic/claude-sonnet-4-6"
description = "Does X"

[llm]
temperature = 0.2
max_tokens = 8192

[system_prompt]
content = """
You are a specialist in X.
"""
```

2. Add a match arm in `build_registry_for_agent` in `src/main.rs` if the agent
   needs a non-default tool set.

3. Test with direct mode:

```bash
cargo run -- --direct my-agent --task "Hello, do X."
```

4. Add the agent to any relevant workflow JSON files.

---

## Adding a New Tool

1. Create `src/tools/<name>.rs`:

```rust
use async_trait::async_trait;
use serde_json::Value;
use crate::tools::traits::{ToolExecutor, ToolResult};

pub struct MyTool;

#[async_trait]
impl ToolExecutor for MyTool {
    fn name(&self) -> &str { "my_tool" }

    fn schema(&self) -> Value {
        serde_json::json!({
            "type": "function",
            "function": {
                "name": "my_tool",
                "description": "Does something useful.",
                "parameters": {
                    "type": "object",
                    "properties": {
                        "arg": {"type": "string", "description": "An argument."}
                    },
                    "required": ["arg"],
                    "additionalProperties": false
                }
            }
        })
    }

    async fn execute(&self, args: Value) -> ToolResult {
        let arg = match args.get("arg").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => return ToolResult::err("missing required argument: arg"),
        };
        ToolResult::ok(format!("processed: {arg}"))
    }
}
```

2. Export it in `src/tools/mod.rs`:

```rust
pub mod my_tool_name;
```

3. Register it in the appropriate agent branch in `build_registry_for_agent`
   in `src/main.rs`:

```rust
"my-agent" => {
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(MyTool));
    Some(reg)
}
```

4. Add unit tests inside the tool module:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    #[tokio::test]
    async fn my_tool_success() {
        let t = MyTool;
        let out = t.execute(serde_json::json!({"arg": "hello"})).await;
        assert!(!out.is_error());
        assert!(out.content().contains("hello"));
    }
}
```

---

## Adding a New Workflow

1. Create `config/workflows/<name>.json` following the schema in
   `docs/api-reference.md`.

2. Test with:

```bash
cargo run -- --workflow <name> --task "..." --out-dir /tmp/test-out
```

3. Check `RUST_LOG=debug` output to trace phase dispatch.

---

## Commit Conventions

Use Conventional Commits format:

```
<type>(<scope>): <imperative short description>

[optional body explaining why]

[optional footer: closes #123]
```

**Types:**

| Type | When to use |
|---|---|
| `feat` | New feature or behavior |
| `fix` | Bug fix |
| `refactor` | Code restructuring without behavior change |
| `perf` | Performance improvement |
| `test` | Adding or updating tests |
| `docs` | Documentation only |
| `chore` | Build system, dependencies, tooling |

**Scope** is optional but recommended. Use the module name or feature area:
`agents`, `workflow`, `tools`, `ipc`, `llm`, `ctrl`, `perf`.

**Examples:**

```
feat(workflow): add wave loop ordinal validation (#114)
fix(llm): prevent plain-text mid-task loops beyond max_turns (#33)
refactor(agents): centralize model resolution in resolve_model (#49)
perf(llm): reuse reqwest::Client via OnceLock (#98)
test(ipc): add roundtrip tests for result_with_usage field
docs: document wave loop assignments.json schema
chore: update fastembed to 5.13
```

**Keep commits atomic.** Each commit should represent one logical change. Split
large features into multiple commits (config parsing, core logic, tests, docs).

Reference issue numbers in the body or footer when the change addresses a
tracked issue. The project uses `#<number>` inline references for traceability.

---

## Pull Request Checklist

Before opening a PR, verify:

- [ ] `cargo test` passes with no failures
- [ ] `cargo clippy --all-targets -- -D warnings` passes with no warnings
- [ ] `cargo fmt --check` passes (run `cargo fmt` to fix)
- [ ] New public items have `///` doc comments with Why/What/Test structure
- [ ] New tools have unit tests in their module
- [ ] New config fields have corresponding TOML parse tests
- [ ] If adding a new execution path, manual integration test was run
- [ ] `CLAUDE.md` is updated if the architecture section is now outdated
- [ ] `docs/api-reference.md` is updated if new config fields or CLI flags were added

---

## Debugging Tips

### Enable verbose logging

```bash
RUST_LOG=debug cargo run -- --workflow prescriptive --task-file /tmp/task.md --out-dir /tmp/out
```

### Inspect NDJSON IPC manually

```bash
# Send a task to a sub-agent and inspect the raw response
echo '{"type":"task","id":"test-1","task":"Write hello world in Python"}' \
  | cargo run -- --agent python-engineer
```

### Check the code index

```bash
cargo run -- code search "error handling"
cargo run -- memory search "prior workflow decision"
```

### Orphan process detection

```bash
cargo run -- --check-orphans
# PID      STATUS     ALIVE    AGENT                    TASK
# 12345    running    no       code-agent               task-id  (ORPHAN)
```

### Performance telemetry

Every workflow run writes a JSON record to `docs/performance/runs/`:

```bash
cat docs/performance/runs/$(ls -t docs/performance/runs/ | head -1) | python3 -m json.tool
```

The top-level log is appended to `docs/performance/runs.log`.
