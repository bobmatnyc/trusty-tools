//! `tm hook --pm-guard-shadow`: the minimal pm-guard, in shadow mode (#11431).
//!
//! Why: owner ruling Dwz (option A of the 2026-11-17 brief) replaces the
//! text-matching guard with four fact-based rules, and runs them log-only
//! first so their would-refuse set can be checked against the #8021 record.
//! Owner ruling D0u (#10690) takes rule 1, the trust-anchor rule, out of
//! that: it enforces in every mode, shadow included, and its denies are
//! recorded.
//! What: [`pm_guard_shadow`] reads one `PreToolUse` payload and [`decide`]s
//! it. A rule-1 hit, or any failure to reach a rule-1 answer (unreadable or
//! oversized stdin, non-JSON, no tool name, no home dir, a panic), is a
//! refusal: one JSON line is appended to `~/.trusty-mpm/guard/shadow.jsonl`
//! and the prefixed `permissionDecision: "deny"` is printed. Rules 2, 3 and 5
//! ([`rules::observed`]; none for the Architect) only append the line and
//! allow; a failure there allows. The old `--pm-guard` is untouched.
//! Test: `shadow_tests.rs`, `enforce_tests.rs`, `replay_tests.rs`,
//! `tests/tm_hook_pm_guard_shadow_11431.rs`.

mod facts;
mod paths;
mod rules;
mod rules_git;

use std::io::{Read, Write as _};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use trusty_mpm::core::config::MpmConfig;
use trusty_mpm::core::guard_policy;
use trusty_mpm::core::session_profile;

use crate::commands::pm_guard_response::build_pm_guard_deny_response;
use crate::commands::pm_guard_trust_anchor::TRUST_ANCHOR_RULE;

pub(crate) use rules::{GuardEnv, Hit};

/// Largest Bash payload the parser runs on; a larger one is refused unjudged.
const MAX_PAYLOAD: u64 = 1 << 20;
/// Largest payload read at all (a large `Write` body); a larger one is refused.
const MAX_READ: u64 = 64 << 20;
/// The would-refuse log, under the home dir.
const LOG_PATH: &str = ".trusty-mpm/guard/shadow.jsonl";
/// The log size that triggers its one rotation.
const MAX_LOG_BYTES: u64 = 10 << 20;
/// The per-session rule-5 allow list, under the home dir.
const ALLOW_PATH: &str = ".trusty-mpm/guard/shadow.toml";

/// What the hook does with one call (#10690).
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum Outcome {
    /// No rule fired: exit 0, print nothing.
    Allow,
    /// An observe-only rule (2, 3, 5) fired: record the line, then allow.
    Record(Value),
    /// Rule 1 fired, or its decision failed: record the line, print a deny.
    Refuse(Value),
}

/// The hook entry point; always exits 0, prints a deny only on a refusal.
///
/// Why: rules 2, 3 and 5 are still in shadow; rule 1 enforces (#10690, D0u).
/// What: silences the panic message, reads stdin (bounded), runs [`decide`]
/// under `catch_unwind` — a panic is a refusal — then records the line and,
/// on [`Outcome::Refuse`], prints the `tm pm-guard:` deny for policy
/// `trust-anchor-write`. A failed log write never changes the decision.
/// Test: `an_r1_write_is_refused_and_recorded`, `an_r1_write_is_refused_and_a_malformed_call_too`.
pub(crate) fn pm_guard_shadow() {
    std::panic::set_hook(Box::new(|_| {}));
    let mut raw = Vec::new();
    let read = std::io::stdin()
        .take(MAX_READ + 1)
        .read_to_end(&mut raw)
        .is_ok();
    let home = dirs::home_dir();
    let outcome = catch_unwind(AssertUnwindSafe(|| {
        decide(read.then_some(&raw[..]), home.as_deref(), &live_env)
    }))
    .unwrap_or_else(|_| Outcome::Refuse(error_line("panic", &raw)));
    let (line, refuse) = match outcome {
        Outcome::Allow => return,
        Outcome::Record(line) => (line, false),
        Outcome::Refuse(line) => (line, true),
    };
    // #10690: the deny goes out before the log write, so a log path that
    // stalls or fails can never turn a refusal into a timeout-allow.
    if refuse {
        let deny = build_pm_guard_deny_response(
            guard_policy::for_rule(TRUST_ANCHOR_RULE),
            &refusal_reason(&line),
        );
        let mut out = std::io::stdout();
        let _ = writeln!(out, "{deny}");
        let _ = out.flush();
    }
    if let Some(home) = &home {
        guarded(|| append_log(home, &line));
    }
}

/// Run `f`, swallowing any panic it raises.
pub(crate) fn guarded(f: impl FnOnce()) {
    let _ = catch_unwind(AssertUnwindSafe(f));
}

/// The deny text for a refusal line: the anchor named, or the failure kind.
fn refusal_reason(line: &Value) -> String {
    match line["fact"]["target"].as_str() {
        Some(target) => format!(
            "rule 1 (trust anchor) refuses this write to `{target}`; a trust anchor decides \
             whether the guard runs. Ask the operator to make this change"
        ),
        None => format!(
            "rule 1 (trust anchor) could not judge this call ({}), so it is refused \
             (fail-closed)",
            line["fact"]["error"].as_str().unwrap_or("unknown")
        ),
    }
}

/// The refusal line for a call rule 1 could not judge; never the payload text.
fn error_line(kind: &str, raw: &[u8]) -> Value {
    json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "session_id": "",
        "tool": "",
        "rule": "R1",
        "fact": { "error": kind },
        "sha256": hex::encode(Sha256::digest(raw)),
        "unparsed": 0,
        "profile": "unknown",
        "enforced": true,
    })
}

