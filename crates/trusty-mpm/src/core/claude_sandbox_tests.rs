//! Tests for the #12077 opt-in sandbox: the switch, the inline payload, and
//! the read-only detection of the keys tm 1.14.14 and 1.14.15 wrote.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};

use super::*;
use crate::core::architect_lockdown::Lockdown;
use crate::core::session_profile::SessionProfile;

/// A canonical temp root with a workspace and a home under it.
fn fixture() -> (tempfile::TempDir, PathBuf, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    let ws = root.join("ws");
    let home = root.join("home");
    std::fs::create_dir_all(&ws).expect("mkdir ws");
    std::fs::create_dir_all(&home).expect("mkdir home");
    (dir, ws, home)
}

fn write_json(path: &Path, value: &Value) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, value.to_string()).expect("write");
}

fn keys(found: &[TmSandboxKey], file: &Path) -> Vec<String> {
    let mut keys: Vec<String> = found
        .iter()
        .filter(|k| k.file == file)
        .map(|k| k.key.clone())
        .collect();
    keys.sort();
    keys
}

/// #12077: only exactly `1` opts in; unset and every other value are off.
#[test]
fn only_exactly_one_opts_in() {
    use std::ffi::OsStr;
    assert!(opt_in_from(Some(OsStr::new("1"))));
    for off in ["", "0", "true", "yes", "on", " 1", "1 ", "01"] {
        assert!(!opt_in_from(Some(OsStr::new(off))), "{off:?} must be off");
    }
    assert!(!opt_in_from(None), "unset is off");
}

/// #12077: the process switch reads `TM_CLAUDE_SANDBOX`, and unset is off.
#[test]
#[serial_test::serial]
fn the_process_switch_reads_the_env_var() {
    use crate::secret_source::test_env::EnvVarGuard;
    {
        let _off = EnvVarGuard::unset(SANDBOX_OPT_IN_ENV);
        assert!(!sandbox_opted_in(), "unset is off");
    }
    {
        let _other = EnvVarGuard::set(SANDBOX_OPT_IN_ENV, "true");
        assert!(!sandbox_opted_in(), "`true` is not the switch");
    }
    let _on = EnvVarGuard::set(SANDBOX_OPT_IN_ENV, "1");
    assert!(sandbox_opted_in());
}

/// #12077 (a): a default launch gets no sandbox payload in any role, with or
/// without a home.
#[test]
fn a_default_launch_gets_no_sandbox_payload() {
    let (_dir, ws, home) = fixture();
    for role in [SandboxRole::Pm, SandboxRole::Agent, SandboxRole::Architect] {
        for home in [Some(home.as_path()), None] {
            let payload = opted_in_launch_sandbox(false, role, &ws, home).expect("never refused");
            assert_eq!(payload, None);
        }
    }
    assert!(!ws.join(".claude").exists(), "nothing written");
}

/// #12077 Fail-Open Check: an explicit opt-in with no home is refused with an
/// error naming the switch and the missing home, in every role; it is never
/// a silent unsandboxed launch.
#[test]
fn an_opt_in_with_no_home_refuses_naming_the_switch_12077() {
    let (_dir, ws, _home) = fixture();
    for role in [SandboxRole::Pm, SandboxRole::Agent, SandboxRole::Architect] {
        let err = opted_in_launch_sandbox(true, role, &ws, None).expect_err("refused");
        let text = err.to_string();
        assert!(text.contains("TM_CLAUDE_SANDBOX"), "{text}");
        assert!(text.contains("home"), "{text}");
        assert!(text.contains(&ws.display().to_string()), "{text}");
    }
}

