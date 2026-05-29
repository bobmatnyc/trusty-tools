//! Part of the `tui` module (split from the original monolithic `tui.rs`
//! to satisfy the 500-line file cap — see #357). Cross-submodule items and
//! external imports resolve through the flat re-exports in `mod.rs`.

use super::*;

/// Handler trait for slash commands and chat dispatch.
///
/// Why: Keeps the ratatui loop ignorant of REPL business logic (slash command
/// table, persona switching, ctrl socket forwarding). The outer `OpenMpmRepl`
/// owns those concerns and implements this trait — the TUI just routes
/// submissions and surfaces results back through the event channel.
#[async_trait::async_trait]
pub trait ReplHandler: Send + Sync {
    /// Process a submitted line. Returns `Ok(true)` to keep looping,
    /// `Ok(false)` to quit. Pushes assistant/status lines through `tx`.
    async fn handle_input(&self, line: String, tx: UnboundedSender<ReplEvent>) -> Result<bool>;
}

/// Run the ratatui-based REPL to completion.
///
/// Why: Stack-safe RAII boundary — even a panic inside the event loop restores
/// the terminal via `restore_terminal`.
/// What: Enters alt-screen + raw mode, spawns the key reader task, spawns the
/// handler dispatcher, runs the render loop until `app.quit` is true.
/// Test: Integration via `scripts/tmux-repl-test.sh`.
pub struct ReplStartup {
    pub project_name: String,
    pub user_label: String,
    pub git_commits: Vec<String>,
    pub initial_status: Option<String>,
    pub initial_history: Vec<String>,
    pub initial_scope: AgentScope,
    pub model_name: String,
    pub provider_name: String,
    pub working_dir: String,
    pub git_branch: Option<String>,
    pub git_dirty: bool,
    pub statusline_config: StatuslineConfig,
    /// Project dir whose `.open-mpm/state/usage.json` we'll read/write for
    /// daily cost accumulation.
    pub project_dir: PathBuf,
    /// System messages to push into the chat scrollback before the first
    /// frame renders (#319). Used by the TM startup-reconcile to surface
    /// "discovered N session(s)" without a separate event flush.
    pub initial_chat_messages: Vec<String>,
    /// Initial TM session count for the statusline `TM:` segment (#319).
    /// Updated by `ReplEvent::TmSessionCount` after subsequent reconciles.
    pub tm_session_count: usize,
    /// Initial count of TM sessions whose adapter is `claude-mpm` (#331).
    /// Updated by `ReplEvent::ClaudeMpmSessionCount` after subsequent reconciles.
    pub claude_mpm_session_count: usize,
    /// Local inference model name for the statusline (#319).
    /// `Some("qwen3:30b")` (vendor prefix stripped) when enabled+available;
    /// `None` when disabled or Ollama not reachable.
    pub local_model: Option<String>,
}

