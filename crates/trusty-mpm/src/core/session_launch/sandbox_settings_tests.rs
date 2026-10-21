//! Tests for the #8928 step 5 per-role sandbox settings generator.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::{
    DENIED_ENV_VARS, SandboxInputs, SandboxRole, SandboxSettingsError, project_policy, user_policy,
    verify_sandbox_settings, write_sandbox_settings,
};

/// A scratch workspace, home and user config dir.
struct Fx {
    _dir: TempDir,
    inputs: SandboxInputs,
    user: PathBuf,
}

impl Fx {
    fn new() -> Self {
        let dir = TempDir::new().expect("tempdir");
        let root = std::fs::canonicalize(dir.path()).expect("canonical");
        let mut inputs = SandboxInputs::new(root.join("ws"), root.join("home"));
        inputs.tmux_socket = Some(PathBuf::from("/private/tmp/tmux-502/tm-ws-abc"));
        inputs.mpm_socket = Some(root.join("Application Support/trusty-mpm.sock"));
        inputs.cargo_target_dir = Some(root.join("home/.trusty-tools/cargo-target/o/r"));
        std::fs::create_dir_all(inputs.workspace.join(".claude")).expect("mkdir ws");
        let user = root.join("home/.trusty-tools/trusty-mpm/claude-config");
        Self {
            _dir: dir,
            inputs,
            user,
        }
    }

    fn file(&self, name: &str) -> PathBuf {
        self.inputs.workspace.join(".claude").join(name)
    }
}

fn read(path: &Path) -> Value {
    let text = std::fs::read_to_string(path).expect("read settings");
    serde_json::from_str(&text).expect("json")
}

fn strings(value: &Value, pointer: &str) -> Vec<String> {
    value
        .pointer(pointer)
        .and_then(Value::as_array)
        .unwrap_or_else(|| panic!("{pointer} is an array in {value}"))
        .iter()
        .map(|v| v.as_str().expect("string").to_owned())
        .collect()
}

fn refused(result: Result<(), SandboxSettingsError>, what: &str) {
    match result {
        Err(SandboxSettingsError::Refused(why)) => assert!(why.contains("#8928"), "{why}"),
        other => panic!("{what} must refuse the launch, got {other:?}"),
    }
}

#[test]
fn the_pm_policy_names_every_socket_and_no_local_binding() {
    let fx = Fx::new();
    let policy = project_policy(SandboxRole::Pm, &fx.inputs);
    let sandbox = &policy["sandbox"];
    assert_eq!(sandbox["enabled"], json!(true));
    assert_eq!(sandbox["allowUnsandboxedCommands"], json!(false));
    assert_eq!(sandbox["failIfUnavailable"], json!(true));
    let sockets = strings(&policy, "/sandbox/network/allowUnixSockets");
    let want = [&fx.inputs.tmux_socket, &fx.inputs.mpm_socket];
    for path in want.into_iter().flatten() {
        assert!(sockets.contains(&path.display().to_string()), "{sockets:?}");
    }
    // The socket path with a space is kept verbatim, as one entry.
    assert!(sockets.iter().any(|s| s.contains("Application Support")));
    assert!(
        sandbox["network"].get("allowLocalBinding").is_none(),
        "the tm CLI uses the daemon socket, never a local port"
    );
    assert!(
        sandbox["network"].get("strictAllowlist").is_none(),
        "user tier only"
    );
    let allow_write = strings(&policy, "/sandbox/filesystem/allowWrite");
    let target = fx.inputs.cargo_target_dir.as_ref().expect("target");
    assert!(allow_write.contains(&target.display().to_string()));
    // Exact files only — never a blanket `.claude/**` rule (#8928).
    let deny = strings(&policy, "/permissions/deny");
    assert!(deny.iter().all(|d| !d.contains(".claude/**")), "{deny:?}");
    let settings = format!("Edit(/{})", fx.file("settings.local.json").display());
    assert!(deny.contains(&settings), "{deny:?}");
    assert!(
        deny.iter().all(|d| d.contains("(//")),
        "absolute rules: {deny:?}"
    );
}

