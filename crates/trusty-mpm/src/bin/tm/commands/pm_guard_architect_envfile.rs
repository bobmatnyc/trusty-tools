//! The Architect's env-file exemption from the #7266 secret-file rule (#8939).
//!
//! Why: owner ruling item 44 — the Architect must manage a project's dotenv
//! files, and every caller is denied any command naming one. The Architect
//! rulings on the #8939 design narrow the grant to two verbs that print no
//! value: `tm env set` (the value arrives on stdin or from the login Keychain,
//! never in argv) and `tm env keys` (names only).
//! What: [`evaluate_secret_file_read_gated`] is the one secret-read decision
//! both deny sites call — `pm_guard` on the guarded path and `pm_guard_floor`
//! under a bypass — so the two cannot drift. Its order: the value-printing
//! rules (a printed credential, the pod/launchd/pm2 env dumps, a launchd
//! plist, and since #8879 a run script whose body reads a credential) deny
//! unconditionally; then the #7266 file rule; then a file-rule
//! deny is lifted only when [`envfile_shape`] matches AND the call is the
//! process-bound Architect's main thread. A shape match with a failed
//! identity denies with the #8878 PR-I identity suffix. [`gate_secret_file_read`]
//! and [`audit_envfile_allow`] write the `architect-envfile` audit line and
//! mint the one-shot grant `tm env` requires (`env_file_grant`).
//! [`envfile_policy`] is the path policy the guard and `tm env` share.
//! Tools (`Read`, `Write`, `Edit`, `Grep`, …) are never exempt.
//! Scope (Architect ruling Q2 addendum): `CLAUDE_PROJECT_DIR` plus the
//! `[supervisor] projects` list of the user config `~/.trusty-mpm/config.toml`,
//! read by the same reader as the #8878 identity check. It never comes from
//! `~/.trusty-mpm/project-paths.json`, which any PM launch writes.
//! FAIL-CLOSED: a path that does not resolve, lies outside that scope, or is
//! not a regular file (a missing file too, for `set`) keeps the deny; so does
//! any identity failure. An unreadable or malformed config lists no root, so
//! only `CLAUDE_PROJECT_DIR` counts; with that unset too, nothing is in scope.
//! Residual 1 (accepted, Architect ruling Q7): the main-thread/subagent split
//! relies on the harness putting `agent_id` in every subagent payload; a
//! subagent shares the Architect's `claude` PID. This applies equally to
//! every #8878 exemption.
//! Test: `pm_guard_architect_envfile_tests.rs`; end to end in
//! `tests/tm_hook_pm_guard_architect_envfile_8939.rs`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::config::MpmConfig;

use crate::commands::env_file::is_key_name;
use crate::commands::env_file_grant;
use crate::commands::pm_guard_architect_reason::with_identity;
use crate::commands::pm_guard_bash::evaluate_credential_print_command;
use crate::commands::pm_guard_deny_log::{AUDIT_POST_TIMEOUT, DenyContext};
use crate::commands::pm_guard_floor::ArchitectGate;
use crate::commands::pm_guard_secret_env_files::evaluate_env_plist_read;
use crate::commands::pm_guard_secret_nested::evaluate_nested_secret_rules;
use crate::commands::pm_guard_secret_read::evaluate_secret_file_read_in;
use crate::commands::pm_guard_secret_script::evaluate_script_body_secret_read;
use crate::commands::pm_guard_trust_anchor::HookEnv;
use crate::commands::pm_guard_trust_anchor_paths::{Resolved, resolve};

/// The rule name on the audit line an exempted call writes.
pub(crate) const ENVFILE_RULE: &str = "architect-envfile";

/// Bytes a shape-matching command may contain: no quote, expansion, glob,
/// redirect, pipe, separator, `=` (so no `VAR=` prefix and no `KEY=value`),
/// tab or newline.
fn is_shape_byte(c: char) -> bool {
    c.is_ascii_alphanumeric() || " _./-@:+,".contains(c)
}

/// One exempted `tm env` call: what the audit line and the grant record.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct EnvfileCall {
    /// `set` or `keys`.
    pub(crate) verb: &'static str,
    /// The resolved env file.
    pub(crate) path: PathBuf,
    /// The key `set` writes; empty for `keys`. Never a value.
    pub(crate) keys: Vec<String>,
    /// The Keychain `(service, account)` `set` reads, by name; never a value.
    pub(crate) keychain: Option<(String, String)>,
}

