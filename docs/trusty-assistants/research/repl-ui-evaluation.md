# REPL UI Evaluation for open-mpm

**Date**: 2026-04-25
**Status**: Complete — recommendation made

---

## 1. Recommendation

**Use `reedline` 0.47.0 directly** — do not use `repl-rs`.

`reedline` is the line-editing backbone of Nushell, actively maintained (last release
April 2026), and provides everything the REPL needs: persistent history, tab completion
with menus, syntax highlighting, ANSI color via `nu-ansi-term`, and Vi/Emacs keybindings.
It does not impose a command-dispatch framework, which is fine — open-mpm already has
one (`handle_command` in `src/ctrl/mod.rs`). The integration is a surgical replacement of
the raw `tokio::io::stdin` + manual `write_all(prompt)` loop with a `reedline::read_line()`
call in a `tokio::task::spawn_blocking` wrapper. Total new code: ~350 LOC.

`repl-rs` is rejected because its callbacks are synchronous (`fn`, not `async fn`), it
pins to rustyline 8.2.0 (current is 18.0.0), and bridging its sync callbacks into tokio
requires either `block_on` (which panics inside `#[tokio::main]`) or a channel-based
round-trip — more code than just using reedline directly.

---

## 2. Candidate Assessment

### 2.1 repl-rs 0.2.9

| Criterion | Assessment |
|---|---|
| Latest version | 0.2.9 (2025-08-13) |
| Total downloads | 51,455 (low) |
| Async support | None. Callbacks are `fn(HashMap<String,Value>, &mut T) -> Result<Option<String>>` — fully synchronous. No `async fn` support. |
| History | Via rustyline 8.2.0 (pinned old version) |
| Autocomplete | `.use_completion(true)` — basic, word-based, no custom completers |
| Color/styling | yansi 0.5.0 for prompt only |
| Maintenance | Active (moved to Codeberg); limited community |
| API surface | Builder pattern is pleasant but opinionated |

**Verdict: Rejected.** The synchronous callback model is a hard blocker. Every REPL
command in open-mpm must `await` a REST call or an inline PM task dispatch. Wrapping
async futures inside sync callbacks requires `Handle::current().block_on(...)`, which
panics when called from within a tokio worker thread (the default `#[tokio::main]`
runtime). The workaround (spawn a new `tokio::runtime::Runtime` per callback) wastes
threads and is architecturally wrong. Additionally, pinning rustyline to 8.2.0 creates a
dep conflict with any project that needs a newer rustyline.

**Bridging attempt would look like this — and this is why it's wrong:**
```rust
// DO NOT DO THIS inside #[tokio::main]
fn submit(args: HashMap<String, Value>, ctx: &mut AppCtx) -> repl_rs::Result<Option<String>> {
    // panics: "Cannot start a runtime from within a runtime"
    tokio::runtime::Handle::current().block_on(async {
        ctx.client.post("/tasks").send().await
    })
}
```

### 2.2 reedline 0.47.0

| Criterion | Assessment |
|---|---|
| Latest version | 0.47.0 (2026-04-11) |
| Total downloads | 2,221,455 (Nushell production dependency) |
| Async support | `read_line()` is blocking, but designed to be called from `spawn_blocking`. ExternalPrinter enables async output from tokio tasks while waiting for input. |
| History | `FileBackedHistory`, `SqliteBackedHistory`; session isolation flag |
| Autocomplete | `Completer` trait with `DefaultCompleter`; `ColumnarMenu`, `IdeMenu`, `ListMenu` |
| Color/styling | `StyledText` + `nu-ansi-term` for full ANSI; `Highlighter` trait for per-keystroke highlighting |
| Maintenance | Actively developed as Nushell's core line editor; ~monthly releases |
| API surface | Low-ceremony; bring your own dispatch loop |

**Verdict: Recommended.** The blocking `read_line()` runs cleanly in
`tokio::task::spawn_blocking`, returning to async code via `await`. The `ExternalPrinter`
feature lets background tasks (PM progress updates, bus messages) print to the terminal
without corrupting the input line — exactly what is needed when a delegated agent streams
output while the user waits.

