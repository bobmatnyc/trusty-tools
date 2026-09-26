//! `tm hook --pm-guard` — wrap heavy builds in `tm build-lease` (#8261).
//!
//! Why: option D moves the machine-wide builder cap from the dispatch to the
//! build command. The rule is evaluated in `pm_guard` BEFORE the subagent
//! exemptions (Guards 1 and 4), the same placement as the #3977 absolute
//! guard, because dispatched agents run nearly every build on this machine and
//! a rule after those exemptions would never see them.
//!
//! Two `PreToolUse` hooks may rewrite one Bash call: this one and `tm hook`'s
//! output-compression rewrite (#1956). Claude Code runs them in parallel, so
//! for a heavy build BOTH emit [`decide_rewrite`]'s single answer —
//! compression first, then the lease inserted inside it — and whichever
//! response is applied, the command is the same.
//!
//! **Permission rules.** Claude Code matches its `permissions` rules against
//! the tool input it will run, which after a rewrite starts with the lease
//! program, so a rule written for the original would no longer match. The
//! hook therefore decides against the ORIGINAL command, from the managed, user
//! and project settings (#8261 round 3):
//! - a `deny` match: no rewrite — Claude Code denies the original as before;
//! - an `ask` match: the rewrite, with `permissionDecision: "ask"`, so the user
//!   is still asked and the build, once approved, is leased;
//! - every segment matching an `allow` rule and none a `deny`/`ask`: the
//!   rewrite with `permissionDecision: "allow"`, as the original was allowed;
//! - otherwise: the rewrite with no decision, so the normal flow applies.
//!
//! `tm build-lease` itself refuses any command the heavy-build classifier does
//! not match, so the lease program never needs an allow rule of its own.
//!
//! What: [`evaluate`] returns a rendered rewrite response, a refusal, or
//! nothing; [`emit_allow`] prints the rewrite at an ALLOW exit, merging any
//! pending agent-cost notice into the one output object.
//! Test: `tests/tm_hook_pm_guard_build_lease.rs`; the rewrite rules in
//! `pm_guard_bash::build_lease_rewrite`; this module's suite.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::build_lease::config::BuildLeaseConfig;

use super::hook_rewrite::rewrite_bash_command_unless_isolated;
use super::pm_guard_bash::build_lease_rewrite::{LeaseRewrite, rewrite_for_lease};
use super::pm_guard_bash::split_shell_segments;
use super::pm_guard_response::{RewriteDecision, build_rewrite_response};

/// What the lease rule decided for one tool call.
///
/// Test: `tests/tm_hook_pm_guard_build_lease.rs`.
pub(crate) enum LeaseVerdict {
    /// Not a heavy build.
    None,
    /// Emit this rendered `updatedInput` response at the ALLOW exit.
    Rewrite(String),
    /// Deny with this reason.
    Deny(String),
}

/// The Bash tool's timeout when a call names none, in milliseconds.
const BASH_DEFAULT_TIMEOUT_MS: u64 = 120_000;

/// The program word the rewrite inserts: this binary's own path, quoted.
///
/// Why: the hook binary is the one version guaranteed to know `build-lease`;
/// a `tm` found on the agent's `PATH` may be older, or absent.
fn tm_program_word() -> String {
    std::env::current_exe()
        .ok()
        .and_then(|p| p.to_str().map(str::to_string))
        .and_then(|p| shlex::try_quote(&p).ok().map(|q| q.into_owned()))
        .unwrap_or_else(|| "tm".to_string())
}

