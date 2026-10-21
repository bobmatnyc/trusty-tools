//! Tests for the Architect lockdown (#10127 S5 P3).
//!
//! Why: the mechanism ships with empty production lists, so every behaviour
//! test injects its own list through [`Lockdown::new`] and drives the real
//! launch seam, [`crate::runtime::cli_launch::isolated_spec_with`].
//! What: hermetic — carrier files live in each test's own `TempDir`.
//! Test: this file.

use std::ffi::OsStr;
use std::path::Path;

use serde_json::Value;

use super::{ARCHITECT_DENY_GLOBS, ARCHITECT_ENV_UNSET, Lockdown, LockdownError, capitalised};
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

/// #10127: the owner's ruling changes only these two lines; until then the
/// mechanism ships with nothing to deny and nothing to unset.
#[test]
fn the_production_lists_ship_empty() {
    assert!(ARCHITECT_DENY_GLOBS.is_empty(), "{ARCHITECT_DENY_GLOBS:?}");
    assert!(ARCHITECT_ENV_UNSET.is_empty(), "{ARCHITECT_ENV_UNSET:?}");
}

/// The Architect launch sites that do NOT apply the lockdown yet (#10127
/// review). Wiring one into [`Lockdown`] removes it from this list.
const UNCOVERED_ARCHITECT_LAUNCH_SITES: &[&str] = &[
    "runtime/managed_launch.rs:249 ManagedLaunch::isolation_args (via runtime/claude_code.rs:577/832)",
    "runtime/claude_code.rs:589 in-place resume command",
    "bin/tm/commands/session/start.rs:138 inplace_session_spec",
    "core/standalone/run.rs:185 run_launch_command (via runtime/cli_launch.rs:353)",
    "client/http_client/session_connect.rs:314 client launch spec",
];

/// #10127 tripwire: a non-empty production list with an uncovered Architect
/// launch site is a fail-open — that site launches the Architect without the
/// deny or the env scrub. Filling either list owes the wiring named here.
#[test]
fn filling_a_production_list_owes_the_uncovered_launch_sites() {
    let filled = !Lockdown::PRODUCTION.is_empty();
    assert!(
        !filled || UNCOVERED_ARCHITECT_LAUNCH_SITES.is_empty(),
        "ARCHITECT_DENY_GLOBS / ARCHITECT_ENV_UNSET are non-empty, but these Architect launch \
         sites do not apply the lockdown yet; wire each one (or refuse an Architect there) and \
         remove it from UNCOVERED_ARCHITECT_LAUNCH_SITES:\n  {}",
        UNCOVERED_ARCHITECT_LAUNCH_SITES.join("\n  ")
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

/// #10127: with the shipped (empty) lists an Architect launch is exactly the
/// pre-#10127 launch — no new `--settings`, the carrier argv unchanged, the
/// scrub unchanged.
#[test]
fn an_empty_deny_list_leaves_the_architect_argv_unchanged() {
    let carrier = Path::new("/h/owner.json");
    let none = Lockdown::new(&[], &[]);
    for settings in [None, Some(carrier)] {
        let shipped = spec(
            SessionProfile::Architect,
            settings,
            &[],
            &Lockdown::PRODUCTION,
        )
        .expect("spec");
        let bare = spec(SessionProfile::Architect, settings, &[], &none).expect("spec");
        assert_eq!(shipped, bare);
        let want: Vec<String> = settings.iter().map(|p| p.display().to_string()).collect();
        assert_eq!(settings_values(&shipped), want);
    }
}
