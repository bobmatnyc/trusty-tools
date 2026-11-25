//! `tm hook --pm-guard-shadow`: the minimal pm-guard, in shadow mode (#11431).
//!
//! Why: owner ruling Dwz (option A of the 2026-11-17 brief) replaces the
//! text-matching guard with four fact-based rules, and runs them log-only
//! first so their would-refuse set can be checked against the #8021 record.
//! What: [`pm_guard_shadow`] reads one `PreToolUse` payload, evaluates
//! rules 1, 2, 3 and 5 ([`rules::evaluate`]; rule 1 only for the Architect)
//! and appends one JSON line per would-refuse to
//! `~/.trusty-mpm/guard/shadow.jsonl`. It never prints a decision and never
//! fails: a parse error, a panic, an oversized payload or an unwritable log
//! all end in exit 0 with no output. The old `--pm-guard` is untouched.
//! Test: `shadow_tests.rs`, `replay_tests.rs`, `tests/tm_hook_pm_guard_shadow_11431.rs`.

mod facts;
mod paths;
mod rules;
mod rules_git;

use std::io::Read;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::session_profile;

pub(crate) use rules::{GuardEnv, Hit};

/// Largest payload read; a larger one is skipped unjudged (bounded work).
const MAX_PAYLOAD: u64 = 1 << 20;
/// The would-refuse log, under the home dir.
const LOG_PATH: &str = ".trusty-mpm/guard/shadow.jsonl";
/// The log size that triggers its one rotation.
const MAX_LOG_BYTES: u64 = 10 << 20;
/// The per-session rule-5 allow list, under the home dir.
const ALLOW_PATH: &str = ".trusty-mpm/guard/shadow.toml";

/// The hook entry point; always returns, never prints, never blocks.
///
/// Why: a shadow guard that can block is not a shadow guard.
/// What: silences the panic message, then runs [`judge`] inside
/// `catch_unwind`; whatever happens the process exits 0 with empty stdout.
/// Test: `a_panicking_judge_is_caught`, `malformed_huge_and_unwritable_inputs_exit_zero`.
pub(crate) fn pm_guard_shadow() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut raw = Vec::new();
    if std::io::stdin()
        .take(MAX_PAYLOAD + 1)
        .read_to_end(&mut raw)
        .is_err()
    {
        return;
    }
    guarded(|| {
        let Some(home) = dirs::home_dir() else { return };
        if let Some(line) = judge(&raw, &home, &live_env) {
            append_log(&home, &line);
        }
    });
}

/// Run `f`, swallowing any panic it raises.
pub(crate) fn guarded(f: impl FnOnce()) {
    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(f));
}

/// The log line for one raw payload, or `None` (no would-refuse, or unreadable).
///
/// Why: kept free of process state so the replay and fail-safe tests drive it.
/// What: an oversized or non-JSON payload → `None`. Otherwise `make_env`
/// builds the [`GuardEnv`] from the payload and the first [`Hit`] becomes
/// `{ts, session_id, tool, rule, fact, sha256, unparsed, profile}`. The
/// command text itself is never logged, only its SHA-256.
/// Test: `a_would_refuse_logs_one_line_without_the_command_text`.
pub(crate) fn judge(
    raw: &[u8],
    home: &Path,
    make_env: &dyn Fn(&Value, &Path) -> GuardEnv,
) -> Option<Value> {
    if raw.len() as u64 > MAX_PAYLOAD {
        return None;
    }
    let payload: Value = serde_json::from_slice(raw).ok()?;
    let tool = payload["tool_name"].as_str()?;
    let input = &payload["tool_input"];
    let env = make_env(&payload, home);
    let (hit, unparsed) = rules::evaluate(tool, input, &env);
    let Hit { rule, fact } = hit?;
    let subject = input["command"]
        .as_str()
        .or_else(|| input["file_path"].as_str())
        .or_else(|| input["notebook_path"].as_str())
        .unwrap_or("");
    Some(json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "session_id": payload["session_id"].as_str().unwrap_or(""),
        "tool": tool,
        "rule": rule,
        "fact": fact,
        "sha256": hex::encode(Sha256::digest(subject.as_bytes())),
        "unparsed": unparsed,
        "profile": if env.architect { "architect" } else { "pm" },
    }))
}

/// The [`GuardEnv`] of a live hook call.
///
/// What: the payload `cwd` (else the process cwd), `CLAUDE_PROJECT_DIR`,
/// the profile from [`session_profile::hook_profile`] and this session's
/// rule-5 allow list from `~/.trusty-mpm/guard/shadow.toml`
/// (`[r5_allow] "<session_id>" = ["<argv prefix>", ...]`, empty by default).
fn live_env(payload: &Value, home: &Path) -> GuardEnv {
    let cwd = payload["cwd"]
        .as_str()
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("/"));
    let profile = session_profile::hook_profile(
        std::env::var_os(session_profile::SESSION_PROFILE_ENV),
        std::env::var_os(session_profile::PROJECT_DIR_ENV),
        MpmConfig::load_default,
    );
    let session = payload["session_id"].as_str().unwrap_or("");
    GuardEnv {
        home: home.to_path_buf(),
        project_dir: session_profile::hook_project_dir(std::env::var_os(
            session_profile::PROJECT_DIR_ENV,
        )),
        cwd,
        r5_allow: r5_allow(&home.join(ALLOW_PATH), session),
        architect: profile.is_architect(),
    }
}

/// This session's rule-5 allow list; an absent or unreadable file is empty.
pub(crate) fn r5_allow(path: &Path, session: &str) -> Vec<String> {
    std::fs::read_to_string(path)
        .ok()
        .and_then(|text| toml::from_str::<toml::Value>(&text).ok())
        .and_then(|v| v.get("r5_allow")?.get(session)?.as_array().cloned())
        .map(|a| {
            a.iter()
                .filter_map(|e| e.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// Append `line` to the log; best effort, a failure is dropped.
///
/// Why: the log names paths and repos a session touched, so only the user
/// may read it, and a runaway session must not fill the disk (#11431 review).
/// What: the directory is created 0700 and the file 0600 (an existing file is
/// re-tightened to 0600). Past [`MAX_LOG_BYTES`] the file rotates once to
/// `shadow.jsonl.1`, replacing an older `.1`. The open uses `O_NOFOLLOW`
/// and writes only to a regular file, so a planted symlink is never
/// followed. Every error is dropped.
/// Test: `the_log_is_private_and_rotates_at_the_cap`, `an_unwritable_log_is_dropped`,
/// `a_symlink_at_the_log_path_is_not_followed`.
pub(crate) fn append_log(home: &Path, line: &Value) {
    use std::io::Write;
    use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
    let path = home.join(LOG_PATH);
    let Some(dir) = path.parent() else { return };
    if std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .is_err()
    {
        return;
    }
    if std::fs::symlink_metadata(&path).is_ok_and(|m| m.is_file() && m.len() > MAX_LOG_BYTES) {
        let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        // #11431: a symlink planted at the log path is never followed.
        .custom_flags(libc::O_NOFOLLOW)
        .open(&path);
    if let Ok(mut f) = file
        && f.metadata().is_ok_and(|m| m.is_file())
    {
        let _ = f.set_permissions(std::fs::Permissions::from_mode(0o600));
        let _ = writeln!(f, "{line}");
    }
}

#[cfg(test)]
#[path = "shadow_tests.rs"]
mod shadow_tests;

#[cfg(test)]
#[path = "replay_tests.rs"]
mod replay_tests;
