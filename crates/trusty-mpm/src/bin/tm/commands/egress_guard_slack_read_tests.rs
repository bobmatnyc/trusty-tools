//! #10387: the read-only Slack allowlist arm of the egress guard.

use std::ffi::OsStr;
use std::path::{Path, PathBuf};

use serde_json::json;
use trusty_channels::policy::{HostError, SlackReadAllowlist, parse_host};

use super::*;
use crate::commands::egress_guard::verdict;

/// A canonical temp root holding the `tm-apex` and `other` project dirs.
struct Fx {
    _tmp: tempfile::TempDir,
    root: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        for dir in ["tm-apex", "other"] {
            std::fs::create_dir(root.join(dir)).expect("mkdir");
        }
        Self { _tmp: tmp, root }
    }

    fn apex(&self) -> PathBuf {
        self.root.join("tm-apex")
    }

    /// The host text granting tm-apex both read tools.
    fn yaml(&self) -> String {
        format!(
            "channels:\n  version: 1\n  slack_read_allowlist:\n    - project: \"{}\"\n      \
             name: tm-apex\n      tools: [slack_read_channel, slack_read_thread]\n",
            self.apex().display()
        )
    }

    fn list(&self) -> Result<SlackReadAllowlist, HostError> {
        parse_host(&self.yaml(), Some(&self.root)).map(|c| c.slack_read_allowlist().clone())
    }
}

/// The guard's first verdict for `tool`, then the lift as `project` with
/// `load`; no trust-anchor deny.
fn judge(
    tool: &str,
    project: Option<&Path>,
    load: impl FnOnce() -> Result<SlackReadAllowlist, HostError>,
) -> (Verdict, Option<Allowed>) {
    let read = Ok(json!({ "tool_name": tool, "tool_input": {} }));
    let table = FamilyTable::builtin();
    let first = verdict(&read, &table, |_| None);
    lift(first, &read, &table, project.map(Path::as_os_str), load)
}

/// The deny reason, panicking on an allow.
fn denied(out: &(Verdict, Option<Allowed>), case: &str) -> String {
    match out {
        (Verdict::Deny { check, reason }, None) => {
            assert_eq!(*check, "egress-family", "{case}");
            reason.clone()
        }
        other => panic!("{case}: must stay denied, got {other:?}"),
    }
}

/// 🔴 #10387: tm-apex reads both tools on both prefixes, one audit fact each.
#[test]
fn slack_read_allowlisted_project_reads_both_tools_on_both_prefixes() {
    let fx = Fx::new();
    for server in SLACK_READ_SERVERS {
        for tool in ["slack_read_channel", "slack_read_thread"] {
            let name = format!("mcp__{server}__{tool}");
            let (v, allowed) = judge(&name, Some(&fx.apex()), || fx.list());
            assert_eq!(v, Verdict::Allow, "{name}");
            let allowed = allowed.expect("audit fact");
            assert_eq!(
                allowed.audit_line(),
                format!("egress-slack-read-allowed project=tm-apex tool={tool} prefix={server}")
            );
        }
    }
}

/// 🔴 #10387: posting, canvas and file tools stay refused for tm-apex, on
/// both prefixes. Mutation proof: `listed` returning `true` turns this red.
#[test]
fn slack_read_posting_tools_stay_refused() {
    let fx = Fx::new();
    for server in SLACK_READ_SERVERS {
        for tool in [
            "slack_send_message",
            "slack_schedule_message",
            "slack_add_reaction",
            "slack_create_canvas",
            "slack_update_canvas",
            "slack_canvas_create",
            "slack_read_canvas",
            "slack_read_file",
            "slack_read_channel_x",
        ] {
            let name = format!("mcp__{server}__{tool}");
            let reason = denied(&judge(&name, Some(&fx.apex()), || fx.list()), &name);
            assert!(
                reason.contains("does not list this tool"),
                "{name}: {reason}"
            );
        }
    }
}

