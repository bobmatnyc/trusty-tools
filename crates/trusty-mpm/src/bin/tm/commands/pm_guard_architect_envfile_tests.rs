//! Unit tests for the #8939 env-file exemption (`pm_guard_architect_envfile.rs`).

use std::path::PathBuf;

use serde_json::json;
use trusty_mpm::core::config::MpmConfig;

use super::*;
use crate::commands::pm_guard_architect_reason::NotArchitect;
use crate::commands::pm_guard_architect_reason::tests::cases;
use crate::commands::pm_guard_bash::LiveGit;
use crate::commands::pm_guard_floor::tests::NoArchitect;
use crate::commands::pm_guard_floor::{Probes, evaluate_floors};
use crate::commands::pm_guard_trust_anchor::ANCHOR_ROOT;
use crate::commands::pm_guard_trust_anchor::tests::{
    Fixture, allowlist, architect_env, fixture, payload, pm_env, spoof_env,
};

/// Write `body` to `name` in the Architect's project directory.
fn envfile(fx: &Fixture, name: &str, body: &str) -> PathBuf {
    let path = fx.project.join(name);
    std::fs::write(&path, body).expect("write env file");
    path
}

fn bash(fx: &Fixture, command: &str) -> Value {
    payload(fx, "Bash", json!({ "command": command }))
}

/// The gated verdict for `call` from the project directory, and the
/// exemption it recorded.
fn verdict(
    fx: &Fixture,
    call: &Value,
    env: HookEnv,
    granted: bool,
) -> (Option<String>, Option<EnvfileCall>) {
    let config = || {
        if granted {
            allowlist(fx)
        } else {
            MpmConfig::default()
        }
    };
    let gate = ArchitectGate::new(call, env, config);
    let tool = call["tool_name"].as_str().unwrap_or_default();
    let deny = evaluate_secret_file_read_gated(tool, call.get("tool_input"), &fx.project, &gate);
    (deny, gate.envfile().cloned())
}

/// The Architect's verdict for a Bash `command`.
fn architect(fx: &Fixture, command: &str) -> Option<String> {
    verdict(fx, &bash(fx, command), architect_env(fx), true).0
}

/// The Architect's verdict for a Bash `command`, the user config read from
/// the fixture home by the production reader.
fn architect_reading_config(fx: &Fixture, command: &str) -> Option<String> {
    let call = bash(fx, command);
    let read = || MpmConfig::load(&fx.home.join(ANCHOR_ROOT));
    let gate = ArchitectGate::new(&call, architect_env(fx), read);
    evaluate_secret_file_read_gated("Bash", call.get("tool_input"), &fx.project, &gate)
}

/// Write the fixture's user config with `roots` as `[supervisor] projects`.
fn list_in_config(fx: &Fixture, roots: &[&std::path::Path]) {
    let roots: Vec<_> = roots
        .iter()
        .map(|r| format!("\"{}\"", r.display()))
        .collect();
    let body = format!("[supervisor]\nprojects = [{}]\n", roots.join(", "));
    std::fs::write(&fx.anchor, body).expect("write config");
}

/// A `.env.local` in `iris`, a directory beside the fixture home and outside
/// the Architect's project; returns `(iris, env file)`.
fn outside_envfile(fx: &Fixture) -> (PathBuf, PathBuf) {
    let iris = fx.home.parent().expect("scratch").join("iris");
    std::fs::create_dir_all(&iris).expect("mkdir iris");
    let p = iris.join(".env.local");
    std::fs::write(&p, "A=1\n").expect("write");
    (iris, p)
}

/// Register `roots` in the fixture home's `project-paths.json`, which any PM
/// launch writes and which grants no scope.
fn register(fx: &Fixture, roots: &[&std::path::Path]) {
    let entries: Vec<_> = roots
        .iter()
        .enumerate()
        .map(|(i, p)| json!({ "alias": format!("p{i}"), "path": p }))
        .collect();
    let file = fx.home.join(ANCHOR_ROOT).join("project-paths.json");
    std::fs::write(file, serde_json::to_string(&entries).expect("json")).expect("register");
}

/// The two exempt shapes on `p`: `keys`, and `set` in each value source.
fn exempt_commands(p: &std::path::Path) -> Vec<String> {
    let p = p.display();
    vec![
        format!("tm env keys {p}"),
        format!("tm env set {p} API_KEY"),
        format!("tm env set {p} API_KEY --from-keychain iris --account bob@x.io"),
        format!("tm env set {p} API_KEY --account bob --from-keychain iris"),
    ]
}

