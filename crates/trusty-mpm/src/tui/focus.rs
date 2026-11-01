//! The tui's Focus key: move to a session's existing tmux window or iTerm2
//! tab, never open a new one (#10476).
//!
//! Why: the owner asked for a Focus key that "jumps to the session's existing
//! window or tab; it never opens a new one" (#10476 comment 2). The guarded
//! tmux switch (#2678) lives in the `tm` binary and stays there (Architect
//! Q4), so the binary registers it here as a plain function at startup.
//! What: [`FocusPlan::decide`] picks the mechanism from the environment, as a
//! pure function. [`run_plan`] carries it out through injected effects: the
//! registered [`FocusHook`] for tmux (`switch-client` to the session, landing
//! on its current window) and [`crate::tui::iterm2::focus_session_tab`] for
//! iTerm2 (select-only AppleScript, exact unique tab name, 5 s bound). Both
//! run off the async worker. Outside both, the result is a one-line
//! notice. Every path returns the notice text the action bar shows. No path
//! creates a tmux session or window, or an iTerm2 tab or window.
//! Test: `plan_table_tmux_iterm_neither`, `no_plan_contains_spawn_class_argv`,
//! `focus_outside_tmux_and_iterm2_is_a_notice_only`,
//! `unset_hook_is_a_notice_not_a_panic`, `iterm_tab_not_found_is_a_notice`.

use std::sync::OnceLock;

use crate::tui::iterm2::ItermMatch;

/// What the binary's guarded tmux switch reports back.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusResult {
    /// The terminal's tmux client now shows the session.
    Switched,
    /// The #2678 guard could not prove which client to move; nothing moved.
    Unresolved,
    /// This process is not inside tmux; nothing moved.
    NotInTmux,
    /// tmux ran and refused (for example, no such session).
    Failed(String),
}

/// The binary's quiet, guarded `switch-client`, given a tmux session name.
pub type FocusHook = fn(&str) -> FocusResult;

/// The hook the `tm` binary registers once at startup.
static FOCUS_HOOK: OnceLock<FocusHook> = OnceLock::new();

/// Register the tmux focus function; the first registration wins.
///
/// Why: the lib cannot reach the binary's guarded switch helpers (Architect
/// Q4 keeps them in `bin/tm/commands/tmux_attach.rs`), so the binary hands
/// one in. Returns `false` when a hook was already set.
pub fn set_focus_hook(hook: FocusHook) -> bool {
    FOCUS_HOOK.set(hook).is_ok()
}

/// The registered hook, if the binary set one.
fn focus_hook() -> Option<FocusHook> {
    FOCUS_HOOK.get().copied()
}

/// The two facts [`FocusPlan::decide`] needs about the terminal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct FocusEnv {
    /// `$TMUX` or `$TMUX_PANE` is set (the #2678 nesting rule).
    pub in_tmux: bool,
    /// `$TERM_PROGRAM` is `iTerm.app` or `$ITERM_SESSION_ID` is set.
    pub in_iterm2: bool,
}

impl FocusEnv {
    /// Read the environment of this process.
    pub fn from_process() -> Self {
        let var = |k: &str| std::env::var(k).ok();
        Self::from_vars(
            var("TMUX"),
            var("TMUX_PANE"),
            var("TERM_PROGRAM"),
            var("ITERM_SESSION_ID"),
        )
    }

    /// The pure decision behind [`FocusEnv::from_process`].
    ///
    /// iTerm2 counts only when this terminal is iTerm2; an installed but
    /// unused iTerm2 (what `iterm2::is_iterm2` also accepts) does not.
    pub fn from_vars(
        tmux: Option<String>,
        tmux_pane: Option<String>,
        term_program: Option<String>,
        iterm_session_id: Option<String>,
    ) -> Self {
        let set = |v: &Option<String>| v.as_deref().is_some_and(|s| !s.is_empty());
        Self {
            in_tmux: set(&tmux) || set(&tmux_pane),
            in_iterm2: term_program.as_deref() == Some("iTerm.app") || set(&iterm_session_id),
        }
    }
}

/// How the Focus key reaches a session.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FocusPlan {
    /// Inside tmux: `switch-client` this terminal's client to the session.
    SwitchClient { session: String },
    /// In iTerm2 outside tmux: select the tab already showing the session.
    ItermSelect { session: String },
    /// Neither: show this line and do nothing else.
    Notice(String),
}

impl FocusPlan {
    /// Pick the mechanism for `session` (a tmux session name).
    ///
    /// What: tmux wins over iTerm2 (an iTerm2 window running tmux switches
    /// through tmux). Outside both, a [`FocusPlan::Notice`].
    /// Test: `plan_table_tmux_iterm_neither`.
    pub fn decide(env: FocusEnv, session: &str) -> Self {
        let session = session.to_string();
        if env.in_tmux {
            FocusPlan::SwitchClient { session }
        } else if env.in_iterm2 {
            FocusPlan::ItermSelect { session }
        } else {
            FocusPlan::Notice(format!(
                "focus works inside tmux or iTerm2 only; nothing opened for '{session}'"
            ))
        }
    }

