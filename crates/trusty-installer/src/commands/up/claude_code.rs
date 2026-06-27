//! STAGE 4b — ensure Claude Code is present and at latest (DOC-12 §6).
//!
//! Why: When the orchestrator stage is opted in and launches Claude Code
//! natively (`env -u ANTHROPIC_API_KEY claude`), a session against a
//! stale/absent runtime fails confusingly mid-session. DOC-12 §6 lifts the
//! presence check to boot and adds a version/latest check + prompt-first upgrade
//! so the failure surfaces *before* a session is attempted.
//!
//! What: The §6 flow as a pure-policy state machine over an injectable
//! `ClaudeEnv` (so the OS calls are mockable): PRESENCE (`which claude`) →
//! VERSION (`claude --version`) → LATEST (native `claude update` probe / npm
//! fallback) → PROMPT-then-UPGRADE (never silent; non-TTY warns, never applies)
//! → CONFIRM (`env -u ANTHROPIC_API_KEY claude` resolves). Absent → guide.
//!
//! Test: `super::tests::claude_*` drive `ensure_claude_code` with a scripted
//! `ClaudeEnv` across absent / current / stale-consent / stale-decline /
//! non-TTY / skip-upgrade / present-policy paths.

use serde::Serialize;

/// The exact managed-session spawn command DOC-12 §6 must re-verify.
///
/// Why: Mirrors `trusty-mpm`'s `ClaudeCodeAdapter::SPAWN_COMMAND` so boot
/// confirms the very command the orchestrator will later run.
///
/// What: `env -u ANTHROPIC_API_KEY claude` — scrubs the API key so Claude Code
/// uses its OAuth credential store (the "native" managed path).
///
/// Test: Referenced in `ensure_claude_code` CONFIRM step; asserted present in
/// `super::tests::claude_current_confirms_managed_path`.
pub const MANAGED_SPAWN_COMMAND: &str = "env -u ANTHROPIC_API_KEY claude";

/// The outcome of the STAGE 4b Claude Code ensure (DOC-12 §6).
///
/// Why: The orchestrator renders this in the §5 report (orchestrator row,
/// runtime sub-cell) and uses `ready` to decide whether the orchestrator stage
/// is fully ready.
///
/// What: `ready` = a usable managed-session runtime is confirmed; `detail` is a
/// human label of what happened; `action` records the upgrade decision.
///
/// Test: `super::tests::claude_*` assert each variant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct ClaudeOutcome {
    /// Whether the managed-session runtime is confirmed usable.
    pub ready: bool,
    /// Action taken: `absent-guided`, `current`, `upgraded`, `declined`,
    /// `skip-upgrade`, `non-tty-warn`, `present-policy`.
    pub action: String,
    /// Human detail for the report.
    pub detail: String,
}

/// Injectable seam over the OS operations DOC-12 §6 performs.
///
/// Why: The §6 flow shells out (`which`, `claude --version`, `claude update`,
/// npm). Hiding those behind a trait makes the prompt-first state machine
/// unit-testable with no `claude` installed.
///
/// What: presence probe, installed-version probe, latest-version lookup, the
/// upgrade action, a managed-path confirm, a yes/no prompt, and a TTY query.
///
/// Test: `super::tests::ScriptedClaudeEnv` implements this.
pub trait ClaudeEnv {
    /// `which claude` — is the runtime on PATH?
    fn present(&self) -> bool;
    /// `claude --version` — installed version string, or `None` if unreadable.
    fn installed_version(&self) -> Option<String>;
    /// Latest available version (native updater probe / npm), or `None`.
    fn latest_version(&self) -> Option<String>;
    /// Apply the upgrade (native `claude update`, npm fallback). Err on failure.
    fn upgrade(&self) -> anyhow::Result<()>;
    /// Confirm `env -u ANTHROPIC_API_KEY claude` resolves post-upgrade.
    fn confirm_managed_path(&self) -> bool;
    /// Prompt the user `[Y/n]`; returns their consent. Only called on a TTY.
    fn prompt_upgrade(&self, installed: &str, latest: &str) -> bool;
    /// Whether we are attached to an interactive TTY (drives the §6 non-TTY rule).
    fn is_tty(&self) -> bool;
}