#[test]
fn the_architect_main_thread_may_list_env_key_names() {
    let fx = fixture();
    let p = envfile(&fx, ".env.local", "API_KEY=x\n");
    for command in [
        format!("tm env keys {}", p.display()),
        "tm env keys .env.local".into(),
    ] {
        let (deny, call) = verdict(&fx, &bash(&fx, &command), architect_env(&fx), true);
        assert_eq!(deny, None, "{command}");
        let call = call.expect("recorded");
        assert_eq!(
            (call.verb, call.path, call.keys),
            ("keys", p.clone(), vec![])
        );
        // The same call from a PM is the #7266 deny.
        assert!(
            verdict(&fx, &bash(&fx, &command), pm_env(&fx), false)
                .0
                .is_some()
        );
    }
}

#[test]
fn the_architect_main_thread_may_set_an_env_key() {
    let fx = fixture();
    let existing = envfile(&fx, ".env", "A=1\n");
    let absent = fx.project.join(".env.production");
    for p in [&existing, &absent] {
        for command in exempt_commands(p).into_iter().skip(1) {
            let (deny, call) = verdict(&fx, &bash(&fx, &command), architect_env(&fx), true);
            assert_eq!(deny, None, "{command}");
            assert_eq!(call.expect("recorded").keys, vec!["API_KEY".to_owned()]);
        }
    }
}

/// Architect ruling Q2 and its addendum: `CLAUDE_PROJECT_DIR` plus the
/// `[supervisor] projects` roots only; `/` and the home directory's ancestors
/// never count as a root.
#[test]
fn a_config_listed_root_is_in_scope_and_another_path_is_not() {
    let fx = fixture();
    let (iris, p) = outside_envfile(&fx);
    let scratch = fx.home.parent().expect("scratch").to_path_buf();
    let command = format!("tm env keys {}", p.display());
    list_in_config(&fx, &[&fx.project]);
    assert!(
        architect_reading_config(&fx, &command).is_some(),
        "unlisted"
    );
    list_in_config(&fx, &[&fx.project, std::path::Path::new("/"), &scratch]);
    let deny = architect_reading_config(&fx, &command);
    assert!(deny.is_some(), "a degenerate root");
    list_in_config(&fx, &[&fx.project, &iris]);
    assert_eq!(architect_reading_config(&fx, &command), None, "listed");
}

/// Architect ruling Q2 addendum: `project-paths.json` is not a trust anchor.
#[test]
fn a_project_paths_json_entry_grants_no_scope() {
    let fx = fixture();
    let iris = fx.home.parent().expect("scratch").join("iris");
    std::fs::create_dir_all(&iris).expect("mkdir iris");
    let p = iris.join(".env.local");
    std::fs::write(&p, "A=1\n").expect("write");
    register(&fx, &[&iris]);
    for command in exempt_commands(&p) {
        assert!(architect(&fx, &command).is_some(), "allowed: {command}");
    }
}

/// Fail closed: an unreadable or malformed user config lists no root, so only
/// `CLAUDE_PROJECT_DIR` counts; with that unset too, nothing is in scope.
#[test]
fn an_unreadable_config_leaves_only_the_project_dir_in_scope() {
    let fx = fixture();
    let (iris, far) = outside_envfile(&fx);
    let near = envfile(&fx, ".env.local", "A=1\n");
    let read = || MpmConfig::load(&fx.home.join(ANCHOR_ROOT));
    let scoped = |p: &std::path::Path, env: &HookEnv| {
        let command = format!("tm env keys {}", p.display());
        envfile_shape(&command, &fx.project, env, read).is_some()
    };
    let no_project = HookEnv {
        project_dir: None,
        ..spoof_env(&fx)
    };
    list_in_config(&fx, &[&fx.project, &iris]);
    assert!(scoped(&far, &no_project), "a readable config lists iris");
    let malformed = format!("[supervisor]\nprojects = [\"{}\"\n", iris.display());
    std::fs::write(&fx.anchor, malformed).expect("write malformed config");
    let unreadable = |fx: &Fixture| {
        std::fs::remove_file(&fx.anchor).expect("remove config");
        std::fs::create_dir(&fx.anchor).expect("a directory where the config is");
    };
    for broken in ["malformed", "unreadable"] {
        if broken == "unreadable" {
            unreadable(&fx);
        }
        assert!(!scoped(&far, &spoof_env(&fx)), "{broken}: iris in scope");
        assert!(scoped(&near, &spoof_env(&fx)), "{broken}: the project dir");
        assert!(!scoped(&near, &no_project), "{broken}: no project dir");
    }
}

