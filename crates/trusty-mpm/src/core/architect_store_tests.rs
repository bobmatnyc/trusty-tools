//! Tests for the daemon's Architect store resolution (#10944). Env and home
//! are injected; no test reads the process env or the real home.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use serde_json::json;
use trusty_architect::dashboard::DashError;

use super::*;

/// A temp home; `architect(name)` makes an Architect directory under it.
struct Home {
    dir: tempfile::TempDir,
}

impl Home {
    fn new() -> Self {
        Self {
            dir: tempfile::tempdir().expect("tempdir"),
        }
    }

    fn path(&self) -> PathBuf {
        std::fs::canonicalize(self.dir.path()).expect("canonical")
    }

    /// An Architect directory with the profile and, if `store`, its store dir.
    fn architect(&self, rel: &str, store: bool) -> PathBuf {
        let dir = self.path().join(rel);
        std::fs::create_dir_all(&dir).expect("mkdir");
        std::fs::write(dir.join(".trusty-mpm.toml"), "profile = \"supervisor\"\n")
            .expect("profile");
        if store {
            std::fs::create_dir_all(dir.join(STORE_SUBDIR)).expect("store");
        }
        dir
    }

    fn record(&self, dirs: &[&Path]) {
        let list = dirs
            .iter()
            .map(|d| format!("{:?}", d.display().to_string()))
            .collect::<Vec<_>>()
            .join(", ");
        self.write_config(&format!("[architect]\nprojects = [{list}]\n"));
    }

    fn write_config(&self, text: &str) {
        let path = user_config_path(&self.path());
        std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
        std::fs::write(path, text).expect("config");
    }
}

fn env_of(pairs: &[(&str, String)]) -> impl Fn(&str) -> Option<String> + use<> {
    let map: HashMap<String, String> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_owned(), v.clone()))
        .collect();
    move |name: &str| map.get(name).cloned()
}

fn no_env(_: &str) -> Option<String> {
    None
}

fn unavailable_text(r: Result<ResolvedRoot, DashError>) -> String {
    match r {
        Err(DashError::Unavailable(text)) => text,
        other => panic!("expected Unavailable, got {other:?}"),
    }
}

#[test]
fn explicit_store_root_is_never_resolved() {
    let params = json!({"store": {"root": "/x/y", "format": "jsonl"}, "collection": "items"});
    let (out, resolved) =
        normalize_params(params.clone(), || panic!("resolver ran")).expect("untouched");
    assert_eq!(out, params);
    assert_eq!(resolved, None);
}

#[test]
fn missing_store_gets_the_resolved_root_and_dir_format() {
    let root = ResolvedRoot {
        path: PathBuf::from("/a/local/owner-queue"),
        source: RootSource::Default,
    };
    let (out, resolved) =
        normalize_params(json!({"collection": "items"}), || Ok(root.clone())).expect("filled");
    assert_eq!(
        out,
        json!({"collection": "items", "store": {"root": "/a/local/owner-queue"}})
    );
    assert_eq!(resolved, Some(root.clone()));
    // The filled store decodes with the `dir` default, as an explicit one does.
    let sel: trusty_architect::dashboard::rpc::StoreSel =
        serde_json::from_value(out["store"].clone()).expect("decodes");
    assert_eq!(sel.format, trusty_architect::dashboard::store::Format::Dir);
    // Null params (no params at all) count as `{}`.
    let (out, _) = normalize_params(serde_json::Value::Null, || Ok(root)).expect("filled");
    assert_eq!(out, json!({"store": {"root": "/a/local/owner-queue"}}));
}

#[test]
fn store_without_root_keeps_format_actor_and_prune_log() {
    let root = ResolvedRoot {
        path: PathBuf::from("/r"),
        source: RootSource::EnvDashboardRoot,
    };
    let params = json!({"store": {"format": "jsonl", "actor": "me", "prune_log": "/p.jsonl"}});
    let (out, _) = normalize_params(params, || Ok(root)).expect("filled");
    assert_eq!(
        out,
        json!({"store": {"root": "/r", "format": "jsonl", "actor": "me", "prune_log": "/p.jsonl"}})
    );
}

#[test]
fn resolver_error_is_returned_and_non_object_params_are_untouched() {
    let err = normalize_params(json!({}), || Err(DashError::Unavailable("no".into())))
        .expect_err("propagated");
    assert!(matches!(err, DashError::Unavailable(_)), "{err:?}");
    let (out, resolved) = normalize_params(json!([1]), || panic!("resolver ran")).expect("same");
    assert_eq!((out, resolved), (json!([1]), None));
}