#[test]
fn the_agent_policy_has_no_tmux_socket() {
    let fx = Fx::new();
    let pm = project_policy(SandboxRole::Pm, &fx.inputs);
    let agent = project_policy(SandboxRole::Agent, &fx.inputs);
    let sockets = strings(&agent, "/sandbox/network/allowUnixSockets");
    let tmux = fx.inputs.tmux_socket.as_ref().expect("tmux");
    assert!(
        !sockets.contains(&tmux.display().to_string()),
        "{sockets:?}"
    );
    assert_eq!(sockets.len(), 1, "{sockets:?}");
    // Everything else matches the PM policy.
    let mut pm_rest = pm.clone();
    pm_rest["sandbox"]["network"]["allowUnixSockets"] = json!(null);
    let mut agent_rest = agent;
    agent_rest["sandbox"]["network"]["allowUnixSockets"] = json!(null);
    assert_eq!(pm_rest, agent_rest);
}

#[test]
fn the_architect_policy_disables_the_sandbox() {
    let fx = Fx::new();
    let policy = project_policy(SandboxRole::Architect, &fx.inputs);
    assert_eq!(policy, json!({ "sandbox": { "enabled": false } }));
    assert_eq!(user_policy(SandboxRole::Architect), None);
    write_sandbox_settings(SandboxRole::Architect, &fx.inputs, &fx.user).expect("write");
    assert!(
        !fx.user.join("settings.json").exists(),
        "no user-tier write"
    );
}

#[test]
fn the_user_policy_carries_strict_allowlist_and_credentials() {
    let policy = user_policy(SandboxRole::Agent).expect("agent user policy");
    assert_eq!(
        policy.pointer("/sandbox/network/strictAllowlist"),
        Some(&json!(true))
    );
    let env = policy
        .pointer("/sandbox/credentials/envVars")
        .and_then(Value::as_array)
        .expect("envVars");
    assert_eq!(env.len(), DENIED_ENV_VARS.len());
    assert!(env.iter().all(|e| e["mode"] == json!("deny")));
}

