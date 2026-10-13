//! Tests for the per-session `--settings` carrier: the deny set, the file,
//! and every fail-open arm (#8988 PR 1b).

use super::*;
use crate::core::launch_policy::compose::owner_file_path;
use tempfile::TempDir;

struct Fx {
    _tmp: TempDir,
    repos: PathBuf,
    state: PathBuf,
    project: PathBuf,
    claude: PathBuf,
}

impl Fx {
    /// A project under owner `acme` and a `.claude.json` naming `servers`.
    fn new(servers: &[&str]) -> Self {
        let tmp = TempDir::new().unwrap();
        let base = tmp.path().canonicalize().unwrap();
        let repos = base.join("repos");
        let project = repos.join("acme").join("widget");
        std::fs::create_dir_all(&project).unwrap();
        let claude = base.join("claude");
        std::fs::create_dir_all(&claude).unwrap();
        let map: serde_json::Map<String, Value> = servers
            .iter()
            .map(|s| ((*s).to_owned(), json!({"type": "stdio", "command": "x"})))
            .collect();
        std::fs::write(
            claude.join(".claude.json"),
            json!({ "mcpServers": map }).to_string(),
        )
        .unwrap();
        Self {
            state: base.join("state"),
            _tmp: tmp,
            repos,
            project,
            claude,
        }
    }
    fn owner(&self, body: &str) {
        std::fs::write(owner_file_path(&self.repos, "acme"), body).unwrap();
    }
    /// An owner file for another owner, e.g. an `extends` parent.
    fn owner_named(&self, owner: &str, body: &str) {
        std::fs::create_dir_all(self.repos.join(owner)).unwrap();
        std::fs::write(owner_file_path(&self.repos, owner), body).unwrap();
    }
    fn provision(&self) -> LaunchSettings {
        self.provision_with(Some(&self.claude))
    }
    fn provision_with(&self, claude_json_dir: Option<&Path>) -> LaunchSettings {
        provision_settings_at(&CarrierInputs {
            cwd: &self.project,
            repos_root: &self.repos,
            state_root: &self.state,
            claude_json_dir,
        })
    }
    /// Plant `acme`'s last-good record as raw JSON, as an older tm wrote it.
    fn plant_last_good(&self, policy: Value) {
        let path = super::super::last_good::last_good_path(&self.state, "acme");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        let record = json!({
            "version": 1,
            "owner": "acme",
            "chain": ["acme"],
            "policy": policy,
            "source_sha256": "planted",
            "recorded_at": "2026-10-01T00:00:00Z"
        });
        std::fs::write(path, record.to_string()).unwrap();
    }
    fn settings_dir(&self) -> PathBuf {
        self.state.join(SESSION_SETTINGS_DIR)
    }
}

fn body_of(settings: &LaunchSettings) -> Value {
    let file = settings.file.as_ref().expect("a settings file");
    serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap()
}

fn names(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| (*s).to_owned()).collect()
}

const USER: &[&str] = &["apex", "heygen", "trusty-search"];

#[test]
fn a_written_policy_is_one_settings_argv_pair() {
    let fx = Fx::new(USER);
    fx.owner("[mcp]\nallow = [\"apex\"]\n");
    let settings = fx.provision();
    let file = settings.file.clone().expect("a settings file");
    assert_eq!(
        settings.argv(),
        vec!["--settings".to_owned(), file.display().to_string()]
    );
    assert_eq!(
        body_of(&settings),
        json!({"deniedMcpServers": [{"serverName": "heygen"}]})
    );
    assert!(file.starts_with(fx.settings_dir()));
    assert!(!file.starts_with(&fx.project));
    assert_eq!(settings.notice, None);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
    }
}

#[test]
fn no_owner_file_writes_nothing_and_adds_no_argv() {
    let fx = Fx::new(USER);
    let settings = fx.provision();
    assert_eq!(settings, LaunchSettings::default());
    assert!(settings.argv().is_empty());
    assert!(!fx.settings_dir().exists());
}

#[test]
fn an_owner_policy_that_subtracts_nothing_adds_no_argv() {
    let fx = Fx::new(USER);
    fx.owner("[mcp]\nclaude_ai_connectors = true\n");
    assert!(fx.provision().argv().is_empty());
    assert!(!fx.settings_dir().exists());
}

