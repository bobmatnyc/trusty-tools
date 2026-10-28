//! `tm hook --egress-guard` with a per-project read-only Slack allowlist,
//! through the real binary (#10387).
//!
//! Why: the grant is only as narrow as the binary Claude Code runs: two exact
//! read tools, on two exact servers, for the project the host `config.yaml`
//! names, and nothing for any fault or any bypass variable.
//! What: a canonical temp `HOME` holding `.trusty-tools/trusty-mpm/config.yaml`
//! and two project dirs; each case pipes one `PreToolUse` payload with
//! `CLAUDE_PROJECT_DIR` set (or removed) and checks the verdict, the deny
//! reason and the stderr audit line.
//! Test: `cargo test -p trusty-mpm --test integration tm_hook_egress_slack_read_10387`.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Output, Stdio};

use crate::tm_hook_egress_guard_8454::{assert_allowed, assert_denied, guard_command_in};

/// The two read tools the allowlist may grant.
const READS: &[&str] = &["slack_read_channel", "slack_read_thread"];

/// The two exact server segments.
const SERVERS: &[&str] = &["slack-mcp", "claude_ai_Slack"];

/// A canonical temp root with `home/`, `tm-apex/` and `other/`.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        for dir in ["home", "tm-apex", "other", "data"] {
            std::fs::create_dir(root.join(dir)).expect("mkdir");
        }
        Self { _tmp: tmp, root }
    }

    fn home(&self) -> PathBuf {
        self.root.join("home")
    }

    fn apex(&self) -> PathBuf {
        self.root.join("tm-apex")
    }

    fn config(&self) -> PathBuf {
        trusty_common::crate_config::crate_config_path_at(&self.home(), "trusty-mpm")
    }

    /// Write `text` as the host config.
    fn write(&self, text: &str) {
        let path = self.config();
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("write config");
    }

    /// The host text granting tm-apex both read tools.
    fn yaml(&self) -> String {
        format!(
            "channels:\n  version: 1\n  slack_read_allowlist:\n    - project: \"{}\"\n      \
             name: tm-apex\n      tools: [slack_read_channel, slack_read_thread]\n",
            self.apex().display()
        )
    }

    /// Run the guard on `tool` as `project` (`None` removes the variable),
    /// with `env` on top.
    fn run(&self, tool: &str, project: Option<&Path>, env: &[(&str, &str)]) -> Output {
        let data = self.root.join("data");
        let mut command = guard_command_in(&self.home(), &data, env);
        command.env_remove("CLAUDE_PROJECT_DIR");
        if let Some(project) = project {
            command.env("CLAUDE_PROJECT_DIR", project);
        }
        let mut child = command
            .stdin(Stdio::piped())
            .spawn()
            .expect("spawn tm hook --egress-guard");
        let payload = serde_json::json!({
            "session_id": "s-10387",
            "hook_event_name": "PreToolUse",
            "tool_name": tool,
            "tool_input": { "channel_id": "C0123", "limit": 5 },
        });
        let mut pipe = child.stdin.take().expect("stdin");
        let _ = pipe.write_all(payload.to_string().as_bytes());
        drop(pipe);
        child.wait_with_output().expect("wait for the guard")
    }
}

/// The deny reason printed on stdout.
fn reason(out: &Output) -> String {
    let value: serde_json::Value =
        serde_json::from_slice(&out.stdout).expect("stdout is one JSON object");
    value["hookSpecificOutput"]["permissionDecisionReason"]
        .as_str()
        .unwrap_or_default()
        .to_owned()
}

/// Assert a deny whose reason contains `want`, with no audit line.
fn assert_refused(out: &Output, case: &str, want: &str) {
    assert_denied(out, case);
    let reason = reason(out);
    assert!(reason.contains(want), "{case}: {reason}");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("egress-slack-read-allowed"),
        "{case}: {stderr}"
    );
}

