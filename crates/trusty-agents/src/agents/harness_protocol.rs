//! Auto-generated harness protocol injected into every agent's system prompt.
//!
//! Why: These are binary-level behavioral requirements, not user-configurable
//! content. Any agent running inside the trusty-agents harness MUST follow these
//! rules or workflow invariants (phase chaining, out_dir isolation, tool
//! dispatch) break. Keeping them as compiled-in constants means a user who
//! deletes or edits the (now non-existent) `config/harness/` directory sees
//! zero change in harness behavior — the protocol ships with the binary.
//! What: Three `&str` constants, selected by the prompt assembly code based
//! on agent runner / `use_finish_task` configuration. Each one carries
//! protocol only (not opinion): out_dir rule, `## Summary` footer, write_file
//! tool usage, and finish_task completion signal.
//! Test: `src/agents/prompt_builder.rs` tests assert each constant contains
//! its load-bearing keywords (out_dir / write_file / finish_task). To change
//! harness behavior, edit this file and recompile.

/// Injected into ALL agents regardless of runner or finish_task setting.
/// Contains only the minimum protocol required to operate within trusty-agents.
pub const BASE_PROTOCOL: &str = r#"
## trusty-agents Harness Protocol

### Output Directory
All files MUST be written to the absolute path provided as `out_dir` in your task context.
Never write to the repository root or any relative path.

- ❌ `write_file(path="main.py")` — lands at git root
- ❌ `write_file(path="./src/main.py")` — same problem
- ✅ `write_file(path="/abs/path/to/out/run-123/src/main.py")` — correct

### Phase Summary
End every response with a `## Summary` section (2–5 sentences):
- What was accomplished
- Key decisions or trade-offs
- Any blockers or issues
- What the next phase needs to know

This summary is forwarded as context to subsequent workflow phases.
"#;

/// Injected for claude-code runner agents with use_finish_task = false.
/// Enforces write_file tool usage instead of prose code output.
pub const CLAUDE_CODE_PROTOCOL: &str = r#"
## write_file Protocol

Use the `write_file` tool for ALL file output — never emit code as prose.

1. First action = `write_file` call (not a text response)
2. One `write_file` call per file
3. After all files written, call `advance_workflow_phase`

Never: code blocks in text, "Here is the implementation:" preambles, `## File: <path>` blocks.
"#;

/// Injected for agents with use_finish_task = true.
pub const FINISH_TASK_PROTOCOL: &str = r#"
## Task Completion
Call `finish_task` as your final action when work is complete. Do not end with plain text.
"#;