/// The [`Outcome`] for one raw payload.
///
/// Why: kept free of process state so the fail-closed and replay tests drive
/// it. #10690: every arm that cannot reach a rule-1 answer refuses.
/// What: `raw = None` (stdin unreadable), no `home`, a payload over
/// [`MAX_READ`], non-JSON, no string `tool_name`, a Bash payload over
/// [`MAX_PAYLOAD`], or a panic in `make_env` or rule 1 → [`Outcome::Refuse`]
/// with `fact.error`. A rule-1 [`Hit`] → [`Outcome::Refuse`]. Otherwise an
/// observe-only hit → [`Outcome::Record`]; none, or a panic there → allow.
/// A line is `{ts, session_id, tool, rule, fact, sha256, unparsed, profile,
/// enforced}`; the command text itself is never logged, only its SHA-256.
/// Test: `an_r1_write_is_refused_and_recorded`,
/// `every_rule1_error_arm_refuses`, `no_stdin_or_no_home_refuses`,
/// `observe_only_rules_record_and_allow`.
pub(crate) fn decide(
    raw: Option<&[u8]>,
    home: Option<&Path>,
    make_env: &dyn Fn(&Value, &Path) -> GuardEnv,
) -> Outcome {
    let refuse = |kind: &str| Outcome::Refuse(error_line(kind, raw.unwrap_or_default()));
    let Some(raw) = raw else {
        return refuse("stdin_unreadable");
    };
    let Some(home) = home else {
        return refuse("no_home_dir");
    };
    if raw.len() as u64 > MAX_READ {
        return refuse("oversized");
    }
    let Ok(payload) = serde_json::from_slice::<Value>(raw) else {
        return refuse("unparseable");
    };
    let Some(tool) = payload["tool_name"].as_str() else {
        return refuse("no_tool_name");
    };
    if tool == "Bash" && raw.len() as u64 > MAX_PAYLOAD {
        return refuse("oversized");
    }
    let input = &payload["tool_input"];
    let decided = catch_unwind(AssertUnwindSafe(|| {
        let env = make_env(&payload, home);
        rules::r1_decide(tool, input, &env).map(|(r1, facts)| (env, r1, facts))
    }));
    let (env, r1, facts) = match decided {
        Ok(Ok(decided)) => decided,
        Ok(Err(kind)) => return refuse(kind),
        Err(_) => return refuse("panic"),
    };
    let line = |hit: Hit, unparsed: usize, enforced: bool| {
        log_line(&payload, tool, input, &env, hit, unparsed, enforced)
    };
    let unparsed = facts.as_ref().map_or(0, |f| f.unparsed);
    if let Some(hit) = r1 {
        return Outcome::Refuse(line(hit, unparsed, true));
    }
    let observed = facts.and_then(|f| {
        catch_unwind(AssertUnwindSafe(|| rules::observed(&f, &env)))
            .ok()
            .flatten()
    });
    match observed {
        Some(hit) => Outcome::Record(line(hit, unparsed, false)),
        None => Outcome::Allow,
    }
}

/// The recorded line of a call [`decide`] judged, or `None` when it allowed.
///
/// Test: `a_would_refuse_logs_one_line_without_the_command_text`.
#[cfg(test)]
pub(crate) fn judge(
    raw: &[u8],
    home: &Path,
    make_env: &dyn Fn(&Value, &Path) -> GuardEnv,
) -> Option<Value> {
    match decide(Some(raw), Some(home), make_env) {
        Outcome::Allow => None,
        Outcome::Record(line) | Outcome::Refuse(line) => Some(line),
    }
}

/// One log line for a [`Hit`].
fn log_line(
    payload: &Value,
    tool: &str,
    input: &Value,
    env: &GuardEnv,
    hit: Hit,
    unparsed: usize,
    enforced: bool,
) -> Value {
    let subject = input["command"]
        .as_str()
        .or_else(|| input["file_path"].as_str())
        .or_else(|| input["notebook_path"].as_str())
        .unwrap_or("");
    json!({
        "ts": chrono::Utc::now().to_rfc3339(),
        "session_id": payload["session_id"].as_str().unwrap_or(""),
        "tool": tool,
        "rule": hit.rule,
        "fact": hit.fact,
        "sha256": hex::encode(Sha256::digest(subject.as_bytes())),
        "unparsed": unparsed,
        "profile": if env.architect { "architect" } else { "pm" },
        // #10690: rule 1 refuses; the other rules only record.
        "enforced": enforced,
    })
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
/// and `O_NONBLOCK` and writes only to a regular file, so a planted symlink
/// is never followed and a planted FIFO never blocks (#10690). Every error
/// is dropped.
/// Test: `the_log_is_private_and_rotates_at_the_cap`, `an_unwritable_log_is_dropped`,
/// `a_symlink_at_the_log_path_is_not_followed`, `a_fifo_at_the_log_path_never_blocks`.
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
    match std::fs::symlink_metadata(&path) {
        // #10690: only a regular file (or none yet) is ever opened.
        Ok(m) if !m.is_file() => return,
        Ok(m) if m.len() > MAX_LOG_BYTES => {
            let _ = std::fs::rename(&path, path.with_extension("jsonl.1"));
        }
        _ => {}
    }
    let file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .mode(0o600)
        // #11431: a symlink planted at the log path is never followed.
        // #10690: a FIFO swapped in after the check fails the open, never blocks.
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
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

#[cfg(test)]
#[path = "enforce_tests.rs"]
mod enforce_tests;