#[test]
fn writing_preserves_unrelated_keys_and_verifies() {
    let fx = Fx::new();
    let existing = json!({
        "hooks": { "Stop": [] },
        "permissions": { "allow": ["Bash(ls:*)"], "deny": ["Read(//x)"] },
        "sandbox": { "operatorNote": { "kept": true } },
    });
    std::fs::write(fx.file("settings.json"), existing.to_string()).expect("seed");
    std::fs::create_dir_all(&fx.user).expect("mkdir user");
    std::fs::write(fx.user.join("settings.json"), r#"{"model":"opus"}"#).expect("seed user");
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("write");
    let project = read(&fx.file("settings.json"));
    assert_eq!(project["hooks"], json!({ "Stop": [] }));
    assert_eq!(project["permissions"]["allow"], json!(["Bash(ls:*)"]));
    let deny = strings(&project, "/permissions/deny");
    assert_eq!(deny[0], "Read(//x)", "operator entries kept first");
    assert_eq!(project["sandbox"]["operatorNote"], json!({ "kept": true }));
    assert_eq!(project["sandbox"]["enabled"], json!(true));
    let user = read(&fx.user.join("settings.json"));
    assert_eq!(user["model"], json!("opus"));
    assert_eq!(
        user.pointer("/sandbox/network/strictAllowlist"),
        Some(&json!(true))
    );
    // A second write adds no duplicate deny entry.
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("rewrite");
    let again = strings(&read(&fx.file("settings.json")), "/permissions/deny");
    assert_eq!(again, deny);
}

#[test]
fn a_local_override_refuses_the_launch() {
    let fx = Fx::new();
    std::fs::write(
        fx.file("settings.local.json"),
        r#"{"sandbox":{"enabled":false}}"#,
    )
    .expect("seed local");
    refused(
        write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user),
        "a local sandbox override",
    );
    // A local file with no sandbox key is fine.
    std::fs::write(fx.file("settings.local.json"), r#"{"model":"x"}"#).expect("local");
    write_sandbox_settings(SandboxRole::Agent, &fx.inputs, &fx.user).expect("allowed");
}

#[test]
fn a_weakened_project_file_refuses_the_launch() {
    let fx = Fx::new();
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("write");
    let path = fx.file("settings.json");
    for (pointer, value) in [
        ("/sandbox/enabled", json!(false)),
        ("/sandbox/allowUnsandboxedCommands", json!(true)),
    ] {
        let mut project = read(&path);
        *project.pointer_mut(pointer).expect("key") = value;
        std::fs::write(&path, project.to_string()).expect("weaken");
        refused(
            verify_sandbox_settings(SandboxRole::Pm, &fx.inputs.workspace, &fx.user),
            pointer,
        );
        write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("restore");
    }
    // The user tier without strictAllowlist refuses too.
    std::fs::write(fx.user.join("settings.json"), "{}").expect("weaken user");
    refused(
        verify_sandbox_settings(SandboxRole::Agent, &fx.inputs.workspace, &fx.user),
        "no strictAllowlist",
    );
    // The Architect's file must disable the sandbox.
    refused(
        verify_sandbox_settings(SandboxRole::Architect, &fx.inputs.workspace, &fx.user),
        "an Architect with the sandbox on",
    );
}

#[test]
fn unreadable_settings_refuse_the_launch() {
    let fx = Fx::new();
    // No project file at all.
    let missing = verify_sandbox_settings(SandboxRole::Pm, &fx.inputs.workspace, &fx.user);
    assert!(
        matches!(missing, Err(SandboxSettingsError::Unreadable { .. })),
        "{missing:?}"
    );
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("write");
    for (file, body) in [
        (fx.file("settings.local.json"), "{ not json"),
        (fx.file("settings.local.json"), "[]"),
        (fx.user.join("settings.json"), "{ not json"),
    ] {
        let before = std::fs::read_to_string(&file).ok();
        std::fs::write(&file, body).expect("corrupt");
        let result = verify_sandbox_settings(SandboxRole::Pm, &fx.inputs.workspace, &fx.user);
        assert!(
            matches!(result, Err(SandboxSettingsError::Unreadable { .. })),
            "{} = {body:?}: {result:?}",
            file.display()
        );
        match before {
            Some(text) => std::fs::write(&file, text).expect("restore"),
            None => std::fs::remove_file(&file).expect("remove"),
        }
    }
}

/// #8928 fix round: the sccache server runs the compiler a client names,
/// outside the sandbox, so no sandboxed role may reach its socket, and the
/// sandboxed env turns the wrapper off.
#[test]
fn sandboxed_roles_never_reach_the_sccache_socket() {
    let fx = Fx::new();
    for role in [SandboxRole::Pm, SandboxRole::Agent] {
        let policy = project_policy(role, &fx.inputs);
        let sockets = strings(&policy, "/sandbox/network/allowUnixSockets");
        assert!(
            sockets.iter().all(|s| !s.ends_with("sccache.sock")),
            "{role:?}: {sockets:?}"
        );
        assert_eq!(
            policy.pointer("/env/RUSTC_WRAPPER"),
            Some(&json!("")),
            "{role:?} runs with no rustc wrapper"
        );
    }
}

/// #8928 fix round: the merge unions the deny lists and `envVars` with what
/// the operator wrote; tm's own scalars still win.
#[test]
fn operator_denies_and_env_vars_survive_a_merge() {
    let fx = Fx::new();
    let project = json!({ "sandbox": {
        "enabled": false,
        "filesystem": { "denyRead": ["/op/secret"], "denyWrite": ["/op/frozen"] },
        "network": { "operatorKey": 1 },
    }});
    std::fs::write(fx.file("settings.json"), project.to_string()).expect("seed project");
    std::fs::create_dir_all(&fx.user).expect("mkdir user");
    let user = json!({ "sandbox": { "credentials": { "envVars": [
        { "name": "OP_TOKEN", "mode": "deny" }
    ]}}});
    std::fs::write(fx.user.join("settings.json"), user.to_string()).expect("seed user");
    write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("write");
    let project = read(&fx.file("settings.json"));
    let deny_read = strings(&project, "/sandbox/filesystem/denyRead");
    assert!(
        deny_read.contains(&"/op/secret".to_owned()),
        "{deny_read:?}"
    );
    let ssh = fx.inputs.home.join(".ssh").display().to_string();
    assert!(deny_read.contains(&ssh), "{deny_read:?}");
    let deny_write = strings(&project, "/sandbox/filesystem/denyWrite");
    assert!(
        deny_write.contains(&"/op/frozen".to_owned()),
        "{deny_write:?}"
    );
    assert_eq!(
        project.pointer("/sandbox/network/operatorKey"),
        Some(&json!(1))
    );
    assert_eq!(
        project.pointer("/sandbox/enabled"),
        Some(&json!(true)),
        "tm wins"
    );
    let user = read(&fx.user.join("settings.json"));
    let names: Vec<&str> = user
        .pointer("/sandbox/credentials/envVars")
        .and_then(Value::as_array)
        .expect("envVars")
        .iter()
        .filter_map(|e| e["name"].as_str())
        .collect();
    assert!(names.contains(&"OP_TOKEN"), "{names:?}");
    assert!(names.contains(&"ANTHROPIC_API_KEY"), "{names:?}");
}

/// #8928 fix round: `excludedCommands` runs a command outside the sandbox even
/// with `allowUnsandboxedCommands: false`, so either tier setting it refuses.
#[test]
fn excluded_commands_refuse_the_launch() {
    let fx = Fx::new();
    let project = json!({ "sandbox": { "excludedCommands": ["docker"] } });
    std::fs::write(fx.file("settings.json"), project.to_string()).expect("seed project");
    match write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user) {
        Err(SandboxSettingsError::Refused(why)) => {
            assert!(why.contains("excludedCommands"), "{why}");
            assert!(why.contains("docker"), "{why}");
        }
        other => panic!("project excludedCommands must refuse, got {other:?}"),
    }
    std::fs::write(fx.file("settings.json"), "{}").expect("reset project");
    std::fs::create_dir_all(&fx.user).expect("mkdir user");
    let user = json!({ "sandbox": { "excludedCommands": ["gh *"] } });
    std::fs::write(fx.user.join("settings.json"), user.to_string()).expect("seed user");
    refused(
        write_sandbox_settings(SandboxRole::Agent, &fx.inputs, &fx.user),
        "user-tier excludedCommands",
    );
    // An empty list excludes nothing.
    std::fs::write(
        fx.user.join("settings.json"),
        r#"{"sandbox":{"excludedCommands":[]}}"#,
    )
    .expect("empty");
    write_sandbox_settings(SandboxRole::Agent, &fx.inputs, &fx.user).expect("allowed");
}

