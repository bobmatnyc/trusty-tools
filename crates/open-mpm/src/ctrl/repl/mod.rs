//! Interactive CTRL REPL — slash-command dispatch, stdin loop, profile setup.
//!
//! Why: The interactive entry point (`run_ctrl`) glues together every piece
//! of ctrl's setup (profile load, self-project detection, docs index, memory
//! seed, message bus, controller socket) with the stdin command loop and
//! chat turn dispatch. Keeping it isolated lets the rest of `ctrl/*` stay
//! focused on dispatch primitives.
//! What: `print_help`, `handle_command`, `run_ctrl`, `run_ctrl_headless`,
//! `run_ctrl_inner`; the first-run profile helpers live in the `profile`
//! submodule.
//! Test: `cmd_*` tests cover slash-command dispatch; the larger setup paths
//! are smoke-tested via tmux REPL harness.

mod profile;

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Instant;

use anyhow::{Context, Result};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::sync::oneshot;

use crate::bus::MessageBus;
use crate::registry::ProjectRegistry;
use crate::session_record;

use super::ctrl_turn::ctrl_chat_turn;
use super::socket::{BindOutcome, CtrlSocket, ctrl_socket_path, cwd_project_id};
use super::socket_listener::spawn_socket_listener;
use super::state::{Ctrl, PmMsg};
use super::util::{append_pm_message, detect_self_project};

use profile::load_or_create_user_profile;

// INTENT: Print the CTRL command reference.
pub(crate) fn print_help() {
    println!(
        "\
CTRL commands:
  /connect <PATH>          Start (or switch to) a PM session for PATH
  /disconnect              Return to CTRL prompt (PM keeps running)
  /status                  List PM sessions, registered projects, live buses
  /send <PROJECT> <MSG>    Send a message to another project via the bus
  /sessions [QUERY]        Search past workflow runs (cross-project)
  /help                    Show this message
  /quit | /exit            Shutdown all sessions and exit"
    );
}

// INTENT: Parse and dispatch a slash command, returning false on quit.
pub(crate) async fn handle_command(ctrl: &mut Ctrl, line: &str) -> Result<bool> {
    let mut parts = line.splitn(2, ' ');
    let cmd = parts.next().unwrap_or("");
    let arg = parts.next().map(str::trim);

    match cmd {
        "/connect" => {
            let path = arg.context("/connect requires a PATH argument")?;
            match ctrl.connect(path).await {
                Ok(msg) => println!("{msg}"),
                Err(e) => eprintln!("connect error: {e:#}"),
            }
        }
        "/disconnect" => {
            println!("{}", ctrl.disconnect());
        }
        "/status" => {
            println!("{}", ctrl.status());

            match ProjectRegistry::new() {
                Ok(reg) => match reg.status_summary().await {
                    Ok(summary) => println!("\n{summary}"),
                    Err(e) => eprintln!("registry error: {e:#}"),
                },
                Err(e) => eprintln!("registry unavailable: {e:#}"),
            }

            match MessageBus::list_running().await {
                Ok(running) if !running.is_empty() => {
                    println!("\n## Live Bus Connections\n");
                    for id in &running {
                        println!("  - {id}");
                    }
                }
                Ok(_) => {}
                Err(e) => eprintln!("bus list_running error: {e:#}"),
            }
        }
        "/send" => {
            let rest = arg.context("/send requires <PROJECT> <MESSAGE>")?;
            let mut parts2 = rest.splitn(2, ' ');
            let target = parts2
                .next()
                .context("/send requires a target project name")?;
            let msg_text = parts2.next().unwrap_or("").trim();
            if let Some(bus) = &ctrl.bus {
                let payload = serde_json::json!({ "type": "task", "text": msg_text });
                match bus.send_to(target, payload).await {
                    Ok(()) => println!("Sent to {target}"),
                    Err(e) => eprintln!("send error: {e:#}"),
                }
            } else {
                eprintln!("Bus not available — inter-project messaging requires a running bus");
            }
        }
        "/sessions" => {
            let query = arg.unwrap_or("");
            match session_record::search(query).await {
                Ok(hits) if hits.is_empty() => println!("(no matching sessions)"),
                Ok(hits) => {
                    for h in hits.iter().take(20) {
                        let score = h.score.as_deref().unwrap_or("-");
                        println!(
                            "{}  {}  {}  cost=${:.2}  mins={}  score={}  task={}",
                            h.timestamp,
                            h.build_id,
                            h.status,
                            h.cost_usd,
                            h.duration_mins,
                            score,
                            h.task
                        );
                    }
                }
                Err(e) => eprintln!("sessions error: {e:#}"),
            }
        }
        "/help" => {
            print_help();
        }
        "/quit" | "/exit" | "/q" => {
            println!("Shutting down...");
            return Ok(false);
        }
        other => {
            eprintln!("Unknown command: {other}  (type /help for commands)");
        }
    }
    Ok(true)
}