/// The lease wait for this call, when it must be shorter than the config's.
///
/// What: half a foreground call's own `timeout` (default 120 s), so a build
/// admitted at the end of the wait still has at least half the call to run —
/// round 2 left it 15 s (#8261 round 3). `None` when that is not shorter than
/// `config_wait`, or the call runs in the background (no timeout applies).
/// Test: `the_wait_fits_inside_the_calls_timeout`.
fn wait_for_call(tool_input: Option<&Value>, config_wait: u64) -> Option<u64> {
    if tool_input
        .and_then(|v| v.get("run_in_background"))
        .and_then(Value::as_bool)
        == Some(true)
    {
        return None;
    }
    let timeout_ms = tool_input
        .and_then(|v| v.get("timeout"))
        .and_then(Value::as_u64)
        .unwrap_or(BASH_DEFAULT_TIMEOUT_MS);
    let fit = (timeout_ms / 1000 / 2).max(1);
    (fit < config_wait).then_some(fit)
}

/// The command both `PreToolUse` hooks emit for a Bash call, if it is leased.
///
/// Why: see the module doc — the two hooks must agree byte for byte.
/// What: [`LeaseRewrite::None`] when the original matches a `deny` Bash
/// permission rule; otherwise [`rewrite_bash_command_unless_isolated`] when it
/// applies (never inside an isolation worktree, #7477), then
/// [`rewrite_for_lease`] over the result, paired with the permission decision
/// the module doc describes.
/// Test: `a_compressible_heavy_build_is_compressed_and_leased`,
/// `an_isolation_worktree_build_is_leased_uncompressed`,
/// `a_deny_rule_on_the_original_skips_the_rewrite`,
/// `an_ask_rule_keeps_the_lease_and_asks`.
pub(crate) fn decide_rewrite(
    command: &str,
    tool_input: Option<&Value>,
    cwd: &Path,
) -> (LeaseRewrite, Option<RewriteDecision>) {
    let lease = BuildLeaseConfig::load_default();
    let heavy = lease.effective_heavy_build_commands();
    let rules = permission_rules(cwd);
    if matches_any(command, &rules.deny) {
        return (LeaseRewrite::None, None);
    }
    // #8261 round 3 (critic finding 3): an ask rule keeps the lease and asks.
    let permission = if matches_any(command, &rules.ask) {
        Some(RewriteDecision::Ask)
    } else if matches_every_segment(command, &rules.allow) {
        Some(RewriteDecision::Allow)
    } else {
        None
    };
    let mut prefix = format!("{} build-lease", tm_program_word());
    if let Some(wait) = wait_for_call(tool_input, lease.effective_lease_wait().as_secs()) {
        prefix.push_str(&format!(" --wait-secs {wait}"));
    }
    prefix.push_str(" --");
    // #7477: no compression wrap inside an isolation worktree; the lease stays.
    let compressed = rewrite_bash_command_unless_isolated(command, Some(cwd));
    (
        rewrite_for_lease(compressed.as_deref().unwrap_or(command), &heavy, &prefix),
        permission,
    )
}

/// The `updatedInput` response carrying `tool_input` with `command` replaced.
///
/// Why: `updatedInput` replaces the tool's arguments, so every other field —
/// `run_in_background`, `timeout`, `description` — is carried over.
/// What: `permission` becomes the response's permission decision.
/// Test: `a_background_build_keeps_run_in_background`,
/// `an_ask_rule_keeps_the_lease_and_asks`.
pub(crate) fn rewrite_response(
    tool_input: Option<&Value>,
    command: &str,
    permission: Option<RewriteDecision>,
) -> String {
    let mut input = tool_input
        .and_then(Value::as_object)
        .cloned()
        .unwrap_or_default();
    input.insert("command".to_string(), Value::String(command.to_string()));
    build_rewrite_response(Value::Object(input), permission).to_string()
}

/// Decide the lease rule for one `PreToolUse` call.
///
/// Test: `tests/tm_hook_pm_guard_build_lease.rs`.
pub(crate) fn evaluate(tool_name: &str, tool_input: Option<&Value>, cwd: &Path) -> LeaseVerdict {
    if tool_name != "Bash" {
        return LeaseVerdict::None;
    }
    let Some(command) = tool_input
        .and_then(|v| v.get("command"))
        .and_then(Value::as_str)
    else {
        return LeaseVerdict::None;
    };
    match decide_rewrite(command, tool_input, cwd) {
        (LeaseRewrite::Rewrite(new), permission) => {
            LeaseVerdict::Rewrite(rewrite_response(tool_input, &new, permission))
        }
        (LeaseRewrite::Refuse(reason), _) => LeaseVerdict::Deny(reason),
        (LeaseRewrite::None, _) => LeaseVerdict::None,
    }
}