#[test]
fn allow_empty_denies_every_non_floor_server() {
    let policy = OwnerPolicy {
        allow: Some(BTreeSet::new()),
        ..OwnerPolicy::default()
    };
    let user = names(&["apex", "claude-in-chrome", "heygen", "trusty-mpm"]);
    let deny = deny_set(&policy, Some(&user));
    assert_eq!(deny, names(&["apex", "heygen"]).into_iter().collect());
}

/// #8988 floor rule: names match exactly and case-sensitively, as Claude Code
/// matches `serverName`. A differently-cased deny is a valid file, is emitted
/// verbatim, and never names the floor server; a differently-cased allow
/// does not allow.
#[test]
fn a_differently_cased_name_is_another_server() {
    let fx = Fx::new(&["apex", "trusty-search", "Trusty-Search"]);
    fx.owner("[mcp]\nallow = [\"APEX\"]\ndeny = [\"Trusty-Search\", \"Claude-In-Chrome\"]\n");
    let settings = fx.provision();
    assert_eq!(
        body_of(&settings),
        json!({"deniedMcpServers": [
            {"serverName": "Claude-In-Chrome"},
            {"serverName": "Trusty-Search"},
            {"serverName": "apex"}
        ]})
    );
    assert_eq!(settings.notice, None, "a valid file, not a fallback");
}

#[test]
fn the_settings_body_carries_only_subtractive_keys() {
    let deny: BTreeSet<String> = names(&["heygen"]).into_iter().collect();
    for (deny, off) in [(&deny, true), (&deny, false), (&BTreeSet::new(), true)] {
        let body = settings_body(deny, off).expect("something to subtract");
        let value: Value = serde_json::from_str(&body).unwrap();
        let keys: Vec<&String> = value.as_object().unwrap().keys().collect();
        for key in keys {
            assert!(
                key == "deniedMcpServers" || key == "disableClaudeAiConnectors",
                "{key}"
            );
        }
        assert_ne!(value["disableClaudeAiConnectors"], json!(false));
    }
    assert_eq!(settings_body(&BTreeSet::new(), false), None);
}

#[test]
fn a_bad_file_launches_with_the_last_good_policy_and_a_loud_notice() {
    let fx = Fx::new(USER);
    fx.owner("[mcp]\ndeny = [\"heygen\"]\n");
    fx.provision();
    fx.owner("[mcp]\ndeny = [\"*\"]\n");
    let settings = fx.provision();
    assert_eq!(
        body_of(&settings),
        json!({"deniedMcpServers": [{"serverName": "heygen"}]})
    );
    let notice = settings.notice.expect("a notice");
    assert!(notice.contains("INVALID"), "{notice}");
    assert!(notice.contains("using last good policy"), "{notice}");
}

#[test]
fn a_bad_file_with_no_last_good_adds_no_argv_and_says_skipped() {
    let fx = Fx::new(USER);
    fx.owner("[agents]\n");
    let settings = fx.provision();
    assert!(settings.argv().is_empty());
    let notice = settings.notice.expect("a notice");
    assert!(notice.contains("owner layer skipped"), "{notice}");
}

/// Fail-Open Check: an unwritable settings file launches WITHOUT `--settings`,
/// and the notice says the policy was not applied — never a flag naming a file
/// that was not written.
#[test]
fn an_unwritable_settings_dir_launches_without_the_flag_and_says_so() {
    let fx = Fx::new(USER);
    fx.owner("[mcp]\nallow = []\n");
    std::fs::create_dir_all(&fx.state).unwrap();
    std::fs::write(fx.settings_dir(), "not a dir").unwrap();
    let settings = fx.provision();
    assert_eq!(settings.file, None);
    assert!(settings.argv().is_empty());
    let notice = settings.notice.expect("a notice");
    assert!(notice.contains("NOT applied"), "{notice}");
}

/// Fail-Open Check: a `.claude.json` that cannot be read keeps the explicit
/// denies and says the allow list could not be applied.
#[test]
fn an_unreadable_user_scope_keeps_the_explicit_denies_and_says_so() {
    let fx = Fx::new(USER);
    std::fs::write(fx.claude.join(".claude.json"), "{").unwrap();
    fx.owner("[mcp]\nallow = [\"apex\"]\ndeny = [\"heygen\"]\n");
    let settings = fx.provision();
    assert_eq!(
        body_of(&settings),
        json!({"deniedMcpServers": [{"serverName": "heygen"}]})
    );
    let notice = settings.notice.expect("a notice");
    assert!(notice.contains("could not be applied"), "{notice}");
}