/// #12077: with no payload the spec unsets the switch, so a stale `1` from
/// the tmux server cannot turn a session's relaunch or divert worker on; with
/// one it assigns `1` exactly once. A `tm run` command removes it the same way.
#[test]
fn an_off_launch_unsets_a_stale_switch_12077() {
    let switch = SANDBOX_OPT_IN_ENV.to_owned();
    let stale = (switch.clone(), "1".to_owned());
    let mut off = crate::runtime::cli_launch::client_spec(Path::new("/w"), None, false);
    off.env_set.push(stale.clone());
    Lockdown::PRODUCTION.scrub_spec_env(SessionProfile::Pm, &mut off);
    assert!(off.env_unset.contains(&switch), "{:?}", off.env_unset);
    assert!(
        off.env_set.iter().all(|(k, _)| k != &switch),
        "{:?}",
        off.env_set
    );

    let payload = json!({ "sandbox": { "enabled": true } });
    let mut on = crate::runtime::cli_launch::client_spec(Path::new("/w"), None, false);
    on.env_set.push(stale.clone());
    Lockdown::PRODUCTION
        .with_sandbox(Some(&payload))
        .scrub_spec_env(SessionProfile::Pm, &mut on);
    assert!(!on.env_unset.contains(&switch), "{:?}", on.env_unset);
    let set: Vec<_> = on.env_set.iter().filter(|(k, _)| k == &switch).collect();
    assert_eq!(set, vec![&stale], "assigned once: {:?}", on.env_set);

    let mut cmd = std::process::Command::new("claude");
    cmd.env(SANDBOX_OPT_IN_ENV, "1");
    Lockdown::PRODUCTION
        .apply_to_command(SessionProfile::Pm, None, &mut cmd)
        .expect("command");
    assert!(
        cmd.get_envs()
            .any(|(k, v)| k == SANDBOX_OPT_IN_ENV && v.is_none()),
        "the switch is removed from the child"
    );
}

/// #12077: an opted-in PM payload holds the project tier and the user tier
/// in one object; an agent's has no tmux socket; the Architect's turns the
/// sandbox off.
#[test]
fn an_opted_in_pm_payload_carries_both_tiers() {
    let (_dir, ws, home) = fixture();
    let pm = opted_in_launch_sandbox(true, SandboxRole::Pm, &ws, Some(&home))
        .expect("not refused")
        .expect("pm");
    assert_eq!(pm.pointer("/sandbox/enabled"), Some(&json!(true)));
    assert_eq!(
        pm.pointer("/sandbox/allowUnsandboxedCommands"),
        Some(&json!(false))
    );
    assert_eq!(
        pm.pointer("/sandbox/network/strictAllowlist"),
        Some(&json!(true))
    );
    let ours = user_policy(SandboxRole::Pm).expect("user policy");
    let domains = "/sandbox/network/allowedDomains";
    assert_eq!(pm.pointer(domains), ours.pointer(domains));
    assert!(pm.pointer("/sandbox/credentials/envVars").is_some());
    assert_eq!(pm.pointer("/env/RUSTC_WRAPPER"), Some(&json!("")));

    let architect = opted_in_launch_sandbox(true, SandboxRole::Architect, &ws, Some(&home))
        .expect("not refused")
        .expect("arch");
    assert_eq!(architect.pointer("/sandbox/enabled"), Some(&json!(false)));
    assert!(architect.pointer("/sandbox/credentials").is_none());
}

/// #12077: the merge unions lists without duplicates, recurses into objects
/// and keeps keys the payload does not name.
#[test]
fn merging_unions_lists_and_keeps_other_keys() {
    let mut body = json!({
        "deniedMcpServers": [{ "serverName": "x" }],
        "permissions": { "deny": ["Workflow"] },
        // A carrier scalar the payload overrides.
        "sandbox": { "enabled": false },
    });
    merge_settings_payload(
        &mut body,
        &json!({
            "permissions": { "deny": ["Workflow", "Read(//h/.ssh/**)"] },
            "sandbox": { "enabled": true },
        }),
    );
    assert_eq!(
        body,
        json!({
            "deniedMcpServers": [{ "serverName": "x" }],
            "permissions": { "deny": ["Workflow", "Read(//h/.ssh/**)"] },
            "sandbox": { "enabled": true },
        })
    );
}