// INTENT: Public entry point for the CTRL interactive REPL.
pub async fn run_ctrl() -> Result<()> {
    run_ctrl_inner(true, None).await
}

/// Headless variant of [`run_ctrl`]: performs all controller initialization
/// (socket binding, docs indexing, memory seeding, message bus, profile load)
/// but skips the interactive stdin loop and the CTRL banner.
///
/// Why: When the rich reedline REPL drives stdin, having `run_ctrl` also
/// read stdin causes both readers to compete for keystrokes. The REPL spawns
/// this variant in a background task so the controller's services stay
/// available while the REPL owns stdin.
/// What: Runs the same setup as `run_ctrl`, then parks on
/// `std::future::pending::<()>().await` until the spawning task is aborted.
/// Test: Spawn `run_ctrl_headless` in a tokio task; verify the controller
/// socket binds and the task remains alive until aborted by the caller.
pub async fn run_ctrl_headless(ready_tx: Option<oneshot::Sender<()>>) -> Result<()> {
    run_ctrl_inner(false, ready_tx).await
}

/// Shared implementation backing both [`run_ctrl`] and [`run_ctrl_headless`].
///
/// Why: Keeping a single setup path guarantees both modes bind the same
/// socket, build the same docs index, and seed the same memory store.
/// What: Runs profile load, self-project detection, docs indexing, memory
/// seeding, message bus startup, and controller socket bind.
/// Test: Both `run_ctrl()` and `run_ctrl_headless()` must succeed.
async fn run_ctrl_inner(with_stdin: bool, ready_tx: Option<oneshot::Sender<()>>) -> Result<()> {
    if with_stdin {
        eprintln!(
            "{} CTRL — machine-level coordination\ntype /help for commands, /connect <PATH> to start a PM session\n",
            crate::build_info::version_string()
        );
    }

    let user_profile = load_or_create_user_profile().await?;

    // SAFETY: single-threaded startup context; set before any subprocess spawn.
    if std::env::var("OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY").is_err() {
        unsafe {
            std::env::set_var("OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY", "1");
        }
        tracing::debug!("CTRL: defaulting OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY=1");
    }

    let mut ctrl = Ctrl::new();
    ctrl.user_profile = user_profile;

    if let Some(self_path) = detect_self_project() {
        tracing::info!(path = %self_path.display(), "self-project detected");
        if let Ok(reg) = ProjectRegistry::new()
            && let Err(e) = reg.register_self_project(&self_path).await
        {
            tracing::warn!(error = %e, "register_self_project failed");
        }
        ctrl.self_project = Some(self_path);
    }

    {
        let docs_root = ctrl
            .self_project
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")))
            .join("docs");
        let slot = ctrl.docs_index.clone();
        tokio::spawn(async move {
            let docs_root_clone = docs_root.clone();
            let idx = tokio::task::spawn_blocking(move || {
                crate::docs_index::DocsIndex::build(&docs_root_clone)
            })
            .await
            .ok();
            if let Some(idx) = idx {
                let n = idx.len();
                if let Ok(mut g) = slot.lock() {
                    *g = Some(Arc::new(idx));
                }
                tracing::info!(
                    "[open-mpm] Docs index: {n} documents indexed from {}",
                    docs_root.display()
                );
            }
        });
    }

    {
        let project_root = ctrl
            .self_project
            .clone()
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));
        tokio::spawn(async move {
            let omd = project_root.join(".open-mpm").join("state");
            if let Err(e) = tokio::fs::create_dir_all(&omd).await {
                tracing::warn!(error = %e, "ctrl doc seed: create state dir failed");
                return;
            }
            let initializer = crate::init::ProjectInitializer::new(&project_root, &omd);
            if let Err(e) = initializer.initialize_if_needed().await {
                tracing::warn!(error = %e, "ctrl: project init failed (continuing)");
            }

            let session_dir = project_root
                .join(".open-mpm")
                .join("sessions")
                .join("default");
            if let Err(e) = tokio::fs::create_dir_all(&session_dir).await {
                tracing::warn!(error = %e, "ctrl doc seed: create session dir failed");
                return;
            }
            let store = match crate::memory::open_memory_store(&session_dir) {
                Ok(s) => s,
                Err(e) => {
                    tracing::warn!(error = %e, "ctrl doc seed: store open failed");
                    return;
                }
            };
            let embedder = match crate::memory::FastEmbedder::new() {
                Ok(e) => e,
                Err(e) => {
                    tracing::warn!(error = %e, "ctrl doc seed: embedder unavailable");
                    return;
                }
            };
            let _ = initializer.seed_all(store.as_ref(), &embedder).await;
        });
    }

    match MessageBus::start("ctrl").await {
        Ok(bus) => {
            let mut rx = bus.subscribe();
            let connected_pms = ctrl.connected_pms.clone();
            tokio::spawn(async move {
                loop {
                    match rx.recv().await {
                        Ok(envelope) => {
                            tracing::info!(
                                "[BUS] from {}: {}",
                                envelope.source_project,
                                serde_json::to_string(&envelope.message)
                                    .unwrap_or_else(|_| "(unserializable)".into())
                            );
                            if let Err(e) = append_pm_message(&envelope) {
                                tracing::warn!(error = %e, "append_pm_message failed");
                            }
                            if let Some(target) = envelope.target_project.as_deref() {
                                let sender_opt = {
                                    let m = connected_pms.lock().await;
                                    m.get(target).cloned()
                                };
                                if let Some(pm_tx) = sender_opt {
                                    let text = envelope
                                        .message
                                        .get("text")
                                        .and_then(|v| v.as_str())
                                        .map(str::to_string)
                                        .unwrap_or_else(|| envelope.message.to_string());
                                    let (reply_tx, reply_rx) = oneshot::channel();
                                    if let Err(e) = pm_tx
                                        .send(PmMsg::Task {
                                            text,
                                            reply: reply_tx,
                                        })
                                        .await
                                    {
                                        tracing::warn!(error = %e, target = %target, "bus relay: PM channel closed");
                                    } else {
                                        let target_owned = target.to_string();
                                        tokio::spawn(async move {
                                            match reply_rx.await {
                                                Ok(Ok(out)) => {
                                                    tracing::info!(
                                                        "[BUS->PM[{target_owned}]] {out}"
                                                    );
                                                }
                                                Ok(Err(e)) => {
                                                    tracing::warn!(error = %e, "bus->PM task error");
                                                }
                                                Err(e) => {
                                                    tracing::warn!(error = %e, "bus->PM reply dropped");
                                                }
                                            }
                                        });
                                    }
                                }
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(n)) => {
                            tracing::warn!(n = n, "CTRL bus relay: lagged, {n} messages dropped");
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
                    }
                }
            });
            ctrl.bus = Some(bus);
        }
        Err(e) => {
            tracing::warn!(error = %e, "CTRL message bus failed to start (inter-project relay unavailable)");
        }
    }

    // Singleton enforcement (#192): probe-then-bind atomically so a second
    // controller never clobbers a live one's socket. `bind_singleton` returns
    // `AlreadyRunning` when a controller already owns the socket — in that case
    // we do NOT start our own accept loop (which would have stolen the socket
    // file and broken the first controller's CLI forwarding). We still proceed
    // with the rest of setup so this process can serve as a local REPL, but it
    // intentionally relinquishes the singleton command port to the incumbent.
    let project_id = cwd_project_id();
    let sock_path = ctrl_socket_path(&project_id);
    match CtrlSocket::bind_singleton_default(&sock_path).await {
        Ok(BindOutcome::Bound(listener)) => {
            tracing::info!(
                "[open-mpm] controller socket listening at {}",
                sock_path.display()
            );
            tokio::spawn(spawn_socket_listener(listener));
        }
        Ok(BindOutcome::AlreadyRunning(stream)) => {
            // Drop the probe stream immediately — we only used it to confirm a
            // live incumbent. This process becomes a local-only REPL and leaves
            // the command port to the existing controller.
            drop(stream);
            tracing::warn!(
                path = %sock_path.display(),
                "ctrl: another controller already owns the socket — running as local REPL only (CLI forwarding routes to the incumbent)"
            );
        }
        Err(e) => {
            tracing::warn!(
                error = %e,
                path = %sock_path.display(),
                "ctrl: failed to bind controller socket (CLI forwarding disabled)"
            );
        }
    }

    if let Some(tx) = ready_tx {
        let _ = tx.send(());
    }

    if !with_stdin {
        std::future::pending::<()>().await;
        return Ok(());
    }

    let mut stdin = BufReader::new(tokio::io::stdin());
    let mut stdout = tokio::io::stdout();

    loop {
        stdout
            .write_all(ctrl.prompt().as_bytes())
            .await
            .context("failed to write prompt")?;
        stdout.flush().await.context("failed to flush prompt")?;

        let mut line = String::new();
        let n = stdin
            .read_line(&mut line)
            .await
            .context("failed to read stdin")?;

        if n == 0 {
            println!("Bye.");
            break;
        }

        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }

        if trimmed.starts_with('/') {
            match handle_command(&mut ctrl, trimmed).await {
                Ok(false) => break,
                Ok(true) => {}
                Err(e) => eprintln!("command error: {e:#}"),
            }
        } else if ctrl.active.is_some() {
            let start = Instant::now();
            match ctrl.dispatch_task(trimmed.to_string()).await {
                Ok(output) => {
                    let elapsed = start.elapsed();
                    println!("{output}");
                    eprintln!(
                        "[TIMING] PM task responded in {:.2}s",
                        elapsed.as_secs_f64()
                    );
                }
                Err(e) => {
                    let elapsed = start.elapsed();
                    eprintln!("task error: {e:#}");
                    eprintln!(
                        "[TIMING] PM task failed after {:.2}s",
                        elapsed.as_secs_f64()
                    );
                }
            }
        } else {
            let start = Instant::now();
            match ctrl_chat_turn(&mut ctrl, trimmed).await {
                Ok(output) => {
                    let elapsed = start.elapsed();
                    if !output.trim().is_empty() {
                        println!("{output}");
                    }
                    eprintln!(
                        "[TIMING] CTRL turn responded in {:.2}s",
                        elapsed.as_secs_f64()
                    );
                }
                Err(e) => {
                    let elapsed = start.elapsed();
                    eprintln!("ctrl error: {e:#}");
                    eprintln!(
                        "[TIMING] CTRL turn failed after {:.2}s",
                        elapsed.as_secs_f64()
                    );
                }
            }
        }
    }

    ctrl.shutdown_all().await;
    Ok(())
}