/// Print an ALLOW exit's single output object.
///
/// What: no rewrite → the agent-cost notice alone, as before. A rewrite → the
/// rewrite, with the cost notice merged in as `additionalContext` when this
/// agent has not been told yet.
pub(crate) fn emit_allow(payload: &Value, cost_notice: Option<String>, rewrite: Option<String>) {
    let Some(rewrite) = rewrite else {
        super::pm_guard::emit_cost_notice(payload, cost_notice);
        return;
    };
    let context = cost_notice.filter(|_| super::pm_guard_cost::claim_warn_notice(payload));
    println!(
        "{}",
        super::pm_guard_worktree_grant::with_additional_context(&rewrite, context.as_deref())
    );
}

/// The Bash permission patterns, by kind.
#[derive(Default)]
struct Rules {
    deny: Vec<String>,
    ask: Vec<String>,
    allow: Vec<String>,
}

/// Every `deny`, `ask` and `allow` Bash pattern in the settings for `cwd`.
///
/// What: managed policy settings, the user settings (`$CLAUDE_CONFIG_DIR` or
/// `~/.claude`), and every ancestor's `.claude/settings.json` and
/// `.claude/settings.local.json`. Unreadable files contribute nothing.
fn permission_rules(cwd: &Path) -> Rules {
    let mut files: Vec<PathBuf> = vec![
        PathBuf::from("/Library/Application Support/ClaudeCode/managed-settings.json"),
        PathBuf::from("/etc/claude-code/managed-settings.json"),
    ];
    let user_dir = std::env::var_os("CLAUDE_CONFIG_DIR")
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".claude")));
    if let Some(dir) = user_dir {
        files.push(dir.join("settings.json"));
    }
    for dir in cwd.ancestors() {
        files.push(dir.join(".claude/settings.json"));
        files.push(dir.join(".claude/settings.local.json"));
    }
    let docs: Vec<Value> = files
        .iter()
        .filter_map(|f| std::fs::read_to_string(f).ok())
        .filter_map(|raw| serde_json::from_str::<Value>(&raw).ok())
        .collect();
    let of_kind = |kind: &str| -> Vec<String> {
        docs.iter()
            .filter_map(|doc| doc["permissions"][kind].as_array())
            .flatten()
            .filter_map(|rule| {
                rule.as_str()
                    .and_then(|r| r.strip_prefix("Bash("))
                    .and_then(|r| r.strip_suffix(')'))
                    .map(str::to_string)
            })
            .collect()
    };
    Rules {
        deny: of_kind("deny"),
        ask: of_kind("ask"),
        allow: of_kind("allow"),
    }
}

/// Whether the whole command, or any one segment of it, matches a pattern.
///
/// What: `prefix:*` matches a segment equal to `prefix` or starting with
/// `prefix ` (Claude Code's prefix rule); a pattern with `*` matches as a
/// glob; anything else matches exactly. Each segment is also tried with its
/// leading `KEY=value` assignments removed.
/// Test: `a_deny_rule_on_the_original_skips_the_rewrite`.
fn matches_any(command: &str, patterns: &[String]) -> bool {
    !patterns.is_empty()
        && (matches_one(command.trim(), patterns)
            || split_shell_segments(command)
                .iter()
                .any(|seg| segment_matches(seg, patterns)))
}

