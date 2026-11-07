//! Tests for the Architect lockdown (#10127 S5 P3).
//!
//! Why: the mechanism tests inject their own list through [`Lockdown::new`]
//! and drive the real launch seam,
//! [`crate::runtime::cli_launch::isolated_spec_with`]; the production lists
//! are pinned to the plan (#10127 D24 = B).
//! What: hermetic — carrier files live in each test's own `TempDir`.
//! Test: this file.

use std::ffi::OsStr;
use std::path::Path;

use serde_json::Value;

use super::{
    ARCHITECT_DENY_GLOBS, ARCHITECT_ENV_UNSET, Lockdown, LockdownError, REGISTRY, capitalised,
    chat_tokens, count_chat_tokens,
};
use crate::core::session_profile::{SessionProfile, launch_env};
use crate::runtime::cli_launch::{CliLaunch, isolated_spec_with};
use crate::runtime::launch_spec::LaunchSpec;

/// A test deny list: one Slack-family glob, one gworkspace tool glob.
const GLOBS: &[&str] = &["mcp__*slack*", "mcp__*gworkspace*__*mail*"];
/// A test env list.
const ENV: &[&str] = &["SLACK_BOT_TOKEN", "TELEGRAM_BOT_TOKEN"];
/// The test lockdown.
const TEST: Lockdown<'static> = Lockdown::new(GLOBS, ENV);

/// The spec for a launch of `profile` in one project, with `settings` as the
/// owner carrier file and `extra` assigned on top of the stamp.
fn spec(
    profile: SessionProfile,
    settings: Option<&Path>,
    extra: &[(String, String)],
    lockdown: &Lockdown<'_>,
) -> Result<LaunchSpec, LockdownError> {
    let mut env = extra.to_vec();
    env.push(launch_env(profile));
    let launch = CliLaunch {
        model: Some("opus"),
        prompt_file: None,
        config_dir: None,
        oauth_token: None,
        mcp_env: &env,
        scoped_mcp: Some(Path::new("/h/mcp.json")),
        settings,
        alternate_screen: false,
    };
    isolated_spec_with(Path::new("/work/architect"), &launch, lockdown)
}

/// Every value that follows a `--settings` flag.
fn settings_values(spec: &LaunchSpec) -> Vec<String> {
    spec.args
        .windows(2)
        .filter(|w| w[0] == "--settings")
        .map(|w| w[1].clone())
        .collect()
}

/// The `permissions.deny` array of an inline `--settings` payload.
fn deny_of(payload: &str) -> Vec<String> {
    let body: Value = serde_json::from_str(payload).expect("the payload is JSON");
    body["permissions"]["deny"]
        .as_array()
        .expect("permissions.deny is an array")
        .iter()
        .map(|v| v.as_str().expect("a string rule").to_owned())
        .collect()
}

/// #10127 D24 = B: the deny list is the plan's full list as written
/// (`s5-p3-plan.md` §1a), the gworkspace braces expanded to four globs.
#[test]
fn the_production_deny_list_is_the_plans_full_list() {
    assert_eq!(
        ARCHITECT_DENY_GLOBS,
        [
            "mcp__*slack*",
            "mcp__*gmail*",
            "mcp__*googlemail*",
            "mcp__*notion*",
            "mcp__*googlechat*",
            "mcp__*google-chat*",
            "mcp__*gchat*",
            "mcp__*gworkspace*__*mail*",
            "mcp__*gworkspace*__*chat*",
            "mcp__*gworkspace*__*message*",
            "mcp__*gworkspace*__*draft*",
        ]
    );
    assert!(Lockdown::PRODUCTION.deny_rules().is_ok());
}