/// #10107: verify the role's sandbox for `fx`.
fn verify(fx: &Fx, role: SandboxRole) -> Result<(), SandboxSettingsError> {
    verify_sandbox_settings(role, &fx.inputs.workspace, &fx.user)
}

/// Which settings file a [`Case`] edits.
#[derive(Debug, Clone, Copy)]
enum Tier {
    Project,
    Local,
    User,
}

/// One #10107 shape: a key set at a JSON pointer in one tier's file.
struct Case {
    what: &'static str,
    tier: Tier,
    pointer: &'static str,
    value: Value,
}

fn case(what: &'static str, tier: Tier, pointer: &'static str, value: Value) -> Case {
    Case {
        what,
        tier,
        pointer,
        value,
    }
}

impl Fx {
    /// The settings file `tier` names.
    fn tier_file(&self, tier: Tier) -> PathBuf {
        match tier {
            Tier::Project => self.file("settings.json"),
            Tier::Local => self.file("settings.local.json"),
            Tier::User => self.user.join("settings.json"),
        }
    }

    /// Set `pointer` to `value` in `tier`'s file, creating objects on the way.
    fn set(&self, tier: Tier, pointer: &str, value: Value) {
        let path = self.tier_file(tier);
        let mut settings = std::fs::read_to_string(&path)
            .ok()
            .and_then(|t| serde_json::from_str::<Value>(&t).ok())
            .unwrap_or_else(|| json!({}));
        let mut slot = &mut settings;
        for key in pointer.trim_start_matches('/').split('/') {
            if !slot.is_object() {
                *slot = json!({});
            }
            slot = &mut slot[key];
        }
        *slot = value;
        std::fs::write(&path, settings.to_string()).expect("write tier");
    }
}

/// Run every case on a fresh, verified PM sandbox, and return the ones whose
/// verify result was not `want_refused`, so one run reports every miss.
/// `home`, when set, replaces the inputs' home after the baseline write.
fn misses(cases: Vec<Case>, want_refused: bool, home: Option<PathBuf>) -> Vec<String> {
    let mut wrong = Vec::new();
    for c in cases {
        let mut fx = Fx::new();
        write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user).expect("baseline");
        if let Some(home) = &home {
            fx.inputs.home = home.clone();
        }
        fx.set(c.tier, c.pointer, c.value.clone());
        let result = verify(&fx, SandboxRole::Pm);
        let refused = matches!(&result, Err(SandboxSettingsError::Refused(_)));
        if refused != want_refused {
            wrong.push(format!(
                "{} ({:?} {} = {}): {result:?}",
                c.what, c.tier, c.pointer, c.value
            ));
        }
    }
    wrong
}

