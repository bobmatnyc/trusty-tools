# Crate Audit: Custom vs. Battle-Tested Libraries

**Date:** 2026-04-25
**Scope:** Full `src/` review against `Cargo.toml` dependencies

---

## Summary Table

| Area | Current approach | Recommended crate | Priority | Effort |
|------|-----------------|-------------------|----------|--------|
| CLI arg parsing | Hand-rolled `std::env::args()` + manual flag scanning in `main.rs`, `ompm.rs`, `search_cmd.rs`, `memories_cmd.rs` | `clap` (derive API) | HIGH | Medium |
| Event bus (in-process) | Custom `OnceLock<broadcast::Sender<Event>>` singleton | Fine as-is (correct use of `tokio::sync::broadcast`) | LOW | None |
| Event bus (topic filtering) | Manual `session_id()` match at subscriber site | `async-broadcast` for topic-keyed channels | LOW | Medium |
| IPC framing | Hand-rolled NDJSON: `serde_json::to_string + '\n'` + `read_line` | Fine as-is; `tarpc` / `tonic` would be over-engineering for this use case | LOW | None |
| Workflow retry / backoff | No retry logic at all; phase failures propagate immediately | `backon` for LLM-call retries and sub-agent spawn retries | MEDIUM | Small |
| Workflow DAG | Linear `Vec<PhaseDef>` iteration + parallel phase via `tokio::spawn` | `petgraph` if DAG topology becomes complex; overkill now | LOW | Large |
| Workflow state machine | Ad-hoc boolean flags in `WorkflowEngine` (`code_phase_used_claude_code`, etc.) | `statig` or `rust-fsm` if state combinatorics grow | LOW | Large |
| Memory / vector store | `redb` + `usearch` + `fastembed-rs` | All three are correct choices; usage is idiomatic | LOW | None |
| BM25 implementation | Custom `Bm25Index` in `src/context/bm25.rs` | `bm25` crate or keep custom (97 lines, correct, tested) | LOW | Small |
| Embedding | `fastembed-rs` with `AllMiniLML6V2` | Already the right call | LOW | None |
| HTTP server | Axum 0.7 + `tower-http` (cors, compression) | Correct; `tower-http::trace::TraceLayer` is missing | MEDIUM | Small |
| SSE streaming | `axum::response::sse::Sse` + `async-stream` | Correct; no custom wrapping needed and none present | LOW | None |
| Config loading | TOML files with `toml` crate + serde derive | Fine; `config` crate only needed for env-override layering | LOW | Medium |
| Error handling | `anyhow` (application) + `thiserror` (library) | Correct and consistent; no `Box<dyn Error>` found | LOW | None |
| Logging / tracing | `tracing` + `tracing-subscriber` fmt with env-filter | Correct; `tower-http::trace::TraceLayer` gap noted above | MEDIUM | Small |
| Process spawning | `tokio::process::Command` with piped stdio | Correct stdlib approach | LOW | None |
| Process supervision | None — no restart/retry on sub-agent crash; `spawn_subagent_*` is fire-once | `tokio-process-stream` or manual supervision for long-running daemons | MEDIUM | Medium |
| Unix socket IPC | Raw `tokio::net::UnixListener` + NDJSON per line | Fine for this use case; `tarpc` would add value only if protocol complexity grows | LOW | None |
| Duplicate spawn logic | `spawn_subagent_and_run_with_full_env_ctx` and `spawn_subagent_with_config_dir` share ~80% identical code | Refactor into one function; not a crate question | MEDIUM | Small |

---

## HIGH Priority Items

### 1. CLI Arg Parsing — Replace with `clap`

**Where:** `src/main.rs` (240+ lines of manual `args[i]` matching), `src/bin/ompm.rs` (manual slice scanning), `src/cli/search_cmd.rs` and `src/cli/memories_cmd.rs` (hand-rolled `parse_args` functions with positional + flag logic).

**Problem:** The hand-rolled parsers are subtly inconsistent — `--json` is detected via `rest.contains(&"--json")` while positional args are extracted by filtering out known flags. Flag ordering matters in some places and not others. Error messages say "usage: ..." as a bare string with no structure. Adding a new flag to any subcommand requires touching the scanner, the match arm, and the help text by hand. The `main.rs` dispatch is a long `if args[i] == "--flag"` chain that is already 200+ lines and growing.

**Fix:** Adopt `clap` with derive macros. Every subcommand becomes a struct with `#[arg(...)]` annotations. Help text, error messages, shell completion, and `--version` output are generated automatically. The existing `Command` enums in `search_cmd` and `memories_cmd` map naturally to `clap::Subcommand`. Effort is roughly a one-day refactor.

---

## MEDIUM Priority Items

### 2. HTTP Request Tracing — Add `tower-http::trace::TraceLayer`

**Where:** `src/api/server.rs` — the Axum router applies `CorsLayer` and `CompressionLayer` from `tower-http` but does not include `TraceLayer`.