### 2.3 rustyline 18.0.0

| Criterion | Assessment |
|---|---|
| Latest version | 18.0.0 (2026-03-29) |
| Total downloads | 32,296,802 (most-used readline in Rust) |
| Async support | None; fully synchronous |
| History | File-backed; `add_history_entry` |
| Autocomplete | `Completer` trait; helper structs for file/keyword completion |
| Color/styling | `Highlighter` trait; `ColorMode` config |
| Maintenance | Excellent; widely deployed |

**Verdict: Viable but reedline is better for this use case.** rustyline is mature and
extremely widely used, but it lacks reedline's `ExternalPrinter` — the key feature needed
for streaming agent output while waiting for the next user input. Both need
`spawn_blocking`, so there is no async advantage to rustyline.

### 2.4 linefeed 0.6.0

| Criterion | Assessment |
|---|---|
| Latest version | 0.6.0 (2019-04-26) |
| Total downloads | 575,784 |
| Async support | None |
| Maintenance | Abandoned (no commits since 2019) |

**Verdict: Rejected.** Unmaintained.

### 2.5 crossterm-based custom REPL

| Criterion | Assessment |
|---|---|
| LOC required | ~600–1000 (raw event loop, cursor handling, history ring buffer) |
| Async support | Native; crossterm's event stream is a futures `Stream` |
| Color/styling | Full control |
| Maintenance | Your code |

**Verdict: Not recommended for this stage.** The implementation cost is high and reedline
already uses crossterm internally. Build a custom REPL only if reedline's menu/completion
model proves incompatible with open-mpm's UX needs.

---

## 3. Architecture: Connected vs. Inline Mode

### 3.1 Connected Mode

The REPL is a thin client. It calls `http://localhost:7654/api/*` (the same axum REST
server the browser uses). Commands map to REST calls via `reqwest`.

```
User -> reedline REPL -> reqwest -> axum /api/tasks POST -> PM orchestrator -> sub-agents
                                                      <-- SSE stream (agent output)
```

**Pros:**
- Zero coupling between REPL and orchestrator code; runs while server is live
- Hot-reload the REPL without restarting the server
- Can run on a different machine (SSH, LAN)
- Forces the REST API to be complete (good discipline)

**Cons:**
- Requires the server to be running first (`--serve`)
- SSE streaming adds latency overhead and parsing complexity
- Token auth must be threaded through (`--token` flag on server)

### 3.2 Inline Mode

The REPL directly calls `ctrl::run_ctrl()` internals in-process. No HTTP round-trip.

```
User -> reedline REPL -> ctrl::dispatch_task() -> PM orchestrator -> sub-agents
```

**Pros:**
- Works offline and without a running server
- Lower latency (no serialization, no HTTP)
- Direct access to bus messages and progress events
- Same as the current `cargo run` experience — just with better line editing

**Cons:**
- REPL must be in the same binary as the orchestrator (it already is)
- Tighter coupling: REPL changes may require orchestrator changes

### 3.3 Recommendation: Inline Mode, with Optional Connected Mode Later

**Start with inline mode.** The current `ctrl::run_ctrl()` is already inline; this is a
drop-in improvement of the raw stdin loop. The REPL replaces the `tokio::io::stdin` +
`write_all(prompt.as_bytes())` loop with `reedline::read_line()` in `spawn_blocking`.

Connected mode can be added later as `ompm connect [--host host] [--port 7654]`, which
is a thin `reqwest`-only binary that requires no orchestrator code.

The `--serve` flag and the REPL are orthogonal: `cargo run` (inline REPL) and
`cargo run -- --serve` (HTTP server for browser/Tauri) can coexist.

---

## 4. Command Vocabulary

The REPL preserves the existing `/command` convention from `src/ctrl/mod.rs`. Bare text
(non-slash) is forwarded as a task to the active PM or CTRL's LLM.