#[test]
fn env_dashboard_root_wins_over_config() {
    let home = Home::new();
    let recorded = home.architect("arch", true);
    home.record(&[&recorded]);
    let explicit = home.path().join("explicit");
    std::fs::create_dir_all(&explicit).expect("mkdir");
    let env = env_of(&[
        (ENV_DASHBOARD_ROOT, explicit.display().to_string()),
        (ENV_ARCHITECT_PROJECT_DIR, recorded.display().to_string()),
    ]);
    let got = resolve_store_root(&env, Some(&home.path())).expect("resolved");
    assert_eq!(got.path, explicit);
    assert_eq!(got.source, RootSource::EnvDashboardRoot);
    // Without DASHBOARD_ROOT, ARCHITECT_PROJECT_DIR wins over the config.
    let other = home.architect("other", true);
    let env = env_of(&[(ENV_ARCHITECT_PROJECT_DIR, other.display().to_string())]);
    let got = resolve_store_root(&env, Some(&home.path())).expect("resolved");
    assert_eq!(got.path, other.join(STORE_SUBDIR));
    assert_eq!(got.source, RootSource::EnvArchitectDir);
}

#[test]
fn recorded_architect_beats_the_default() {
    let home = Home::new();
    home.architect(DEFAULT_ARCHITECT_DIR, true);
    let recorded = home.architect("elsewhere/arch", true);
    home.record(&[&recorded]);
    let got = resolve_store_root(&no_env, Some(&home.path())).expect("resolved");
    assert_eq!(got.path, recorded.join(STORE_SUBDIR));
    assert_eq!(
        got.source,
        RootSource::Config(user_config_path(&home.path()))
    );
}

#[test]
fn default_dir_is_used_when_no_config() {
    let home = Home::new();
    let default = home.architect(DEFAULT_ARCHITECT_DIR, true);
    let got = resolve_store_root(&no_env, Some(&home.path())).expect("resolved");
    assert_eq!(got.path, default.join(STORE_SUBDIR));
    assert_eq!(got.source, RootSource::Default);
}

#[test]
fn missing_owner_queue_dir_is_an_error_not_empty() {
    let home = Home::new();
    let recorded = home.architect("arch", false);
    home.record(&[&recorded]);
    let text = unavailable_text(resolve_store_root(&no_env, Some(&home.path())));
    let tried = recorded.join(STORE_SUBDIR).display().to_string();
    assert!(text.contains(&tried), "{text}");
    assert!(text.contains("the Architect recorded in"), "{text}");
    assert!(text.contains("tm fleet init"), "{text}");
}

#[test]
fn unresolvable_root_names_the_path_and_source() {
    let home = Home::new();
    let text = unavailable_text(resolve_store_root(&no_env, Some(&home.path())));
    let tried = home.path().join(DEFAULT_ARCHITECT_DIR).join(STORE_SUBDIR);
    assert!(text.contains(&tried.display().to_string()), "{text}");
    assert!(text.contains("the default"), "{text}");
    assert!(text.contains("--architect-store"), "{text}");
    let env = env_of(&[(ENV_DASHBOARD_ROOT, "relative/store".to_owned())]);
    let text = unavailable_text(resolve_store_root(&env, Some(&home.path())));
    assert!(
        text.contains("relative/store (from env DASHBOARD_ROOT) is not an absolute path"),
        "{text}"
    );
}

#[test]
fn two_recorded_architects_are_refused() {
    let home = Home::new();
    let a = home.architect("a", true);
    let b = home.architect("b", true);
    home.record(&[&a, &b]);
    let text = unavailable_text(resolve_store_root(&no_env, Some(&home.path())));
    assert!(text.contains("more than one Architect"), "{text}");
    assert!(
        text.contains(&a.display().to_string()) && text.contains(&b.display().to_string()),
        "{text}"
    );
}

#[test]
fn recorded_dir_without_profile_is_refused_not_defaulted() {
    let home = Home::new();
    // A usable default exists; falling back to it would read and write it.
    home.architect(DEFAULT_ARCHITECT_DIR, true);
    let recorded = home.path().join("no-profile");
    std::fs::create_dir_all(recorded.join(STORE_SUBDIR)).expect("mkdir");
    home.record(&[&recorded]);
    let text = unavailable_text(resolve_store_root(&no_env, Some(&home.path())));
    assert!(text.contains(&recorded.display().to_string()), "{text}");
    assert!(text.contains("config.toml"), "{text}");
    assert!(text.contains("tm fleet init"), "{text}");
}

#[test]
fn malformed_config_is_refused_not_defaulted() {
    let home = Home::new();
    // A usable default exists; a defaulting loader would silently pick it.
    home.architect(DEFAULT_ARCHITECT_DIR, true);
    home.write_config("[architect\nprojects = 3\n");
    let text = unavailable_text(resolve_store_root(&no_env, Some(&home.path())));
    assert!(text.contains("is malformed"), "{text}");
    assert!(text.contains("config.toml"), "{text}");
}

#[test]
fn no_home_is_refused() {
    let text = unavailable_text(resolve_store_root(&no_env, None));
    assert!(text.contains("no home directory"), "{text}");
    let text = unavailable_text(resolve_store_root(&no_env, Some(Path::new("."))));
    assert!(text.contains("no home directory"), "{text}");
}

#[test]
fn root_that_is_a_file_is_refused() {
    let home = Home::new();
    let file = home.path().join("store-file");
    std::fs::write(&file, "x").expect("file");
    let env = env_of(&[(ENV_DASHBOARD_ROOT, file.display().to_string())]);
    let text = unavailable_text(resolve_store_root(&env, Some(&home.path())));
    assert!(text.contains("is not a directory"), "{text}");
}
