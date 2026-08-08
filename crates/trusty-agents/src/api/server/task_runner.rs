//! Subprocess workflow execution + recap dispatch (#149, #151, #371).
//!
//! Why: The orchestrator binary already wires build counters, tracing,
//! registries, skill discovery, etc. Re-using it as a subprocess avoids
//! duplicating 200+ lines of setup and keeps the server self-contained.
//! Recap dispatch is centralised so both the in-process and subprocess task
//! completion paths behave identically.
//! What: `run_task` spawns `trusty-agents --workflow … --json` (or `--direct
//! <agent>`), relays the child's progress/event lines, and parses stdout into
//! a `PmResponse`. `maybe_emit_recap` ticks the recap tracker and persists a
//! recap when the interval fires.
//! Test: Exercised via integration; recap module unit tests cover assembly.

use anyhow::Result;

use super::handlers::TaskRequest;
use super::state::{AppState, state_dir};
use crate::api::types::{PhaseProgress, PmResponse, PmStatus};
use crate::events::{self, EVENT_LINE_PREFIX, Event};
use crate::recap::{self, RecapPhase, RecapTask};

/// Execute a `TaskRequest` by invoking `trusty-agents --workflow ... --json`
/// (or `--direct <agent>` when `agent` is set) as a subprocess.
///
/// Why: The orchestrator binary already wires build counters, tracing,
/// registries, skill discovery, etc. Re-using it avoids duplicating 200+
/// lines of setup and keeps the server self-contained.
/// What: Builds argv, spawns the child, parses stdout as JSON `PmResponse`.
/// Maps non-JSON stdout or non-zero exit to a `PmResponse::error`.
/// Test: Exercised via integration tests; unit-tested via `TaskRequest`
/// parsing.
pub(super) async fn run_task(id: &str, req: TaskRequest, state: AppState) -> Result<PmResponse> {
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, BufReader};
    use tokio::process::Command;

    // #151 phase-4: direct agent dispatch. When `agent` is set, call
    // `trusty-agents --direct <agent> --task <text>` which bypasses workflow
    // orchestration.
    let mut cmd = Command::new(current_exe()?);
    let is_direct = req.agent.is_some();
    if let Some(agent) = &req.agent {
        cmd.arg("--direct").arg(agent);
    } else {
        let workflow = req.workflow.as_deref().unwrap_or("prescriptive");
        cmd.arg("--workflow").arg(workflow);
        // `--json` only affects workflow mode (direct mode emits raw content).
        cmd.arg("--json");
    }
    cmd.arg("--task").arg(&req.task);
    if let Some(out_dir) = &req.out_dir {
        cmd.arg("--out-dir").arg(out_dir);
    }
    if let Some(task_file) = &req.task_file {
        cmd.arg("--task-file").arg(task_file);
    }

    // Tauri GUI: honour per-task working directory so project-scoped PMs run
    // in the user-selected project root.
    if let Some(project_path) = &req.project_path {
        let p = std::path::Path::new(project_path);
        if p.is_dir() {
            cmd.current_dir(p);
        } else {
            tracing::warn!(
                ?project_path,
                "project_path is not a directory; ignoring and using caller cwd"
            );
        }
    }

    // #149: Pipe stderr so we can sniff `__OMPM_PROGRESS__` lines and stream
    // them into the stored PmResponse. Other stderr lines pass through to our
    // own stderr (the original `inherit()` behavior, but with a parse layer).
    //
    // #3063: `kill_on_drop(true)` is what makes `DELETE /api/task/:id` (and
    // `POST /api/clear-context`) actually kill this subprocess rather than
    // merely abandoning the awaiting future. When the enclosing `tokio::spawn`
    // task in `handlers::submit_task` is aborted, this async fn's frame —
    // including the local `child` below — is dropped at whatever `.await`
    // point it's suspended at; `Child::drop` then calls `start_kill()`,
    // sending the OS process a kill signal instead of leaving it orphaned.
    cmd.stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .kill_on_drop(true);

    tracing::info!(task_id = %id, "spawning workflow subprocess");
    let mut child = cmd.spawn()?;

    // #4321: the child inherits this process's whole environment, so every
    // provider credential we hold is a credential IT holds — and its stderr is
    // now user-facing (narrative → chat bubble, `tasks.json`, the task API).
    // Resolve the known values ONCE here (each call opens the secure store) and
    // scrub them out of anything the child says.
    let secrets = std::sync::Arc::new(trusty_common::credentials::resolved_secret_values());

    // #149: Drain stderr in a background task, parsing progress events.
    let stderr_handle = child.stderr.take();
    let id_for_stderr = id.to_string();
    let state_for_stderr = state.clone();
    let secrets_for_stderr = std::sync::Arc::clone(&secrets);
    let stderr_join = tokio::spawn(async move {
        // #4321: the child's diagnostics are the ONLY explanation of a
        // non-zero exit, and until now they went to the parent's own stderr
        // and nowhere else — invisible when the parent is a GUI-launched
        // sidecar. Keep a bounded tail so `run_task` can put them in the
        // response the user actually reads.
        let mut tail: std::collections::VecDeque<String> = std::collections::VecDeque::new();
        if let Some(stderr) = stderr_handle {
            let mut lines = BufReader::new(stderr).lines();
            while let Ok(Some(line)) = lines.next_line().await {
                push_stderr_tail(&mut tail, &line, &secrets_for_stderr);
                // #192 Phase B: relay structured Event JSON from the child
                // subprocess to the parent's process-global event bus. SSE
                // subscribers see them in real time. We deliberately check
                // EVENT_LINE_PREFIX BEFORE OMPM_PROGRESS so the new typed
                // protocol takes precedence; the legacy progress line stays
                // as a fallback for older child binaries.
                if let Some(rest) = line.strip_prefix(EVENT_LINE_PREFIX) {
                    match serde_json::from_str::<Event>(rest.trim()) {
                        Ok(ev) => events::publish(ev),
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                line = %rest,
                                "failed to parse OMPM_EVENT line"
                            );
                            eprintln!("{line}");
                        }
                    }
                } else if let Some(rest) = line.strip_prefix("__OMPM_PROGRESS__ ") {
                    match serde_json::from_str::<PhaseProgress>(rest.trim()) {
                        Ok(ev) => {
                            // Fan out to BOTH the legacy in-memory store
                            // (still consumed by `GET /api/task/:id` polling
                            // clients) and the new event bus so SSE
                            // subscribers see phase transitions even when the
                            // child only emits the legacy line.
                            let phase = ev.name.clone();
                            let status = ev.status.clone();
                            state_for_stderr.append_progress(&id_for_stderr, ev).await;
                            if status == "running" {
                                events::publish(Event::PhaseStarted {
                                    session_id: id_for_stderr.clone(),
                                    phase,
                                });
                            } else {
                                events::publish(Event::PhaseDone {
                                    session_id: id_for_stderr.clone(),
                                    phase,
                                    status,
                                });
                            }
                        }
                        Err(e) => {
                            tracing::debug!(
                                error = %e,
                                line = %rest,
                                "failed to parse OMPM_PROGRESS event"
                            );
                            // Still forward the raw line to our stderr.
                            eprintln!("{line}");
                        }
                    }
                } else {
                    // Pass through non-progress lines so existing log output
                    // remains visible in the parent's stderr.
                    eprintln!("{line}");
                }
            }
        }
        Vec::from(tail)
    });

    let mut stdout_buf = Vec::new();
    if let Some(mut so) = child.stdout.take() {
        so.read_to_end(&mut stdout_buf).await?;
    }
    let status = child.wait().await?;
    // Drain stderr task before returning so we don't drop progress events.
    // #4321: it now also hands back the tail of the child's diagnostics.
    let stderr_tail = stderr_join.await.unwrap_or_default();

    if !status.success() {
        return Ok(PmResponse::error(
            id,
            format_subprocess_failure(status.code(), &stderr_tail, &secrets),
        ));
    }

    let stdout = String::from_utf8_lossy(&stdout_buf);
    if is_direct {
        // Direct mode returns raw content; wrap it in an agent_response envelope.
        let mut resp = PmResponse::running(id);
        resp.response_type = crate::api::types::PmResponseType::AgentResponse;
        resp.status = PmStatus::Success;
        resp.narrative = stdout.trim().to_string();
        resp.timestamp = crate::api::types::now_iso8601();
        return Ok(resp);
    }

    match serde_json::from_str::<PmResponse>(&stdout) {
        Ok(mut r) => {
            // Preserve the server-assigned id so polling works.
            r.id = id.to_string();
            Ok(r)
        }
        Err(e) => Ok(PmResponse::error(
            id,
            format!("failed to parse workflow JSON output: {e}"),
        )),
    }
}