/// #10127 Q3 (#8454 box 14): the env list is every Slack and Telegram
/// credential the registry names, and nothing else.
#[test]
fn architect_env_unset_is_every_chat_token_in_the_registry() {
    assert_eq!(
        ARCHITECT_ENV_UNSET,
        [
            "SLACK_BOT_TOKEN",
            "SLACK_USER_TOKEN",
            "SLACK_APP_TOKEN",
            "TELEGRAM_BOT_TOKEN"
        ]
    );
    for (_, var) in REGISTRY {
        let chat = var.starts_with("SLACK_") || var.starts_with("TELEGRAM_");
        assert_eq!(ARCHITECT_ENV_UNSET.contains(var), chat, "{var}");
    }
}

/// #10127 Q3: the list is derived, so a new Slack-family entry joins it and a
/// look-alike key (`slackware`) does not.
#[test]
fn chat_tokens_follow_a_new_registry_entry() {
    const FAKE: &[(&str, &str)] = &[
        ("openai", "OPENAI_API_KEY"),
        ("slack", "SLACK_BOT_TOKEN"),
        ("slack-webhook", "SLACK_WEBHOOK_URL"),
        ("slackware", "SLACKWARE_KEY"),
        ("telegram", "TELEGRAM_BOT_TOKEN"),
    ];
    const N: usize = count_chat_tokens(FAKE);
    assert_eq!(
        chat_tokens::<N>(FAKE),
        ["SLACK_BOT_TOKEN", "SLACK_WEBHOOK_URL", "TELEGRAM_BOT_TOKEN"]
    );
}

/// Whether Claude Code's `*` glob `pattern` matches `name`.
fn glob_matches(pattern: &str, name: &str) -> bool {
    let mut parts = pattern.split('*');
    let first = parts.next().unwrap_or_default();
    let Some(mut rest) = name.strip_prefix(first) else {
        return false;
    };
    let tail: Vec<&str> = parts.collect();
    for (i, part) in tail.iter().enumerate() {
        if i + 1 == tail.len() {
            return rest.ends_with(part);
        }
        match rest.find(part) {
            Some(at) => rest = &rest[at + part.len()..],
            None => return false,
        }
    }
    rest.is_empty()
}

/// #10127 C2: `ask_owner` is the Architect's only outbound path, so no rule
/// (glob or capitalised variant) matches it or any other trusty-mpm tool; the
/// families' own tools are matched.
#[test]
fn the_family_deny_never_matches_ask_owner_or_the_trusty_mpm_tools() {
    let rules = Lockdown::PRODUCTION.deny_rules().expect("rules");
    let tools: Vec<String> = crate::mcp::tools::tool_catalog()
        .iter()
        .map(|t| format!("mcp__trusty-mpm__{}", t["name"].as_str().expect("a name")))
        .collect();
    // #10423: `channel_send` is the Architect's staged org send, allowlisted
    // like `ask_owner` (#8454): no deny rule may match it.
    for allowed in [
        "mcp__trusty-mpm__ask_owner",
        "mcp__trusty-mpm__channel_send",
    ] {
        assert!(tools.contains(&allowed.to_owned()), "{allowed}: {tools:?}");
    }
    for tool in &tools {
        for rule in &rules {
            assert!(!glob_matches(rule, tool), "{rule} must not match {tool}");
        }
    }
    for denied in [
        "mcp__slack-mcp__send_message",
        "mcp__claude_ai_Slack__slack_send_message",
        "mcp__gmail__send",
        "mcp__claude_ai_Notion__search",
        "mcp__google-chat__post",
        "mcp__gworkspace-mcp__send_gmail_message",
        "mcp__gworkspace-mcp__create_draft",
    ] {
        assert!(rules.iter().any(|r| glob_matches(r, denied)), "{denied}");
    }
    assert!(
        !rules
            .iter()
            .any(|r| glob_matches(r, "mcp__gworkspace-mcp__list_files"))
    );
}