/// Run the DOC-12 §6 ensure-Claude-Code-@-latest flow.
///
/// Why: The single entry point STAGE 4 calls when the orchestrator is opted in
/// and its runtime kind is `claude-code`. Encodes the RESOLVED-Q3 invariant
/// that an upgrade is **never applied silently** — consent is required on a TTY,
/// and a non-TTY only warns.
///
/// What:
/// - absent → guide (print `install_hint`), `ready=false`, never auto-install.
/// - `min_policy == "present"` or `skip_upgrade` → accept installed, warn if
///   stale, confirm managed path.
/// - else compare installed vs latest: if current → confirm; if stale → on a
///   TTY PROMPT then (on consent) upgrade + re-confirm; non-TTY → warn, do not
///   apply.
///
/// Test: `super::tests::claude_absent_guides`,
/// `claude_current_confirms_managed_path`, `claude_stale_consent_upgrades`,
/// `claude_stale_decline_keeps`, `claude_non_tty_warns`,
/// `claude_skip_upgrade_accepts`, `claude_present_policy_accepts`.
pub fn ensure_claude_code(
    env: &dyn ClaudeEnv,
    install_hint: &str,
    min_policy: &str,
    skip_upgrade: bool,
) -> ClaudeOutcome {
    // STEP 1 — PRESENCE.
    if !env.present() {
        return ClaudeOutcome {
            ready: false,
            action: "absent-guided".to_owned(),
            detail: format!(
                "Claude Code (`claude`) not found on PATH. Install it to use the \
                 managed-session path, then re-run `tctl up --with-mpm`. Hint: {install_hint}"
            ),
        };
    }

    let installed = env
        .installed_version()
        .unwrap_or_else(|| "unknown".to_owned());

    // `present` policy (or skip): accept the installed runtime, warn if stale.
    if min_policy.eq_ignore_ascii_case("present") || skip_upgrade {
        let ready = env.confirm_managed_path();
        let action = if skip_upgrade {
            "skip-upgrade"
        } else {
            "present-policy"
        };
        return ClaudeOutcome {
            ready,
            action: action.to_owned(),
            detail: format!("accepted installed claude {installed} (policy={min_policy})"),
        };
    }

    // STEP 2/3 — VERSION + LATEST.
    let latest = env.latest_version();
    let is_current = matches!(&latest, Some(l) if l == &installed) || latest.is_none();
    if is_current {
        let ready = env.confirm_managed_path();
        return ClaudeOutcome {
            ready,
            action: "current".to_owned(),
            detail: format!("claude {installed} is current"),
        };
    }
    let latest = latest.unwrap_or_else(|| "latest".to_owned());

    // STEP 4 — PROMPT then UPGRADE (never silent).
    if !env.is_tty() {
        // Non-TTY: warn, do not apply (RESOLVED Q3 / §3.5 non-TTY rule).
        return ClaudeOutcome {
            ready: env.confirm_managed_path(),
            action: "non-tty-warn".to_owned(),
            detail: format!(
                "claude {installed} installed; latest is {latest}. Not upgrading without a TTY \
                 (re-run interactively, or pass --skip-claude-upgrade to suppress)."
            ),
        };
    }

    if !env.prompt_upgrade(&installed, &latest) {
        // User declined: keep the installed version.
        return ClaudeOutcome {
            ready: env.confirm_managed_path(),
            action: "declined".to_owned(),
            detail: format!("user declined upgrade; keeping claude {installed} (latest {latest})"),
        };
    }

    // Consent given — apply.
    if let Err(e) = env.upgrade() {
        tracing::error!(error = %e, "claude upgrade failed");
        return ClaudeOutcome {
            ready: env.confirm_managed_path(),
            action: "upgrade-failed".to_owned(),
            detail: format!("upgrade to {latest} failed: {e}; keeping claude {installed}"),
        };
    }

    // STEP 5 — CONFIRM.
    let ready = env.confirm_managed_path();
    ClaudeOutcome {
        ready,
        action: "upgraded".to_owned(),
        detail: format!("upgraded claude {installed} → {latest}; managed path confirmed={ready}"),
    }
}
