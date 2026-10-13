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
    fn provision(&self) -> LaunchSettings {
        provision_settings_at(&CarrierInputs {
            cwd: &self.project,
            repos_root: &self.repos,
            state_root: &self.state,
            claude_json_dir: Some(&self.claude),
        })
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
    fx.owner("[tools]\n");
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
    assert_eq!(first_body, json!({"deniedMcpServers": [{"serverName": "heygen"}]}));

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