| Command | Arguments | Description |
|---|---|---|
| `/connect <path>` | project path | Start PM session for project at path |
| `/disconnect` | — | Return to CTRL prompt (PM keeps running) |
| `/status` | — | Show active PM, queued tasks, running agents |
| `/projects` | — | List known projects from registry |
| `/tasks` | — | Show active and recently completed tasks |
| `/history [n]` | optional count | Show last n session exchanges (default 10) |
| `/memory recall <query>` | query string | Semantic memory search |
| `/memory store <key> <value>` | key, value | Store a memory entry |
| `/sessions [--project p]` | optional filter | List past sessions |
| `/send <project> <message>` | project name, message | Send inter-project bus message |
| `/search <query>` | query | Search project docs semantically |
| `/help` | — | Print command list |
| `/quit` | — | Shutdown and exit |

**Tab completion targets:**
- Command names after `/`
- Project names for `/connect` and `/send`
- Agent names when applicable

---

## 5. Implementation Plan

### 5.1 Files to Create or Modify

| File | Action | Est. LOC |
|---|---|---|
| `Cargo.toml` | Add `reedline = "0.47"` and `nu-ansi-term = "0.50"` | +2 |
| `src/ctrl/repl.rs` | New: reedline setup, `OmpmCompleter`, `OmpmHighlighter`, `run_repl()` | ~250 |
| `src/ctrl/mod.rs` | Replace raw stdin loop (lines 789–842) with `run_repl()` call | ~-50, +5 |

Total net change: ~+200 LOC after removing the old loop.

### 5.2 Dependency Addition

```toml
# Cargo.toml
reedline = "0.47"
nu-ansi-term = "0.50"
```

`nu-ansi-term` is already a transitive dep of reedline; pinning it directly ensures the
color values used in the REPL match the palette without version skew.

### 5.3 Estimated Effort

2–3 hours for a working inline REPL with history, completion, and color. Another 1 hour
for `ExternalPrinter` integration (streaming agent output). Connected mode: additional
4–6 hours.

---

## 6. Convention Alignment

The web UI (Tauri chat interface design doc: `docs/research/tauri-chat-interface-design.md`)
uses:

| Element | Web/Tauri color | ANSI terminal equivalent |
|---|---|---|
| Primary / PM messages | `#3B4CCA` (indigo) | `nu_ansi_term::Color::Rgb(59,76,202)` |
| Agent messages | `#2EC4B6` (teal) | `nu_ansi_term::Color::Rgb(46,196,182)` |
| Running/amber accent | `#FF9F1C` (amber) | `nu_ansi_term::Color::Rgb(255,159,28)` |
| Error messages | `#E71D36` (red) | `nu_ansi_term::Color::Rgb(231,29,54)` |
| Prompt | `CTRL> ` / `PM[name]> ` | Indigo bold for CTRL, teal for PM |

Message hierarchy in the terminal mirrors the web UI:

```
CTRL> write a markdown formatter        ← user input (plain)

[PM: open-mpm] Delegating to python-engineer...  ← PM message (indigo)
[agent: python-engineer] Writing script...        ← agent message (teal)
[agent: python-engineer] Done. Output in /tmp/    ← agent message (teal)

[PM: open-mpm] Task complete.                     ← PM message (indigo)
```

The `ExternalPrinter` in reedline is the mechanism for printing PM and agent messages
while `read_line()` is blocked waiting for the next user command. This is the terminal
equivalent of the web UI's streaming chat bubbles.

---

## 7. Prototype Sketch

