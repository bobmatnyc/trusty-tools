//! The opt-in `Notification` hook entry and its push target (#8392).
//!
//! Why: Claude Code fires `Notification` when a session needs permission or
//! sits idle awaiting input — the "needs user action" signal a fleet
//! supervisor wants pushed rather than polled. [`super::mpm_hook_additions_with_exe`]
//! writes six lifecycle events and never this one, and an operator who did not
//! ask must keep exactly that, so the entry is opt-in. Nothing names "the
//! supervisor session", so where `tm hook` forwards the event is configured,
//! never inferred.
//! What: [`NotificationHookConfig`] is the `[notification_hook]` section of the
//! user-level `~/.trusty-mpm/config.toml`; [`apply_notification_hook`] writes
//! (opt-in on) or removes (off) tm's own `Notification` entry and nothing else;
//! [`repair_notification_hook`] is the `tm doctor --fix` step over the same
//! plan; [`resolve_push_target`] names the inbox `tm hook` appends to.
//! Test: `notification_tests.rs`.

use std::ffi::OsString;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Deserializer, Serialize};
use serde_json::Value;

use super::StableHookExeError;
use crate::core::doctor_repair::{RepairMode, RepairStep, StepStatus};

/// The Claude Code hook event this module owns.
pub const NOTIFICATION_EVENT: &str = "Notification";

/// Env var naming the push-target inbox directory; outranks the config key.
pub const NOTIFY_INBOX_ENV: &str = "TRUSTY_MPM_NOTIFY_INBOX";

/// The file `tm hook` appends to inside the inbox directory.
///
/// Why: the Architect's poller and `tm-fleet-check` skill already read
/// `<inbox>/events.jsonl` (the prototype's `notify-supervisor.sh` contract), so
/// the same name makes the forward a drop-in for #8436 P3.
pub const INBOX_EVENTS_FILE: &str = "events.jsonl";

/// The `tm doctor` check name the repair step reports under.
pub const NOTIFICATION_HOOK_CHECK: &str = "notification_hook";

/// `[notification_hook]` in the user-level `~/.trusty-mpm/config.toml` (#8392).
///
/// Why: the opt-in and the push target are operator choices that must sit
/// outside every project's committed files, like the #8453 `[supervisor]`
/// allowlist — a cloned repository must not be able to turn the forward on or
/// point it somewhere.
/// What: `enabled` — `tm install` and `tm doctor --fix --yes` write tm's
/// `Notification` entry when `true` and remove it when `false`; only the TOML
/// boolean `true` enables it, and any other value is OFF (see
/// [`lenient_opt_in`]). `inbox` — an absolute directory `tm hook` appends one
/// line per `Notification` to; [`NOTIFY_INBOX_ENV`] outranks it. The two are
/// independent: an entry written by another tool still forwards when `inbox`
/// is set.
/// Test: `an_unparseable_opt_in_is_off_and_keeps_the_rest_of_the_config`,
/// `the_opt_in_and_inbox_parse`.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct NotificationHookConfig {
    /// Write tm's `Notification` hook entry. Default `false`.
    #[serde(deserialize_with = "lenient_opt_in")]
    pub enabled: bool,
    /// Absolute inbox directory `tm hook` forwards `Notification` events to.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub inbox: Option<PathBuf>,
}

/// Read the opt-in so that anything but the boolean `true` is OFF.
///
/// Why (#8392 Fail-Open Check): a strict `bool` field turns `enabled = "yes"`
/// into a parse error, and `MpmConfig::load` answers a parse error by dropping
/// the WHOLE file. Reading the value leniently keeps every other section and
/// still lands on OFF, the side that writes nothing.
/// What: `true` only for a boolean `true`; any other value logs one warning and
/// yields `false`.
/// Test: `an_unparseable_opt_in_is_off_and_keeps_the_rest_of_the_config`.
fn lenient_opt_in<'de, D: Deserializer<'de>>(deserializer: D) -> Result<bool, D::Error> {
    match Value::deserialize(deserializer)? {
        Value::Bool(on) => Ok(on),
        other => {
            tracing::warn!(
                "[notification_hook] enabled = {other} is not a boolean; treating the opt-in as off"
            );
            Ok(false)
        }
    }
}

/// Where `tm hook` forwards a `Notification`, if anywhere.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PushTarget {
    /// No target configured: no forward, no error.
    Unset,
    /// Append to `<dir>/events.jsonl`.
    Inbox(PathBuf),
    /// A configured value that is not an absolute path. The value itself is
    /// not carried, so a log line cannot echo it.
    NotAbsolute,
}