#[test]
fn the_envfile_exemption_holds_under_each_bypass() {
    let fx = fixture();
    let p = envfile(&fx, ".env.local", "A=1\n");
    let probes = Probes {
        git: &LiveGit,
        panes: &NoArchitect,
    };
    for command in exempt_commands(&p) {
        let call = bash(&fx, &command);
        let gate = ArchitectGate::new(&call, architect_env(&fx), || allowlist(&fx));
        assert_eq!(
            evaluate_floors(&call, &fx.project, &gate, &probes, true),
            None,
            "{command}"
        );
        assert!(gate.envfile().is_some(), "{command}");
        let gate = ArchitectGate::new(&call, pm_env(&fx), MpmConfig::default);
        let deny = evaluate_floors(&call, &fx.project, &gate, &probes, true);
        assert_eq!(deny.map(|d| d.rule), Some("secret-file-read"), "{command}");
    }
}

#[test]
fn a_subagent_of_the_architect_may_not_use_the_envfile_exemption() {
    let fx = fixture();
    let p = envfile(&fx, ".env.local", "A=1\n");
    let suffix = NotArchitect::Subagent.to_string();
    for command in exempt_commands(&p) {
        let mut call = bash(&fx, &command);
        call["agent_id"] = json!("agent-7");
        let (deny, recorded) = verdict(&fx, &call, architect_env(&fx), true);
        let deny = deny.expect("denied");
        assert!(deny.contains(&suffix), "{command}: {deny}");
        assert_eq!(recorded, None);
        let nested = HookEnv {
            sub_agent: true,
            ..architect_env(&fx)
        };
        let deny = verdict(&fx, &bash(&fx, &command), nested, true).0;
        assert!(deny.expect("denied").contains(&suffix), "{command}");
    }
}

#[test]
fn a_pm_may_not_use_the_envfile_exemption() {
    let fx = fixture();
    let p = envfile(&fx, ".env.local", "A=1\n");
    let suffix = NotArchitect::NoSupervisorStamp.to_string();
    for command in exempt_commands(&p) {
        let env = HookEnv {
            project_dir: Some(fx.project.clone().into_os_string()),
            ..pm_env(&fx)
        };
        let deny = verdict(&fx, &bash(&fx, &command), env, false).0;
        assert!(deny.expect("denied").contains(&suffix), "{command}");
    }
}

#[test]
fn every_identity_failure_denies_the_envfile_exemption() {
    for (fx, case) in cases() {
        // The file is in the Architect's project, which is either
        // `CLAUDE_PROJECT_DIR` or, when `granted`, listed in the config; so the
        // shape matches in every case and the deny must name the identity reason.
        let p = envfile(&fx, ".env.local", "A=1\n");
        let mut call = case.call.clone();
        call["tool_name"] = json!("Bash");
        call["tool_input"] = json!({ "command": format!("tm env keys {}", p.display()) });
        let (deny, recorded) = verdict(&fx, &call, case.env.clone(), case.granted);
        match case.want {
            Ok(()) => assert_eq!(deny, None, "{}", case.name),
            Err(why) => {
                let deny = deny.unwrap_or_else(|| panic!("{} allowed", case.name));
                assert!(deny.contains(&why.to_string()), "{}: {deny}", case.name);
                assert_eq!(recorded, None, "{}", case.name);
            }
        }
    }
}