```rust
// src/ctrl/repl.rs
//
// Why: Replace raw tokio::io::stdin loop with reedline for history, completion,
// and ANSI color — matching the web/Tauri UI conventions in the terminal.
// What: OmpmHighlighter colors the prompt prefix by context; OmpmCompleter
// provides tab completion for /commands and known project names.
// Test: cargo run; type /h<TAB> → expands to /help; up-arrow recalls last input.

use std::borrow::Cow;
use std::sync::Arc;

use anyhow::Result;
use nu_ansi_term::{Color, Style};
use reedline::{
    Completer, DefaultHistory, ExternalPrinter, FileBackedHistory, Highlighter,
    Prompt, PromptEditMode, PromptHistorySearch, PromptHistorySearchStatus,
    Reedline, ReedlineEvent, Signal, Span, Suggestion,
};

// ----- Color palette (matches web/Tauri UI) -----
const INDIGO: Color = Color::Rgb(59, 76, 202);   // PM messages, CTRL prompt
const TEAL: Color = Color::Rgb(46, 196, 182);    // Agent messages, PM prompt
const AMBER: Color = Color::Rgb(255, 159, 28);   // Running / warning
const DIM: Color = Color::DarkGray;

// ----- Prompt -----
pub struct OmpmPrompt {
    pub text: String, // e.g. "CTRL> " or "PM[proj]> "
}

impl Prompt for OmpmPrompt {
    fn render_prompt_left(&self) -> Cow<str> {
        Cow::Owned(
            Style::new()
                .bold()
                .fg(if self.text.starts_with("PM[") { TEAL } else { INDIGO })
                .paint(&self.text)
                .to_string(),
        )
    }
    fn render_prompt_right(&self) -> Cow<str> { Cow::Borrowed("") }
    fn render_prompt_indicator(&self, _: PromptEditMode) -> Cow<str> { Cow::Borrowed("") }
    fn render_prompt_multiline_indicator(&self) -> Cow<str> { Cow::Borrowed("::: ") }
    fn render_prompt_history_search_indicator(
        &self,
        history_search: PromptHistorySearch,
    ) -> Cow<str> {
        let indicator = match history_search.status {
            PromptHistorySearchStatus::Passing => "",
            PromptHistorySearchStatus::Failing => "failing ",
        };
        Cow::Owned(format!("({}reverse-i-search)`{}': ", indicator, history_search.term))
    }
}

// ----- Completer -----
pub struct OmpmCompleter {
    commands: Vec<String>,
    projects: Vec<String>,
}

impl OmpmCompleter {
    pub fn new(projects: Vec<String>) -> Self {
        let commands = vec![
            "/connect".into(), "/disconnect".into(), "/status".into(),
            "/projects".into(), "/tasks".into(), "/history".into(),
            "/memory".into(), "/sessions".into(), "/send".into(),
            "/search".into(), "/help".into(), "/quit".into(),
        ];
        Self { commands, projects }
    }
}

impl Completer for OmpmCompleter {
    fn complete(&mut self, line: &str, pos: usize) -> Vec<Suggestion> {
        let word = &line[..pos];
        // Complete /commands
        if word.starts_with('/') {
            return self.commands.iter()
                .filter(|c| c.starts_with(word))
                .map(|c| Suggestion {
                    value: c.clone(),
                    description: None,
                    style: None,
                    extra: None,
                    span: Span::new(0, pos),
                    append_whitespace: true,
                })
                .collect();
        }
        // Complete project names after /connect or /send
        let parts: Vec<&str> = word.splitn(3, ' ').collect();
        if parts.len() == 2
            && (parts[0] == "/connect" || parts[0] == "/send")
        {
            return self.projects.iter()
                .filter(|p| p.starts_with(parts[1]))
                .map(|p| Suggestion {
                    value: p.clone(),
                    description: None,
                    style: None,
                    extra: None,
                    span: Span::new(parts[0].len() + 1, pos),
                    append_whitespace: true,
                })
                .collect();
        }
        vec![]
    }
}

// ----- Highlighter -----
pub struct OmpmHighlighter;

impl Highlighter for OmpmHighlighter {
    fn highlight(&self, line: &str, _cursor: usize) -> reedline::StyledText {
        use reedline::StyledText;
        let mut styled = StyledText::new();
        if line.starts_with('/') {
            styled.push((Style::new().fg(AMBER), line.to_string()));
        } else {
            styled.push((Style::new(), line.to_string()));
        }
        styled
    }
}