/// How many trailing stderr lines `run_task` keeps to explain a failed child.
const STDERR_TAIL_LINES: usize = 20;

/// How many bytes of any single stderr line are kept.
const STDERR_TAIL_LINE_BYTES: usize = 500;

/// Record one child stderr line into the bounded failure-explanation tail
/// (#4321).
///
/// Why: a non-zero child exit used to reach the user as nothing but
/// `subprocess exited with status Some(1)` — the child's own message ("no
/// `.trusty-agents/agents/` found in /") went to the parent's stderr, which is
/// invisible when the parent is the GUI-launched API sidecar. The tail is
/// bounded because a chatty or looping child must not be able to grow the
/// stored `PmResponse` without limit.
/// What: skips the two structured protocol prefixes (those are events, not
/// diagnostics), strips the child's tracing colour codes via the crate's
/// existing [`crate::debugger::tui::strip_ansi`] — this text lands in a chat
/// bubble, where a raw `\x1b[2m` renders as garbage — removes any credential in
/// `secrets` via [`trusty_common::credentials::scrub_secrets`], truncates the
/// line on a char boundary to [`STDERR_TAIL_LINE_BYTES`], and evicts the oldest
/// entry past [`STDERR_TAIL_LINES`]. The scrub happens BEFORE the truncation
/// deliberately: truncating first could cut a credential in half and leave the
/// surviving fragment permanently in the tail, past any later scrub's reach.
/// Test: `stderr_tail_keeps_only_the_last_n_lines`,
/// `stderr_tail_skips_structured_protocol_lines`,
/// `stderr_tail_truncates_an_overlong_line`, `stderr_tail_strips_ansi_colour`,
/// `stderr_tail_redacts_a_known_credential`,
/// `stderr_tail_redacts_every_credential_the_process_holds`,
/// `stderr_tail_leaves_the_message_intact_for_an_empty_credential`.
fn push_stderr_tail(tail: &mut std::collections::VecDeque<String>, line: &str, secrets: &[String]) {
    if line.starts_with(EVENT_LINE_PREFIX) || line.starts_with("__OMPM_PROGRESS__ ") {
        return;
    }
    let line = crate::debugger::tui::strip_ansi(line);
    let line = trusty_common::credentials::scrub_secrets(&line, secrets);
    if line.trim().is_empty() {
        return;
    }
    let mut end = STDERR_TAIL_LINE_BYTES.min(line.len());
    while end < line.len() && !line.is_char_boundary(end) {
        end -= 1;
    }
    tail.push_back(line[..end].to_string());
    while tail.len() > STDERR_TAIL_LINES {
        tail.pop_front();
    }
}

