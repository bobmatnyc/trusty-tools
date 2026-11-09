//! The notification-hook step of `tm fleet init` and its `tm fleet status`
//! row (#10885).
//!
//! Why: tm's user-level `Notification` hook appends each PM permission prompt
//! or input request to the Architect's `inbox/events.jsonl`. Without it the
//! Architect sees a blocked session only on its next pane poll. The hook is
//! opt-in (`[notification_hook]` in `~/.trusty-mpm/config.toml`), and nothing
//! on the Architect setup path turned it on or reported it.
//! What: [`step`] is the init step under the issue's five-row contract;
//! [`status_check`] is the informational status row. Both read the managed
//! settings through [`probe_notification_hook`], the plan
//! `tm doctor --fix --only notification_hook` runs, so neither can disagree
//! with the repair.
//! Test: `notify_hook_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::standalone::hooks::notification::{
    HookSync, NOTIFICATION_HOOK_FIX, NotificationHookConfig, apply_notification_hook,
    probe_notification_hook,
};
use trusty_mpm::core::trusty_tools_config::managed_claude_config_dir_at;

use super::config::{self, Edit};
use super::status::Check;
use super::{Step, read_or_empty, user_config_path};

/// The inbox directory below the Architect directory.
pub(crate) const INBOX_DIR: &str = "inbox";

/// The tm-managed Claude Code settings under `home`, which the hook goes into.
pub(crate) fn settings_path(home: &Path) -> PathBuf {
    managed_claude_config_dir_at(home).join("settings.json")
}

/// The line every Skipped outcome prints.
fn fix_line() -> String {
    format!("edit ~/.trusty-mpm/config.toml, then {NOTIFICATION_HOOK_FIX}")
}

/// Enable the hook for the Architect in `dir` (canonical); see the module doc.
///
/// Why: the issue's init-step contract. The step never fails `init` except
/// on the `config.toml` write, like the Intent Ledger hook step.
/// What: re-reads `home`'s config (the allowlist write ran first).
/// (1) No `notification_hook` key: checks the managed settings with
/// [`probe_notification_hook`] first, so a file the writer refuses (or no
/// stable `tm` binary) is [`Step::Skipped`] with nothing written; then creates
/// `dir/inbox` (a failure is Skipped, nothing written), writes the two keys ([`config::enable_notification_hook`]) and applies the hook:
/// [`Step::Changed`]. A config write error is [`Step::Failed`]; an apply
/// error after it is [`Step::Skipped`] saying the config was written.
/// (2) Present, `inbox` is `dir/inbox`: [`Step::Unchanged`], writing nothing,
/// with a note when the hook is off or not applied.
/// (3) Present, `inbox` elsewhere or unset: [`Step::Skipped`], writing nothing.
/// `exe` overrides the hook binary (tests); production passes `None`.
/// Test: `a_first_run_enables_the_hook_and_applies_it`,
/// `a_second_run_is_unchanged_and_writes_nothing`,
/// `an_inbox_set_elsewhere_is_skipped_and_the_config_keeps_it`,
/// `a_malformed_managed_settings_file_is_skipped_and_nothing_is_written`,
/// `a_config_write_failure_fails_the_step`.
pub(crate) fn step(dir: &Path, home: &Path, exe: Option<&Path>) -> Step {
    let config_path = user_config_path(home);
    let settings = settings_path(home);
    let inbox = dir.join(INBOX_DIR);
    let edited = read_or_empty(&config_path).and_then(|raw| {
        let edit = config::enable_notification_hook(&raw, &inbox, &config_path)?;
        let (_, typed) = config::parse_user_config(&raw, &config_path)?;
        Ok((edit, typed.notification_hook))
    });
    let text = match edited {
        Err(e) => return Step::Failed(format!("`[notification_hook]`: {e:#}")),
        Ok((Edit::Unchanged, hook)) => {
            return present(&hook, &inbox, &config_path, &settings, exe);
        }
        Ok((Edit::Changed(text), _)) => text,
    };
    // #10885 Fail-Open: refuse before the first write, so a skip writes nothing.
    if let HookSync::Refused(why) = probe_notification_hook(&settings, exe, true) {
        return Step::Skipped(format!(
            "`[notification_hook]` not enabled: {} cannot take the Notification hook ({why}); \
             nothing was written. Fix that file and re-run `tm fleet init`, or {}",
            settings.display(),
            fix_line()
        ));
    }
    // #10885: `tm hook` appends to `<inbox>/events.jsonl` and creates no directory.
    if let Err(e) = std::fs::create_dir_all(&inbox) {
        return Step::Skipped(format!(
            "`[notification_hook]` not enabled: cannot create {}: {e}; nothing was written. \
             Fix that and re-run `tm fleet init`, or {}",
            inbox.display(),
            fix_line()
        ));
    }
    if let Err(e) = trusty_common::atomic_file::write_atomic(&config_path, text.as_bytes()) {
        return Step::Failed(format!(
            "`[notification_hook]` not enabled: cannot write {}: {e}",
            config_path.display()
        ));
    }
    match apply_notification_hook(&settings, exe, true) {
        Ok(_) => Step::Changed(format!(
            "wrote `[notification_hook]` enabled = true, inbox = {} in {} and applied the \
             Notification hook to {}",
            inbox.display(),
            config_path.display(),
            settings.display()
        )),
        Err(e) => Step::Skipped(format!(
            "wrote `[notification_hook]` in {}, but the Notification hook was not applied to {} \
             ({e:#}); {}",
            config_path.display(),
            settings.display(),
            fix_line()
        )),
    }
}