**Problem:** All HTTP requests are invisible to `tracing` subscribers. Debugging latency or errors in the API server requires adding ad-hoc `tracing::info!` calls at each route handler. `tower-http` is already a direct dependency (v0.5 with `cors` + `compression-gzip` features). Adding `TraceLayer` is one line in the router builder and provides structured `INFO`-level request/response spans with latency automatically.

**Fix:** Enable `tower-http = { features = [..., "trace"] }` in `Cargo.toml` and add `.layer(TraceLayer::new_for_http())` to the Axum `Router`. No logic changes needed.

### 3. LLM / Sub-agent Retry — Add `backon`

**Where:** `src/subprocess.rs` — `spawn_subagent_and_run_with_full_env_ctx` makes a single attempt and fails hard on any process error. `src/llm/mod.rs` — LLM API calls via `async-openai` also have no transient-error retry.

**Problem:** OpenRouter and the Anthropic API both return transient 429 (rate-limit) and 5xx errors. The current code surfaces these as hard `anyhow::Error` failures that abort the entire phase. Users have to re-run the whole workflow. The `backon` crate (v1.x, 2024 standard) provides zero-boilerplate exponential backoff with `FibonacciBuilder` and `ExponentialBuilder` that compose naturally with `async` closures.

**Fix:** Wrap the `async-openai` `create_chat_completion` call and the `tokio::process::Command::spawn` + IPC round-trip in `backon::ExponentialBuilder::default().with_max_times(3)`. This is a small, targeted change at two call sites.

### 4. Duplicate Subprocess Spawn Logic

**Where:** `src/subprocess.rs` — `spawn_subagent_and_run_with_full_env_ctx` (lines 278–415) and `spawn_subagent_with_config_dir` (lines 466–583) share approximately 80% identical code: same `Command` setup, same writer/reader `tokio::spawn` pattern, same `#147` rescue logic, same `record_mistake_fire_and_forget` call.

**Problem:** Bug fixes and behavioral changes must be applied to both functions simultaneously. The `#147` non-zero exit rescue was already duplicated. This is not a missing crate — it is a refactoring opportunity to extract a single `spawn_and_run_inner(cmd: Command, agent_name, task, history, ctx)` that both public functions delegate to.

---

## Items That Are Fine To Keep Custom

**Event bus (`src/events.rs`):** The `OnceLock<broadcast::Sender<Event>>` singleton is 200 lines, well-tested, purpose-built for this use case, and has clear semantics. `async-broadcast` would add topic routing but the current per-subscriber `session_id` filter is sufficient. No replacement needed.

**NDJSON IPC (`src/ipc/mod.rs`):** The framing is two functions — `serialize_message` (adds `\n`) and `parse_message` (strips `\n`, calls `serde_json::from_str`). It is correct, tested, and matches the protocol documented in `CLAUDE.md`. Adopting `tarpc` or `tonic` for this use case would be extreme over-engineering and would break the sub-agent process boundary model.

**BM25 (`src/context/bm25.rs`):** 120 lines, standard k1=1.5/b=0.75 constants, well-tested. The `bm25` crate on crates.io adds no meaningful benefit over this implementation for the corpus sizes in play (in-memory turn history). Replacing it would be churn for no gain.

**Memory store (`src/memory/redb_usearch.rs`):** `redb` + `usearch` + `fastembed-rs` is the correct stack. `redb` is idiomatic (transactions, typed table definitions). `usearch` is the right ANN index for embedded use. `fastembed-rs` is used correctly with a `Mutex<TextEmbedding>` guard for the non-`Sync` ONNX session. No changes needed.

**Config loading (TOML + serde):** Agent TOML files use `toml` crate with serde derive — the standard approach. The `config` crate would only add value if env-variable overlay on top of TOML was desired (e.g., `OPEN_MPM_AGENT_MODEL` overriding `[llm] model`). The codebase already handles model overrides via explicit `OPEN_MPM_MODEL_<AGENT>` env vars, so the `config` crate adds little here.

**Unix socket controller (`src/ctrl/socket.rs`):** Raw `tokio::net::UnixListener` is the correct choice. The protocol is a single NDJSON command/response pair per connection — there is no need for an RPC framework.

**Error handling:** `anyhow` is used throughout application code, `thiserror` is used for the one library-style error enum (`WorkflowError`). No `Box<dyn Error>` found in non-test code. The error discipline is consistent and correct.

**Tracing setup:** `tracing` + `tracing-subscriber::fmt` with `EnvFilter` in `main.rs` is the standard approach. The `tracing-subscriber` features cover everything the project needs.

---

## Missing Feature Gaps (Not Crate Replacements)

These are capabilities the project lacks entirely that established crates would cover cleanly:

| Gap | Crate | Notes |
|-----|-------|-------|
| HTTP request/response structured logging | `tower-http` `trace` feature | Already a dependency; just needs feature + one router line |
| Transient error retry (LLM, subprocess) | `backon` | ~10 lines at two call sites |
| Shell completion for `ompm` and `open-mpm` | `clap_complete` (ships with `clap`) | Free once `clap` is adopted |
| Structured CLI help text | `clap` | Free once `clap` is adopted |