/// #10127 Q5: each word after the `mcp__` prefix gains a capital, so a
/// differently-cased server name (`claude_ai_Slack`) is denied too.
#[test]
fn capitalised_upper_cases_each_word_after_the_mcp_prefix() {
    for (glob, want) in [
        ("mcp__*slack*", "mcp__*Slack*"),
        ("mcp__*gworkspace*__*mail*", "mcp__*Gworkspace*__*Mail*"),
        ("mcp__*google-chat*", "mcp__*Google-Chat*"),
        ("mcp__*Notion*", "mcp__*Notion*"),
        ("bash", "Bash"),
    ] {
        assert_eq!(capitalised(glob), want, "{glob}");
    }
}

/// #10127: with a non-empty list the Architect argv carries ONE `--settings`
/// whose `permissions.deny` holds every glob and its capitalised variant, and
/// no allow or ask key; a PM launch of the same project carries none.
#[test]
fn architect_launch_carries_every_glob_and_its_capitalised_variant() {
    let architect = spec(SessionProfile::Architect, None, &[], &TEST).expect("spec");
    let values = settings_values(&architect);
    assert_eq!(values.len(), 1, "one --settings: {:?}", architect.args);
    let deny = deny_of(&values[0]);
    for glob in GLOBS {
        assert!(deny.contains(&(*glob).to_owned()), "{glob} in {deny:?}");
        assert!(
            deny.contains(&capitalised(glob)),
            "{glob} variant in {deny:?}"
        );
    }
    let body: Value = serde_json::from_str(&values[0]).expect("json");
    assert_eq!(body["permissions"].as_object().map(|p| p.len()), Some(1));

    let pm = spec(SessionProfile::Pm, None, &[], &TEST).expect("spec");
    assert!(settings_values(&pm).is_empty(), "PM: {:?}", pm.args);
}

/// #10127: Claude Code keeps only the last `--settings`, so an owner carrier
/// file and the deny travel as one payload; the owner keys survive. A PM
/// launch keeps the carrier file argv byte for byte.
#[test]
fn architect_lockdown_merges_into_the_owner_carrier_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let carrier = tmp.path().join("owner.json");
    std::fs::write(
        &carrier,
        r#"{"deniedMcpServers":[{"serverName":"x"}],"disableClaudeAiConnectors":true}"#,
    )
    .expect("write carrier");

    let architect = spec(SessionProfile::Architect, Some(&carrier), &[], &TEST).expect("spec");
    let values = settings_values(&architect);
    assert_eq!(values.len(), 1, "one --settings: {:?}", architect.args);
    let body: Value = serde_json::from_str(&values[0]).expect("json");
    assert_eq!(body["deniedMcpServers"][0]["serverName"], "x");
    assert_eq!(body["disableClaudeAiConnectors"], true);
    assert!(deny_of(&values[0]).contains(&"mcp__*Slack*".to_owned()));

    let pm = spec(SessionProfile::Pm, Some(&carrier), &[], &TEST).expect("spec");
    assert_eq!(settings_values(&pm), [carrier.display().to_string()]);
}

/// #10127 Fail-Open Check: an owner carrier file that cannot be read or is not
/// an object refuses the Architect launch. Falling back to the bare carrier
/// argv (the pre-#10127 path) would launch without the deny, and this fails.
#[test]
fn a_carrier_file_that_cannot_be_read_refuses_the_architect_launch() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("gone.json");
    let err = spec(SessionProfile::Architect, Some(&missing), &[], &TEST)
        .expect_err("a missing carrier refuses the launch");
    assert!(matches!(err, LockdownError::ReadCarrier { .. }), "{err:?}");
    assert!(err.to_string().contains("refusing the launch"), "{err}");

    for body in ["[1]", "not json", r#"{"permissions":[]}"#] {
        let bad = tmp.path().join("bad.json");
        std::fs::write(&bad, body).expect("write");
        let err = spec(SessionProfile::Architect, Some(&bad), &[], &TEST)
            .expect_err("a malformed carrier refuses the launch");
        assert!(
            matches!(err, LockdownError::BadCarrier { .. }),
            "{body}: {err:?}"
        );
    }
}