/// The step for a config that already has `[notification_hook]`; writes nothing.
fn present(
    hook: &NotificationHookConfig,
    inbox: &Path,
    config_path: &Path,
    settings: &Path,
    exe: Option<&Path>,
) -> Step {
    match &hook.inbox {
        Some(set) if same_dir(set, inbox) => {
            let note = match probe_notification_hook(settings, exe, hook.enabled) {
                _ if !hook.enabled => " (enabled is not true, so the hook stays off)".to_owned(),
                HookSync::InSync => String::new(),
                HookSync::Pending | HookSync::Refused(_) => format!(
                    "; not applied to {}: run `{NOTIFICATION_HOOK_FIX}`",
                    settings.display()
                ),
            };
            Step::Unchanged(format!(
                "`[notification_hook]` in {} points at {}{note}",
                config_path.display(),
                inbox.display()
            ))
        }
        other => Step::Skipped(format!(
            "`[notification_hook]` in {} points elsewhere ({}), so it was not changed; to use \
             this Architect's inbox {}: {}",
            config_path.display(),
            other
                .as_ref()
                .map_or_else(|| "no inbox set".to_owned(), |p| p.display().to_string()),
            inbox.display(),
            fix_line()
        )),
    }
}

/// The informational `notification_hook` row of `tm fleet status`.
///
/// Why (#10885): the operator needs to see whether the Architect gets pushed
/// events or falls back to pane capture; like `skills` and `bundle`, the row
/// never affects `complete`.
/// What: from `home`'s `[notification_hook]` and [`probe_notification_hook`]:
/// `off`; `points elsewhere: <path>` when `inbox` is not `dir/inbox`;
/// `enabled but not applied` when the probe is not in sync; else
/// `on → <inbox>`, the only ok state. Reads only.
/// Test: `the_status_row_reports_each_of_the_four_states`.
pub(crate) fn status_check(dir: &Path, home: &Path, exe: Option<&Path>) -> Check {
    let hook = MpmConfig::load(&home.join(".trusty-mpm")).notification_hook;
    let inbox = dir.join(INBOX_DIR);
    let (ok, detail) = match &hook.inbox {
        _ if !hook.enabled => (false, "off".to_owned()),
        Some(set) if same_dir(set, &inbox) => {
            match probe_notification_hook(&settings_path(home), exe, true) {
                HookSync::InSync => (true, format!("on → {}", set.display())),
                HookSync::Pending => (
                    false,
                    format!("enabled but not applied; run `{NOTIFICATION_HOOK_FIX}`"),
                ),
                HookSync::Refused(why) => (false, format!("enabled but not applied ({why})")),
            }
        }
        Some(set) => (false, format!("points elsewhere: {}", set.display())),
        None => (false, "points elsewhere: no inbox set".to_owned()),
    };
    Check {
        name: "notification_hook",
        ok,
        detail,
    }
}

/// Whether two directory paths name the same directory; the inbox itself
/// may not exist yet, so a missing leaf is resolved through its parent.
fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| {
        std::fs::canonicalize(p).ok().or_else(|| {
            let parent = std::fs::canonicalize(p.parent()?).ok()?;
            Some(parent.join(p.file_name()?))
        })
    };
    a == b || matches!((canon(a), canon(b)), (Some(x), Some(y)) if x == y)
}