/// #12077: with a payload every profile gets ONE inline `--settings` that
/// merges the owner carrier and sets the switch for the session's own
/// relaunch; without one the argv is the carrier path, exactly as before.
#[test]
fn an_opted_in_launch_carries_the_sandbox_inline_12077() {
    let (dir, _ws, _home) = fixture();
    let carrier = dir.path().join("owner.json");
    std::fs::write(&carrier, r#"{"permissions":{"deny":["Workflow"]}}"#).expect("carrier");
    let payload = json!({ "sandbox": { "enabled": true }, "permissions": { "deny": ["X"] } });
    for profile in [SessionProfile::Pm, SessionProfile::Architect] {
        let argv = Lockdown::PRODUCTION
            .with_sandbox(Some(&payload))
            .settings_argv(profile, Some(&carrier))
            .expect("argv");
        assert_eq!(argv.len(), 2, "{argv:?}");
        let inline: Value = serde_json::from_str(&argv[1]).expect("inline JSON");
        assert_eq!(inline.pointer("/sandbox/enabled"), Some(&json!(true)));
        let deny = inline.pointer("/permissions/deny").expect("deny");
        assert!(deny.as_array().expect("list").contains(&json!("Workflow")));
        assert!(deny.as_array().expect("list").contains(&json!("X")));
    }
    let plain = Lockdown::PRODUCTION
        .with_sandbox(None)
        .settings_argv(SessionProfile::Pm, Some(&carrier))
        .expect("argv");
    assert_eq!(
        plain,
        vec!["--settings".to_owned(), carrier.display().to_string()]
    );
    let mut cmd = std::process::Command::new("claude");
    Lockdown::PRODUCTION
        .with_sandbox(Some(&payload))
        .apply_to_command(SessionProfile::Pm, None, &mut cmd)
        .expect("command");
    let on = cmd
        .get_envs()
        .any(|(k, v)| k == SANDBOX_OPT_IN_ENV && v == Some(std::ffi::OsStr::new("1")));
    assert!(on, "the switch reaches the child");
    let mut plain_cmd = std::process::Command::new("claude");
    Lockdown::PRODUCTION
        .apply_to_command(SessionProfile::Pm, None, &mut plain_cmd)
        .expect("command");
    // #12077: with no payload the switch is removed, never inherited.
    assert!(
        plain_cmd
            .get_envs()
            .any(|(k, v)| k == SANDBOX_OPT_IN_ENV && v.is_none())
    );
}

/// #12077 (e): with no role record tm owns nothing in the project files, even
/// when they hold sandbox keys equal to tm's policy.
#[test]
fn no_role_record_means_tm_owns_nothing_in_the_project() {
    let (_dir, ws, home) = fixture();
    let pm = opted_in_launch_sandbox(true, SandboxRole::Pm, &ws, Some(&home))
        .expect("not refused")
        .expect("pm");
    write_json(&ws.join(".claude/settings.json"), &pm);
    write_json(&ws.join(".claude/settings.local.json"), &pm);
    let user_tier = home.join("claude-config");
    let found = detect_tm_sandbox_keys(&ws, &user_tier).expect("detect");
    assert!(found.is_empty(), "{found:?}");
    let settings = std::fs::read_to_string(ws.join(".claude/settings.json")).expect("read");
    assert_eq!(serde_json::from_str::<Value>(&settings).expect("json"), pm);
}

/// #12077 (e): with a role record, only the keys and list entries equal to
/// the recorded policy are tm's; operator keys and changed values are not.
#[test]
fn a_recorded_policy_marks_only_matching_project_keys() {
    let (_dir, ws, _home) = fixture();
    let record = json!({
        "pm": {
            "env": { "RUSTC_WRAPPER": "" },
            "permissions": { "deny": ["Read(//h/.ssh/**)", "Edit(//w/.mcp.json)"] },
            "sandbox": { "enabled": true, "failIfUnavailable": true,
                         "network": { "allowUnixSockets": ["/s/mpm.sock"] } },
        }
    });
    write_json(&ws.join(".trusty-mpm").join(ROLE_RECORD_FILE), &record);
    let project = ws.join(".claude/settings.json");
    write_json(
        &project,
        &json!({
            "theme": "dark",
            "env": { "RUSTC_WRAPPER": "sccache" },
            "permissions": { "deny": ["Read(//h/.ssh/**)", "Bash(rm:*)"] },
            "sandbox": { "enabled": true, "failIfUnavailable": false,
                         "network": { "allowUnixSockets": ["/s/mpm.sock", "/op.sock"] } },
        }),
    );
    let user_tier = ws.join("no-user-tier");
    let found = detect_tm_sandbox_keys(&ws, &user_tier).expect("detect");
    assert_eq!(
        keys(&found, &project),
        vec![
            "permissions.deny[Read(//h/.ssh/**)]".to_owned(),
            "sandbox.enabled".to_owned(),
            "sandbox.network.allowUnixSockets[/s/mpm.sock]".to_owned(),
        ]
    );
    assert_eq!(found.len(), 3, "{found:?}");
}

/// #12077 (e): tm's user tier matches only tm's exact value set: the
/// strict-allowlist flag, the whole domain list, and each deny entry.
#[test]
fn the_user_tier_matches_only_tms_exact_values() {
    let (_dir, ws, home) = fixture();
    let user_tier = home.join("claude-config");
    let file = user_tier.join("settings.json");
    let ours = user_policy(SandboxRole::Pm).expect("user policy");
    let mut env_vars = ours
        .pointer("/sandbox/credentials/envVars")
        .and_then(Value::as_array)
        .cloned()
        .expect("envVars");
    let tm_entries = env_vars.len();
    // An operator entry, and one of tm's names with another mode.
    env_vars.push(json!({ "name": "MY_TOKEN", "mode": "deny" }));
    let first = env_vars[0]["name"].clone();
    env_vars[0] = json!({ "name": first, "mode": "allow" });
    write_json(
        &file,
        &json!({
            "theme": "light",
            "sandbox": {
                "network": { "strictAllowlist": true, "allowedDomains": ["github.com"] },
                "credentials": { "envVars": env_vars },
            },
        }),
    );
    let found = detect_tm_sandbox_keys(&ws, &user_tier).expect("detect");
    let got = keys(&found, &file);
    assert!(
        got.contains(&"sandbox.network.strictAllowlist".to_owned()),
        "{got:?}"
    );
    assert!(
        !got.iter()
            .any(|k| k.starts_with("sandbox.network.allowedDomains")),
        "a partial domain list is not tm's: {got:?}"
    );
    let entries = got.iter().filter(|k| k.contains("envVars[")).count();
    assert_eq!(entries, tm_entries - 1, "{got:?}");
    assert!(!got.iter().any(|k| k.contains("MY_TOKEN")), "{got:?}");

    let domains = ours
        .pointer("/sandbox/network/allowedDomains")
        .cloned()
        .expect("domains");
    write_json(
        &file,
        &json!({ "sandbox": { "network": { "allowedDomains": domains } } }),
    );
    let found = detect_tm_sandbox_keys(&ws, &user_tier).expect("detect");
    assert_eq!(
        keys(&found, &file),
        vec!["sandbox.network.allowedDomains".to_owned()]
    );
}

/// #12077 (e): a corrupt role record is an error, never a guess, and the
/// detection never writes or deletes a file.
#[test]
fn a_corrupt_role_record_is_an_error_not_a_guess() {
    let (_dir, ws, home) = fixture();
    let record = ws.join(".trusty-mpm").join(ROLE_RECORD_FILE);
    std::fs::create_dir_all(record.parent().expect("parent")).expect("mkdir");
    std::fs::write(&record, "{not json").expect("write");
    let project = ws.join(".claude/settings.json");
    write_json(&project, &json!({ "sandbox": { "enabled": true } }));
    let before = std::fs::read_to_string(&project).expect("read");
    let err = detect_tm_sandbox_keys(&ws, &home.join("claude-config")).expect_err("corrupt");
    assert!(err.to_string().contains(ROLE_RECORD_FILE), "{err}");
    assert_eq!(std::fs::read_to_string(&project).expect("read"), before);
    assert_eq!(std::fs::read_to_string(&record).expect("read"), "{not json");
}