/// Whether EVERY segment of `command` matches an allow pattern — Claude Code
/// auto-approves a compound command only when each part is allowed.
/// Test: `an_ask_rule_keeps_the_lease_and_asks`.
fn matches_every_segment(command: &str, patterns: &[String]) -> bool {
    let segments = split_shell_segments(command);
    !patterns.is_empty()
        && !segments.is_empty()
        && segments.iter().all(|seg| segment_matches(seg, patterns))
}

/// One segment, as written or with its `KEY=value` prefix removed.
fn segment_matches(seg: &str, patterns: &[String]) -> bool {
    let seg = seg.trim();
    let bare = seg
        .split_whitespace()
        .skip_while(|w| super::hook_rewrite::is_env_assignment(w))
        .collect::<Vec<_>>()
        .join(" ");
    matches_one(seg, patterns) || matches_one(&bare, patterns)
}

fn matches_one(candidate: &str, patterns: &[String]) -> bool {
    patterns.iter().any(|p| match p.strip_suffix(":*") {
        Some(prefix) => candidate == prefix || candidate.starts_with(&format!("{prefix} ")),
        None if p.contains('*') => glob_match(p, candidate),
        None => candidate == p,
    })
}

/// `*`-only glob match.
fn glob_match(pattern: &str, text: &str) -> bool {
    let parts: Vec<&str> = pattern.split('*').collect();
    let mut rest = text;
    for (i, part) in parts.iter().enumerate() {
        if i == 0 {
            let Some(after) = rest.strip_prefix(part) else {
                return false;
            };
            rest = after;
        } else if i == parts.len() - 1 {
            return rest.ends_with(part);
        } else if let Some(pos) = rest.find(part) {
            rest = &rest[pos + part.len()..];
        } else {
            return false;
        }
    }
    rest.is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cwd() -> tempfile::TempDir {
        tempfile::tempdir().expect("tempdir")
    }

    /// A background call: no Bash timeout, so no `--wait-secs` is inserted.
    fn bg() -> Option<&'static Value> {
        static INPUT: std::sync::OnceLock<Value> = std::sync::OnceLock::new();
        Some(INPUT.get_or_init(|| serde_json::json!({ "run_in_background": true })))
    }

    #[test]
    fn a_compressible_heavy_build_is_compressed_and_leased() {
        let dir = cwd();
        let (LeaseRewrite::Rewrite(out), _) = decide_rewrite("cargo test -p x", bg(), dir.path())
        else {
            panic!("a heavy build is rewritten");
        };
        assert!(out.starts_with("{ "), "compression wraps first: {out}");
        assert!(out.contains(" build-lease -- cargo test -p x;"), "{out}");
        assert!(
            out.ends_with("| tm compress --tool \"cargo test\""),
            "{out}"
        );
    }

    /// #7477 x #8261: an isolation worktree gets the lease but no compression
    /// wrap, whose brace-group shape the harness classifier refuses there.
    #[test]
    fn an_isolation_worktree_build_is_leased_uncompressed() {
        let cwd = Path::new("/repo/.claude/worktrees/agent-a/crates/x");
        let (LeaseRewrite::Rewrite(out), _) = decide_rewrite("cargo test -p x", bg(), cwd) else {
            panic!("a heavy build is leased in an isolation worktree too");
        };
        assert!(out.ends_with(" build-lease -- cargo test -p x"), "{out}");
        assert!(!out.contains("tm compress"), "{out}");
        assert!(!out.starts_with("{ "), "{out}");
    }

    #[test]
    fn a_background_build_keeps_run_in_background() {
        let input = serde_json::json!({
            "command": "cargo test",
            "run_in_background": true,
            "description": "run tests",
        });
        let rendered = rewrite_response(Some(&input), "tm build-lease -- cargo test", None);
        let parsed: Value = serde_json::from_str(&rendered).expect("json");
        let updated = &parsed["hookSpecificOutput"]["updatedInput"];
        assert_eq!(updated["command"], "tm build-lease -- cargo test");
        assert_eq!(updated["run_in_background"], true);
        assert_eq!(updated["description"], "run tests");
    }

    /// Critic round 1 (MEDIUM): the wait fits inside the call's own timeout.
    #[test]
    fn the_wait_fits_inside_the_calls_timeout() {
        let short = serde_json::json!({ "command": "cargo test", "timeout": 60_000 });
        assert_eq!(wait_for_call(Some(&short), 90), Some(30));
        assert_eq!(
            wait_for_call(None, 90),
            Some(60),
            "the default 120 s: the wait takes half, the build keeps half"
        );
        let long = serde_json::json!({ "command": "cargo test", "timeout": 600_000 });
        assert_eq!(wait_for_call(Some(&long), 90), None, "300 s > 90");
        let tiny = serde_json::json!({ "command": "cargo test", "timeout": 1_000 });
        assert_eq!(wait_for_call(Some(&tiny), 90), Some(1));
        let bg = serde_json::json!({ "command": "cargo test", "timeout": 5_000, "run_in_background": true });
        assert_eq!(wait_for_call(Some(&bg), 90), None);
        let dir = cwd();
        let (LeaseRewrite::Rewrite(out), _) =
            decide_rewrite("cargo check", Some(&short), dir.path())
        else {
            panic!("rewritten");
        };
        assert!(
            out.contains(" build-lease --wait-secs 30 -- cargo check"),
            "{out}"
        );
    }

    /// Critic round 1: a deny rule written for the original command keeps
    /// applying — the rewrite that would dodge it is skipped.
    #[test]
    fn a_deny_rule_on_the_original_skips_the_rewrite() {
        let dir = cwd();
        std::fs::create_dir_all(dir.path().join(".claude")).expect("mkdir");
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{"permissions":{"deny":["Bash(cargo install:*)"]}}"#,
        )
        .expect("settings");
        for denied in [
            "cargo install --path x",
            "cd y && CARGO_BUILD_JOBS=2 cargo install tm",
        ] {
            assert!(
                matches!(
                    decide_rewrite(denied, None, dir.path()),
                    (LeaseRewrite::None, _)
                ),
                "{denied}"
            );
        }
        assert!(matches!(
            decide_rewrite("cargo check", None, dir.path()),
            (LeaseRewrite::Rewrite(_), None)
        ));
        assert!(glob_match("cargo * --release", "cargo build --release"));
        assert!(!glob_match("cargo * --release", "cargo build"));
    }

    /// #8261 round 3 (critic finding 3): an ask rule keeps the lease and asks;
    /// allow is emitted only when every segment of the original is allowed.
    #[test]
    fn an_ask_rule_keeps_the_lease_and_asks() {
        let dir = cwd();
        std::fs::create_dir_all(dir.path().join(".claude")).expect("mkdir");
        std::fs::write(
            dir.path().join(".claude/settings.json"),
            r#"{"permissions":{"ask":["Bash(git push:*)"],"allow":["Bash(cargo test:*)","Bash(cargo check:*)"]}}"#,
        )
        .expect("settings");
        let (rewrite, permission) = decide_rewrite("cargo test && git push", bg(), dir.path());
        let LeaseRewrite::Rewrite(out) = rewrite else {
            panic!("an ask rule must not drop the lease: {rewrite:?}");
        };
        assert!(out.contains("build-lease -- cargo test"), "{out}");
        assert_eq!(permission, Some(RewriteDecision::Ask));
        assert_eq!(
            decide_rewrite("cargo test -p x", None, dir.path()).1,
            Some(RewriteDecision::Allow)
        );
        assert_eq!(
            decide_rewrite("cargo test && rm -rf x", None, dir.path()).1,
            None,
            "one unallowed segment: no allow"
        );
        let rendered = rewrite_response(None, &out, Some(RewriteDecision::Ask));
        let parsed: Value = serde_json::from_str(&rendered).expect("json");
        assert_eq!(parsed["hookSpecificOutput"]["permissionDecision"], "ask");
    }
}