/// #10127 Q4: an entry that is not a tool-name glob would deny nothing (Claude
/// Code skips an `mcp__` rule with `(`), so it refuses the launch.
#[test]
fn a_deny_entry_that_is_not_a_tool_glob_refuses_the_launch() {
    for entry in ["mcp__slack(send)", "", "mcp__a b"] {
        let globs = [entry];
        let lockdown = Lockdown::new(&globs, &[]);
        let err =
            spec(SessionProfile::Architect, None, &[], &lockdown).expect_err("a bad entry refuses");
        assert!(
            matches!(err, LockdownError::BadGlob(_)),
            "{entry:?}: {err:?}"
        );
    }
}

/// #10127 Q3: an Architect child inherits none of the env list — unset, and
/// dropped even when the launch itself assigned it; a PM launch keeps them.
#[test]
fn architect_launch_unsets_the_env_list_and_a_pm_launch_does_not() {
    let assigned = [("SLACK_BOT_TOKEN".to_owned(), "xoxb-test".to_owned())];
    let architect = spec(SessionProfile::Architect, None, &assigned, &TEST).expect("spec");
    let pm = spec(SessionProfile::Pm, None, &assigned, &TEST).expect("spec");
    let architect_cmd = architect.to_command();
    let pm_cmd = pm.to_command();
    for name in ENV {
        assert!(architect.env_unset.iter().any(|n| n == name), "{name}");
        assert!(architect.env_set.iter().all(|(k, _)| k != name), "{name}");
        // The child's env: an `env_remove` reads back as `None`.
        assert!(
            architect_cmd
                .get_envs()
                .any(|(k, v)| k == OsStr::new(name) && v.is_none()),
            "{name} must be removed from the Architect child"
        );
        assert!(pm.env_unset.iter().all(|n| n != name), "PM keeps {name}");
        assert!(
            pm_cmd
                .get_envs()
                .all(|(k, v)| k != OsStr::new(name) || v.is_some()),
            "PM child keeps {name}"
        );
    }
    assert!(pm.env_set.contains(&assigned[0]), "{:?}", pm.env_set);
}

/// #10127: under an empty lockdown an Architect launch is the pre-#10127
/// launch — no new `--settings`, the carrier argv unchanged, no new unset.
#[test]
fn an_empty_deny_list_leaves_the_architect_argv_unchanged() {
    let carrier = Path::new("/h/owner.json");
    let none = Lockdown::new(&[], &[]);
    for settings in [None, Some(carrier)] {
        let bare = spec(SessionProfile::Architect, settings, &[], &none).expect("spec");
        let want: Vec<String> = settings.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(settings_values(&bare), want);
        assert!(!bare.env_unset.iter().any(|n| n == "SLACK_BOT_TOKEN"));
    }
}

/// #10127: `tm fleet init`, `tm launch` and `tm connect` build through
/// [`crate::runtime::cli_launch::isolated_spec`], which applies the shipped
/// lockdown: the deny with no owner policy and with one, every chat token
/// unset.
#[test]
fn architect_isolated_spec_carries_the_production_lockdown() {
    use crate::core::architect_lockdown::{assert_chat_tokens_unset, assert_production_deny};
    let tmp = tempfile::tempdir().expect("tempdir");
    let carrier = tmp.path().join("owner.json");
    std::fs::write(&carrier, "{}").expect("carrier");
    for settings in [None, Some(carrier.as_path())] {
        let env = [launch_env(SessionProfile::Architect)];
        let launch = CliLaunch {
            model: Some("opus"),
            prompt_file: None,
            config_dir: None,
            oauth_token: None,
            mcp_env: &env,
            scoped_mcp: None,
            settings,
            alternate_screen: false,
        };
        let spec = crate::runtime::cli_launch::isolated_spec(Path::new("/work/architect"), &launch)
            .expect("spec");
        assert_production_deny(&spec.args);
        assert_chat_tokens_unset(&spec.env_unset);
    }
}