/// Fail-Open Check: a deleted file whose record cannot be retired still
/// launches with no owner layer, and the notice names the stale record.
#[cfg(unix)]
#[test]
fn a_failed_retire_is_named_in_the_launch_notice() {
    use std::os::unix::fs::PermissionsExt;
    let fx = Fx::new(USER);
    fx.owner("[mcp]\ndeny = [\"heygen\"]\n");
    fx.provision();
    std::fs::remove_file(owner_file_path(&fx.repos, "acme")).unwrap();
    let dir = fx.state.join(super::super::last_good::LAST_GOOD_DIR);
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o500)).unwrap();
    let settings = fx.provision();
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    assert!(settings.argv().is_empty());
    let notice = settings.notice.expect("a notice");
    assert!(notice.contains("could not be retired"), "{notice}");
}

#[test]
fn an_unrelocated_launch_reads_the_operators_claude_json() {
    let dir = Path::new("/relocated");
    assert_eq!(claude_json_dir(Some(dir)), Some(dir.to_path_buf()));
    let expected =
        crate::core::session_record::claude_config_dir_override().or_else(dirs::home_dir);
    assert_eq!(claude_json_dir(None), expected);
}

#[test]
fn shell_words_quote_the_path() {
    let settings = LaunchSettings {
        file: Some(PathBuf::from("/h/with space/s.json")),
        notice: None,
    };
    assert_eq!(
        settings.shell_words(),
        " '--settings' '/h/with space/s.json'"
    );
    assert_eq!(LaunchSettings::default().shell_words(), "");
}

// #8988: two launches in one cwd that read different `.claude.json` files get
// different allow-subtractions; neither may overwrite the other's file.
#[test]
fn launches_in_one_cwd_with_different_claude_json_get_different_files() {
    let fx = Fx::new(&["apex", "heygen"]);
    fx.owner("[mcp]\nallow = [\"apex\"]\n");
    let first = fx.provision();
    let first_body = body_of(&first);
    assert_eq!(
        first_body,
        json!({"deniedMcpServers": [{"serverName": "heygen"}]})
    );

    // A second launch whose `claude` reads a `.claude.json` with other servers.
    std::fs::write(
        fx.claude.join(".claude.json"),
        json!({ "mcpServers": { "apex": {}, "slack": {} } }).to_string(),
    )
    .unwrap();
    let second = fx.provision();

    assert_ne!(first.file, second.file, "one name for two policies");
    assert_eq!(
        body_of(&second),
        json!({"deniedMcpServers": [{"serverName": "slack"}]})
    );
    // The first launch's file still holds the first launch's policy.
    assert_eq!(body_of(&first), first_body);
}

#[test]
fn identical_policies_in_one_cwd_share_one_file() {
    let fx = Fx::new(USER);
    fx.owner("[mcp]\nallow = [\"apex\"]\n");
    assert_eq!(fx.provision().file, fx.provision().file);
    assert_eq!(std::fs::read_dir(fx.settings_dir()).unwrap().count(), 1);
}

/// Every key, at any depth, of a settings body; and every string value.
fn keys_and_strings(value: &Value, keys: &mut Vec<String>, strings: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            for (k, v) in map {
                keys.push(k.clone());
                keys_and_strings(v, keys, strings);
            }
        }
        Value::Array(items) => items
            .iter()
            .for_each(|v| keys_and_strings(v, keys, strings)),
        Value::String(s) => strings.push(s.clone()),
        _ => {}
    }
}

/// #8992 Q-A: an owner may only turn skills off, so every `skillOverrides`
/// value the carrier writes is `"off"` — never `"on"`.
#[test]
fn the_carrier_never_emits_on() {
    let fx = Fx::new(USER);
    fx.owner("[skills]\noff = [\"xlsx\", \"breeze-voice\"]\n");
    let settings = fx.provision();
    assert_eq!(
        body_of(&settings),
        json!({"skillOverrides": {"breeze-voice": "off", "xlsx": "off"}})
    );
    assert_eq!(settings.notice, None);
}