// ----- Main REPL entry point -----
/// Run the interactive REPL loop with reedline.
///
/// Why: Replaces the raw tokio::io::stdin + write_all(prompt) loop in
/// src/ctrl/mod.rs to gain history (up-arrow), tab completion for /commands
/// and project names, and ANSI color matching the web/Tauri UI palette.
/// What: Runs reedline in spawn_blocking so the async executor stays free.
/// The ExternalPrinter lets background PM/agent tasks print to stdout while
/// read_line() is blocked waiting for user input.
/// Test: cargo run; up-arrow recalls last command; /h<TAB> completes to /help.
pub async fn run_repl(
    prompt_text: String,
    projects: Vec<String>,
    printer: ExternalPrinter<String>,
) -> Result<Option<String>> {
    tokio::task::spawn_blocking(move || -> Result<Option<String>> {
        let history = Box::new(
            FileBackedHistory::with_file(50, dirs::home_dir()
                .unwrap_or_default()
                .join(".open-mpm")
                .join("repl-history.txt"))
                .unwrap_or_else(|_| FileBackedHistory::new(50)),
        );

        let mut line_editor = Reedline::create()
            .with_history(history)
            .with_completer(Box::new(OmpmCompleter::new(projects)))
            .with_highlighter(Box::new(OmpmHighlighter))
            .with_external_printer(printer);

        let prompt = OmpmPrompt { text: prompt_text };

        match line_editor.read_line(&prompt)? {
            Signal::Success(line) => Ok(Some(line)),
            Signal::CtrlC => Ok(Some(String::new())), // empty = continue
            Signal::CtrlD => Ok(None), // None = exit
        }
    })
    .await?
}
```

### Replacing the existing loop in src/ctrl/mod.rs

The current loop (lines 789–842) becomes:

```rust
// In run_ctrl(), replace the raw stdin loop with:
use crate::ctrl::repl::{run_repl, OmpmHighlighter};
use reedline::ExternalPrinter;

let printer: ExternalPrinter<String> = ExternalPrinter::default();

// Hand the printer clone to the bus relay task so agent output prints
// without corrupting the input line. (The relay task already exists at
// line ~707 in ctrl/mod.rs — pass printer.clone() into its closure.)

loop {
    let projects = ctrl.known_project_names(); // new helper returning Vec<String>
    let prompt = ctrl.prompt();

    match run_repl(prompt, projects, printer.clone()).await? {
        None => {
            println!("Bye.");
            break;
        }
        Some(line) if line.trim().is_empty() => continue,
        Some(line) => {
            let trimmed = line.trim().to_string();
            if trimmed.starts_with('/') {
                match handle_command(&mut ctrl, &trimmed).await {
                    Ok(false) => break,
                    Ok(true) => {}
                    Err(e) => eprintln!("command error: {e:#}"),
                }
            } else if ctrl.active.is_some() {
                match ctrl.dispatch_task(trimmed).await {
                    Ok(output) => println!("{}", output),
                    Err(e) => eprintln!("task error: {e:#}"),
                }
            } else {
                match ctrl_chat_turn(&mut ctrl, &trimmed).await {
                    Ok(output) if !output.trim().is_empty() => println!("{output}"),
                    Ok(_) => {}
                    Err(e) => eprintln!("ctrl error: {e:#}"),
                }
            }
        }
    }
}
ctrl.shutdown_all().await;
```

---

## 8. Rejected Approaches Summary

| Approach | Rejection Reason |
|---|---|
| `repl-rs 0.2.9` | Sync-only callbacks; cannot `await` inside them without panicking in tokio. Pins rustyline 8.x (stale). |
| `linefeed 0.6.0` | Abandoned in 2019. |
| `crossterm` custom | High implementation cost (~700 LOC) for no benefit over reedline. |
| Connected-mode first | Deferred: adds reqwest round-trip complexity and requires `--serve` to be running. Inline mode is simpler and matches current UX. |

---

## 9. Open Questions

1. **ExternalPrinter message formatting**: The relay task (bus messages from other PMs)
   currently prints raw text via `eprintln!`. Should bus messages get teal formatting
   (`[PM:name] message`) before being sent to the ExternalPrinter? Recommendation: yes,
   apply the same color rules as agent output.

2. **History file location**: `~/.open-mpm/repl-history.txt` is proposed. Should it be
   per-project or global? Recommendation: global, matching shell history conventions.

3. **Connected mode priority**: If the server is already running (`--serve`), should
   `cargo run` (REPL) automatically detect and use connected mode? Recommendation: no
   — keep inline as default, add explicit `ompm connect` subcommand later.
