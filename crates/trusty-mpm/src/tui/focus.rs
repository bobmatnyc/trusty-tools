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
//! iTerm2 (select-only AppleScript). Outside both, the result is a one-line
//! notice. Every path returns the notice text the action bar shows. No path
//! creates a tmux session or window, or an iTerm2 tab or window.
//! Test: `plan_table_tmux_iterm_neither`, `no_plan_contains_spawn_class_argv`,
//! `focus_outside_tmux_and_iterm2_is_a_notice_only`,
//! `unset_hook_is_a_notice_not_a_panic`, `iterm_tab_not_found_is_a_notice`.

use std::sync::OnceLock;

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
    /// resolved by its guard), or `osascript -e <select-only script>`.
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
                crate::tui::iterm2::focus_script(session),
            ],
            FocusPlan::Notice(_) => Vec::new(),
        }
    }
}

/// The iTerm2 select function: `Ok(true)` found and selected, `Ok(false)` no
/// tab shows the session.
pub type ItermSelect = fn(&str) -> Result<bool, String>;

/// Carry out `plan` through the given effects; return the notice text.
///
/// Why: the effects are parameters so every branch is testable without tmux
/// or iTerm2.
/// What: `SwitchClient` calls `hook` (an unset hook is a notice, never a
/// panic); `ItermSelect` calls `iterm`; `Notice` returns its line.
/// Test: `unset_hook_is_a_notice_not_a_panic`, `hook_results_map_to_notices`,
/// `iterm_tab_not_found_is_a_notice`.
pub fn run_plan(plan: &FocusPlan, hook: Option<FocusHook>, iterm: ItermSelect) -> String {
    match plan {
        FocusPlan::SwitchClient { session } => match hook.map(|h| h(session)) {
            None => format!("focus is unavailable in this build; nothing moved for '{session}'"),
            Some(FocusResult::Switched) => format!("focused '{session}'"),
            Some(FocusResult::Unresolved) => format!(
                "could not verify this terminal's tmux client (#2678); nothing moved for '{session}'"
            ),
            Some(FocusResult::NotInTmux) => {
                format!("not inside tmux; nothing moved for '{session}'")
            }
            Some(FocusResult::Failed(e)) => format!("focus '{session}' failed: {e}"),
        },
        FocusPlan::ItermSelect { session } => match iterm(session) {
            Ok(true) => format!("focused the iTerm2 tab for '{session}'"),
            Ok(false) => format!("no iTerm2 tab shows '{session}'; nothing opened"),
            Err(e) => format!("iTerm2 focus failed: {e}"),
        },
        FocusPlan::Notice(line) => line.clone(),
    }
}

/// Focus the session named `session` from this process; return the notice.
///
/// Test: the parts are tested through [`FocusPlan::decide`] and
/// [`run_plan`]; the real switch runs only in the sandbox live check.
pub fn focus_session(session: &str) -> String {
    let plan = FocusPlan::decide(FocusEnv::from_process(), session);
    run_plan(&plan, focus_hook(), crate::tui::iterm2::focus_session_tab)
}

#[cfg(test)]
#[path = "focus_tests.rs"]
mod tests;