/// 🔴 #10387: tm-apex reads both tools on both prefixes; each call prints
/// exactly one audit line with no call content.
#[test]
fn slack_read_allowlist_allows_tm_apex_reads_with_one_audit_line() {
    let fx = Fx::new();
    fx.write(&fx.yaml());
    for server in SERVERS {
        for tool in READS {
            let name = format!("mcp__{server}__{tool}");
            let out = fx.run(&name, Some(&fx.apex()), &[]);
            assert_allowed(&out, &name);
            let stderr = String::from_utf8_lossy(&out.stderr);
            let line =
                format!("egress-slack-read-allowed project=tm-apex tool={tool} prefix={server}");
            assert_eq!(stderr.matches(&line).count(), 1, "{name}: {stderr}");
            assert!(!stderr.contains("C0123"), "{name}: content in audit");
        }
    }
}

/// 🔴 #10387: posting, canvas, reaction and file tools stay refused for
/// tm-apex on both prefixes.
#[test]
fn slack_read_allowlist_refuses_tm_apex_posts_and_other_reads() {
    let fx = Fx::new();
    fx.write(&fx.yaml());
    for server in SERVERS {
        for tool in [
            "slack_send_message",
            "slack_schedule_message",
            "slack_add_reaction",
            "slack_create_canvas",
            "slack_update_canvas",
            "slack_read_canvas",
            "slack_read_file",
        ] {
            let name = format!("mcp__{server}__{tool}");
            let out = fx.run(&name, Some(&fx.apex()), &[]);
            assert_refused(&out, &name, "does not list this tool");
        }
    }
}

/// 🔴 #10387: another project, a missing `CLAUDE_PROJECT_DIR`, a lookalike
/// server, and every config fault refuse with today's deny.
#[test]
fn slack_read_allowlist_fault_arms_refuse() {
    let fx = Fx::new();
    let read = "mcp__slack-mcp__slack_read_channel";
    fx.write(&fx.yaml());
    let other = fx.root.join("other");
    let out = fx.run(read, Some(&other), &[]);
    assert_refused(&out, "another project", "no slack_read_allowlist entry");
    let out = fx.run(read, None, &[]);
    assert_refused(
        &out,
        "no CLAUDE_PROJECT_DIR",
        "CLAUDE_PROJECT_DIR is not set",
    );
    let out = fx.run("mcp__slack-evil__slack_read_channel", Some(&fx.apex()), &[]);
    assert_refused(&out, "lookalike server", "it is a slack tool");
    assert!(!reason(&out).contains("does not apply"), "never judged");
    let wildcard = fx.yaml().replace("slack_read_thread]", "\"*\"]");
    let unknown = fx
        .yaml()
        .replace("name: tm-apex", "name: tm-apex\n      all: true");
    for (case, text, want) in [
        ("malformed", "channels: [".to_owned(), "(Malformed)"),
        ("unknown key", unknown, "(Invalid)"),
        ("wildcard entry", wildcard, "(SlackReadAllowlist)"),
    ] {
        fx.write(&text);
        assert_refused(&fx.run(read, Some(&fx.apex()), &[]), case, want);
    }
    std::fs::remove_file(fx.config()).expect("rm config");
    assert_refused(&fx.run(read, Some(&fx.apex()), &[]), "missing", "(Missing)");
    // A directory where the file belongs: the read refuses it.
    std::fs::create_dir(fx.config()).expect("dir at config.yaml");
    assert_refused(
        &fx.run(read, Some(&fx.apex()), &[]),
        "unreadable",
        "(NotRegular)",
    );
}

/// 🔴 #10387: the pm-guard bypass variables and the subagent marker widen
/// nothing: another project and a post stay refused under each.
#[test]
fn slack_read_allowlist_bypass_envs_do_not_widen() {
    let fx = Fx::new();
    fx.write(&fx.yaml());
    let other = fx.root.join("other");
    for env in [
        ("TRUSTY_MPM_PM_UNRESTRICTED", "1"),
        ("TRUSTY_MPM_DISABLE_HOOKS", "1"),
        ("CLAUDE_MPM_SUB_AGENT", "1"),
    ] {
        let case = env.0;
        let out = fx.run("mcp__slack-mcp__slack_read_channel", Some(&other), &[env]);
        assert_refused(&out, case, "no slack_read_allowlist entry");
        let out = fx.run(
            "mcp__claude_ai_Slack__slack_send_message",
            Some(&fx.apex()),
            &[env],
        );
        assert_refused(&out, case, "does not list this tool");
    }
}