/// Build the user-facing narrative for a child that exited non-zero (#4321).
///
/// Why: exit status alone is undiagnosable. Both GUI delivery paths render
/// this string verbatim (`ui/src-tauri/src/task_commands.rs`,
/// `ui/src/lib/transport.ts`), so whatever the child said has to be in it.
/// What: keeps `subprocess exited with status {code:?}` as the leading text —
/// the two existing delivery-path tests match on it as a prefix — and appends
/// the captured tail, or an explicit statement that the child said nothing.
/// The assembled narrative is passed through
/// [`trusty_common::credentials::scrub_secrets`] on the way out. `push_stderr_tail`
/// already scrubbed each line, so this second pass finds nothing today; it is
/// here because this function is the boundary where the narrative becomes
/// user-facing, and a future contributor adding another source of text to it
/// should not have to know that the redaction lived somewhere upstream. Note
/// what it does NOT cover: the raw child lines this module also `eprintln!`s to
/// the parent's own stderr are unscrubbed, unchanged from the pre-#4321
/// `Stdio::inherit()` behaviour — the parent's stderr already holds the parent's
/// environment and is not a user-facing sink.
/// Test: `subprocess_failure_narrative_carries_child_stderr`,
/// `subprocess_failure_narrative_states_when_child_was_silent`,
/// `subprocess_failure_narrative_keeps_the_legacy_prefix`,
/// `subprocess_failure_narrative_redacts_a_credential_reaching_it_unscrubbed`.
fn format_subprocess_failure(
    code: Option<i32>,
    stderr_tail: &[String],
    secrets: &[String],
) -> String {
    let head = format!("subprocess exited with status {code:?}");
    if stderr_tail.is_empty() {
        return format!("{head} (the child wrote nothing to stderr)");
    }
    let body = format!(
        "{head}\n\nchild stderr (last {} line(s)):\n{}",
        stderr_tail.len(),
        stderr_tail.join("\n")
    );
    trusty_common::credentials::scrub_secrets(&body, secrets)
}

