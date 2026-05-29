//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

pub(crate) async fn process_event<H: ReplHandler + 'static>(
    ev: ReplEvent,
    app: &Arc<Mutex<ReplApp>>,
    current_task: &Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    tx: &UnboundedSender<ReplEvent>,
    handler: &Arc<H>,
) {
    match ev {
        ReplEvent::Key(k) => {
            // Run the key through the editor, then drain any picker
            // selection that Enter just produced (so the synthetic Submit
            // is dispatched in this same event tick) and any pending
            // cancel signal (Up-arrow while busy → abort the in-flight
            // handler task).
            let (submit, picker_choice, cancel, pending_submit) = {
                let mut a = app.lock().await;
                let s = handle_key(&mut a, k);
                let pc = a.pending_picker_selection.take();
                let cancel = std::mem::replace(&mut a.pending_cancel, false);
                let ps = a.pending_submit.take();
                (s, pc, cancel, ps)
            };
            if cancel {
                // Up-arrow while LLM was busy: abort the in-flight task,
                // surface a status line so the user knows the cancel
                // landed, and clear the busy flag so the input bar / chat
                // hint stops showing thinking. The Up-arrow handler in
                // `handle_key` already restored `last_prompt` into the
                // input buffer.
                let mut slot = current_task.lock().await;
                if let Some(h) = slot.take() {
                    h.abort();
                }
                drop(slot);
                let mut a = app.lock().await;
                a.thinking = false;
                a.thinking_lines.clear();
                a.busy_since = None;
                a.streaming_preview.clear();
                a.push_status("cancelled");
            }
            if let Some((kind, selected)) = picker_choice {
                let cmd = match kind {
                    PickerKind::Model => format!("/model {}", selected),
                    PickerKind::Provider => format!("/provider {}", selected),
                };
                let _ = tx.send(ReplEvent::Submit(cmd));
            }
            // Inline-choice dispatch (e.g. `/switch <persona>` chosen from
            // the flat list shown after `/switch` with no arg). Synthesizes
            // a Submit so the slash handler runs in the existing pipeline.
            if let Some(cmd) = pending_submit {
                let _ = tx.send(ReplEvent::Submit(cmd));
            }
            if let Some(line) = submit {
                // Echo user line + remember + clear thinking immediately.
                // Also stash the line into `last_prompt` so Up-arrow can
                // recall it.
                {
                    let mut a = app.lock().await;
                    a.push_user(&line);
                    a.remember_input(&line);
                    a.last_prompt = line.clone();
                    a.thinking = true;
                    a.thinking_lines.clear();
                    // Activity panel: mark busy timestamp + clear any leftover
                    // preview from the prior turn.
                    a.busy_since = Some(std::time::Instant::now());
                    a.streaming_preview.clear();
                }
                // Dispatch handler in a background task so the render
                // loop keeps responding to scroll/resize while the LLM
                // call is in flight. Store the JoinHandle in
                // `current_task` so Up-arrow can abort it.
                let h = handler.clone();
                let dtx = tx.clone();
                let app_for_quit = app.clone();
                let _task_slot = current_task.clone();
                let handle = tokio::spawn(async move {
                    let res = h.handle_input(line, dtx.clone()).await;
                    match res {
                        Ok(true) => {}
                        Ok(false) => {
                            let mut a = app_for_quit.lock().await;
                            a.quit = true;
                        }
                        Err(e) => {
                            let _ = dtx.send(ReplEvent::LlmResponse {
                                text: format!("error: {e:#}"),
                                is_error: true,
                            });
                        }
                    }
                    let _ = dtx.send(ReplEvent::LlmThinking(false));
                });
                let mut slot = current_task.lock().await;
                // Drop any orphaned previous handle (shouldn't happen in
                // practice — busy gating prevents concurrent submits — but
                // be defensive).
                if let Some(prev) = slot.take() {
                    prev.abort();
                }
                *slot = Some(handle);
            }
        }
        ReplEvent::Resize(_, _) => {
            // Repaint will pick up new dims naturally.
        }
        ReplEvent::LlmResponse { text, is_error } => {
            {
                let mut a = app.lock().await;
                a.push_assistant(text, is_error);
                a.thinking = false;
                a.thinking_lines.clear();
                a.busy_since = None;
                a.streaming_preview.clear();
            }
            // Task done — drop the JoinHandle so a stale slot can't shadow
            // a future cancel target.
            let mut slot = current_task.lock().await;
            *slot = None;
        }
        ReplEvent::LlmThinking(b) => {
            {
                let mut a = app.lock().await;
                a.thinking = b;
                if !b {
                    a.thinking_lines.clear();
                    a.busy_since = None;
                    a.streaming_preview.clear();
                }
            }
            if !b {
                let mut slot = current_task.lock().await;
                *slot = None;
            }
        }
        ReplEvent::ThinkingStep(s) => {
            let mut a = app.lock().await;
            // #298: Real-time token feel during streaming. We don't yet have
            // per-token usage events on the bus (only LlmRequested at start
            // and LlmResponded at end), so each ThinkingStep nudges
            // `tokens_out` by a small estimate. When the real completion
            // count lands via TokenUpdate it overrides cleanly because the
            // event bus delivers it via accumulation either way. If a step
            // text contains an explicit `↓ N tokens` (or `↓N tokens`)
            // pattern, we use that exact value instead of the estimate.
            if let Some(parsed) = parse_token_count_from_step(&s) {
                // Replace, not increment — explicit counts are absolute.
                if parsed > a.tokens_out {
                    a.tokens_out = parsed;
                }
            } else {
                a.tokens_out = a.tokens_out.saturating_add(8);
            }
            // Dedup consecutive identical lines so a chatty event bus
            // doesn't flood the chat area with repeats.
            if a.thinking_lines.last().map(|x| x.as_str()) != Some(s.as_str()) {
                // Mirror the latest step into the preview area as a
                // best-effort "in-progress response" surface — until proper
                // token streaming is wired through, this is the closest
                // signal we have.
                a.streaming_preview = s.clone();
                a.thinking_lines.push(s);
            }
        }
        ReplEvent::StatusMessage(s) => {
            let mut a = app.lock().await;
            a.push_status(s);
        }
        ReplEvent::LabelChanged(s) => {
            let mut a = app.lock().await;
            a.project_name = s;
        }
        ReplEvent::AgentScopeChanged(scope) => {
            let mut a = app.lock().await;
            a.agent_scope = scope;
        }
        ReplEvent::Submit(line) => {
            // Synthetic submission (currently used by the picker overlay
            // when the user presses Enter on a /model or /provider choice).
            // Mirrors the Key(Enter) -> handler dispatch path so the slash
            // command flows through `try_handle_slash` exactly as a typed
            // command would.
            let h = handler.clone();
            let dtx = tx.clone();
            let app_for_quit = app.clone();
            let task_slot = current_task.clone();
            let handle = tokio::spawn(async move {
                let res = h.handle_input(line, dtx.clone()).await;
                match res {
                    Ok(true) => {}
                    Ok(false) => {
                        let mut a = app_for_quit.lock().await;
                        a.quit = true;
                    }
                    Err(e) => {
                        let _ = dtx.send(ReplEvent::LlmResponse {
                            text: format!("error: {e:#}"),
                            is_error: true,
                        });
                    }
                }
                let _ = dtx.send(ReplEvent::LlmThinking(false));
            });
            let mut slot = task_slot.lock().await;
            if let Some(prev) = slot.take() {
                prev.abort();
            }
            *slot = Some(handle);
        }
        ReplEvent::OpenPicker { items, title, kind } => {
            let mut a = app.lock().await;
            a.picker = Some(PickerState {
                items,
                selected: 0,
                title,
                kind,
            });
        }
        ReplEvent::OllamaModelsLoaded(models) => {
            let mut a = app.lock().await;
            a.ollama_models = models;
        }
        ReplEvent::SetChoices { items, context } => {
            let mut a = app.lock().await;
            a.choices = items;
            a.choice_cursor = 0;
            a.choices_context = context;
        }
        ReplEvent::TmSessionCount(n) => {
            let mut a = app.lock().await;
            a.tm_session_count = n;
        }
        ReplEvent::ClaudeMpmSessionCount(n) => {
            let mut a = app.lock().await;
            a.claude_mpm_session_count = n;
        }
        ReplEvent::Scroll(delta) => {
            let mut a = app.lock().await;
            a.scroll(delta);
        }
        ReplEvent::TokenUpdate { prompt, completion } => {
            let mut a = app.lock().await;
            a.tokens_in = a.tokens_in.saturating_add(prompt);
            a.tokens_out = a.tokens_out.saturating_add(completion);
            persist_daily_usage_if_due(&mut a);
        }
        ReplEvent::TokenReset => {
            let mut a = app.lock().await;
            a.tokens_in = 0;
            a.tokens_out = 0;
        }
        ReplEvent::StatuslineUpdate { model, provider } => {
            let mut a = app.lock().await;
            // Regenerate `status_line` so the rich statusline picks up the
            // new model/provider (the rich renderer reads `status_line`, not
            // `model_name`/`provider_name`). Format mirrors the startup
            // string built in `repl/mod.rs::OpenMpmRepl::run` (#296):
            //   "✓ LLM: provider:model · All systems go."
            a.status_line = Some(format!(
                "✓ LLM: {}:{} · All systems go.",
                provider,
                strip_vendor_prefix(&model)
            ));
            a.model_name = model;
            a.provider_name = provider;
        }
    }
}