/// 🔴 #10387: every fault keeps today's deny and names the fault, never a
/// config value.
#[test]
fn slack_read_faults_keep_the_deny() {
    let fx = Fx::new();
    let tool = "mcp__slack-mcp__slack_read_channel";
    let other = fx.root.join("other");
    let apex = fx.apex();
    let link = fx.root.join("link");
    std::os::unix::fs::symlink(fx.apex(), &link).expect("symlink");
    let wildcard = fx.yaml().replace("slack_read_thread]", "\"*\"]");
    let unknown = fx
        .yaml()
        .replace("name: tm-apex", "name: tm-apex\n      all: true");
    let cases: Vec<(
        &str,
        Option<&Path>,
        Result<SlackReadAllowlist, HostError>,
        &str,
    )> = vec![
        (
            "another project",
            Some(&other),
            fx.list(),
            "no slack_read_allowlist entry",
        ),
        (
            "missing CLAUDE_PROJECT_DIR",
            None,
            fx.list(),
            "CLAUDE_PROJECT_DIR is not set",
        ),
        (
            "empty CLAUDE_PROJECT_DIR",
            Some(Path::new("")),
            fx.list(),
            "is not set",
        ),
        (
            "symlinked project dir",
            Some(&link),
            fx.list(),
            "not a canonical absolute",
        ),
        (
            "relative project dir",
            Some(Path::new("tm-apex")),
            fx.list(),
            "not a canonical",
        ),
        (
            "missing config",
            Some(&apex),
            Err(HostError::Missing),
            "(Missing)",
        ),
        (
            "unreadable config",
            Some(&apex),
            Err(HostError::Read {
                reason: "secret-value".into(),
            }),
            "(Read)",
        ),
        (
            "malformed config",
            Some(&apex),
            parse_host("channels: [", Some(&fx.root)).map(|c| c.slack_read_allowlist().clone()),
            "(Malformed)",
        ),
        (
            "unknown key",
            Some(&apex),
            parse_host(&unknown, Some(&fx.root)).map(|c| c.slack_read_allowlist().clone()),
            "(Invalid)",
        ),
        (
            "wildcard entry",
            Some(&apex),
            parse_host(&wildcard, Some(&fx.root)).map(|c| c.slack_read_allowlist().clone()),
            "(SlackReadAllowlist)",
        ),
    ];
    for (case, project, list, want) in cases {
        let reason = denied(&judge(tool, project, || list), case);
        assert!(reason.contains("#8454"), "{case}: today's deny: {reason}");
        assert!(reason.contains(want), "{case}: {reason}");
        assert!(!reason.contains("secret-value"), "{case}: value echoed");
    }
}

/// 🔴 #10387: a lookalike server or tool spelling is not judged at all: the
/// deny is today's, unchanged, and the config is never read.
#[test]
fn slack_read_lookalike_names_are_not_judged() {
    let fx = Fx::new();
    for name in [
        "mcp__slack-evil__slack_read_channel",
        "mcp__Slack-MCP__slack_read_channel",
        "mcp__claude_ai_slack__slack_read_channel",
        "mcp__slack_mcp__slack_read_channel",
        "mcp__slack-mcp-x__slack_read_channel",
        "mcp__x__slack-mcp__slack_read_channel",
    ] {
        let (v, allowed) = judge(name, Some(&fx.apex()), || panic!("{name}: config read"));
        assert!(allowed.is_none(), "{name}");
        let Verdict::Deny { check, reason } = v else {
            panic!("{name}: must stay denied");
        };
        assert_eq!(check, "egress-family", "{name}");
        assert!(!reason.contains("#10387 read-only"), "{name}: {reason}");
    }
    // A case variant of the tool reaches the name check and fails it.
    let reason = denied(
        &judge(
            "mcp__slack-mcp__Slack_Read_Channel",
            Some(&fx.apex()),
            || fx.list(),
        ),
        "tool case",
    );
    assert!(reason.contains("does not list this tool"), "{reason}");
}

/// 🔴 #10387: the trust-anchor floor runs first and a lift never touches it;
/// an allowed or non-family verdict passes through untouched.
#[test]
fn slack_read_trust_anchor_deny_is_never_lifted() {
    let fx = Fx::new();
    let read = Ok(json!({ "tool_name": "mcp__slack-mcp__slack_read_channel", "tool_input": {} }));
    let table = FamilyTable::builtin();
    let first = verdict(&read, &table, |_| Some("anchor".into()));
    let apex = fx.apex();
    let (v, allowed) = lift(first, &read, &table, Some(apex.as_os_str()), || fx.list());
    assert!(allowed.is_none());
    assert!(matches!(v, Verdict::Deny { check, .. } if check == "trust-anchor-write"));
    let unrelated = Ok(json!({ "tool_name": "Read", "tool_input": {} }));
    let (v, allowed) = lift(
        Verdict::Allow,
        &unrelated,
        &table,
        Some(OsStr::new("/x")),
        || panic!("config read"),
    );
    assert_eq!((v, allowed), (Verdict::Allow, None));
}