/// Resolve the current executable path (used for self-respawn).
///
/// Why: `run_task` re-invokes the orchestrator binary to inherit full
/// env/init; it must locate its own path at runtime.
/// What: Wraps `std::env::current_exe`, mapping its error into `anyhow`.
/// Test: Side-effect-only; exercised whenever `run_task` spawns a child.
fn current_exe() -> Result<std::path::PathBuf> {
    std::env::current_exe().map_err(Into::into)
}

/// #371: After a task completes, tick the recap tracker; if the configured
/// interval has been hit, assemble a recap from the last N task histories,
/// persist it, and emit a `RecapGenerated` event.
///
/// Why: Tasks complete on two distinct code paths (Conversational/Research
/// in-process branch, and the prescriptive subprocess branch). Centralising
/// recap dispatch keeps both call sites identical and ensures the GUI's
/// RecapPanel works regardless of which intent class produced the run.
/// What: Acquires the recap tracker lock, calls `tick`. On trigger, snapshots
/// the most recent N tasks from `AppState`, converts each `PhaseProgress`
/// into a `(name, status)` tuple, calls `assemble_recap`, saves to disk and
/// publishes `Event::RecapGenerated`. All disk + LLM-free path — safe to call
/// inside the tokio task that finalised the response.
/// Test: covered by integration; recap module unit tests cover the assembly
/// and persistence primitives.
pub(super) async fn maybe_emit_recap(state: &AppState, session_id: &str) {
    let triggered = {
        let mut tracker = state.recap_tracker.lock().await;
        tracker.tick(session_id)
    };
    if !triggered {
        return;
    }

    // Snapshot the last N tasks from the response store. We pull from the
    // global `AppState.list()` since per-session task threading isn't tracked
    // here yet — the recap interval is small enough (default 5) that the
    // newest-first window approximates "last N completed in this session".
    let interval = state.recap_tracker.lock().await.config.interval.max(1);
    let recent = state.list().await;
    let tasks: Vec<RecapTask> = recent
        .into_iter()
        .take(interval)
        .map(|r| {
            let phases: Vec<RecapPhase> = r
                .phases_completed
                .iter()
                .map(|p| (p.name.clone(), p.status.clone()))
                .collect();
            // Use id as task prompt placeholder — TaskRequest text isn't
            // currently retained in PmResponse.
            (r.id.clone(), r.narrative.clone(), phases)
        })
        .collect();

    if tasks.is_empty() {
        return;
    }

    let recap = recap::assemble_recap(session_id, &tasks);
    let dir = state_dir();
    if let Err(e) = recap::save_recap(&dir, &recap) {
        tracing::warn!(?e, session_id, "failed to persist recap");
    }
    events::publish(Event::RecapGenerated {
        session_id: session_id.to_string(),
        summary: recap.summary.clone(),
        table_rows: recap
            .rows
            .iter()
            .map(|row| (row.step.clone(), row.result.clone()))
            .collect(),
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::VecDeque;

    /// Capture `lines` with no credentials configured — the shape every
    /// pre-#4321 test asserts.
    fn tail_of(lines: &[&str]) -> Vec<String> {
        tail_of_with_secrets(lines, &[])
    }

    /// Capture `lines` as a process holding `secrets` would.
    fn tail_of_with_secrets(lines: &[&str], secrets: &[String]) -> Vec<String> {
        let mut t: VecDeque<String> = VecDeque::new();
        for l in lines {
            push_stderr_tail(&mut t, l, secrets);
        }
        Vec::from(t)
    }

    /// The #4321 reproducer, verbatim: `tagent --workflow prescriptive --json
    /// --task <text>` spawned with the GUI sidecar's cwd (`/`). Before the fix
    /// the user saw only the first line of this narrative and had no way to
    /// learn what the child objected to.
    #[test]
    fn subprocess_failure_narrative_carries_child_stderr() {
        let tail = tail_of(&[
            "  INFO trusty_agents::runtime::startup: trusty-agents v0.38.6 build #1736",
            "Error: no `.trusty-agents/agents/` found in /.",
        ]);
        let narrative = format_subprocess_failure(Some(1), &tail, &[]);
        assert!(
            narrative.contains("no `.trusty-agents/agents/` found in /."),
            "child's real error must reach the user: {narrative}"
        );
    }

    #[test]
    fn subprocess_failure_narrative_states_when_child_was_silent() {
        let narrative = format_subprocess_failure(Some(1), &[], &[]);
        assert!(narrative.contains("wrote nothing to stderr"), "{narrative}");
    }

    /// Both GUI delivery paths (`task_commands.rs`, `transport.ts`) and their
    /// tests match this text as a prefix, so it must stay leading.
    #[test]
    fn subprocess_failure_narrative_keeps_the_legacy_prefix() {
        assert!(
            format_subprocess_failure(Some(1), &["boom".to_string()], &[])
                .starts_with("subprocess exited with status Some(1)")
        );
    }

    #[test]
    fn stderr_tail_keeps_only_the_last_n_lines() {
        let lines: Vec<String> = (0..STDERR_TAIL_LINES + 5)
            .map(|i| format!("l{i}"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let tail = tail_of(&refs);
        assert_eq!(tail.len(), STDERR_TAIL_LINES);
        assert_eq!(tail.first().unwrap(), "l5");
        assert_eq!(tail.last().unwrap(), &format!("l{}", STDERR_TAIL_LINES + 4));
    }

    #[test]
    fn stderr_tail_skips_structured_protocol_lines() {
        let event_line = format!("{EVENT_LINE_PREFIX}{{\"kind\":\"x\"}}");
        let tail = tail_of(&[
            &event_line,
            "__OMPM_PROGRESS__ {\"name\":\"research\"}",
            "",
            "Error: real failure",
        ]);
        assert_eq!(tail, vec!["Error: real failure".to_string()]);
    }

    /// The child's tracing output is colourised; the narrative is rendered as
    /// chat text, so the escapes have to go.
    #[test]
    fn stderr_tail_strips_ansi_colour() {
        let tail = tail_of(&["\x1b[2m2026-08-08T17:49:13Z\x1b[0m \x1b[31mError: boom\x1b[0m"]);
        assert_eq!(tail, vec!["2026-08-08T17:49:13Z Error: boom".to_string()]);
    }

    #[test]
    fn stderr_tail_truncates_an_overlong_line() {
        // Multi-byte chars: the truncation must land on a char boundary
        // rather than panicking on a slice into the middle of one.
        let long = "é".repeat(STDERR_TAIL_LINE_BYTES);
        let tail = tail_of(&[&long]);
        assert!(tail[0].len() <= STDERR_TAIL_LINE_BYTES);
        assert!(!tail[0].is_empty());
    }

    /// The security finding this fix closes: the child inherits every provider
    /// credential the parent holds, so a child that echoes one into its own
    /// error message would have put it straight into the narrative the GUI
    /// renders and `tasks.json` stores.
    #[test]
    fn stderr_tail_redacts_a_known_credential() {
        let key = "sk-or-v1-childechoedthiskey0001"; // pragma: allowlist secret
        let tail = tail_of_with_secrets(
            &[&format!("Error: OpenRouter rejected key {key} (401)")],
            &[key.to_string()],
        );
        let narrative = format_subprocess_failure(Some(1), &tail, &[key.to_string()]);
        assert!(!narrative.contains(key), "credential leaked: {narrative}");
        assert!(narrative.contains("[REDACTED]"), "{narrative}");
        assert!(
            narrative.contains("OpenRouter rejected key"),
            "the diagnostic itself must survive: {narrative}"
        );
    }

    /// The process can hold several provider credentials at once (#4321 names
    /// OPENROUTER / ANTHROPIC / CLAUDE_CODE_OAUTH / ATLASCLOUD), and one tail
    /// can quote more than one. Redacting only the first is still a leak.
    #[test]
    fn stderr_tail_redacts_every_credential_the_process_holds() {
        let secrets: Vec<String> = vec![
            "sk-or-v1-openroutervalue00001".into(), // pragma: allowlist secret
            "sk-ant-api03-anthropicvalue01".into(), // pragma: allowlist secret
            "sk-ant-oat01-oauthtokenvalue1".into(), // pragma: allowlist secret
        ];
        let lines: Vec<String> = secrets
            .iter()
            .map(|s| format!("Error: auth failed with {s}"))
            .collect();
        let refs: Vec<&str> = lines.iter().map(String::as_str).collect();
        let tail = tail_of_with_secrets(&refs, &secrets);
        let narrative = format_subprocess_failure(Some(1), &tail, &secrets);
        for s in &secrets {
            assert!(!narrative.contains(s.as_str()), "leaked {s}: {narrative}");
        }
        assert_eq!(narrative.matches("[REDACTED]").count(), 3, "{narrative}");
    }

    /// The footgun: an unset or placeholder credential resolving to an empty
    /// (or one-character) value must not turn the diagnostic into confetti.
    /// Replacing `""` would blank the message the user needs.
    #[test]
    fn stderr_tail_leaves_the_message_intact_for_an_empty_credential() {
        let msg = "Error: no `.trusty-agents/agents/` found in /.";
        let junk: Vec<String> = vec!["".into(), "x".into(), "in".into(), "found".into()];
        let tail = tail_of_with_secrets(&[msg], &junk);
        assert_eq!(tail, vec![msg.to_string()]);
        let narrative = format_subprocess_failure(Some(1), &tail, &junk);
        assert!(narrative.contains(msg), "message mangled: {narrative}");
        assert!(!narrative.contains("[REDACTED]"), "{narrative}");
    }

    /// The boundary guard: `format_subprocess_failure` is where the narrative
    /// becomes user-facing, so it must scrub whatever it is handed — even a
    /// tail that (unlike today's) reached it without having been scrubbed at
    /// capture time.
    #[test]
    fn subprocess_failure_narrative_redacts_a_credential_reaching_it_unscrubbed() {
        let key = "sk-ant-api03-unscrubbedtail001"; // pragma: allowlist secret
        let unscrubbed = vec![format!("Error: bad credential {key}")];
        let narrative = format_subprocess_failure(Some(1), &unscrubbed, &[key.to_string()]);
        assert!(!narrative.contains(key), "credential leaked: {narrative}");
        assert!(narrative.contains("[REDACTED]"), "{narrative}");
    }
}