/// #8992: `[tools] deny` travels as `permissions.deny` in the same file.
#[test]
fn an_owner_tools_deny_writes_permissions_deny() {
    let fx = Fx::new(USER);
    fx.owner("[tools]\ndeny = [\"mcp__heygen\", \"NotebookEdit\"]\n");
    assert_eq!(
        body_of(&fx.provision()),
        json!({"permissions": {"deny": ["NotebookEdit", "mcp__heygen"]}})
    );
}

/// #7422 trust gate, extended by #8992: with every table set, the file holds
/// only subtractive keys — no `permissions.allow`, no `ask`, no `"on"`.
#[test]
fn the_settings_file_never_carries_an_allow_on_or_ask() {
    let fx = Fx::new(USER);
    fx.owner(
        "[mcp]\nallow = [\"apex\"]\ndeny = [\"slack\"]\nclaude_ai_connectors = false\n\
         [skills]\noff = [\"xlsx\"]\n[tools]\ndeny = [\"NotebookEdit\"]\n",
    );
    let body = body_of(&fx.provision());
    assert_eq!(
        body,
        json!({
            "deniedMcpServers": [{"serverName": "heygen"}, {"serverName": "slack"}],
            "disableClaudeAiConnectors": true,
            "permissions": {"deny": ["NotebookEdit"]},
            "skillOverrides": {"xlsx": "off"}
        })
    );
    let (mut keys, mut strings) = (Vec::new(), Vec::new());
    keys_and_strings(&body, &mut keys, &mut strings);
    for key in &keys {
        let lower = key.to_ascii_lowercase();
        assert!(
            !lower.contains("allow") && lower != "ask",
            "the settings file carries `{key}`: {body}"
        );
    }
    assert!(!strings.iter().any(|s| s == "on"), "{body}");
}

/// Fail-Open Check (#8992): the skill and tool parts do not depend on the
/// user scope, so an unreadable `.claude.json` — or none located at all —
/// still writes them.
#[test]
fn an_unreadable_claude_json_still_applies_skills_and_tools_deny() {
    let fx = Fx::new(USER);
    std::fs::write(fx.claude.join(".claude.json"), "{").unwrap();
    fx.owner("[mcp]\nallow = [\"apex\"]\n[skills]\noff = [\"xlsx\"]\n[tools]\ndeny = [\"NotebookEdit\"]\n");
    let expected = json!({
        "permissions": {"deny": ["NotebookEdit"]},
        "skillOverrides": {"xlsx": "off"}
    });
    let unreadable = fx.provision();
    assert_eq!(body_of(&unreadable), expected);
    let notice = unreadable.notice.expect("a notice");
    assert!(notice.contains("could not be applied"), "{notice}");
    let unlocated = fx.provision_with(None);
    assert_eq!(body_of(&unlocated), expected);
}

/// Fail-Open Check (#8992): an unwritable settings dir launches a skills-only
/// policy WITHOUT `--settings`, and the notice says the policy was not applied.
#[test]
fn an_unwritable_settings_dir_with_skills_launches_without_the_flag_and_says_so() {
    let fx = Fx::new(USER);
    fx.owner("[skills]\noff = [\"xlsx\"]\n");
    std::fs::create_dir_all(&fx.state).unwrap();
    std::fs::write(fx.settings_dir(), "not a dir").unwrap();
    let settings = fx.provision();
    assert_eq!(settings.file, None);
    assert!(settings.argv().is_empty());
    let notice = settings.notice.expect("a notice");
    assert!(
        notice.contains("owner launch policy NOT applied"),
        "{notice}"
    );
}

/// #8992: a bad `[skills]` table falls back to the last good skills and tools,
/// and the notice names the offending key.
#[test]
fn a_bad_skills_table_with_a_last_good_keeps_the_last_good_skills_and_floor() {
    let fx = Fx::new(USER);
    fx.owner("[skills]\noff = [\"xlsx\"]\n[tools]\ndeny = [\"NotebookEdit\"]\n");
    let good = body_of(&fx.provision());
    fx.owner("[skills]\noff = [\"xlsx\", \"tm-workflow\"]\n");
    let settings = fx.provision();
    assert_eq!(body_of(&settings), good);
    let notice = settings.notice.expect("a notice");
    assert!(notice.contains("using last good policy"), "{notice}");
    assert!(notice.contains("skills.off"), "{notice}");
}