/// A dotenv file inside the project scope, as [`envfile_policy`] resolved it.
///
/// Why: #8939 fix round — the verb must touch only the path the policy
/// checked, never its argv path. The field is private, so only the policy
/// makes one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ScopedEnvfile(PathBuf);

impl ScopedEnvfile {
    /// The canonical path the policy checked.
    pub(crate) fn path(&self) -> &Path {
        &self.0
    }
}

/// The one env-file policy the guard and `tm env` share (#8939 fix round).
///
/// Why: the guard and the verb each checked a different half; the verb
/// checked neither the dotenv name nor the scope.
/// What: [`place_envfile`] (dotenv name, regular file or absent when
/// `may_be_absent`, not a symlink, resolved) then [`in_scope`]. `None` means
/// refuse.
/// Test: `a_config_listed_root_is_in_scope_and_another_path_is_not`,
/// `an_architect_bound_set_on_a_non_env_file_never_reads_the_keychain`,
/// `an_out_of_scope_env_file_is_refused_by_the_verb`.
pub(crate) fn envfile_policy(
    word: &str,
    cwd: &Path,
    may_be_absent: bool,
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> Option<ScopedEnvfile> {
    let path = place_envfile(word, cwd, may_be_absent)?;
    in_scope(&path, env, config).then_some(ScopedEnvfile(path))
}

/// The secret-read decision for one tool call: `Some(reason)` denies.
///
/// Why: #8939 — one function for both deny sites, so the guarded and the
/// bypassed path cannot drift.
/// What: see the module doc. On an exemption the call is recorded on `gate`
/// for [`audit_envfile_allow`].
/// Test: `the_architect_main_thread_may_list_env_key_names`,
/// `the_architect_main_thread_may_set_an_env_key`,
/// `the_value_rules_run_before_the_envfile_exemption`,
/// `every_identity_failure_denies_the_envfile_exemption`.
pub(crate) fn evaluate_secret_file_read_gated(
    tool_name: &str,
    tool_input: Option<&Value>,
    hook_cwd: &Path,
    gate: &ArchitectGate<'_>,
) -> Option<String> {
    let command = (tool_name == "Bash").then(|| {
        tool_input
            .and_then(|v| v.get("command"))
            .and_then(Value::as_str)
            .unwrap_or_default()
    });
    // 1. The value-printing rules, unconditionally: the Architect included.
    if let Some(reason) = command
        .and_then(|c| {
            evaluate_credential_print_command(c).or_else(|| evaluate_nested_secret_rules(c))
        })
        .or_else(|| evaluate_env_plist_read(tool_name, tool_input, hook_cwd))
        // #8879: a script the command runs is judged by its body.
        .or_else(|| command.and_then(|c| evaluate_script_body_secret_read(c, hook_cwd)))
    {
        return Some(reason);
    }
    // 2. The #7266 file rule. Its entry point re-runs the Bash value rules,
    // which step 1 has already cleared, so only the file rule can answer.
    // #8093: the hook cwd lets the copy grant see its destination.
    let reason = evaluate_secret_file_read_in(tool_name, tool_input, Some(hook_cwd))?;
    // 3. The exemption: the shape first, then the identity.
    let shape = |c| envfile_shape(c, hook_cwd, gate.env(), || gate.config());
    let Some(call) = command.and_then(shape) else {
        return Some(reason);
    };
    match gate.identity() {
        Ok(()) => {
            gate.record_envfile(call);
            None
        }
        Err(why) => Some(with_identity(reason, why)),
    }
}

/// [`evaluate_secret_file_read_gated`] for `pm_guard`, auditing an exemption.
pub(crate) async fn gate_secret_file_read(
    ctx: &DenyContext<'_>,
    tool_name: &str,
    tool_input: Option<&Value>,
    hook_cwd: &Path,
    gate: &ArchitectGate<'_>,
) -> Option<String> {
    let deny = evaluate_secret_file_read_gated(tool_name, tool_input, hook_cwd, gate);
    if deny.is_none() {
        audit_envfile_allow(ctx, gate).await;
    }
    deny
}

/// Mint the `tm env` grant, then POST the `architect-envfile` allow line,
/// when `gate` recorded an exemption.
///
/// Why: Architect ruling Q6 — each exempted call leaves an audit line with the
/// path and key names only. Best effort, like every guard audit POST. The
/// grant (#8939 fix round) is what lets the verb run at all; a failed mint
/// leaves the verb refusing.
/// Test: `an_exempted_call_is_audited_with_the_path_and_key_names_only`,
/// `the_main_thread_grant_lets_the_verb_run_once`.
pub(crate) async fn audit_envfile_allow(ctx: &DenyContext<'_>, gate: &ArchitectGate<'_>) {
    mint_envfile_grant(gate);
    let Some(body) = allow_audit_body(ctx, gate) else {
        return;
    };
    let Ok(client) = reqwest::Client::builder()
        .connect_timeout(std::time::Duration::from_millis(500))
        .timeout(AUDIT_POST_TIMEOUT)
        .build()
    else {
        return;
    };
    let _ = client
        .post(format!("{}/hooks", ctx.url))
        .json(&body)
        .send()
        .await;
}

/// The audit body for the exemption `gate` recorded, or `None`.
pub(crate) fn allow_audit_body(ctx: &DenyContext<'_>, gate: &ArchitectGate<'_>) -> Option<Value> {
    let call = gate.envfile()?;
    let path = call.path.display().to_string();
    Some(serde_json::json!({
        "session_id": ctx.session_id,
        "event": "PreToolUse",
        "payload": {
            "cwd": ctx.cwd,
            "tool": ctx.tool_name,
            "pm_guard_decision": "allow",
            "pm_guard_rule": ENVFILE_RULE,
            "pm_guard_reason": format!("{ENVFILE_RULE}: `tm env {}` on {path}", call.verb),
            "path": path,
            "keys": call.keys,
        }
    }))
}

/// Write the one-shot `tm env` grant for the exemption `gate` recorded.
///
/// What: nothing when no exemption was recorded, the home is unknown, or the
/// call is a `set` with no `--from-keychain` (a grant would bind no value,
/// and `tm env` refuses that form); a write error is logged and leaves no
/// grant, so the verb refuses.
/// Test: `a_stdin_form_set_gets_no_grant_and_is_refused`.
pub(crate) fn mint_envfile_grant(gate: &ArchitectGate<'_>) {
    let (Some(call), Some(home)) = (gate.envfile(), gate.env().home.as_deref()) else {
        return;
    };
    // #8939 delta critic MEDIUM: only a Keychain-sourced `set` is granted.
    if call.verb == "set" && call.keychain.is_none() {
        return;
    }
    if let Err(e) = env_file_grant::mint(home, call) {
        tracing::warn!("tm env grant not written: {e}");
    }
}

/// The exempt shape of `command`, or `None`.
///
/// Why: #8939 design §2 with rulings Q1-Q4 — only `tm env keys <path>` and
/// `tm env set <path> <KEY> [--from-keychain <service> --account <account>]`,
/// as one bare-`tm` segment, on a dotenv file inside the project scope.
/// What: every byte passes [`is_shape_byte`]; the words are split on spaces
/// and must be exactly one of the two forms, with no word after `set`/`keys`
/// starting with `-` bar the two flags, each once. The path is placed against
/// `hook_cwd`, must be a regular file (or absent, for `set`) and not a
/// symlink, and must resolve, lexical and resolved names both in the dotenv
/// family, inside [`in_scope`]. `config` is the user config reader; it runs
/// only once the path is placed.
/// Test: `the_architect_main_thread_still_may_not_print_an_env_value`,
/// `a_config_listed_root_is_in_scope_and_another_path_is_not`.
pub(crate) fn envfile_shape(
    command: &str,
    hook_cwd: &Path,
    env: &HookEnv,
    config: impl FnOnce() -> MpmConfig,
) -> Option<EnvfileCall> {
    if command.is_empty() || !command.chars().all(is_shape_byte) {
        return None;
    }
    let words: Vec<&str> = command.split(' ').filter(|w| !w.is_empty()).collect();
    let (verb, path, keys, keychain) = match words.as_slice() {
        ["tm", "env", "keys", path] => ("keys", *path, Vec::new(), None),
        ["tm", "env", "set", path, key, flags @ ..] => {
            is_key_name(key).then_some(())?;
            (
                "set",
                *path,
                vec![(*key).to_owned()],
                keychain_flags(flags)?,
            )
        }
        _ => return None,
    };
    let file = envfile_policy(path, hook_cwd, verb == "set", env, config)?;
    Some(EnvfileCall {
        verb,
        path: file.0,
        keys,
        keychain,
    })
}

/// `Some(None)` for no flags; `Some(Some((service, account)))` for
/// `--from-keychain <service> --account <account>` in either order, with no
/// value starting with `-`; `None` for anything else.
fn keychain_flags(flags: &[&str]) -> Option<Option<(String, String)>> {
    match flags {
        [] => Some(None),
        [a, x, b, y] if a != b && !x.starts_with('-') && !y.starts_with('-') => match (*a, *b) {
            ("--from-keychain", "--account") => Some(Some(((*x).to_owned(), (*y).to_owned()))),
            ("--account", "--from-keychain") => Some(Some(((*y).to_owned(), (*x).to_owned()))),
            _ => None,
        },
        _ => None,
    }
}

/// Whether `name` is in the dotenv family: `.env`, or `.env.<anything>`.
/// Placeholder names (`.env.example`) never reach here: the file rule allows
/// them for every caller.
pub(crate) fn is_dotenv_name(name: &str) -> bool {
    name == ".env"
        || name
            .strip_prefix(".env.")
            .is_some_and(|rest| !rest.is_empty())
}

/// The resolved env file `word` names, or `None`.
///
/// What: `word` must not start with `-`; a relative word joins `hook_cwd`,
/// which must be absolute. The entry must be a regular file, not a symlink;
/// `may_be_absent` also accepts "not found". Both the lexical and the resolved
/// basename must be [`is_dotenv_name`].
fn place_envfile(word: &str, hook_cwd: &Path, may_be_absent: bool) -> Option<PathBuf> {
    if word.starts_with('-') {
        return None;
    }
    let placed = if Path::new(word).is_absolute() {
        PathBuf::from(word)
    } else if hook_cwd.is_absolute() {
        hook_cwd.join(word)
    } else {
        return None;
    };
    let dotenv = |p: &Path| {
        p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(is_dotenv_name)
    };
    if !dotenv(&placed) {
        return None;
    }
    match std::fs::symlink_metadata(&placed) {
        Ok(meta) if meta.file_type().is_file() => {}
        Err(e) if may_be_absent && e.kind() == std::io::ErrorKind::NotFound => {}
        _ => return None,
    }
    match resolve(&placed) {
        Resolved::Path(resolved) if dotenv(&resolved) => Some(resolved),
        _ => None,
    }
}

/// Whether the resolved `path` lies strictly inside `CLAUDE_PROJECT_DIR` or a
/// `[supervisor] projects` root (Architect ruling Q2 and its addendum).
///
/// What: the listed roots come from `config()` — the user config, never
/// `project-paths.json` (see the module doc); an unreadable or malformed
/// config loads as the default, which lists none. A relative listed entry is
/// skipped, as `session_profile::path_is_listed` skips it. Each root is
/// canonicalized; a root that does not resolve, is `/`, or is the home
/// directory or one of its ancestors is skipped.
/// Test: `a_config_listed_root_is_in_scope_and_another_path_is_not`,
/// `a_project_paths_json_entry_grants_no_scope`,
/// `an_unreadable_config_leaves_only_the_project_dir_in_scope`.
fn in_scope(path: &Path, env: &HookEnv, config: impl FnOnce() -> MpmConfig) -> bool {
    let home = env
        .home
        .as_deref()
        .and_then(|h| std::fs::canonicalize(h).ok());
    let project = env.project_dir.as_ref().map(PathBuf::from);
    // #8939 Q2 addendum: the trust anchor is the only source of listed roots.
    let listed = config().supervisor.projects.into_iter();
    let listed = listed.filter(|root| root.is_absolute());
    project.into_iter().chain(listed).any(|root| {
        let Ok(root) = std::fs::canonicalize(&root) else {
            return false;
        };
        let degenerate =
            root.parent().is_none() || home.as_deref().is_some_and(|h| h.starts_with(&root));
        !degenerate && path != root && path.starts_with(&root)
    })
}

#[cfg(test)]
#[path = "pm_guard_architect_envfile_tests.rs"]
mod tests;