/// #10107 item 1: a sandbox-widening key in the user tier, which the
/// key-by-key merge keeps, or in the project tier, refuses the launch.
#[test]
fn widening_sandbox_keys_refuse_the_launch() {
    use Tier::{Project, User};
    let sccache = "/Users/x/Library/Caches/Mozilla.sccache/sccache.sock";
    let net = "/sandbox/network";
    let cases = vec![
        case(
            "user allowAllUnixSockets",
            User,
            "/sandbox/network/allowAllUnixSockets",
            json!(true),
        ),
        case(
            "user allowLocalBinding",
            User,
            "/sandbox/network/allowLocalBinding",
            json!(true),
        ),
        case(
            "user filesystem.disabled",
            User,
            "/sandbox/filesystem/disabled",
            json!(true),
        ),
        case(
            "user allowRead",
            User,
            "/sandbox/filesystem/allowRead",
            json!(["/Users/x/.ssh"]),
        ),
        case(
            "user allowUnixSockets re-adding sccache",
            User,
            "/sandbox/network/allowUnixSockets",
            json!([sccache]),
        ),
        case(
            "user allowedDomains",
            User,
            "/sandbox/network/allowedDomains",
            json!(["evil.example"]),
        ),
        case(
            "user allowWrite",
            User,
            "/sandbox/filesystem/allowWrite",
            json!(["/Users/x/.cargo/bin"]),
        ),
        case(
            "user enabled: false",
            User,
            "/sandbox/enabled",
            json!(false),
        ),
        case("user network as a non-object", User, net, json!("open")),
        case(
            "project allowAllUnixSockets",
            Project,
            "/sandbox/network/allowAllUnixSockets",
            json!(true),
        ),
        case(
            "project allowLocalBinding",
            Project,
            "/sandbox/network/allowLocalBinding",
            json!(true),
        ),
        case(
            "project filesystem.disabled",
            Project,
            "/sandbox/filesystem/disabled",
            json!(true),
        ),
        case(
            "project allowRead",
            Project,
            "/sandbox/filesystem/allowRead",
            json!(["/Users/x/.aws"]),
        ),
        // Fail-closed: a value that is not `false` is not a `false`.
        case(
            "project allowLocalBinding as a string",
            Project,
            "/sandbox/network/allowLocalBinding",
            json!("yes"),
        ),
        case(
            "project allowRead as a string",
            Project,
            "/sandbox/filesystem/allowRead",
            json!("/Users/x/.aws"),
        ),
    ];
    let wrong = misses(cases, true, None);
    assert!(wrong.is_empty(), "not refused:\n{}", wrong.join("\n"));
}

/// #10107 item 1: the project policy pins the permissive booleans to `false`,
/// over an operator's `true`.
#[test]
fn the_project_policy_pins_the_permissive_booleans_off() {
    let fx = Fx::new();
    let seeded = json!({ "sandbox": {
        "allowAppleEvents": true,
        "network": { "allowAllUnixSockets": true, "allowLocalBinding": true },
        "filesystem": { "disabled": true },
    }});
    std::fs::write(fx.file("settings.json"), seeded.to_string()).expect("seed");
    let written = write_sandbox_settings(SandboxRole::Pm, &fx.inputs, &fx.user);
    let project = read(&fx.file("settings.json"));
    let mut wrong = Vec::new();
    for pointer in [
        "/sandbox/allowAppleEvents",
        "/sandbox/network/allowAllUnixSockets",
        "/sandbox/network/allowLocalBinding",
        "/sandbox/filesystem/disabled",
    ] {
        if project.pointer(pointer) != Some(&json!(false)) {
            wrong.push(format!("{pointer} = {:?}", project.pointer(pointer)));
        }
    }
    assert!(wrong.is_empty(), "not pinned false:\n{}", wrong.join("\n"));
    written.expect("the pinned policy verifies");
}