/// #8992: a bad `[tools]` table with no last good skips the layer and names
/// the key, never refusing the launch.
#[test]
fn a_bad_tools_table_with_no_last_good_skips_the_layer_and_names_the_key() {
    let fx = Fx::new(USER);
    fx.owner("[tools]\ndeny = [\"Agent(engineer)\"]\n");
    let settings = fx.provision();
    assert!(settings.argv().is_empty());
    let notice = settings.notice.expect("a notice");
    assert!(notice.contains("owner layer skipped"), "{notice}");
    assert!(notice.contains("tools.deny"), "{notice}");
}

/// #8992: a last-good record written before a tool or skill joined the floor
/// has that name dropped when it is read back.
#[test]
fn a_last_good_tools_deny_naming_a_later_added_floor_tool_is_dropped() {
    let fx = Fx::new(USER);
    fx.plant_last_good(json!({
        "allow": null,
        "deny": [],
        "claude_ai_connectors": null,
        "skills_off": ["tm-workflow", "xlsx"],
        "tools_deny": ["Bash", "NotebookEdit", "mcp__trusty-search__search"]
    }));
    fx.owner("[mcp\n");
    let settings = fx.provision();
    assert_eq!(
        body_of(&settings),
        json!({
            "permissions": {"deny": ["NotebookEdit"]},
            "skillOverrides": {"xlsx": "off"}
        })
    );
}

/// Guard (#8992): a v1 record from before `skills_off`/`tools_deny` existed
/// still loads, so an upgrade keeps the fallback the operator already had.
#[test]
fn a_v1_last_good_record_without_the_new_fields_still_loads() {
    let fx = Fx::new(USER);
    fx.plant_last_good(json!({
        "allow": null,
        "deny": ["heygen"],
        "claude_ai_connectors": null
    }));
    fx.owner("[mcp\n");
    let settings = fx.provision();
    assert_eq!(
        body_of(&settings),
        json!({"deniedMcpServers": [{"serverName": "heygen"}]})
    );
    let notice = settings.notice.expect("a notice");
    assert!(notice.contains("using last good policy"), "{notice}");
}

/// #8992: `extends` unions `[skills] off` and `[tools] deny` down the chain.
#[test]
fn extends_unions_skills_and_tools() {
    let fx = Fx::new(USER);
    fx.owner_named(
        "base",
        "[skills]\noff = [\"xlsx\"]\n[tools]\ndeny = [\"NotebookEdit\"]\n",
    );
    fx.owner("extends = \"base\"\n[skills]\noff = [\"pdf\"]\n[tools]\ndeny = [\"mcp__heygen\"]\n");
    assert_eq!(
        body_of(&fx.provision()),
        json!({
            "permissions": {"deny": ["NotebookEdit", "mcp__heygen"]},
            "skillOverrides": {"pdf": "off", "xlsx": "off"}
        })
    );
}

/// #8992 Q-A: there is no `on`, and an empty child list removes nothing.
#[test]
fn a_child_cannot_remove_a_parents_skill_off() {
    let fx = Fx::new(USER);
    fx.owner_named("base", "[skills]\noff = [\"xlsx\"]\n");
    fx.owner("extends = \"base\"\n[skills]\noff = []\n");
    assert_eq!(
        body_of(&fx.provision()),
        json!({"skillOverrides": {"xlsx": "off"}})
    );
}

/// #8992: the owner `off` travels in the per-session file only; the project
/// tier's `skillOverrides` (#7751) keeps its bytes.
#[test]
fn the_project_tier_skill_overrides_file_is_never_touched() {
    let fx = Fx::new(USER);
    let project_settings = fx.project.join(".claude").join("settings.json");
    std::fs::create_dir_all(project_settings.parent().unwrap()).unwrap();
    let before = br#"{"skillOverrides":{"xlsx":"on","pdf":"off"}}"#;
    std::fs::write(&project_settings, before).unwrap();
    fx.owner("[skills]\noff = [\"xlsx\"]\n");
    let settings = fx.provision();
    assert_eq!(
        body_of(&settings),
        json!({"skillOverrides": {"xlsx": "off"}})
    );
    assert!(!settings.file.unwrap().starts_with(&fx.project));
    assert_eq!(std::fs::read(&project_settings).unwrap(), before);
}
