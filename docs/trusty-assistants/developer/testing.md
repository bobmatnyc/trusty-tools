# Testing

## Running the test suite

```bash
# All tests
cargo test

# Only binary tests (skip integration tests under tests/)
cargo test --bins

# Run a single module
cargo test docs_index

# With logging
RUST_LOG=debug cargo test -- --nocapture
```

The full suite runs ~700 tests in under 10 seconds on modern hardware.

## Test layout

- **Unit tests** live alongside source in `mod tests` blocks gated by
  `#[cfg(test)]`. They cover individual functions and types in isolation.
- **Integration tests** live in `tests/`. They drive the binary end-to-end:
  - `tests/api_e2e.rs` — exercises the HTTP API
  - `tests/cli_project.rs` — exercises CLI subcommands
- **Doc tests** appear in some modules' `///` examples — `cargo test`
  runs them automatically.

## Test conventions

### Naming

`test_should_<expected_behavior>_when_<condition>`, or for short bug-fix
tests, `<feature>_<specific_assertion>`. Examples from the codebase:

- `docs_index_finds_relevant_document`
- `auth_middleware_rejects_request_without_token`
- `wave_loop_runs_one_agent_per_file`

### Async

Use `#[tokio::test]`:

```rust
#[tokio::test]
async fn submit_task_returns_running() {
    let state = AppState::default();
    let app = build_router(state);
    // … oneshot request, assert response …
}
```

### Temp directories

Use `std::env::temp_dir().join(format!("…_{}", uuid::Uuid::new_v4()))`
and clean up at the end of the test. Several existing tests follow this
pattern (see `docs_index::tests`).

### LLM-dependent tests

Tests that require a live API key gate on the env var:

```rust
#[tokio::test]
async fn integration_test() {
    if std::env::var("OPENROUTER_API_KEY").is_err() {
        eprintln!("skipping: OPENROUTER_API_KEY not set");
        return;
    }
    // …
}
```

CI runs the suite without API keys, so these tests must skip cleanly.

## Mocking patterns

### Mock agent runners

`tests/api_e2e.rs` uses a mock implementation of `AgentRunner` that returns
fixed strings, so the workflow engine can be exercised without LLM calls.
Pattern:

```rust
struct MockRunner { reply: String }

#[async_trait]
impl AgentRunner for MockRunner {
    async fn run(&self, _ctx: RunContext) -> Result<AgentOutput> {
        Ok(AgentOutput { content: self.reply.clone(), … })
    }
}
```

### Mock HTTP clients

For the LLM client, use `wiremock` or hand-roll a `tokio::net::TcpListener`
for fixed responses. The existing tests prefer the latter for simplicity.

## Coverage targets

| Area | Target |
|---|---|
| Critical paths (PM dispatch, workflow engine, IPC) | ~95% |
| Tool implementations | ~90% |
| LLM adapters | ~80% (gated on live API tests for end-to-end) |
| UI / Tauri code | manual smoke tests |

Run `cargo tarpaulin` (not currently in CI) to measure.

## What to write tests for

✅ Business logic: TF-IDF math, workflow phase transitions, NDJSON parsing
✅ Edge cases: empty inputs, malformed JSON, missing files
✅ Error paths: ensure `Err(...)` flows through correctly
✅ State mutations: `AppState` transitions, `WorkflowContext` accumulation
✅ Public APIs: every `pub fn` should have at least one test

❌ Don't test:
- Framework internals (axum, tokio internals)
- Trivial getters / `#[derive]` boilerplate
- Generated code
- LLM responses (mock them)

## Linting

```bash
cargo clippy --all-targets
```

The codebase carries a small number of pre-existing clippy warnings
(documented as known). New PRs should not introduce additional warnings.

## Formatting

```bash
cargo fmt
```

CI fails on formatting diffs. Run `cargo fmt` before every commit.

## End-to-end smoke test

```bash
# Build release binary
cargo build --release

# Start API server on a free port
./target/release/open-mpm --api --port 7654 &
SERVER_PID=$!

# Wait for binding
sleep 2

# Health check
curl -s http://localhost:7654/api/health | jq
# {"status":"ok","version":"0.1.37"}

# Docs search
curl -s "http://localhost:7654/api/docs/search?q=workflow" | jq
# {"results":[{...}], "status":"ok"}

# Cleanup
kill $SERVER_PID
```