/// #10107 item 2: Apple Events, and an edit-tool allow rule outside the
/// workspace and the `allowWrite` roots, refuse the launch in any tier.
#[test]
fn apple_events_and_outside_edit_rules_refuse_the_launch() {
    use Tier::{Local, Project, User};
    let allow = "/permissions/allow";
    let cases = vec![
        case(
            "user allowAppleEvents",
            User,
            "/sandbox/allowAppleEvents",
            json!(true),
        ),
        case(
            "project allowAppleEvents",
            Project,
            "/sandbox/allowAppleEvents",
            json!(true),
        ),
        case(
            "project Edit(~/.cargo/bin/**)",
            Project,
            allow,
            json!(["Bash(ls:*)", "Edit(~/.cargo/bin/**)"]),
        ),
        case(
            "project Edit(//usr/local/bin/**)",
            Project,
            allow,
            json!(["Edit(//usr/local/bin/**)"]),
        ),
        case(
            "local Edit(~/.cargo/bin/tm)",
            Local,
            allow,
            json!(["Edit(~/.cargo/bin/tm)"]),
        ),
        case(
            "user Edit(//etc/**)",
            User,
            allow,
            json!(["Edit(//etc/**)"]),
        ),
        case(
            "project Edit(../escape/**)",
            Project,
            allow,
            json!(["Edit(../escape/**)"]),
        ),
        case(
            "project Edit(/src/../../escape)",
            Project,
            allow,
            json!(["Edit(/src/../../escape)"]),
        ),
        case("project Edit(//**)", Project, allow, json!(["Edit(//**)"])),
        case("project bare Edit", Project, allow, json!(["Edit"])),
        case(
            "project Write(~/.cargo/bin/**)",
            Project,
            allow,
            json!(["Write(~/.cargo/bin/**)"]),
        ),
        case(
            "project Edit(~bob/x)",
            Project,
            allow,
            json!(["Edit(~bob/x)"]),
        ),
        // Fail-closed: an allow list verify cannot read refuses.
        case(
            "project permissions.allow as a string",
            Project,
            allow,
            json!("Edit(//**)"),
        ),
        case("project non-string allow rule", Project, allow, json!([7])),
    ];
    let wrong = misses(cases, true, None);
    assert!(wrong.is_empty(), "not refused:\n{}", wrong.join("\n"));
}

/// #10107 item 2: an edit-tool allow rule inside the workspace or an
/// `allowWrite` root, and non-edit rules, still verify — the refusal is not
/// blanket.
#[test]
fn inside_edit_rules_still_verify() {
    use Tier::{Local, Project};
    let allow = "/permissions/allow";
    let cases = vec![
        case(
            "project Edit(/src/**)",
            Project,
            allow,
            json!(["Edit(/src/**)"]),
        ),
        case(
            "project relative rules",
            Project,
            allow,
            json!(["Edit(docs/*.md)", "Edit(./notes/**)", "Edit(*.rs)"]),
        ),
        case(
            "project Edit(~/.cargo/registry/**), an allowWrite root",
            Project,
            allow,
            json!(["Edit(~/.cargo/registry/**)"]),
        ),
        case("local Edit(/**)", Local, allow, json!(["Edit(/**)"])),
        case(
            "non-edit rules",
            Project,
            allow,
            json!([
                "Bash(ls:*)",
                "Read(//etc/**)",
                "WebFetch(domain:github.com)"
            ]),
        ),
        case(
            "project allowAppleEvents: false",
            Project,
            "/sandbox/allowAppleEvents",
            json!(false),
        ),
    ];
    let wrong = misses(cases, false, None);
    assert!(wrong.is_empty(), "wrongly refused:\n{}", wrong.join("\n"));
}

/// #10107 fail-closed: a `~` rule with no absolute home to place it on
/// refuses rather than passing unchecked.
#[test]
fn an_edit_rule_with_no_home_to_place_it_refuses() {
    let cases = vec![case(
        "project Edit(~/.cargo/registry/**) with no home",
        Tier::Project,
        "/permissions/allow",
        json!(["Edit(~/.cargo/registry/**)"]),
    )];
    let wrong = misses(cases, true, Some(PathBuf::new()));
    assert!(wrong.is_empty(), "not refused:\n{}", wrong.join("\n"));
}