/// Resolve the push target from the env var, then the config key.
///
/// Why (#8392): the target comes from configuration only — no session is
/// inferred. Taking both inputs as values keeps the rule testable without
/// touching the process environment.
/// What: a non-empty `env_value` wins, else `config.inbox`, else
/// [`PushTarget::Unset`]. A relative value is [`PushTarget::NotAbsolute`]:
/// resolving it against whatever cwd a hook runs in would scatter inboxes.
/// Test: `the_env_var_outranks_the_config_key`,
/// `an_unset_target_is_unset_and_a_relative_one_is_refused`.
pub fn resolve_push_target(
    env_value: Option<OsString>,
    config: &NotificationHookConfig,
) -> PushTarget {
    let chosen = env_value
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| config.inbox.clone());
    match chosen {
        None => PushTarget::Unset,
        Some(dir) if dir.is_absolute() => PushTarget::Inbox(dir),
        Some(_) => PushTarget::NotAbsolute,
    }
}

/// Build the one-event `Notification` block that runs `tm hook`.
///
/// Why: the same `<abs exe> hook` command the lifecycle block uses, so
/// [`super::is_mpm_hook_command`] recognises the entry as tm's for the
/// replace-by-identity strip and for removal.
/// What: `{"hooks":{"Notification":[{"matcher":"*","hooks":[{"type":"command",
/// "command":"<exe> hook","timeout":5}]}]}}`, or the resolver's refusal.
/// Test: `opt_in_on_writes_one_entry_and_rerunning_is_idempotent`.
pub fn notification_hook_additions_with_exe(
    exe_override: Option<&Path>,
) -> Result<Value, StableHookExeError> {
    let cmd = super::mpm_hook_command(exe_override)?;
    Ok(serde_json::json!({
        "hooks": {
            NOTIFICATION_EVENT: [{
                "matcher": "*",
                "hooks": [{ "type": "command", "command": cmd, "timeout": 5 }]
            }]
        }
    }))
}

/// Write (opt-in on) or remove (off) tm's own `Notification` entry.
///
/// Why (#8392): the writer behind `tm install` and `tm doctor --fix --yes`.
/// It must never cost the operator an entry of their own, so it touches only
/// `Notification` entries [`super::is_mpm_hook_command`] recognises, and it
/// refuses a file it cannot read as a settings object rather than rewrite it
/// — unlike the lifecycle writer's #7789 copy-aside-and-rewrite.
/// What: resolves the hook command first when `enabled` (a refusal writes
/// nothing). Off with no file is a no-op that creates nothing. Otherwise, under
/// the settings lock, [`plan_notification_hook`] computes the new value; an
/// unchanged plan returns `Ok(false)` without writing, and a changed one is
/// snapshotted and published atomically. Returns `true` when the file changed.
/// Test: `opt_in_off_installs_exactly_the_six_lifecycle_events`,
/// `opt_in_on_writes_one_entry_and_rerunning_is_idempotent`,
/// `opt_in_off_removes_only_the_entry_tm_wrote`,
/// `a_malformed_settings_file_is_refused_and_left_byte_identical`.
pub fn apply_notification_hook(
    settings_path: &Path,
    exe_override: Option<&Path>,
    enabled: bool,
) -> anyhow::Result<bool> {
    let additions = if enabled {
        Some(notification_hook_additions_with_exe(exe_override)?)
    } else {
        None
    };
    if !enabled && !settings_path.exists() {
        return Ok(false);
    }
    crate::core::settings_lock::with_settings_lock(settings_path, || {
        let Some(next) = plan_notification_hook(settings_path, additions.as_ref())? else {
            return Ok(false);
        };
        if let Some(parent) = settings_path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        super::backup::snapshot_then_prune(
            settings_path,
            super::backup::HOOK_SETTINGS_SNAPSHOTS_KEPT,
        )
        .map_err(|e| anyhow::anyhow!("snapshot {}: {e}", settings_path.display()))?;
        crate::core::settings_lock::publish(settings_path, &next)
            .map_err(|e| anyhow::anyhow!("write {}: {e}", settings_path.display()))?;
        Ok(true)
    })?
}