    /// The command this plan runs, as argv; empty for a notice.
    ///
    /// Why: one place a test can read every command a plan can issue.
    /// What: the `switch-client` shape the binary builds
    /// (`bin/tm/commands/tmux_attach.rs::switch_client_argv`, client tty
    /// resolved by its guard), or the two read-then-select `osascript` scripts.
    /// Test: `no_plan_contains_spawn_class_argv`.
    pub fn argv(&self) -> Vec<String> {
        match self {
            FocusPlan::SwitchClient { session } => vec![
                "tmux".to_string(),
                "switch-client".to_string(),
                "-c".to_string(),
                "<client-tty>".to_string(),
                "-t".to_string(),
                crate::core::tmux::exact_session_target(session),
            ],
            FocusPlan::ItermSelect { session } => vec![
                "osascript".to_string(),
                "-e".to_string(),
                crate::tui::iterm2::list_script().to_string(),
                "-e".to_string(),
                crate::tui::iterm2::focus_script(&format!("<id of the tab named {session}>")),
            ],
            FocusPlan::Notice(_) => Vec::new(),
        }
    }
}

/// Carry out `plan` through the given effects; return the notice text.
///
/// Why: the effects are parameters so every branch is testable without tmux
/// or iTerm2. #10476 review: both effects run off the async worker — the
/// tmux hook on the blocking pool, the iTerm2 effect as a bounded tokio
/// child — so a slow terminal never stalls the runtime.
/// What: an empty session name is refused first, before either effect runs.
/// `SwitchClient` calls `hook` (an unset hook is a notice, never a panic);
/// `ItermSelect` awaits `iterm`; `Notice` returns its line.
/// Test: `unset_hook_is_a_notice_not_a_panic`, `hook_results_map_to_notices`,
/// `iterm_tab_not_found_is_a_notice`, `ambiguous_iterm_match_is_a_notice`,
/// `empty_session_name_runs_no_effect`.
pub async fn run_plan<F, Fut>(plan: &FocusPlan, hook: Option<FocusHook>, iterm: F) -> String
where
    F: FnOnce(String) -> Fut,
    Fut: std::future::Future<Output = Result<ItermMatch, String>>,
{
    match plan {
        FocusPlan::Notice(line) => line.clone(),
        FocusPlan::SwitchClient { session } | FocusPlan::ItermSelect { session }
            if session.trim().is_empty() =>
        {
            "this session has no tmux name; nothing to focus".to_string()
        }
        FocusPlan::SwitchClient { session } => switch_notice(session, hook).await,
        FocusPlan::ItermSelect { session } => iterm_notice(session, iterm(session.clone()).await),
    }
}

/// Run `hook` on the blocking pool and word its result.
async fn switch_notice(session: &str, hook: Option<FocusHook>) -> String {
    let Some(hook) = hook else {
        return format!("focus is unavailable in this build; nothing moved for '{session}'");
    };
    let name = session.to_string();
    let result = tokio::task::spawn_blocking(move || hook(&name))
        .await
        .unwrap_or_else(|e| FocusResult::Failed(format!("focus task: {e}")));
    match result {
        FocusResult::Switched => format!("focused '{session}'"),
        FocusResult::Unresolved => format!(
            "could not verify this terminal's tmux client (#2678); nothing moved for '{session}'"
        ),
        FocusResult::NotInTmux => format!("not inside tmux; nothing moved for '{session}'"),
        FocusResult::Failed(e) => format!("focus '{session}' failed: {e}"),
    }
}

/// Word the iTerm2 effect's result.
fn iterm_notice(session: &str, result: Result<ItermMatch, String>) -> String {
    match result {
        Ok(ItermMatch::Selected) => format!("focused the iTerm2 tab for '{session}'"),
        Ok(ItermMatch::Missing) => format!("no iTerm2 tab is named '{session}'; nothing opened"),
        Ok(ItermMatch::Ambiguous(n)) => {
            format!("{n} iTerm2 tabs are named '{session}' (ambiguous); nothing selected")
        }
        Err(e) => format!("iTerm2 focus failed: {e}"),
    }
}

/// Focus the session named `session` from this process; return the notice.
///
/// Test: the parts are tested through [`FocusPlan::decide`] and
/// [`run_plan`]; the real switch runs only in the sandbox live check.
pub async fn focus_session(session: &str) -> String {
    let plan = FocusPlan::decide(FocusEnv::from_process(), session);
    run_plan(&plan, focus_hook(), |name| async move {
        crate::tui::iterm2::focus_session_tab(&name).await
    })
    .await
}

#[cfg(test)]
#[path = "focus_tests.rs"]
mod tests;