pub async fn run_tui<H: ReplHandler + 'static>(
    startup: ReplStartup,
    handler: Arc<H>,
) -> Result<()> {
    let mut terminal = setup_terminal()?;

    let mut app = ReplApp::new(startup.project_name, startup.user_label);
    app.git_commits = startup.git_commits;
    app.status_line = startup.initial_status;
    app.history = startup.initial_history;
    app.agent_scope = startup.initial_scope;
    app.model_name = startup.model_name;
    app.provider_name = startup.provider_name;
    app.working_dir = startup.working_dir;
    app.git_branch = startup.git_branch;
    app.git_dirty = startup.git_dirty;
    app.statusline_config = startup.statusline_config;
    app.usage_project_dir = startup.project_dir.clone();
    app.tm_session_count = startup.tm_session_count;
    app.claude_mpm_session_count = startup.claude_mpm_session_count;
    app.local_model = startup.local_model;
    // #319: surface any startup chat messages (e.g. TM reconcile result)
    // before the first frame renders so users see them immediately.
    for msg in startup.initial_chat_messages {
        app.push_status(msg);
    }
    // Load any cost already accumulated today (from prior sessions). When
    // the file is missing or dated to a previous day, this returns 0.0.
    let initial = crate::usage::daily::load(&startup.project_dir);
    app.daily_cost_start = initial.cost_usd;

    let app = Arc::new(Mutex::new(app));
    // Currently in-flight handler task (if any). Stored OUTSIDE `ReplApp`
    // because `JoinHandle` is not `Clone` and `ReplApp` derives Clone for
    // the snapshot-on-render pattern. Aborted by Up-arrow when busy (see
    // #XXX); cleared by the spawned task when it completes.
    let current_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>> = Arc::new(Mutex::new(None));
    let (tx, rx) = mpsc::unbounded_channel::<ReplEvent>();

    // #368: Non-blocking update check. Spawn a task that hits GitHub
    // releases; when a newer version is found, surface it as a status
    // message in the chat (and the user can run `/update` to upgrade).
    {
        let update_tx = tx.clone();
        tokio::spawn(async move {
            if let Some(info) = crate::update::check_for_update().await {
                let msg = format!(
                    "Update available: open-mpm v{} (you have v{}) — run /update to upgrade",
                    info.latest_version,
                    env!("CARGO_PKG_VERSION")
                );
                let _ = update_tx.send(ReplEvent::StatusMessage(msg));
            }
        });
    }

    // Spawn key event reader thread. Crossterm's blocking `event::read` is
    // happiest on a dedicated OS thread so it doesn't park the tokio runtime.
    let key_tx = tx.clone();
    let key_thread = std::thread::spawn(move || {
        loop {
            match event::read() {
                Ok(CtEvent::Key(k)) => {
                    if k.kind != KeyEventKind::Press && k.kind != KeyEventKind::Repeat {
                        continue;
                    }
                    if key_tx.send(ReplEvent::Key(k)).is_err() {
                        break;
                    }
                }
                Ok(CtEvent::Resize(c, r)) => {
                    if key_tx.send(ReplEvent::Resize(c, r)).is_err() {
                        break;
                    }
                }
                // #329: Mouse-wheel scroll. EnableMouseCapture is on at startup
                // so these events arrive — previously they were dropped by the
                // catch-all `Ok(_) => continue` arm. -3/+3 mirrors a typical
                // 3-line wheel notch (PageUp/Down use 10).
                Ok(CtEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollUp,
                    ..
                })) => {
                    if key_tx.send(ReplEvent::Scroll(-3)).is_err() {
                        break;
                    }
                }
                Ok(CtEvent::Mouse(MouseEvent {
                    kind: MouseEventKind::ScrollDown,
                    ..
                })) => {
                    if key_tx.send(ReplEvent::Scroll(3)).is_err() {
                        break;
                    }
                }
                Ok(_) => continue,
                Err(_) => break,
            }
        }
    });

    let result = event_loop(
        &mut terminal,
        app.clone(),
        current_task.clone(),
        tx.clone(),
        rx,
        handler,
    )
    .await;

    restore_terminal(&mut terminal).ok();
    drop(tx); // drop the sender so the key reader exits cleanly
    let _ = key_thread.join();

    result
}

pub(crate) fn setup_terminal() -> Result<Terminal<CrosstermBackend<Stdout>>> {
    enable_raw_mode().context("enable raw mode")?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture).context("enter alt screen")?;
    let backend = CrosstermBackend::new(stdout);
    Terminal::new(backend).context("construct terminal")
}

pub(crate) fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<Stdout>>) -> Result<()> {
    disable_raw_mode().ok();
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )
    .ok();
    terminal.show_cursor().ok();
    Ok(())
}

pub(crate) async fn event_loop<H: ReplHandler + 'static>(
    terminal: &mut Terminal<CrosstermBackend<Stdout>>,
    app: Arc<Mutex<ReplApp>>,
    current_task: Arc<Mutex<Option<tokio::task::JoinHandle<()>>>>,
    tx: UnboundedSender<ReplEvent>,
    mut rx: UnboundedReceiver<ReplEvent>,
    handler: Arc<H>,
) -> Result<()> {
    // Initial paint.
    {
        let snap = app.lock().await.clone();
        terminal.draw(|f| draw(f, &snap))?;
    }

    // Periodic tick keeps the frame fresh even when nothing is happening,
    // so stray stderr writes from background tasks (logging, MCP init) get
    // overwritten on the next paint instead of permanently corrupting the
    // alt-screen.
    // 100ms tick keeps the activity-area spinner animating smoothly and the
    // `Xs` elapsed timer ticking in real time. Idle frames are cheap (full
    // diff) so this doesn't burn meaningful CPU.
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(100));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        tokio::select! {
            biased;
            _ = tick.tick() => {
                // Bump the tick counter BEFORE snapshotting so the spinner
                // index advances every frame (10fps). u64 wraps to 0 after
                // ~5.8B years at 10Hz — practically infinite.
                let snap = {
                    let mut a = app.lock().await;
                    a.tick_count = a.tick_count.wrapping_add(1);
                    a.rainbow_tick = a.rainbow_tick.wrapping_add(1);
                    a.clone()
                };
                terminal.draw(|f| draw(f, &snap))?;
                if snap.quit {
                    return Ok(());
                }
                continue;
            }
            ev = rx.recv() => {
                let Some(ev) = ev else { return Ok(()); };
                process_event(ev, &app, &current_task, &tx, &handler).await;
                let snap = app.lock().await.clone();
                terminal.draw(|f| draw(f, &snap))?;
                if snap.quit {
                    return Ok(());
                }
            }
        }
    }
}