/// The settings value after the `Notification` change, or `None` when the file
/// already matches.
///
/// Why: one plan shared by the writer and the doctor's dry run, so a preview
/// can never promise a change the write would not make.
/// What: reads the file strictly ([`read_settings_object`]), strips tm-owned
/// `Notification` entries only, then merges `additions` when present. Foreign
/// entries keep their place; tm's entry lands after them.
/// Test: `opt_in_off_removes_only_the_entry_tm_wrote`.
fn plan_notification_hook(
    settings_path: &Path,
    additions: Option<&Value>,
) -> anyhow::Result<Option<Value>> {
    let original = read_settings_object(settings_path)?;
    let mut next = original.clone();
    super::strip_hook_entries_matching_for_events(
        &mut next,
        Some(&[NOTIFICATION_EVENT.to_string()]),
        super::is_mpm_hook_command,
    );
    if let Some(additions) = additions {
        next = trusty_common::claude_config::merge_hook_entries(&next, additions);
    }
    Ok((next != original).then_some(next))
}

/// Read a settings file that this module may rewrite, refusing anything else.
///
/// Why (#8392 Fail-Open Check): a file that is not a settings object — or
/// whose `hooks` / `hooks.Notification` has the wrong shape, which the merge
/// would coerce and so discard — holds content this writer cannot preserve.
/// Refusing leaves it byte-identical and reports no success.
/// What: `{}` for an absent or whitespace-only file; the parsed object when
/// `hooks` is absent or an object and `hooks.Notification` is absent or an
/// array; otherwise an error naming the file.
/// Test: `a_malformed_settings_file_is_refused_and_left_byte_identical`.
fn read_settings_object(settings_path: &Path) -> anyhow::Result<Value> {
    let text = match std::fs::read_to_string(settings_path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(serde_json::json!({})),
        Err(e) => anyhow::bail!("read {}: {e}", settings_path.display()),
    };
    if text.trim().is_empty() {
        return Ok(serde_json::json!({}));
    }
    let refuse = || {
        anyhow::anyhow!(
            "{} is not a settings object tm can edit safely; left unchanged — fix it, then re-run",
            settings_path.display()
        )
    };
    let value: Value = serde_json::from_str(&text).map_err(|_| refuse())?;
    let hooks = value.get("hooks");
    let well_formed = value.is_object()
        && hooks.is_none_or(Value::is_object)
        && hooks
            .and_then(|h| h.get(NOTIFICATION_EVENT))
            .is_none_or(Value::is_array);
    if well_formed {
        Ok(value)
    } else {
        Err(refuse())
    }
}

/// The `tm doctor --fix` step that brings tm's `Notification` entry in line
/// with the opt-in.
///
/// Why (#8392): the issue's closure names the doctor fix path as the way to
/// write the entry once the flag is set; the same step removes it once the
/// flag is cleared.
/// What: no step when the file already matches (or is absent with the opt-in
/// off). Otherwise one step: [`StepStatus::Planned`] in a dry run,
/// [`StepStatus::Applied`] after [`apply_notification_hook`], and
/// [`StepStatus::Refused`] for an unresolvable hook binary or a file
/// [`read_settings_object`] refuses — never a success for either.
/// Test: `the_doctor_step_plans_applies_and_then_goes_silent`,
/// `the_doctor_step_refuses_a_malformed_file`.
pub fn repair_notification_hook(
    settings_path: &Path,
    exe_override: Option<&Path>,
    enabled: bool,
    mode: RepairMode,
) -> Vec<RepairStep> {
    let what = if enabled {
        "write the tm `Notification` hook entry ([notification_hook] enabled = true)"
    } else {
        "remove the tm `Notification` hook entry ([notification_hook] is off)"
    };
    let step = |status| RepairStep {
        check: NOTIFICATION_HOOK_CHECK,
        path: settings_path.to_path_buf(),
        what: what.to_string(),
        status,
    };
    if !enabled && !settings_path.exists() {
        return Vec::new();
    }
    let additions = match enabled.then(|| notification_hook_additions_with_exe(exe_override)) {
        Some(Err(e)) => return vec![step(StepStatus::Refused(e.to_string()))],
        Some(Ok(v)) => Some(v),
        None => None,
    };
    match plan_notification_hook(settings_path, additions.as_ref()) {
        Ok(None) => Vec::new(),
        Err(e) => vec![step(StepStatus::Refused(e.to_string()))],
        Ok(Some(_)) if mode == RepairMode::DryRun => vec![step(StepStatus::Planned)],
        Ok(Some(_)) => match apply_notification_hook(settings_path, exe_override, enabled) {
            Ok(_) => vec![step(StepStatus::Applied { backup: None })],
            Err(e) => vec![step(StepStatus::Failed(e.to_string()))],
        },
    }
}

#[cfg(test)]
#[path = "notification_tests.rs"]
mod tests;