#[test]
fn the_architect_main_thread_still_may_not_print_an_env_value() {
    let fx = fixture();
    let p = envfile(&fx, ".env.local", "A=1\n");
    let tfvars = envfile(&fx, "prod.tfvars", "a = 1\n");
    let elsewhere = fx.home.parent().expect("scratch").join("elsewhere");
    std::fs::create_dir_all(&elsewhere).expect("mkdir");
    let outside = elsewhere.join(".env.local");
    std::fs::write(&outside, "A=1\n").expect("write");
    let link = fx.project.join(".env.link");
    std::os::unix::fs::symlink(&p, &link).expect("symlink");
    std::fs::create_dir_all(fx.project.join(".env.d")).expect("mkdir");
    let (p, t, o, l) = (
        p.display(),
        tfvars.display(),
        outside.display(),
        link.display(),
    );
    let d = fx.project.join(".env.d");
    let m = fx.project.join(".env.missing");
    let (d, m) = (d.display(), m.display());
    let commands = [
        format!("cat {p}"),
        format!("sed -n 1p {p}"),
        format!("grep -o '^[A-Z_]*=.*' {p}"),
        format!("cut -d= -f2 {p}"),
        format!("source {p} && env"),
        format!("set -a; . {p}; env"),
        format!("echo A=2 >> {p}"),
        format!("tm env keys {p} | cat"),
        format!("./tm env keys {p}"),
        format!("/usr/local/bin/tm env keys {p}"),
        format!("sh -c 'tm env keys {p}'"),
        format!("sudo tm env keys {p}"),
        format!("env tm env keys {p}"),
        format!("X=1 tm env keys {p}"),
        format!("tm env keys $(echo {p})"),
        format!("tm env keys {p} > /tmp/out"),
        format!("tm env keys {p}; cat {p}"),
        format!("tm env keys {p} --json"),
        format!("tm env keys {t}"),
        format!("tm env keys {o}"),
        format!("tm env keys {l}"),
        format!("tm env keys {d}"),
        format!("tm env keys {m}"),
        format!("tm env set {p} API_KEY=value"),
        format!("tm env set {p} API_KEY value"),
        format!("tm env set {p} API_KEY --from-keychain iris"),
        format!("tm env set {p} API_KEY --from-keychain iris --account a --verbose"),
        format!("tm env set {p} 1KEY"),
        format!("tm env set {o} API_KEY"),
        "gh auth token".to_owned(),
    ];
    for command in &commands {
        assert!(architect(&fx, command).is_some(), "allowed: {command}");
    }
    let path = p.to_string();
    let tools = [
        ("Read", json!({ "file_path": path })),
        (
            "Read",
            json!({ "file_path": path, "offset": 1, "limit": 1 }),
        ),
        ("Grep", json!({ "pattern": "A", "path": path })),
        (
            "Edit",
            json!({ "file_path": path, "old_string": "A", "new_string": "B" }),
        ),
        ("MultiEdit", json!({ "file_path": path, "edits": [] })),
        ("Write", json!({ "file_path": path, "content": "A=2\n" })),
        (
            "Write",
            json!({ "file_path": m.to_string(), "content": "A=2\n" }),
        ),
    ];
    for (tool, input) in tools {
        let call = payload(&fx, tool, input.clone());
        let deny = verdict(&fx, &call, architect_env(&fx), true).0;
        assert!(deny.is_some(), "allowed: {tool} {input}");
    }
}

#[test]
fn the_value_rules_run_before_the_envfile_exemption() {
    let fx = fixture();
    let agents = fx.project.join("LaunchAgents");
    std::fs::create_dir_all(&agents).expect("mkdir");
    // A launchd-directory file the plist rule reads and cannot judge: it denies.
    let p = agents.join(".env.local");
    let plist = "<plist><dict><key>EnvironmentVariables</key><dict><key>X";
    std::fs::write(&p, plist).expect("write");
    for command in [
        format!("tm env keys {}", p.display()),
        "tm env set .env.local X $(gh auth token)".to_owned(),
    ] {
        let (deny, recorded) = verdict(&fx, &bash(&fx, &command), architect_env(&fx), true);
        assert!(deny.is_some(), "allowed: {command}");
        assert_eq!(recorded, None, "{command}");
    }
}

#[test]
fn an_exempted_call_is_audited_with_the_path_and_key_names_only() {
    let fx = fixture();
    let p = envfile(&fx, ".env.local", "API_KEY=x\n");
    let call = bash(&fx, &format!("tm env set {} API_KEY", p.display()));
    let ctx = DenyContext::from_payload("http://127.0.0.1:1", &call);
    let gate = ArchitectGate::new(&call, architect_env(&fx), || allowlist(&fx));
    assert_eq!(
        evaluate_secret_file_read_gated("Bash", call.get("tool_input"), &fx.project, &gate),
        None
    );
    let body = allow_audit_body(&ctx, &gate).expect("an allow line");
    let line = &body["payload"];
    assert_eq!(line["pm_guard_decision"], "allow");
    assert_eq!(line["pm_guard_rule"], ENVFILE_RULE);
    assert_eq!(line["path"], p.display().to_string());
    assert_eq!(line["keys"], json!(["API_KEY"]));
    assert!(!body.to_string().contains("API_KEY=x"), "{body}");
    // A denied call writes no allow line.
    let gate = ArchitectGate::new(&call, pm_env(&fx), MpmConfig::default);
    assert!(
        evaluate_secret_file_read_gated("Bash", call.get("tool_input"), &fx.project, &gate)
            .is_some()
    );
    assert_eq!(allow_audit_body(&ctx, &gate), None);
}
